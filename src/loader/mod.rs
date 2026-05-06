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

//! Image loading (`ImageLoader`), decode pipeline ([`decode`]), helper modules, and GPU texture cache.

use eframe::egui;
use std::collections::HashMap;

mod decode;
mod hdr_fallback;
mod metadata;
mod orchestrator;
mod orientation;
mod preview_caps;
mod types;

#[allow(unused_imports)] // Re-export-only surface for `crate::loader::*`; rustc may lint `MONITOR_PREVIEW_CAP`.
pub use preview_caps::{
    hq_preview_max_side, refresh_hq_preview_monitor_cap, MONITOR_PREVIEW_CAP, PREVIEW_LIMIT,
};
pub use orchestrator::ImageLoader;
pub use types::*;

pub(crate) use hdr_fallback::{
    cheap_hdr_sdr_placeholder_rgba8, hdr_display_requests_sdr_preview,
    hdr_sdr_fallback_rgba8_eager_or_placeholder, hdr_to_sdr_with_user_tone,
};
pub(crate) use metadata::extract_exif_thumbnail;
pub(crate) use orientation::{
    apply_exif_orientation_to_hdr_pair, apply_exif_orientation_to_image_data,
    hdr_gain_map_decode_capacity,
};

// ---------------------------------------------------------------------------
// Texture cache
// ---------------------------------------------------------------------------

pub struct TextureCache {
    pub textures: HashMap<usize, egui::TextureHandle>,
    /// Original image dimensions (may differ from texture size for Tiled previews).
    original_res: HashMap<usize, (u32, u32)>,
    /// Flag indicating if the image was Tiled/Large and needs TileManager reconstruction.
    is_tiled: HashMap<usize, bool>,
    max_size: usize,
}

impl TextureCache {
    pub fn new(max_size: usize) -> Self {
        Self {
            textures: HashMap::new(),
            original_res: HashMap::new(),
            is_tiled: HashMap::new(),
            max_size,
        }
    }

    pub fn insert(
        &mut self,
        index: usize,
        handle: egui::TextureHandle,
        orig_w: u32,
        orig_h: u32,
        tiled: bool,
        current_index: usize,
        total_count: usize,
    ) -> Option<usize> {
        self.textures.insert(index, handle);
        self.original_res.insert(index, (orig_w, orig_h));
        self.is_tiled.insert(index, tiled);
        self.evict(current_index, total_count)
    }

    pub fn get_original_res(&self, index: usize) -> Option<(u32, u32)> {
        self.original_res.get(&index).copied()
    }

    pub fn remove(&mut self, index: usize) {
        self.textures.remove(&index);
        self.original_res.remove(&index);
        self.is_tiled.remove(&index);
    }

    /// Check if the image at index is a Tiled/Large image.

    pub fn get(&self, index: usize) -> Option<&egui::TextureHandle> {
        self.textures.get(&index)
    }

    pub fn contains(&self, index: usize) -> bool {
        self.textures.contains_key(&index)
    }

    pub fn is_preview_placeholder(&self, index: usize) -> bool {
        self.is_tiled.get(&index).copied().unwrap_or(false)
    }

    /// Longer side of the **uploaded** preview texture in pixels (not the full-image logical size).
    /// Used to avoid replacing a stage-2 HQ preview with a stage-1 bootstrap when re-opening a file.
    pub fn cached_preview_max_side(&self, index: usize) -> Option<u32> {
        self.textures.get(&index).map(|h| {
            let s = h.size();
            s[0].max(s[1]) as u32
        })
    }

    pub fn clear_all(&mut self) {
        self.textures.clear();
        self.original_res.clear();
        self.is_tiled.clear();
    }

    fn evict(&mut self, current_index: usize, total_count: usize) -> Option<usize> {
        if self.textures.len() <= self.max_size {
            return None;
        }
        // Evict the texture with the greatest CIRCULAR distance from current_index.
        // In a 100-image list, index 99 is distance 1 from index 0 (wrapping around).
        let to_remove = self.textures.keys().copied().max_by_key(|&idx| {
            if total_count == 0 {
                (idx as isize - current_index as isize).unsigned_abs()
            } else {
                let forward = (idx + total_count - current_index) % total_count;
                let backward = (current_index + total_count - idx) % total_count;
                forward.min(backward)
            }
        });

        if let Some(idx) = to_remove {
            self.textures.remove(&idx);
            self.original_res.remove(&idx);
            self.is_tiled.remove(&idx);
            Some(idx)
        } else {
            None
        }
    }
}
