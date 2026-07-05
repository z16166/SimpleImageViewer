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

//! [`TiledImageSource`] implementations: in-memory raster, HDR SDR fallback, LibRAW refinement.

use crossbeam_channel::Sender;
use image::{DynamicImage, GenericImageView};
use parking_lot::{Condvar, Mutex, RwLock as PLRwLock};
use std::path::PathBuf;
use std::sync::Arc;

use crate::constants::RGBA_CHANNELS;
use crate::loader::types::{
    DecodedImage, RefinementRequest, TiledImageSource, source_key_for_path,
};
use simple_image_viewer::simd_downsample::downsample_rgba8_box;

/// Aspect-preserving downscale for in-memory RGBA8 tiles.
///
/// Uses a SIMD-accelerated box-filter (area-averaging) downsample instead of
/// [`image::imageops::resize`] with Triangle filtering.  Strip thumbnail quality
/// requirements are modest and box filtering is a better match for downscaling.
fn memory_rgba_preview(
    width: u32,
    height: u32,
    pixels: &[u8],
    max_w: u32,
    max_h: u32,
) -> (u32, u32, Vec<u8>) {
    if width == 0 || height == 0 {
        return (0, 0, Vec::new());
    }
    // Guard against mismatched buffer length (downstream SIMD paths use
    // get_unchecked which is UB when the slice is too short).
    if pixels.len() < (width as usize * height as usize * 4) {
        return (0, 0, Vec::new());
    }
    let scale = (max_w as f64 / width as f64)
        .min(max_h as f64 / height as f64)
        .min(1.0);
    let out_w = (width as f64 * scale).round().max(1.0) as u32;
    let out_h = (height as f64 * scale).round().max(1.0) as u32;
    let out = downsample_rgba8_box(pixels, width, height, out_w, out_h);
    let Some(expected_out_bytes) = out_w
        .checked_mul(out_h)
        .and_then(|px| px.checked_mul(4))
        .map(|len| len as usize)
    else {
        return (0, 0, Vec::new());
    };
    if out.len() != expected_out_bytes {
        return (0, 0, Vec::new());
    }
    crate::preload_debug!(
        "[PreloadDebug][Strip] memory preview logical={}x{} max={}x{} -> {}x{}",
        width,
        height,
        max_w,
        max_h,
        out_w,
        out_h
    );
    (out_w, out_h, out)
}

/// A TiledImageSource that serves tiles from an in-memory byte buffer.
/// Primarily used for common formats (PNG, JPEG, etc.) that exceed the GPU's single texture limit.
pub(crate) struct MemoryImageSource {
    width: u32,
    height: u32,
    pixels: Arc<Vec<u8>>,
    hdr_sdr_fallback: bool,
}

impl MemoryImageSource {
    pub(crate) fn new(width: u32, height: u32, pixels: Arc<Vec<u8>>) -> Self {
        Self::new_with_hdr_sdr_fallback(width, height, pixels, false)
    }

    pub(crate) fn new_with_hdr_sdr_fallback(
        width: u32,
        height: u32,
        pixels: Arc<Vec<u8>>,
        hdr_sdr_fallback: bool,
    ) -> Self {
        Self {
            width,
            height,
            pixels,
            hdr_sdr_fallback,
        }
    }
}

pub(crate) struct HdrSdrTiledFallbackSource {
    source: Arc<dyn crate::hdr::tiled::HdrTiledSource>,
}

impl HdrSdrTiledFallbackSource {
    pub(crate) fn new(source: Arc<dyn crate::hdr::tiled::HdrTiledSource>) -> Self {
        Self { source }
    }
}

impl TiledImageSource for HdrSdrTiledFallbackSource {
    fn width(&self) -> u32 {
        self.source.width()
    }

    fn height(&self) -> u32 {
        self.source.height()
    }

    fn is_hdr_sdr_fallback(&self) -> bool {
        true
    }

    fn extract_tile(&self, x: u32, y: u32, w: u32, h: u32) -> Arc<Vec<u8>> {
        let _ = (x, y, self);
        let byte_len = w as usize * h as usize * 4;
        let mut pixels = vec![0_u8; byte_len];
        for px in pixels.chunks_exact_mut(4) {
            px[3] = 255;
        }
        Arc::new(pixels)
    }

    fn generate_preview(&self, max_w: u32, max_h: u32) -> (u32, u32, Vec<u8>) {
        self.source
            .generate_sdr_preview(max_w, max_h)
            .unwrap_or_else(|err| {
                log::warn!("[Loader] HDR SDR preview fallback failed: {err}");
                let scale = (max_w as f32 / self.width() as f32)
                    .min(max_h as f32 / self.height() as f32)
                    .min(1.0);
                let width = ((self.width() as f32 * scale).round() as u32).max(1);
                let height = ((self.height() as f32 * scale).round() as u32).max(1);
                (width, height, vec![0; width as usize * height as usize * 4])
            })
    }

    fn full_pixels(&self) -> Option<Arc<Vec<u8>>> {
        None
    }
}

impl TiledImageSource for MemoryImageSource {
    fn width(&self) -> u32 {
        self.width
    }

    fn height(&self) -> u32 {
        self.height
    }

    fn is_hdr_sdr_fallback(&self) -> bool {
        self.hdr_sdr_fallback
    }

    fn extract_tile(&self, x: u32, y: u32, w: u32, h: u32) -> Arc<Vec<u8>> {
        let mut tile_pixels = Vec::with_capacity((w * h * 4) as usize);
        let stride = self.width as usize * 4;

        for row in y..(y + h) {
            let start = (row as usize * stride) + (x as usize * 4);
            let end = start + (w as usize * 4);
            if end <= self.pixels.len() {
                tile_pixels.extend_from_slice(&self.pixels[start..end]);
            } else {
                // Safety fallback for out-of-bounds
                tile_pixels.resize(tile_pixels.len() + (w * 4) as usize, 0);
            }
        }
        Arc::new(tile_pixels)
    }

    fn generate_preview(&self, max_w: u32, max_h: u32) -> (u32, u32, Vec<u8>) {
        memory_rgba_preview(self.width, self.height, &self.pixels, max_w, max_h)
    }

    fn generate_full_image_preview(&self, max_w: u32, max_h: u32) -> (u32, u32, Vec<u8>) {
        memory_rgba_preview(self.width, self.height, &self.pixels, max_w, max_h)
    }

    fn full_pixels(&self) -> Option<Arc<Vec<u8>>> {
        Some(Arc::clone(&self.pixels))
    }

    fn exif_orientation_rotate_in_memory_rgba(&self) -> bool {
        !self.hdr_sdr_fallback
    }
}

// ---------------------------------------------------------------------------
// RAW HDR tiled source (scene-linear buffer filled by async HQ demosaic)
// ---------------------------------------------------------------------------

pub(crate) struct RawHdrRefiningSource {
    buffer: Arc<PLRwLock<Option<crate::hdr::types::HdrImageBuffer>>>,
    logical_width: u32,
    logical_height: u32,
}

impl RawHdrRefiningSource {
    pub(crate) fn new(
        buffer: Arc<PLRwLock<Option<crate::hdr::types::HdrImageBuffer>>>,
        logical_width: u32,
        logical_height: u32,
    ) -> Self {
        Self {
            buffer,
            logical_width,
            logical_height,
        }
    }
}

impl crate::hdr::tiled::HdrTiledSource for RawHdrRefiningSource {
    fn source_kind(&self) -> crate::hdr::tiled::HdrTiledSourceKind {
        crate::hdr::tiled::HdrTiledSourceKind::InMemory
    }

    fn width(&self) -> u32 {
        self.logical_width
    }

    fn height(&self) -> u32 {
        self.logical_height
    }

    fn color_space(&self) -> crate::hdr::types::HdrColorSpace {
        crate::hdr::types::HdrColorSpace::LinearSrgb
    }

    fn metadata(&self) -> crate::hdr::types::HdrImageMetadata {
        crate::raw_processor::raw_scene_linear_metadata()
    }

    fn generate_hdr_preview(
        &self,
        max_w: u32,
        max_h: u32,
    ) -> Result<crate::hdr::types::HdrImageBuffer, String> {
        let guard = self.buffer.read();
        let image = guard
            .as_ref()
            .ok_or_else(|| "RAW HDR buffer not yet refined".to_string())?;
        crate::hdr::tiled::downsample_hdr_image_nearest(image, max_w, max_h)
    }

    fn generate_sdr_preview(&self, max_w: u32, max_h: u32) -> Result<(u32, u32, Vec<u8>), String> {
        let guard = self.buffer.read();
        let image = guard
            .as_ref()
            .ok_or_else(|| "RAW HDR buffer not yet refined".to_string())?;
        crate::hdr::tiled::sdr_preview_from_hdr_image_nearest(image, max_w, max_h)
    }

    fn extract_tile_rgba32f_arc(
        &self,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    ) -> Result<Arc<crate::hdr::tiled::HdrTileBuffer>, String> {
        let guard = self.buffer.read();
        let image = guard
            .as_ref()
            .ok_or_else(|| "RAW HDR buffer not yet refined".to_string())?;
        crate::hdr::tiled::validate_tile_bounds(image.width, image.height, x, y, width, height)?;

        let mut tile = Vec::with_capacity((width as usize) * (height as usize) * 4);
        let source_stride = image.width as usize * 4;
        let row_len = width as usize * 4;
        let start_x = x as usize * 4;

        for row in y..(y + height) {
            let start = row as usize * source_stride + start_x;
            let end = start + row_len;
            tile.extend_from_slice(&image.rgba_f32[start..end]);
        }

        Ok(Arc::new(
            crate::hdr::tiled::HdrTileBuffer::new_with_metadata(
                width,
                height,
                image.color_space,
                image.metadata.clone(),
                Arc::new(tile),
            ),
        ))
    }

    fn defers_loader_hq_preview(&self) -> bool {
        true
    }
}

// ---------------------------------------------------------------------------
// RAW Image Support (LibRaw)
// ---------------------------------------------------------------------------

pub(crate) struct RawImageSource {
    path: PathBuf,
    /// True RAW sensor dimensions (not thumbnail dimensions).
    width: u32,
    height: u32,
    /// Initially holds the embedded preview at its ORIGINAL resolution (NOT upscaled).
    /// After HQ refinement, holds the full demosaiced preview at develop resolution.
    developed_image: Arc<PLRwLock<Option<DynamicImage>>>,
    refine_tx: Sender<RefinementRequest>,
    orientation_override: i32,
    /// When false, [`Self::request_refinement`] is a no-op (performance mode uses embedded only).
    needs_refinement: bool,
    hdr_target_capacity: f32,
    hdr_tone_map: crate::hdr::types::HdrToneMapSettings,
    hdr_developed_image: Option<Arc<PLRwLock<Option<crate::hdr::types::HdrImageBuffer>>>>,
}

pub(crate) struct RawImageSourceParams {
    pub(crate) raw_width: u32,
    pub(crate) raw_height: u32,
    pub(crate) refine_tx: Sender<RefinementRequest>,
    pub(crate) orientation_override: i32,
    pub(crate) needs_refinement: bool,
    pub(crate) hdr_target_capacity: f32,
    pub(crate) hdr_tone_map: crate::hdr::types::HdrToneMapSettings,
    pub(crate) hdr_developed_image:
        Option<Arc<PLRwLock<Option<crate::hdr::types::HdrImageBuffer>>>>,
}

impl RawImageSource {
    pub(crate) fn new(
        path: PathBuf,
        preview: DecodedImage,
        params: RawImageSourceParams,
    ) -> Result<Self, String> {
        let RawImageSourceParams {
            raw_width,
            raw_height,
            refine_tx,
            orientation_override,
            needs_refinement,
            hdr_target_capacity,
            hdr_tone_map,
            hdr_developed_image,
        } = params;
        // IMPORTANT: Store preview at its ORIGINAL resolution — NO upscaling!
        // Previously this called resize_exact(raw_width, raw_height) which allocated
        // ~400MB per image (e.g. 11648×8736×4). With rapid switching and prefetching,
        // multiple concurrent allocations of this size caused OOM crashes.
        // Instead, extract_tile() maps coordinates from RAW space to preview space on demand.
        //
        // ALSO: We do NOT send a refinement request here. Refinement is deferred until
        // the image becomes the actively-viewed one (via request_refinement()). This
        // prevents prefetched images from each spawning ~400MB LibRaw develop tasks.

        let rgba = preview.into_rgba8_image().map_err(|err| {
            format!(
                "RAW preview buffer is invalid for {}: {}",
                path.display(),
                err
            )
        })?;
        let developed_image = Arc::new(PLRwLock::new(Some(DynamicImage::ImageRgba8(rgba))));

        let refine_tx = refine_tx.clone();

        Ok(Self {
            path,
            width: raw_width,
            height: raw_height,
            developed_image,
            refine_tx,
            orientation_override,
            needs_refinement,
            hdr_target_capacity,
            hdr_tone_map,
            hdr_developed_image,
        })
    }
}

impl TiledImageSource for RawImageSource {
    fn width(&self) -> u32 {
        self.width
    }

    fn height(&self) -> u32 {
        self.height
    }

    fn extract_tile(&self, x: u32, y: u32, w: u32, h: u32) -> Arc<Vec<u8>> {
        let img_lock = self.developed_image.read();
        if let Some(ref img) = *img_lock {
            let (iw, ih) = img.dimensions();
            if iw == self.width && ih == self.height {
                // Full-res developed image available — direct crop, no scaling needed.
                if let Some(rgba) = img.as_rgba8() {
                    let mut result = vec![0u8; (w * h * 4) as usize];
                    for row in 0..h {
                        let src_y = y + row;
                        let src_offset = (src_y * iw + x) as usize * 4;
                        let dst_offset = (row * w) as usize * 4;
                        let len =
                            (w as usize * 4).min(rgba.as_raw().len().saturating_sub(src_offset));
                        if len > 0 {
                            result[dst_offset..dst_offset + len]
                                .copy_from_slice(&rgba.as_raw()[src_offset..src_offset + len]);
                        }
                    }
                    Arc::new(result)
                } else {
                    let crop = img.crop_imm(x, y, w, h);
                    Arc::new(crop.into_rgba8().into_raw())
                }
            } else {
                // Preview image (smaller than RAW dimensions).
                let scale_x = iw as f64 / self.width as f64;
                let scale_y = ih as f64 / self.height as f64;
                let px = (x as f64 * scale_x) as u32;
                let py = (y as f64 * scale_y) as u32;
                let pw = ((w as f64 * scale_x).ceil() as u32)
                    .min(iw.saturating_sub(px))
                    .max(1);
                let ph = ((h as f64 * scale_y).ceil() as u32)
                    .min(ih.saturating_sub(py))
                    .max(1);
                let crop = img.crop_imm(px, py, pw, ph);
                let resized = crop.resize_exact(w, h, image::imageops::FilterType::Lanczos3);
                Arc::new(resized.into_rgba8().into_raw())
            }
        } else {
            Arc::new(vec![0; (w * h * RGBA_CHANNELS as u32) as usize])
        }
    }

    fn generate_preview(&self, max_w: u32, max_h: u32) -> (u32, u32, Vec<u8>) {
        let img_lock = self.developed_image.read();
        if let Some(ref img) = *img_lock {
            let scaled = img.thumbnail(max_w, max_h);
            let rgba = scaled.to_rgba8();
            (rgba.width(), rgba.height(), rgba.into_raw())
        } else {
            (0, 0, Vec::new())
        }
    }

    fn full_pixels(&self) -> Option<Arc<Vec<u8>>> {
        let img_lock = self.developed_image.read();
        if let Some(ref img) = *img_lock {
            let (iw, ih) = img.dimensions();
            // Only return pixels when we have the full-res developed image.
            // If it's still the small preview, the stride would mismatch
            // self.width/self.height and corrupt downstream consumers (e.g. printing).
            if iw == self.width && ih == self.height {
                Some(Arc::new(img.to_rgba8().into_raw()))
            } else {
                None
            }
        } else {
            None
        }
    }

    fn request_refinement(&self, index: usize, decode_profile: crate::loader::DecodeProfile) {
        if !self.needs_refinement {
            crate::preload_debug!(
                "[PreloadDebug][RAW] refine_skip idx={} reason=needs_refinement_false path={}",
                index,
                self.path.display()
            );
            log::debug!(
                "[RawImageSource] Skipping refinement for {:?} (performance mode / embedded-only)",
                self.path.file_name().unwrap_or_default()
            );
            return;
        }
        crate::preload_debug!(
            "[PreloadDebug][RAW] refine_queue idx={} hdr_cap={:.3} path={}",
            index,
            self.hdr_target_capacity,
            self.path.display()
        );
        log::debug!(
            "[RawImageSource] Triggering HQ refinement for index={}",
            index
        );
        let _ = self.refine_tx.send(RefinementRequest {
            path: self.path.clone(),
            index,
            decode_profile,
            source_key: source_key_for_path(&self.path),
            orientation_override: Some(self.orientation_override),
            logical_width: self.width,
            logical_height: self.height,
            developed_image: self.developed_image.clone(),
            hdr_developed_image: self.hdr_developed_image.clone(),
            hdr_target_capacity: self.hdr_target_capacity,
            hdr_tone_map: self.hdr_tone_map,
        });
    }

    fn defers_loader_hq_preview(&self) -> bool {
        self.needs_refinement
    }
}

// ---------------------------------------------------------------------------
// PSD v1 (async full decode on the refinement pool; placeholder until pixels land)
// ---------------------------------------------------------------------------

const PSD_V1_PLACEHOLDER_RGBA: [u8; 4] = [32, 32, 32, 255];

#[derive(Debug)]
enum PsdV1DecodeState {
    Pending,
    Ready,
    Failed(String),
}

pub(crate) struct PsdV1LoadNotify {
    pub index: usize,
    pub decode_profile: crate::loader::DecodeProfile,
    pub source_key: crate::loader::SourceKey,
    pub load_tx: crate::loader::orchestrator::LoaderOutputSender,
}

pub(crate) struct PsdV1AsyncSource {
    width: u32,
    height: u32,
    pixels: Arc<PLRwLock<Option<Arc<Vec<u8>>>>>,
    decode_state: Arc<(Mutex<PsdV1DecodeState>, Condvar)>,
}

impl PsdV1AsyncSource {
    pub(crate) fn new(
        mmap: memmap2::Mmap,
        path: std::path::PathBuf,
        width: u32,
        height: u32,
        notify: Option<PsdV1LoadNotify>,
    ) -> Arc<Self> {
        use crate::loader::preview_caps::REFINEMENT_POOL;
        use crate::loader::{ImageData, apply_exif_orientation_to_image_data};

        let mmap = Arc::new(mmap);
        let pixels = Arc::new(PLRwLock::new(None));
        let decode_state = Arc::new((Mutex::new(PsdV1DecodeState::Pending), Condvar::new()));
        let decode_mmap = Arc::clone(&mmap);
        let decode_pixels = Arc::clone(&pixels);
        let decode_state_worker = Arc::clone(&decode_state);

        REFINEMENT_POOL.spawn(move || {
            let finish = |state: PsdV1DecodeState| {
                let (lock, cvar) = &*decode_state_worker;
                *lock.lock() = state;
                cvar.notify_all();
            };

            let result = crate::hdr::exr_tiled::catch_exr_panic("PSD v1 decode", || {
                let psd_file = psd::Psd::from_bytes(&decode_mmap[..])
                    .map_err(|e| format!("Failed to parse PSD: {e}"))?;
                Ok((psd_file.width(), psd_file.height(), psd_file.rgba()))
            });

            match result {
                Ok((w, h, px)) => {
                    let img = DecodedImage::new(w, h, px);
                    let oriented_pixels = match apply_exif_orientation_to_image_data(
                        &path,
                        ImageData::Static(img),
                        Some(&decode_mmap[..]),
                    ) {
                        ImageData::Static(oriented) => oriented.into_arc_pixels(),
                        ImageData::Tiled(source) => {
                            source.full_pixels().unwrap_or_else(|| Arc::new(Vec::new()))
                        }
                        other => {
                            log::error!(
                                "[Loader] PSD v1 unexpected oriented shape for {}: {:?}",
                                path.display(),
                                std::mem::discriminant(&other)
                            );
                            finish(PsdV1DecodeState::Failed(
                                "PSD v1 orientation produced unexpected image shape".into(),
                            ));
                            return;
                        }
                    };
                    if oriented_pixels.is_empty() {
                        finish(PsdV1DecodeState::Failed(
                            "PSD v1 decode produced empty pixel buffer".into(),
                        ));
                        return;
                    }
                    *decode_pixels.write() = Some(Arc::clone(&oriented_pixels));
                    finish(PsdV1DecodeState::Ready);
                    if let Some(notify) = notify {
                        notify_psd_v1_decode_complete(notify, width, height, &oriented_pixels);
                    }
                }
                Err(e) => {
                    const PSD_DECODE_PANIC_PREFIX: &str = "PSD v1 decode: decoder panic: ";
                    if let Some(msg) = e.strip_prefix(PSD_DECODE_PANIC_PREFIX) {
                        log::error!(
                            "[Loader] PSD decoder panicked for {}: {}",
                            path.display(),
                            msg
                        );
                    } else {
                        log::error!("[Loader] PSD decode failed for {}: {e}", path.display());
                    }
                    finish(PsdV1DecodeState::Failed(e));
                }
            }
        });

        Arc::new(Self {
            width,
            height,
            pixels,
            decode_state,
        })
    }
}

fn notify_psd_v1_decode_complete(
    notify: PsdV1LoadNotify,
    width: u32,
    height: u32,
    pixels: &Arc<Vec<u8>>,
) {
    use crate::loader::preview_caps::hq_preview_max_side;
    use crate::loader::{LoaderOutput, PreviewBundle, PreviewResult};

    let limit = hq_preview_max_side();
    let (pw, ph, preview_pixels) = memory_rgba_preview(width, height, pixels, limit, limit);
    if pw > 0 && ph > 0 {
        let preview = DecodedImage::new(pw, ph, preview_pixels);
        let _ = notify.load_tx.send(LoaderOutput::Preview(PreviewResult {
            index: notify.index,
            decode_profile: notify.decode_profile.clone(),
            source_key: notify.source_key,
            preview_bundle: PreviewBundle::refined().with_sdr(preview),
            error: None,
            cpu_demosaic_ms: None,
            raw_bootstrap_osd: None,
            sdr_texture_tag: Some(crate::loader::TexturePreviewBufferTag::TiledRefinedLoader),
        }));
    }
}

impl TiledImageSource for PsdV1AsyncSource {
    fn width(&self) -> u32 {
        self.width
    }

    fn height(&self) -> u32 {
        self.height
    }

    fn extract_tile(&self, x: u32, y: u32, w: u32, h: u32) -> Arc<Vec<u8>> {
        if let Some(px) = self.pixels.read().as_ref() {
            let mut tile_pixels = Vec::with_capacity((w * h * 4) as usize);
            let stride = self.width as usize * 4;
            for row in y..(y + h) {
                let start = row as usize * stride + x as usize * 4;
                let end = start + w as usize * 4;
                if end <= px.len() {
                    tile_pixels.extend_from_slice(&px[start..end]);
                } else {
                    tile_pixels.resize(tile_pixels.len() + w as usize * 4, 0);
                }
            }
            return Arc::new(tile_pixels);
        }
        let mut tile_pixels = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..(w * h) {
            tile_pixels.extend_from_slice(&PSD_V1_PLACEHOLDER_RGBA);
        }
        Arc::new(tile_pixels)
    }

    fn generate_preview(&self, max_w: u32, max_h: u32) -> (u32, u32, Vec<u8>) {
        if let Some(px) = self.pixels.read().as_ref() {
            return memory_rgba_preview(self.width, self.height, px, max_w, max_h);
        }
        // Do not synthesize a solid-color bootstrap preview: the loader would upload it
        // and the async HQ preview would flash gray -> image on the first frame.
        (0, 0, Vec::new())
    }

    fn generate_full_image_preview(&self, max_w: u32, max_h: u32) -> (u32, u32, Vec<u8>) {
        self.generate_preview(max_w, max_h)
    }

    fn full_pixels(&self) -> Option<Arc<Vec<u8>>> {
        self.pixels.read().clone()
    }

    fn defers_loader_hq_preview(&self) -> bool {
        self.pixels.read().is_none()
    }

    fn wait_for_async_pixels(&self, timeout: std::time::Duration) -> Result<(), String> {
        let (lock, cvar) = &*self.decode_state;
        let mut state = lock.lock();
        let deadline = std::time::Instant::now() + timeout;
        while matches!(*state, PsdV1DecodeState::Pending) {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err("PSD decode timed out waiting for async pixels".into());
            }
            cvar.wait_for(&mut state, remaining);
        }
        match &*state {
            PsdV1DecodeState::Ready => Ok(()),
            PsdV1DecodeState::Failed(msg) => Err(msg.clone()),
            PsdV1DecodeState::Pending => {
                Err("PSD decode timed out waiting for async pixels".into())
            }
        }
    }
}

#[cfg(test)]
mod memory_preview_tests {
    use super::memory_rgba_preview;
    use crate::loader::preview_aspect_matches_logical;

    #[test]
    fn memory_rgba_preview_preserves_panorama_aspect() {
        let logical_w = 10u32;
        let logical_h = 100u32;
        let pixels = vec![0u8; logical_w as usize * logical_h as usize * 4];
        let (out_w, out_h, out_pixels) =
            memory_rgba_preview(logical_w, logical_h, &pixels, 128, 128);
        assert_eq!(out_pixels.len(), out_w as usize * out_h as usize * 4);
        assert!(preview_aspect_matches_logical(
            out_w, out_h, logical_w, logical_h
        ));
        assert!(out_h > out_w);
        assert_ne!(out_w, out_h);
    }

    #[test]
    fn memory_rgba_preview_keeps_square_images_square() {
        let side = 256u32;
        let pixels = vec![0u8; side as usize * side as usize * 4];
        let (out_w, out_h, out_pixels) = memory_rgba_preview(side, side, &pixels, 128, 128);
        assert_eq!(out_pixels.len(), out_w as usize * out_h as usize * 4);
        assert_eq!(out_w, out_h);
        assert!(preview_aspect_matches_logical(out_w, out_h, side, side));
    }
}
