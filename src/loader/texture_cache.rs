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

//! GPU texture cache for decoded image previews (`egui` handles).

use eframe::egui;
use std::collections::HashMap;

fn permute_usize_hashmap<T>(map: &mut HashMap<usize, T>, old_to_new: &[usize]) {
    let taken = std::mem::take(map);
    for (old_idx, value) in taken {
        if old_idx < old_to_new.len() {
            map.insert(old_to_new[old_idx], value);
        }
    }
}

pub struct TextureCache {
    pub textures: HashMap<usize, egui::TextureHandle>,
    /// Original image dimensions (may differ from texture size for tiled previews).
    original_res: HashMap<usize, (u32, u32)>,
    /// True when the index uses the tiled pyramid pipeline (PSB/EXR/large raster).
    needs_tile_manager: HashMap<usize, bool>,
    max_size: usize,
}

impl TextureCache {
    pub fn new(max_size: usize) -> Self {
        Self {
            textures: HashMap::new(),
            original_res: HashMap::new(),
            needs_tile_manager: HashMap::new(),
            max_size,
        }
    }

    pub fn insert(
        &mut self,
        index: usize,
        handle: egui::TextureHandle,
        orig_w: u32,
        orig_h: u32,
        needs_tile_manager: bool,
        current_index: usize,
        total_count: usize,
    ) -> Option<usize> {
        self.textures.insert(index, handle);
        self.original_res.insert(index, (orig_w, orig_h));
        self.needs_tile_manager.insert(index, needs_tile_manager);
        self.evict(current_index, total_count)
    }

    pub fn get_original_res(&self, index: usize) -> Option<(u32, u32)> {
        self.original_res.get(&index).copied()
    }

    pub fn set_original_res(&mut self, index: usize, orig_w: u32, orig_h: u32) {
        if self.textures.contains_key(&index) {
            self.original_res.insert(index, (orig_w, orig_h));
        }
    }

    pub fn remove(&mut self, index: usize) {
        self.textures.remove(&index);
        self.original_res.remove(&index);
        self.needs_tile_manager.remove(&index);
    }

    pub fn relocate(&mut self, from: usize, to: usize) {
        if from == to {
            return;
        }
        if let Some(tex) = self.textures.remove(&from) {
            self.textures.insert(to, tex);
        }
        if let Some(res) = self.original_res.remove(&from) {
            self.original_res.insert(to, res);
        }
        if let Some(flag) = self.needs_tile_manager.remove(&from) {
            self.needs_tile_manager.insert(to, flag);
        }
    }

    pub fn permute(&mut self, old_to_new: &[usize]) {
        permute_usize_hashmap(&mut self.textures, old_to_new);
        permute_usize_hashmap(&mut self.original_res, old_to_new);
        permute_usize_hashmap(&mut self.needs_tile_manager, old_to_new);
    }

    pub fn get(&self, index: usize) -> Option<&egui::TextureHandle> {
        self.textures.get(&index)
    }

    pub fn contains(&self, index: usize) -> bool {
        self.textures.contains_key(&index)
    }

    pub fn needs_tile_manager(&self, index: usize) -> bool {
        self.needs_tile_manager
            .get(&index)
            .copied()
            .unwrap_or(false)
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
        self.needs_tile_manager.clear();
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
            self.needs_tile_manager.remove(&idx);
            Some(idx)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_texture_cache_relocate() {
        let ctx = egui::Context::default();
        let color_image = egui::ColorImage::from_rgba_unmultiplied([1, 1], &[0, 0, 0, 255]);
        let handle = ctx.load_texture("test_tex", color_image, egui::TextureOptions::LINEAR);

        let mut cache = TextureCache::new(5);
        cache.insert(3, handle, 100, 200, true, 3, 10);

        assert!(cache.contains(3));
        assert!(!cache.contains(7));
        assert_eq!(cache.get_original_res(3), Some((100, 200)));
        assert!(cache.needs_tile_manager(3));

        cache.relocate(3, 7);

        assert!(!cache.contains(3));
        assert!(cache.contains(7));
        assert_eq!(cache.get_original_res(7), Some((100, 200)));
        assert!(cache.needs_tile_manager(7));
    }
}
