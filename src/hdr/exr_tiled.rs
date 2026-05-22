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

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::{cell::Cell, panic::AssertUnwindSafe};

use crate::hdr::tiled::{
    HdrTileBuffer, HdrTileCache, HdrTiledSource, HdrTiledSourceKind,
    configured_hdr_tile_cache_max_bytes, hdr_preview_from_tiled_source_nearest,
    sdr_preview_from_hdr_preview,
};
use crate::hdr::types::{HdrColorSpace, HdrImageBuffer, HdrImageMetadata, HdrPixelFormat};

// `std::panic::set_hook` runs on the thread that panicked. Suppression must be thread-local so a
// decoder panic on e.g. `siv-psd-v1` is gated by that thread's depth, not the Rayon parent.
thread_local! {
    static SUPPRESS_EXR_PANIC_HOOK_DEPTH: Cell<u32> = const { Cell::new(0) };
}

const OPENEXR_DEEP_SCANLINE_STORAGE: i32 = 2;
const OPENEXR_DEEP_TILED_STORAGE: i32 = 3;

#[derive(Debug)]
pub struct ExrTiledImageSource {
    path: PathBuf,
    context: crate::hdr::openexr_core_backend::OpenExrCoreReadContext,
    width: u32,
    height: u32,
    part_index: usize,
    storage: i32,
    color_space: HdrColorSpace,
    has_subsampled_channels: bool,
    tile_cache: Mutex<HdrTileCache>,
    scanline_band_prefills: Mutex<HashSet<(u32, u32)>>,
    scanline_band_prefills_ready: Condvar,
}

struct ScanlineBandPrefillLeader<'a> {
    source: &'a ExrTiledImageSource,
    band_key: (u32, u32),
}

impl Drop for ScanlineBandPrefillLeader<'_> {
    fn drop(&mut self) {
        if let Ok(mut in_flight) = self.source.scanline_band_prefills.lock() {
            in_flight.remove(&self.band_key);
        }
        self.source.scanline_band_prefills_ready.notify_all();
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ExrPartInfo {
    pub(crate) index: usize,
    pub(crate) is_displayable_color: bool,
    pub(crate) is_depth_only: bool,
}

impl ExrTiledImageSource {
    pub fn open(path: &Path) -> Result<Self, String> {
        Self::open_with_cache_budget(path, configured_hdr_tile_cache_max_bytes())
    }

    pub fn open_with_cache_budget(path: &Path, max_cache_bytes: usize) -> Result<Self, String> {
        let context = crate::hdr::openexr_core_backend::OpenExrCoreReadContext::open(path)?;
        let parts = exr_part_infos_from_context(&context)?;
        let part_index = default_display_part_index(&parts).unwrap_or(0);
        let part = context.part(part_index)?;
        if is_deep_storage(part.storage) {
            return Err("deep data not supported yet by OpenEXRCore flat image path".to_string());
        }
        validate_openexr_core_channels(&part.channels)?;
        let has_subsampled_channels = part
            .channels
            .iter()
            .any(|channel| channel.x_sampling != 1 || channel.y_sampling != 1);
        let color_space =
            crate::hdr::openexr_core_backend::OpenExrCoreReadContext::infer_exr_display_color_space_for_path(
                path,
            );

        Ok(Self {
            path: path.to_path_buf(),
            context,
            width: part.width,
            height: part.height,
            part_index,
            storage: part.storage,
            color_space,
            has_subsampled_channels,
            tile_cache: Mutex::new(HdrTileCache::new(max_cache_bytes)),
            scanline_band_prefills: Mutex::new(HashSet::new()),
            scanline_band_prefills_ready: Condvar::new(),
        })
    }

    pub(crate) fn requires_disk_backed_decode(&self) -> bool {
        false
    }

    pub(crate) fn has_subsampled_channels(&self) -> bool {
        self.has_subsampled_channels
    }

    fn should_prefill_scanline_band(&self, x: u32, y: u32, width: u32, height: u32) -> bool {
        if self.storage != openexr_core_sys::EXR_STORAGE_SCANLINE {
            return false;
        }
        let tile_size = crate::tile_cache::get_tile_size();
        x % tile_size == 0
            && y % tile_size == 0
            && width == tile_size.min(self.width.saturating_sub(x))
            && height == tile_size.min(self.height.saturating_sub(y))
            && self.width > width
    }

    fn prefill_scanline_band_tiles(&self, y: u32, height: u32) -> Result<(), String> {
        #[cfg(feature = "tile-debug")]
        let started_at = std::time::Instant::now();
        let tile_size = crate::tile_cache::get_tile_size();
        let band_height = tile_size.min(self.height.saturating_sub(y));
        if band_height != height {
            return Ok(());
        }

        let keys = scanline_band_tile_keys(self.width, self.height, y, tile_size, tile_size);
        if keys.len() <= 1 {
            return Ok(());
        }
        #[cfg(feature = "tile-debug")]
        let tile_count = keys.len();
        let band_key = (y, band_height);
        if let Ok(mut cache) = self.tile_cache.lock() {
            if keys.iter().all(|key| cache.get(*key).is_some()) {
                #[cfg(feature = "tile-debug")]
                log::debug!(
                    "[HDR][band][exr] file=\"{}\" y={} height={} tiles={} cache=hit elapsed_ms={:.2}",
                    self.path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("<unknown>"),
                    y,
                    band_height,
                    tile_count,
                    started_at.elapsed().as_secs_f64() * 1000.0
                );
                return Ok(());
            }
        }

        loop {
            let mut in_flight = self.scanline_band_prefills.lock().unwrap();
            if in_flight.insert(band_key) {
                break;
            }

            in_flight = self
                .scanline_band_prefills_ready
                .wait_while(in_flight, |in_flight| in_flight.contains(&band_key))
                .unwrap();
            drop(in_flight);

            if let Ok(mut cache) = self.tile_cache.lock() {
                if keys.iter().all(|key| cache.get(*key).is_some()) {
                    #[cfg(feature = "tile-debug")]
                    log::debug!(
                        "[HDR][band][exr] file=\"{}\" y={} height={} tiles={} cache=coalesced elapsed_ms={:.2}",
                        self.path
                            .file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or("<unknown>"),
                        y,
                        band_height,
                        tile_count,
                        started_at.elapsed().as_secs_f64() * 1000.0
                    );
                    return Ok(());
                }
            }
        }

        let _band_leader = ScanlineBandPrefillLeader {
            source: self,
            band_key,
        };

        let result = (|| -> Result<(), String> {
            let context = exr_file_context("extract EXR scanline tile band", &self.path);
            let band = catch_exr_panic(&context, || {
                self.context.extract_scanline_rgba32f_tile(
                    self.part_index,
                    0,
                    y,
                    self.width,
                    band_height,
                )
            })?;

            let mut tiles = Vec::with_capacity(keys.len());
            for (tile_x, tile_y, tile_width, tile_height) in keys {
                let mut rgba = Vec::with_capacity(tile_width as usize * tile_height as usize * 4);
                let start_x = tile_x as usize * 4;
                let row_len = tile_width as usize * 4;
                let source_stride = band.width as usize * 4;
                for row in 0..tile_height as usize {
                    let start = row * source_stride + start_x;
                    let end = start + row_len;
                    rgba.extend_from_slice(&band.rgba[start..end]);
                }
                tiles.push((
                    (tile_x, tile_y, tile_width, tile_height),
                    Arc::new(HdrTileBuffer::new_with_metadata(
                        tile_width,
                        tile_height,
                        self.color_space,
                        HdrImageMetadata::from_color_space(self.color_space),
                        Arc::new(rgba),
                    )),
                ));
            }

            if let Ok(mut cache) = self.tile_cache.lock() {
                for (key, tile) in tiles {
                    cache.insert(key, tile);
                }
            }
            #[cfg(feature = "tile-debug")]
            log::debug!(
                "[HDR][band][exr] file=\"{}\" y={} height={} width={} tiles={} cache=miss elapsed_ms={:.2}",
                self.path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("<unknown>"),
                y,
                band_height,
                self.width,
                tile_count,
                started_at.elapsed().as_secs_f64() * 1000.0
            );
            Ok(())
        })();

        result
    }
}

impl HdrTiledSource for ExrTiledImageSource {
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
        self.color_space
    }

    fn generate_hdr_preview(&self, max_w: u32, max_h: u32) -> Result<HdrImageBuffer, String> {
        let context = exr_file_context("generate EXR HDR preview", &self.path);
        catch_exr_panic(&context, || {
            if self.storage == openexr_core_sys::EXR_STORAGE_SCANLINE
                && !self.has_subsampled_channels
            {
                let preview = self.context.extract_scanline_rgba32f_preview_nearest(
                    self.part_index,
                    max_w,
                    max_h,
                )?;
                return Ok(HdrImageBuffer {
                    width: preview.width,
                    height: preview.height,
                    format: HdrPixelFormat::Rgba32Float,
                    color_space: self.color_space,
                    metadata: HdrImageMetadata::from_color_space(self.color_space),
                    rgba_f32: Arc::new(preview.rgba),
                });
            }
            hdr_preview_from_tiled_source_nearest(self, max_w, max_h)
        })
    }

    fn generate_sdr_preview(&self, max_w: u32, max_h: u32) -> Result<(u32, u32, Vec<u8>), String> {
        let context = exr_file_context("generate EXR SDR preview", &self.path);
        catch_exr_panic(&context, || {
            if self.storage == openexr_core_sys::EXR_STORAGE_SCANLINE
                && !self.has_subsampled_channels
            {
                let preview = self.context.extract_scanline_rgba32f_preview_nearest(
                    self.part_index,
                    max_w,
                    max_h,
                )?;
                let hdr = HdrImageBuffer {
                    width: preview.width,
                    height: preview.height,
                    format: HdrPixelFormat::Rgba32Float,
                    color_space: self.color_space,
                    metadata: HdrImageMetadata::from_color_space(self.color_space),
                    rgba_f32: Arc::new(preview.rgba),
                };
                return sdr_preview_from_hdr_preview(&hdr);
            }

            let preview = hdr_preview_from_tiled_source_nearest(self, max_w, max_h)?;
            sdr_preview_from_hdr_preview(&preview)
        })
    }

    fn extract_tile_rgba32f_arc(
        &self,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    ) -> Result<Arc<HdrTileBuffer>, String> {
        let key = (x, y, width, height);
        if let Ok(mut cache) = self.tile_cache.lock() {
            if let Some(tile) = cache.get(key) {
                return Ok(tile);
            }
        }

        if self.should_prefill_scanline_band(x, y, width, height) {
            self.prefill_scanline_band_tiles(y, height)?;
            if let Ok(mut cache) = self.tile_cache.lock() {
                if let Some(tile) = cache.get(key) {
                    return Ok(tile);
                }
            }
        }

        let context = exr_file_context("extract EXR HDR tile", &self.path);
        let tile = catch_exr_panic(&context, || {
            self.context
                .extract_scanline_rgba32f_tile(self.part_index, x, y, width, height)
        })?;
        let tile = Arc::new(HdrTileBuffer::new_with_metadata(
            tile.width,
            tile.height,
            self.color_space,
            HdrImageMetadata::from_color_space(self.color_space),
            Arc::new(tile.rgba),
        ));

        if let Ok(mut cache) = self.tile_cache.lock() {
            cache.insert(key, Arc::clone(&tile));
        }
        Ok(tile)
    }

    fn cached_tile_rgba32f_arc(
        &self,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    ) -> Option<Arc<HdrTileBuffer>> {
        self.tile_cache
            .lock()
            .ok()
            .and_then(|mut cache| cache.get((x, y, width, height)))
    }

    fn protect_cached_tiles(&self, keys: &[(u32, u32, u32, u32)]) {
        if let Ok(mut cache) = self.tile_cache.lock() {
            cache.set_protected_keys(keys.iter().copied());
        }
    }
}

pub(crate) fn exr_file_context(action: &str, path: &Path) -> String {
    format!("{action} ({})", path.display())
}

pub(crate) fn catch_exr_panic<T>(
    context: &str,
    f: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    struct SuppressionGuard;
    impl Drop for SuppressionGuard {
        fn drop(&mut self) {
            SUPPRESS_EXR_PANIC_HOOK_DEPTH.with(|depth| {
                depth.set(depth.get().saturating_sub(1));
            });
        }
    }

    SUPPRESS_EXR_PANIC_HOOK_DEPTH.with(|depth| depth.set(depth.get() + 1));
    let _guard = SuppressionGuard;

    std::panic::catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|panic| {
        let message = panic
            .downcast_ref::<&str>()
            .map(|message| (*message).to_string())
            .or_else(|| panic.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".to_string());
        Err(format!("{context}: decoder panic: {message}"))
    })
}

pub(crate) fn is_exr_panic_hook_suppressed() -> bool {
    SUPPRESS_EXR_PANIC_HOOK_DEPTH.with(|depth| depth.get() > 0)
}

fn exr_part_infos_from_context(
    context: &crate::hdr::openexr_core_backend::OpenExrCoreReadContext,
) -> Result<Vec<ExrPartInfo>, String> {
    let mut parts = Vec::new();
    for index in 0..context.part_count() {
        let part = context.part(index)?;
        let is_deep = is_deep_storage(part.storage);
        let channel_names = part
            .channels
            .iter()
            .map(|channel| channel.name.as_str())
            .collect::<Vec<_>>();
        let has_luma = has_channel(&channel_names, "Y");
        let has_rgb = has_channel(&channel_names, "R")
            && has_channel(&channel_names, "G")
            && has_channel(&channel_names, "B");
        let is_displayable_color = has_rgb
            || has_luma
            || channel_names
                .iter()
                .any(|name| is_generic_sample_channel(name));
        let is_depth_only =
            !is_displayable_color && channel_names.iter().any(|name| is_depth_channel_name(name));
        parts.push(ExrPartInfo {
            index,
            is_displayable_color: is_displayable_color && !is_deep,
            is_depth_only,
        });
    }
    Ok(parts)
}

pub(crate) fn default_display_part_index(parts: &[ExrPartInfo]) -> Option<usize> {
    parts
        .iter()
        .find(|part| part.is_displayable_color)
        .map(|part| part.index)
        .or_else(|| {
            parts
                .iter()
                .find(|part| !part.is_depth_only)
                .map(|part| part.index)
        })
}

pub(crate) fn exr_dimensions_unvalidated(path: &Path) -> Result<(u32, u32), String> {
    let context = crate::hdr::openexr_core_backend::OpenExrCoreReadContext::open(path)?;
    let parts = exr_part_infos_from_context(&context)?;
    let part_index = default_display_part_index(&parts).unwrap_or(0);
    let part = context.part(part_index)?;
    Ok((part.width, part.height))
}

pub(crate) fn decode_deep_exr_image(path: &Path) -> Result<HdrImageBuffer, String> {
    let context = crate::hdr::openexr_core_backend::OpenExrCoreReadContext::open(path)?;
    let parts = exr_part_infos_from_context(&context)?;
    let part_index = default_display_part_index(&parts).unwrap_or(0);
    let part = context.part(part_index)?;
    let color_space =
        crate::hdr::openexr_core_backend::OpenExrCoreReadContext::infer_exr_display_color_space_for_path(
            path,
        );
    let rgba_f32 = crate::hdr::openexr_core_backend::deep_scanline_flatten_rgba_via_imf(
        path,
        part.width,
        part.height,
    )?;
    Ok(HdrImageBuffer {
        width: part.width,
        height: part.height,
        format: HdrPixelFormat::Rgba32Float,
        color_space,
        metadata: HdrImageMetadata::from_color_space(color_space),
        rgba_f32: Arc::new(rgba_f32),
    })
}

fn validate_openexr_core_channels(
    channels: &[crate::hdr::openexr_core_backend::OpenExrCoreChannelInfo],
) -> Result<(), String> {
    let names = channels
        .iter()
        .map(|channel| channel.name.as_str())
        .collect::<Vec<_>>();
    if has_channel(&names, "Y") {
        return Ok(());
    }
    if names.iter().any(|name| is_generic_sample_channel(name)) {
        return Ok(());
    }
    for required in ["R", "G", "B"] {
        if !has_channel(&names, required) {
            return Err(format!(
                "EXR layer does not contain required {required} channel"
            ));
        }
    }
    Ok(())
}

fn has_channel(names: &[&str], required: &str) -> bool {
    names.iter().any(|name| name.eq_ignore_ascii_case(required))
}

fn is_depth_channel_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == "z" || lower == "depth" || lower.ends_with(".z") || lower.ends_with(".depth")
}

fn is_generic_sample_channel(name: &str) -> bool {
    !name.eq_ignore_ascii_case("A") && !is_depth_channel_name(name)
}

fn is_deep_storage(storage: i32) -> bool {
    matches!(
        storage,
        OPENEXR_DEEP_SCANLINE_STORAGE | OPENEXR_DEEP_TILED_STORAGE
    )
}

fn scanline_band_tile_keys(
    source_width: u32,
    source_height: u32,
    y: u32,
    tile_width: u32,
    tile_height: u32,
) -> Vec<(u32, u32, u32, u32)> {
    if source_width == 0 || source_height == 0 || tile_width == 0 || tile_height == 0 {
        return Vec::new();
    }
    let band_y = (y / tile_height) * tile_height;
    if band_y >= source_height {
        return Vec::new();
    }
    let height = tile_height.min(source_height - band_y);
    let mut keys = Vec::new();
    let mut x = 0;
    while x < source_width {
        let width = tile_width.min(source_width - x);
        keys.push((x, band_y, width, height));
        x += tile_width;
    }
    keys
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    #[test]
    fn exr_panic_boundary_converts_decoder_panic_to_error() {
        let err = super::catch_exr_panic("test EXR boundary", || -> Result<(), String> {
            panic!("synthetic EXR decoder panic")
        })
        .expect_err("panic should be converted to an error");

        assert!(err.contains("test EXR boundary"));
        assert!(err.contains("synthetic EXR decoder panic"));
    }

    #[test]
    fn exr_panic_boundary_suppresses_global_panic_hook_until_unwind_is_caught() {
        let err = super::catch_exr_panic("test EXR hook suppression", || -> Result<(), String> {
            assert!(super::is_exr_panic_hook_suppressed());
            panic!("synthetic hook suppression panic")
        })
        .expect_err("panic should be converted to an error");

        assert!(err.contains("synthetic hook suppression panic"));
        assert!(!super::is_exr_panic_hook_suppressed());
    }

    #[test]
    fn exr_file_context_includes_action_and_path() {
        let context =
            super::exr_file_context("decode EXR display image", Path::new("samples/problem.exr"));

        assert!(context.contains("decode EXR display image"));
        assert!(context.contains("samples"));
        assert!(context.contains("problem.exr"));
    }

    #[test]
    fn default_display_part_prefers_displayable_color_over_depth() {
        let parts = vec![
            super::ExrPartInfo {
                index: 0,
                is_displayable_color: false,
                is_depth_only: true,
            },
            super::ExrPartInfo {
                index: 1,
                is_displayable_color: true,
                is_depth_only: false,
            },
        ];

        assert_eq!(super::default_display_part_index(&parts), Some(1));
    }

    #[test]
    fn scanline_band_tile_keys_cover_full_horizontal_band() {
        let keys = super::scanline_band_tile_keys(24576, 8192, 6656, 512, 512);

        assert_eq!(keys.len(), 48);
        assert_eq!(keys[0], (0, 6656, 512, 512));
        assert_eq!(keys[27], (13824, 6656, 512, 512));
        assert_eq!(keys[47], (24064, 6656, 512, 512));
    }
}
