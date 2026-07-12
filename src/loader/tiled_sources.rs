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

use crossbeam_channel::{Sender, TrySendError};
use image::{DynamicImage, GenericImageView};
use parking_lot::{Mutex, RwLock as PLRwLock};
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;

use crate::constants::RGBA_CHANNELS;
use crate::loader::types::{
    DecodedImage, RawDevelopedImageRank, RefinementRequest, TiledImageSource, source_key_for_path,
};
use simple_image_viewer::simd_downsample::downsample_rgba8_box;

fn checked_rgba8_len(width: u32, height: u32) -> Option<usize> {
    (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(RGBA_CHANNELS))
}

fn checked_rgba8_row_len(width: u32) -> Option<usize> {
    (width as usize).checked_mul(RGBA_CHANNELS)
}

fn checked_tile_rect_inside(
    full_width: u32,
    full_height: u32,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
) -> Option<()> {
    if width == 0 || height == 0 {
        return None;
    }
    let end_x = x.checked_add(width)?;
    let end_y = y.checked_add(height)?;
    if end_x > full_width || end_y > full_height {
        return None;
    }
    Some(())
}

fn checked_tile_row_range(
    full_width: u32,
    full_height: u32,
    pixels_len: usize,
    x: u32,
    row: u32,
    width: u32,
) -> Option<Range<usize>> {
    if row >= full_height || x.checked_add(width)? > full_width {
        return None;
    }
    let stride = checked_rgba8_row_len(full_width)?;
    let row_start = (row as usize).checked_mul(stride)?;
    let x_offset = checked_rgba8_row_len(x)?;
    let row_len = checked_rgba8_row_len(width)?;
    let start = row_start.checked_add(x_offset)?;
    let end = start.checked_add(row_len)?;
    if end > pixels_len {
        return None;
    }
    Some(start..end)
}

fn extract_rgba8_tile_from_pixels(
    full_width: u32,
    full_height: u32,
    pixels: &[u8],
    x: u32,
    y: u32,
    width: u32,
    height: u32,
) -> Option<Vec<u8>> {
    checked_tile_rect_inside(full_width, full_height, x, y, width, height)?;
    let expected_source_len = checked_rgba8_len(full_width, full_height)?;
    if pixels.len() < expected_source_len {
        return None;
    }

    let expected_tile_len = checked_rgba8_len(width, height)?;
    let end_y = y.checked_add(height)?;
    let mut tile_pixels = Vec::with_capacity(expected_tile_len);
    for row in y..end_y {
        let range = checked_tile_row_range(full_width, full_height, pixels.len(), x, row, width)?;
        tile_pixels.extend_from_slice(&pixels[range]);
    }
    (tile_pixels.len() == expected_tile_len).then_some(tile_pixels)
}

fn empty_tile_pixels() -> Arc<Vec<u8>> {
    Arc::new(Vec::new())
}

fn solid_rgba8_tile(
    full_width: u32,
    full_height: u32,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    rgba: [u8; RGBA_CHANNELS],
) -> Arc<Vec<u8>> {
    if checked_tile_rect_inside(full_width, full_height, x, y, width, height).is_none() {
        return empty_tile_pixels();
    }
    let Some(len) = checked_rgba8_len(width, height) else {
        return empty_tile_pixels();
    };
    let mut pixels = vec![0_u8; len];
    for px in pixels.chunks_exact_mut(RGBA_CHANNELS) {
        px.copy_from_slice(&rgba);
    }
    Arc::new(pixels)
}

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
    let Some(expected_input_bytes) = checked_rgba8_len(width, height) else {
        return (0, 0, Vec::new());
    };
    if pixels.len() < expected_input_bytes {
        return (0, 0, Vec::new());
    }
    let scale = (max_w as f64 / width as f64)
        .min(max_h as f64 / height as f64)
        .min(1.0);
    let out_w = (width as f64 * scale).round().max(1.0) as u32;
    let out_h = (height as f64 * scale).round().max(1.0) as u32;
    let out = downsample_rgba8_box(pixels, width, height, out_w, out_h);
    let Some(expected_out_bytes) = checked_rgba8_len(out_w, out_h) else {
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

    /// Solid black tiles only -- never decode SDR rows/tiles from the HDR
    /// source (checklist #26: HDR display must not pay for SDR tile work).
    fn extract_tile(&self, x: u32, y: u32, w: u32, h: u32) -> Arc<Vec<u8>> {
        solid_rgba8_tile(self.width(), self.height(), x, y, w, h, [0, 0, 0, 255])
    }

    fn generate_preview(&self, max_w: u32, max_h: u32) -> (u32, u32, Vec<u8>) {
        self.source
            .generate_sdr_preview(max_w, max_h)
            .unwrap_or_else(|err| {
                log::warn!("[Loader] HDR SDR preview fallback failed: {err}");
                if self.width() == 0 || self.height() == 0 {
                    return (0, 0, Vec::new());
                }
                let scale = (max_w as f32 / self.width() as f32)
                    .min(max_h as f32 / self.height() as f32)
                    .min(1.0);
                let width = ((self.width() as f32 * scale).round() as u32).max(1);
                let height = ((self.height() as f32 * scale).round() as u32).max(1);
                let Some(byte_len) = checked_rgba8_len(width, height) else {
                    return (0, 0, Vec::new());
                };
                (width, height, vec![0; byte_len])
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
        extract_rgba8_tile_from_pixels(self.width, self.height, &self.pixels, x, y, w, h)
            .map(Arc::new)
            .unwrap_or_else(empty_tile_pixels)
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
    tile_cache: Mutex<crate::hdr::tiled::HdrTileCache>,
    logical_width: u32,
    logical_height: u32,
}

impl RawHdrRefiningSource {
    pub(crate) fn new(
        buffer: Arc<PLRwLock<Option<crate::hdr::types::HdrImageBuffer>>>,
        logical_width: u32,
        logical_height: u32,
    ) -> Self {
        Self::new_with_cache_budget(
            buffer,
            logical_width,
            logical_height,
            crate::hdr::tiled::configured_hdr_tile_cache_max_bytes(),
        )
    }

    pub(crate) fn new_with_cache_budget(
        buffer: Arc<PLRwLock<Option<crate::hdr::types::HdrImageBuffer>>>,
        logical_width: u32,
        logical_height: u32,
        max_cache_bytes: usize,
    ) -> Self {
        Self {
            buffer,
            tile_cache: Mutex::new(crate::hdr::tiled::HdrTileCache::new(max_cache_bytes)),
            logical_width,
            logical_height,
        }
    }

    #[cfg(test)]
    pub(crate) fn cached_tile_count(&self) -> usize {
        self.tile_cache.lock().len()
    }

    #[cfg(test)]
    pub(crate) fn cached_tile_bytes(&self) -> usize {
        self.tile_cache.lock().current_bytes()
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

    fn cached_tile_rgba32f_arc(
        &self,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    ) -> Option<Arc<crate::hdr::tiled::HdrTileBuffer>> {
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
    ) -> Result<Arc<crate::hdr::tiled::HdrTileBuffer>, String> {
        let key = (x, y, width, height);
        {
            let mut cache = self.tile_cache.lock();
            if let Some(tile) = cache.get(key) {
                return Ok(tile);
            }
        }

        let guard = self.buffer.read();
        let image = guard
            .as_ref()
            .ok_or_else(|| "RAW HDR buffer not yet refined".to_string())?;
        crate::hdr::tiled::validate_tile_bounds(image.width, image.height, x, y, width, height)?;

        let expected_source_len = checked_rgba8_len(image.width, image.height)
            .ok_or_else(|| "RAW HDR source dimensions overflow".to_string())?;
        if image.rgba_f32.len() < expected_source_len {
            return Err("RAW HDR source buffer is shorter than dimensions".to_string());
        }
        let tile_len = checked_rgba8_len(width, height)
            .ok_or_else(|| "RAW HDR tile dimensions overflow".to_string())?;
        let source_stride = checked_rgba8_row_len(image.width)
            .ok_or_else(|| "RAW HDR source stride overflow".to_string())?;
        let row_len = checked_rgba8_row_len(width)
            .ok_or_else(|| "RAW HDR tile row length overflow".to_string())?;
        let start_x =
            checked_rgba8_row_len(x).ok_or_else(|| "RAW HDR tile x offset overflow".to_string())?;
        let end_y = y
            .checked_add(height)
            .ok_or_else(|| "RAW HDR tile y range overflow".to_string())?;

        let mut tile = Vec::with_capacity(tile_len);
        for row in y..end_y {
            let row_start = (row as usize)
                .checked_mul(source_stride)
                .ok_or_else(|| "RAW HDR tile row offset overflow".to_string())?;
            let start = row_start
                .checked_add(start_x)
                .ok_or_else(|| "RAW HDR tile start offset overflow".to_string())?;
            let end = start
                .checked_add(row_len)
                .ok_or_else(|| "RAW HDR tile end offset overflow".to_string())?;
            if end > image.rgba_f32.len() {
                return Err("RAW HDR tile range exceeds source buffer".to_string());
            }
            tile.extend_from_slice(&image.rgba_f32[start..end]);
        }
        if tile.len() != tile_len {
            return Err("RAW HDR tile length mismatch".to_string());
        }

        let tile = Arc::new(crate::hdr::tiled::HdrTileBuffer::new_with_metadata(
            width,
            height,
            image.color_space,
            image.metadata.clone(),
            Arc::new(tile),
        ));
        drop(guard);

        self.tile_cache.lock().insert(key, Arc::clone(&tile));

        Ok(tile)
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
    developed_image_rank: Arc<PLRwLock<RawDevelopedImageRank>>,
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
    pub(crate) initial_image_rank: RawDevelopedImageRank,
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
            initial_image_rank,
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
        let developed_image_rank = Arc::new(PLRwLock::new(initial_image_rank));

        let refine_tx = refine_tx.clone();

        Ok(Self {
            path,
            width: raw_width,
            height: raw_height,
            developed_image,
            developed_image_rank,
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
        if checked_tile_rect_inside(self.width, self.height, x, y, w, h).is_none() {
            return empty_tile_pixels();
        }

        let img_lock = self.developed_image.read();
        if let Some(ref img) = *img_lock {
            let (iw, ih) = img.dimensions();
            let rank = *self.developed_image_rank.read();
            if rank == RawDevelopedImageRank::FullResolutionDeveloped {
                // Rank, not dimensions, decides whether this buffer is full develop output.
                // Dimensions are validated only as a corruption guard before direct tiling.
                if iw != self.width || ih != self.height {
                    log::error!(
                        "[RawImageSource] Full-resolution RAW buffer dimensions mismatch: got {}x{}, expected {}x{} for {:?}",
                        iw,
                        ih,
                        self.width,
                        self.height,
                        self.path.file_name().unwrap_or_default()
                    );
                    return empty_tile_pixels();
                }
                if let Some(rgba) = img.as_rgba8() {
                    extract_rgba8_tile_from_pixels(iw, ih, rgba.as_raw(), x, y, w, h)
                        .map(Arc::new)
                        .unwrap_or_else(empty_tile_pixels)
                } else {
                    let crop = img.crop_imm(x, y, w, h);
                    Arc::new(crop.into_rgba8().into_raw())
                }
            } else {
                // Embedded preview image: map RAW-space tile coordinates into preview space.
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
            solid_rgba8_tile(self.width, self.height, x, y, w, h, [0, 0, 0, 0])
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
        if *self.developed_image_rank.read() != RawDevelopedImageRank::FullResolutionDeveloped {
            return None;
        }

        let img_lock = self.developed_image.read();
        let img = (*img_lock).as_ref()?;

        let (iw, ih) = img.dimensions();
        if iw != self.width || ih != self.height {
            log::error!(
                "[RawImageSource] Full-resolution RAW buffer dimensions mismatch: got {}x{}, expected {}x{} for {:?}",
                iw,
                ih,
                self.width,
                self.height,
                self.path.file_name().unwrap_or_default()
            );
            return None;
        }

        Some(Arc::new(img.to_rgba8().into_raw()))
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
        let source_key = source_key_for_path(&self.path);
        let request = RefinementRequest {
            path: self.path.clone(),
            index,
            decode_profile,
            source_key,
            orientation_override: Some(self.orientation_override),
            logical_width: self.width,
            logical_height: self.height,
            developed_image: self.developed_image.clone(),
            developed_image_rank: self.developed_image_rank.clone(),
            hdr_developed_image: self.hdr_developed_image.clone(),
            hdr_target_capacity: self.hdr_target_capacity,
            hdr_tone_map: self.hdr_tone_map,
        };
        match self.refine_tx.try_send(request) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                crate::preload_debug!(
                    "[PreloadDebug][RAW] refine_drop idx={} reason=queue_full path={}",
                    index,
                    self.path.display()
                );
                log::debug!(
                    "[RawImageSource] Dropping HQ refinement request for index={} because queue is full",
                    index
                );
            }
            Err(TrySendError::Disconnected(_)) => {
                log::debug!(
                    "[RawImageSource] Dropping HQ refinement request for index={} because worker is closed",
                    index
                );
            }
        }
    }

    fn defers_loader_hq_preview(&self) -> bool {
        self.needs_refinement
    }
}

#[cfg(test)]
mod memory_preview_tests {
    use super::{
        MemoryImageSource, RawHdrRefiningSource, RawImageSource, RawImageSourceParams,
        memory_rgba_preview,
    };
    use crate::hdr::tiled::HdrTiledSource;
    use crate::loader::{
        DecodedImage, RawDevelopedImageRank, TiledImageSource, decode_profile_stub,
        preview_aspect_matches_logical,
    };
    use parking_lot::RwLock as PLRwLock;
    use std::sync::Arc;

    fn raw_hdr_source_with_pixels(
        width: u32,
        height: u32,
        pixels: Arc<Vec<f32>>,
    ) -> RawHdrRefiningSource {
        RawHdrRefiningSource::new(
            Arc::new(PLRwLock::new(Some(crate::hdr::types::HdrImageBuffer {
                width,
                height,
                format: crate::hdr::types::HdrPixelFormat::Rgba32Float,
                color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
                metadata: crate::hdr::types::HdrImageMetadata::default(),
                rgba_f32: pixels,
            }))),
            width,
            height,
        )
    }

    fn raw_source_with_rank(rank: RawDevelopedImageRank) -> RawImageSource {
        let (refine_tx, _refine_rx) = crossbeam_channel::unbounded();
        RawImageSource::new(
            "rank-test.raw".into(),
            DecodedImage::new(2, 2, vec![0; 16]),
            RawImageSourceParams {
                raw_width: 2,
                raw_height: 2,
                refine_tx,
                initial_image_rank: rank,
                orientation_override: 0,
                needs_refinement: true,
                hdr_target_capacity: 1.0,
                hdr_tone_map: crate::hdr::types::HdrToneMapSettings::default(),
                hdr_developed_image: None,
            },
        )
        .expect("raw source")
    }

    #[test]
    fn raw_full_pixels_uses_rank_not_dimensions() {
        let embedded = raw_source_with_rank(RawDevelopedImageRank::EmbeddedPreview);
        assert!(
            embedded.full_pixels().is_none(),
            "embedded preview rank must not be treated as full pixels even when dimensions match"
        );

        let full = raw_source_with_rank(RawDevelopedImageRank::FullResolutionDeveloped);
        assert_eq!(full.full_pixels().expect("full pixels").len(), 16);
    }

    #[test]
    fn raw_refinement_request_preserves_embedded_rank() {
        let (refine_tx, refine_rx) = crossbeam_channel::unbounded();
        let source = RawImageSource::new(
            "rank-test.raw".into(),
            DecodedImage::new(2, 2, vec![0; 16]),
            RawImageSourceParams {
                raw_width: 2,
                raw_height: 2,
                refine_tx,
                initial_image_rank: RawDevelopedImageRank::EmbeddedPreview,
                orientation_override: 0,
                needs_refinement: true,
                hdr_target_capacity: 1.0,
                hdr_tone_map: crate::hdr::types::HdrToneMapSettings::default(),
                hdr_developed_image: None,
            },
        )
        .expect("raw source");

        source.request_refinement(7, decode_profile_stub());
        let request = refine_rx.try_recv().expect("refinement request");
        assert_eq!(
            *request.developed_image_rank.read(),
            RawDevelopedImageRank::EmbeddedPreview
        );
        assert!(request.developed_image.read().is_some());
    }

    #[test]
    fn raw_hdr_refining_source_rejects_overflowing_dimensions() {
        let source = raw_hdr_source_with_pixels(u32::MAX, u32::MAX, Arc::new(Vec::new()));

        assert!(source.extract_tile_rgba32f_arc(0, 0, 1, 1).is_err());
    }

    #[test]
    fn raw_hdr_refining_source_rejects_short_source_buffer() {
        let source = raw_hdr_source_with_pixels(2, 2, Arc::new(vec![0.0; 3]));

        assert!(source.extract_tile_rgba32f_arc(0, 0, 1, 1).is_err());
    }

    #[test]
    fn raw_hdr_refining_source_caches_extracted_tiles() {
        let source = raw_hdr_source_with_pixels(2, 1, Arc::new(vec![0.0; 8]));

        let first = source
            .extract_tile_rgba32f_arc(0, 0, 1, 1)
            .expect("first tile");
        let second = source
            .extract_tile_rgba32f_arc(0, 0, 1, 1)
            .expect("cached tile");

        assert!(Arc::ptr_eq(&first, &second));
        assert!(
            source.cached_tile_rgba32f_arc(0, 0, 1, 1).is_some(),
            "tile worker readiness requires RAW HDR sources to expose cached tiles"
        );
        assert_eq!(source.cached_tile_count(), 1);
        assert_eq!(source.cached_tile_bytes(), 16);
    }

    #[test]
    fn raw_hdr_refining_source_evicts_tiles_by_budget() {
        let source = RawHdrRefiningSource::new_with_cache_budget(
            Arc::new(PLRwLock::new(Some(crate::hdr::types::HdrImageBuffer {
                width: 3,
                height: 1,
                format: crate::hdr::types::HdrPixelFormat::Rgba32Float,
                color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
                metadata: crate::hdr::types::HdrImageMetadata::default(),
                rgba_f32: Arc::new(vec![0.0; 12]),
            }))),
            3,
            1,
            32,
        );

        source.extract_tile_rgba32f_arc(0, 0, 1, 1).expect("tile 0");
        source.extract_tile_rgba32f_arc(1, 0, 1, 1).expect("tile 1");
        source.extract_tile_rgba32f_arc(2, 0, 1, 1).expect("tile 2");

        assert!(source.cached_tile_rgba32f_arc(0, 0, 1, 1).is_none());
        assert!(source.cached_tile_rgba32f_arc(1, 0, 1, 1).is_some());
        assert!(source.cached_tile_rgba32f_arc(2, 0, 1, 1).is_some());
        assert_eq!(source.cached_tile_count(), 2);
        assert!(source.cached_tile_bytes() <= 32);
    }

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

    #[test]
    fn memory_rgba_preview_rejects_overflowing_dimensions() {
        let (out_w, out_h, out_pixels) =
            memory_rgba_preview(u32::MAX, u32::MAX, &[0, 0, 0, 0], 128, 128);

        assert_eq!((out_w, out_h), (0, 0));
        assert!(out_pixels.is_empty());
    }

    #[test]
    fn memory_image_source_rejects_invalid_tile_ranges() {
        let pixels = Arc::new((0u8..64).collect::<Vec<_>>());
        let source = MemoryImageSource::new(4, 4, pixels);

        assert_eq!(source.extract_tile(1, 1, 2, 2).len(), 16);
        assert!(source.extract_tile(3, 0, 2, 1).is_empty());
        assert!(source.extract_tile(u32::MAX, 0, 1, 1).is_empty());
        assert!(source.extract_tile(0, u32::MAX, 1, 1).is_empty());
        assert!(source.extract_tile(0, 0, u32::MAX, 1).is_empty());
    }

    #[test]
    fn memory_image_source_rejects_short_source_buffer() {
        let source = MemoryImageSource::new(4, 4, Arc::new(vec![0; 4]));

        assert!(source.extract_tile(0, 0, 1, 1).is_empty());
    }
}
