// Simple Image Viewer - A high-performance, cross-platform image viewer
// Copyright (C) 2024-2026 Simple Image Viewer Contributors
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! Disk-backed tiled HDR source for layers-only PSD/PSB documents.
//!
//! Used when an oversized PSB has a blank (or absent) flattened Image Data
//! section but drawable layers. Instead of compositing a full-canvas RGBA f32
//! buffer (which would defeat the point of disk tiling on multi-GB files),
//! each requested tile is composited on demand from the parsed layer stack via
//! [`crate::psb_hdr_tile_composite::composite_hdr_tile_with_visibility`].
//!
//! The per-record visibility mask (P2 strict / P2.5a Layer Comp / P2.5b
//! reveal) is resolved once at open time by
//! [`crate::psb_hdr_main::resolve_hdr_disk_visibility_plan`] using a geometric
//! drawable-output check (no full composite).
//!
//! Current scope is RGB 16/32 only; see `docs/psd-psb-known-limits.md`.

use memmap2::Mmap;
use parking_lot::Mutex;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::hdr::tiled::{
    HdrTileBuffer, HdrTileCache, HdrTiledSource, HdrTiledSourceKind,
    configured_hdr_tile_cache_max_bytes, hdr_preview_from_tiled_source_nearest,
    validate_tile_bounds,
};
use crate::hdr::types::{
    DEFAULT_SDR_WHITE_NITS, HdrColorProfile, HdrColorSpace, HdrImageMetadata, HdrLuminanceMetadata,
    HdrReference, HdrTransferFunction,
};
use crate::loader::PsdOsdInfo;
use crate::psb_hdr_tile_composite::composite_hdr_tile_with_visibility;
use crate::psb_icc_hdr::probe_icc_hdr;
use crate::psb_layer_composite::{LayerInfo, parse_layer_records_from_index};
use crate::psb_reader::extract_icc_profile_from_ir;
use crate::psb_section_index::PsdSectionIndex;

/// Disk-backed HDR source that composites tiles on demand from a PSD/PSB layer
/// stack.
pub struct PsbHdrTiledLayerSource {
    path: PathBuf,
    /// Keeps the memory mapping alive; `layer_info.channel_data` borrows into it.
    #[allow(dead_code)]
    mmap: Arc<Mmap>,
    /// Parsed layer stack. `channel_data` is a `'static` slice into `mmap`; see
    /// the SAFETY note in [`open_hdr_tiled_layers_source`].
    layer_info: LayerInfo<'static>,
    width: u32,
    height: u32,
    visible: Vec<bool>,
    osd: PsdOsdInfo,
    transfer: HdrTransferFunction,
    sdr_white_nits: f32,
    metadata: HdrImageMetadata,
    tile_cache: Mutex<HdrTileCache>,
}

impl std::fmt::Debug for PsbHdrTiledLayerSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PsbHdrTiledLayerSource")
            .field("path", &self.path)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("records", &self.layer_info.records.len())
            .field("osd", &self.osd)
            .finish()
    }
}

impl PsbHdrTiledLayerSource {
    /// OSD stage that produced the resolved visibility plan.
    pub(crate) fn osd(&self) -> PsdOsdInfo {
        self.osd.clone()
    }

    fn extract_tile_uncached(
        &self,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    ) -> Result<HdrTileBuffer, String> {
        composite_hdr_tile_with_visibility(
            &self.layer_info,
            &self.visible,
            x,
            y,
            width,
            height,
            self.transfer,
            self.sdr_white_nits,
            None,
        )
        .map_err(|e| e.as_str().to_string())
    }

    /// Strip-probe the composited output for any drawable (non-blank) pixel.
    ///
    /// The visibility plan already guarantees at least one geometrically
    /// drawable layer, but a fully transparent / zero-information composite
    /// should still degrade to SDR. Accumulates nonzero-RGB and nonzero-alpha
    /// across full-width row strips (independent per-strip checks are wrong
    /// when one strip is all-RGB-0 and another is all-alpha-0).
    pub(crate) fn probe_has_drawable_output(
        &self,
        cancel: Option<&AtomicBool>,
    ) -> Result<bool, crate::loader::DecodeError> {
        if self.width == 0 || self.height == 0 {
            return Ok(false);
        }
        let strip_rows = crate::constants::PSB_DISK_TILED_BLANK_PROBE_STRIP_ROWS;
        let mut any_rgb = false;
        let mut any_alpha = false;
        let mut y = 0u32;
        while y < self.height {
            crate::psb_reader::check_decode_cancel(cancel)?;
            let h = (self.height - y).min(strip_rows);
            let tile = composite_hdr_tile_with_visibility(
                &self.layer_info,
                &self.visible,
                0,
                y,
                self.width,
                h,
                self.transfer,
                self.sdr_white_nits,
                cancel,
            )?;
            feed_rgba32f_blank_flags(&tile.rgba_f32, &mut any_rgb, &mut any_alpha);
            if any_rgb && any_alpha {
                return Ok(true);
            }
            y = y.saturating_add(h);
        }
        Ok(any_rgb && any_alpha)
    }
}

/// Open a disk-backed HDR layer-composite source for an oversized PSD/PSB.
///
/// Returns `Err` when the document is not eligible (non-HDR depth, unsupported
/// color mode) or has no drawable visible layers, so the caller can degrade to
/// the SDR state machine.
pub fn open_hdr_tiled_layers_source(
    path: &Path,
    strategy: crate::settings::PsdHiddenLayerStrategy,
    cancel: Option<&AtomicBool>,
) -> Result<PsbHdrTiledLayerSource, String> {
    let file = {
        let mut opts = std::fs::OpenOptions::new();
        opts.read(true);
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::fs::OpenOptionsExt;
            const FILE_FLAG_RANDOM_ACCESS: u32 = 0x10000000;
            opts.custom_flags(FILE_FLAG_RANDOM_ACCESS);
        }
        opts.open(path)
            .map_err(|e| format!("Cannot open PSD/PSB HDR layer tiled file: {e}"))?
    };
    let mmap = Arc::new(unsafe { Mmap::map(&file).map_err(|e| format!("Mmap failed: {e}"))? });
    open_hdr_tiled_layers_source_from_mmap(path, mmap, strategy, cancel)
}

/// Build an HDR layer tiled source from an already-mapped file (checklist #29).
pub fn open_hdr_tiled_layers_source_from_mmap(
    path: &Path,
    mmap: Arc<Mmap>,
    strategy: crate::settings::PsdHiddenLayerStrategy,
    cancel: Option<&AtomicBool>,
) -> Result<PsbHdrTiledLayerSource, String> {
    let bytes: &[u8] = &mmap[..];

    let index = PsdSectionIndex::parse(bytes).map_err(|e| e.to_string())?;
    if index.depth != 16 && index.depth != 32 {
        return Err(format!(
            "PSD/PSB HDR layer tiled source requires 16 or 32-bit depth; got {}",
            index.depth
        ));
    }
    if index.color_mode != 3 {
        return Err(format!(
            "PSD/PSB HDR layer tiled source supports RGB color mode only; got {}",
            index.color_mode
        ));
    }

    let info = parse_layer_records_from_index(&index, bytes)?;
    if info.records.is_empty() {
        return Err("PSD/PSB HDR layer tiled source found no layer records".to_string());
    }

    let plan = crate::psb_hdr_main::resolve_hdr_disk_visibility_plan(
        &index, bytes, &info, cancel, strategy,
    )
    .map_err(|e| e.as_str().to_string())?;

    // Transfer function: 32-bit float PSD is scene-linear by spec; 16-bit uses
    // the probed ICC transfer only when the profile marks HDR.
    let embedded_icc = extract_icc_profile_from_ir(bytes, index.ir_start, index.ir_end);
    let icc_probe = embedded_icc
        .as_deref()
        .map(probe_icc_hdr)
        .unwrap_or_default();
    let transfer = if index.depth == 32 || !icc_probe.marks_hdr {
        HdrTransferFunction::Linear
    } else {
        icc_probe.transfer
    };
    let color_profile = if let Some(icc) = embedded_icc {
        HdrColorProfile::Icc(Arc::new(icc))
    } else {
        HdrColorProfile::LinearSrgb
    };
    let metadata = HdrImageMetadata {
        transfer_function: HdrTransferFunction::Linear,
        reference: HdrReference::DisplayReferred,
        color_profile,
        luminance: HdrLuminanceMetadata {
            mastering_max_nits: icc_probe.peak_nits,
            sdr_white_nits: Some(DEFAULT_SDR_WHITE_NITS),
            ..Default::default()
        },
        gain_map: None,
        raw_gpu_source: None,
    };

    let width = info.width;
    let height = info.height;

    let LayerInfo {
        records,
        channel_data,
        width: info_w,
        height: info_h,
        depth,
        color_mode,
        is_psb,
        cmyk_icc,
    } = info;
    // SAFETY: `channel_data` points into the immutable, heap-stable memory map
    // owned by `mmap`. That `Arc<Mmap>` is stored in the returned struct and is
    // never mutated or unmapped while the struct lives, so extending the slice
    // to `'static` keeps it valid for the lifetime of the source.
    let channel_data: &'static [u8] =
        unsafe { std::mem::transmute::<&[u8], &'static [u8]>(channel_data) };
    let layer_info = LayerInfo {
        records,
        channel_data,
        width: info_w,
        height: info_h,
        depth,
        color_mode,
        is_psb,
        cmyk_icc,
    };

    if plan.visible.len() != layer_info.records.len() {
        return Err("PSD/PSB HDR layer tiled visibility mask length mismatch".to_string());
    }

    Ok(PsbHdrTiledLayerSource {
        path: path.to_path_buf(),
        mmap,
        layer_info,
        width,
        height,
        visible: plan.visible,
        osd: plan.osd,
        transfer,
        sdr_white_nits: DEFAULT_SDR_WHITE_NITS,
        metadata,
        tile_cache: Mutex::new(HdrTileCache::new(configured_hdr_tile_cache_max_bytes())),
    })
}

impl HdrTiledSource for PsbHdrTiledLayerSource {
    fn source_kind(&self) -> HdrTiledSourceKind {
        HdrTiledSourceKind::DiskBacked
    }

    fn source_name(&self) -> String {
        self.path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string())
    }

    fn width(&self) -> u32 {
        self.width
    }

    fn height(&self) -> u32 {
        self.height
    }

    fn color_space(&self) -> HdrColorSpace {
        HdrColorSpace::LinearSrgb
    }

    fn metadata(&self) -> HdrImageMetadata {
        self.metadata.clone()
    }

    fn generate_hdr_preview(
        &self,
        max_w: u32,
        max_h: u32,
    ) -> Result<crate::hdr::types::HdrImageBuffer, String> {
        hdr_preview_from_tiled_source_nearest(self, max_w, max_h)
    }

    fn generate_sdr_preview(&self, max_w: u32, max_h: u32) -> Result<(u32, u32, Vec<u8>), String> {
        let preview = self.generate_hdr_preview(max_w, max_h)?;
        let pixels = crate::hdr::decode::hdr_to_sdr_rgba8(&preview, 0.0)?;
        Ok((preview.width, preview.height, pixels))
    }

    fn cached_tile_rgba32f_arc(
        &self,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    ) -> Option<Arc<HdrTileBuffer>> {
        self.tile_cache.lock().get((x, y, width, height))
    }

    fn protect_cached_tiles(&self, tiles: &[(u32, u32, u32, u32)]) {
        self.tile_cache
            .lock()
            .set_protected_keys(tiles.iter().copied());
    }

    fn extract_tile_rgba32f_arc(
        &self,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    ) -> Result<Arc<HdrTileBuffer>, String> {
        validate_tile_bounds(self.width, self.height, x, y, width, height)?;
        let key = (x, y, width, height);
        {
            let mut cache = self.tile_cache.lock();
            if let Some(tile) = cache.get(key) {
                return Ok(tile);
            }
        }
        let tile = Arc::new(self.extract_tile_uncached(x, y, width, height)?);
        self.tile_cache.lock().insert(key, Arc::clone(&tile));
        Ok(tile)
    }
}

fn feed_rgba32f_blank_flags(pixels: &[f32], any_rgb: &mut bool, any_alpha: &mut bool) {
    const EPS: f32 = 1e-8;
    let mut i = 0usize;
    while i + 4 <= pixels.len() {
        if pixels[i].abs() > EPS || pixels[i + 1].abs() > EPS || pixels[i + 2].abs() > EPS {
            *any_rgb = true;
        }
        if pixels[i + 3].abs() > EPS {
            *any_alpha = true;
        }
        if *any_rgb && *any_alpha {
            return;
        }
        i += 4;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::PsdHiddenLayerStrategy;

    /// Build a tiny layers-only PSD: one visible 32-bit float RGBA layer
    /// covering the whole `width x height` canvas over a blank flat section.
    fn write_temp_layers_only_psd_32(
        name: &str,
        width: u32,
        height: u32,
        rgba: [f32; 4],
    ) -> PathBuf {
        let layer_pixels = (width * height) as usize;
        let plane = |value: f32| {
            let mut ch = vec![0u8, 0u8]; // compression 0 (raw)
            for _ in 0..layer_pixels {
                ch.extend_from_slice(&value.to_be_bytes());
            }
            ch
        };
        // Photoshop layer channel order: alpha (-1), R (0), G (1), B (2).
        let channels = [
            (-1i16, plane(rgba[3])),
            (0i16, plane(rgba[0])),
            (1i16, plane(rgba[1])),
            (2i16, plane(rgba[2])),
        ];
        // Layer extra: empty mask, empty blending ranges, empty padded name.
        let extra = vec![0u8; 12];

        let mut layer_record = Vec::new();
        layer_record.extend_from_slice(&0i32.to_be_bytes()); // top
        layer_record.extend_from_slice(&0i32.to_be_bytes()); // left
        layer_record.extend_from_slice(&(height as i32).to_be_bytes()); // bottom
        layer_record.extend_from_slice(&(width as i32).to_be_bytes()); // right
        layer_record.extend_from_slice(&(channels.len() as u16).to_be_bytes());
        for (id, data) in &channels {
            layer_record.extend_from_slice(&id.to_be_bytes());
            layer_record.extend_from_slice(&(data.len() as u32).to_be_bytes());
        }
        layer_record.extend_from_slice(b"8BIM");
        layer_record.extend_from_slice(b"norm");
        layer_record.extend_from_slice(&[255, 0, 0, 0]); // opacity, clipping, flags, filler
        layer_record.extend_from_slice(&(extra.len() as u32).to_be_bytes());
        layer_record.extend_from_slice(&extra);

        let mut layer_info = Vec::new();
        layer_info.extend_from_slice(&1i16.to_be_bytes());
        layer_info.extend_from_slice(&layer_record);
        for (_, data) in &channels {
            layer_info.extend_from_slice(data);
        }

        let mut layer_mask_info = Vec::new();
        layer_mask_info.extend_from_slice(&(layer_info.len() as u32).to_be_bytes());
        layer_mask_info.extend_from_slice(&layer_info);

        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"8BPS");
        bytes.extend_from_slice(&1u16.to_be_bytes()); // PSD version 1
        bytes.extend_from_slice(&[0u8; 6]);
        bytes.extend_from_slice(&3u16.to_be_bytes()); // channels
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&32u16.to_be_bytes()); // depth
        bytes.extend_from_slice(&3u16.to_be_bytes()); // RGB
        bytes.extend_from_slice(&0u32.to_be_bytes()); // color mode data
        bytes.extend_from_slice(&0u32.to_be_bytes()); // image resources
        bytes.extend_from_slice(&(layer_mask_info.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&layer_mask_info);
        bytes.extend_from_slice(&0u16.to_be_bytes()); // raw flat Image Data
        bytes.extend(std::iter::repeat_n(0u8, layer_pixels * 3 * 4)); // blank flat

        let mut path = std::env::temp_dir();
        path.push(format!(
            "{name}_{}_{}.psd",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, bytes).expect("write temp layers PSD");
        path
    }

    #[test]
    fn open_layers_source_extracts_red_tile() {
        let path =
            write_temp_layers_only_psd_32("psb_hdr_tiled_layers_red", 2, 2, [1.0, 0.1, 0.1, 1.0]);
        let source = open_hdr_tiled_layers_source(&path, PsdHiddenLayerStrategy::Heuristic, None)
            .expect("open layers-only HDR tiled source");
        assert_eq!(source.source_kind(), HdrTiledSourceKind::DiskBacked);
        assert_eq!(source.osd(), crate::loader::PsdOsdInfo::p2_strict());
        assert!(
            source
                .probe_has_drawable_output(None)
                .expect("probe drawable")
        );

        let tile = source
            .extract_tile_rgba32f_arc(0, 0, 2, 2)
            .expect("extract 2x2 tile");
        assert_eq!((tile.width, tile.height), (2, 2));
        // Top-left pixel is the opaque red layer sample.
        let px = &tile.rgba_f32[0..4];
        assert!(
            px[0] > px[1] && px[0] > px[2],
            "expected red-dominant: {px:?}"
        );
        assert!((px[3] - 1.0).abs() < 1e-4, "expected opaque alpha: {px:?}");
        let _ = std::fs::remove_file(path);
    }
}
