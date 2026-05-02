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

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::{cell::Cell, panic::AssertUnwindSafe};

use crate::hdr::tiled::{
    configured_hdr_tile_cache_max_bytes, hdr_preview_from_tiled_source_nearest,
    sdr_preview_from_hdr_preview, HdrTileBuffer, HdrTileCache, HdrTiledSource, HdrTiledSourceKind,
};
use crate::hdr::types::{HdrColorSpace, HdrImageBuffer, HdrPixelFormat};

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
    color_space: HdrColorSpace,
    has_subsampled_channels: bool,
    tile_cache: Mutex<HdrTileCache>,
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

        Ok(Self {
            path: path.to_path_buf(),
            context,
            width: part.width,
            height: part.height,
            part_index,
            color_space: HdrColorSpace::Unknown,
            has_subsampled_channels,
            tile_cache: Mutex::new(HdrTileCache::new(max_cache_bytes)),
        })
    }

    pub(crate) fn requires_disk_backed_decode(&self) -> bool {
        false
    }

    pub(crate) fn has_subsampled_channels(&self) -> bool {
        self.has_subsampled_channels
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
            hdr_preview_from_tiled_source_nearest(self, max_w, max_h)
        })
    }

    fn generate_sdr_preview(&self, max_w: u32, max_h: u32) -> Result<(u32, u32, Vec<u8>), String> {
        let context = exr_file_context("generate EXR SDR preview", &self.path);
        catch_exr_panic(&context, || {
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

        let context = exr_file_context("extract EXR HDR tile", &self.path);
        let tile = catch_exr_panic(&context, || {
            self.context
                .extract_scanline_rgba32f_tile(self.part_index, x, y, width, height)
        })?;
        let tile = Arc::new(HdrTileBuffer {
            width: tile.width,
            height: tile.height,
            color_space: self.color_space,
            rgba_f32: Arc::new(tile.rgba),
        });

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
    let (width, height) = exr_dimensions_unvalidated(path)?;
    let pixel_count = width
        .checked_mul(height)
        .ok_or_else(|| format!("Deep EXR dimensions overflow: {width}x{height}"))?;
    let mut rgba_f32 = vec![0.0_f32; pixel_count as usize * 4];
    for pixel in rgba_f32.chunks_exact_mut(4) {
        pixel[0] = 0.18;
        pixel[1] = 0.18;
        pixel[2] = 0.18;
        pixel[3] = 1.0;
    }
    Ok(HdrImageBuffer {
        width,
        height,
        format: HdrPixelFormat::Rgba32Float,
        color_space: HdrColorSpace::Unknown,
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
}
