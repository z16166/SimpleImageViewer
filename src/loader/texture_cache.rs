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

pub struct TextureCacheInsert {
    pub orig_w: u32,
    pub orig_h: u32,
    pub needs_tile_manager: bool,
    pub current_index: usize,
    pub total_count: usize,
}

pub struct TextureCache {
    pub textures: HashMap<usize, egui::TextureHandle>,
    /// Original image dimensions (may differ from texture size for tiled previews).
    original_res: HashMap<usize, (u32, u32)>,
    /// True when the index uses the tiled pyramid pipeline (PSB/EXR/large raster).
    needs_tile_manager: HashMap<usize, bool>,
    /// Cached keys for bounded eviction scans (len <= max_size + 1).
    cached_indices: Vec<usize>,
    /// `(current_index, total_count)` for which `evict_furthest_idx` was computed.
    evict_anchor: (usize, usize),
    /// Index with greatest circular distance from `evict_anchor`.
    evict_furthest_idx: Option<usize>,
    evict_furthest_dist: usize,
    max_size: usize,
}

fn circular_distance(current_index: usize, total_count: usize, idx: usize) -> usize {
    if total_count == 0 {
        (idx as isize - current_index as isize).unsigned_abs()
    } else {
        let forward = (idx + total_count - current_index) % total_count;
        let backward = (current_index + total_count - idx) % total_count;
        forward.min(backward)
    }
}

impl TextureCache {
    pub fn new(max_size: usize) -> Self {
        Self {
            textures: HashMap::new(),
            original_res: HashMap::new(),
            needs_tile_manager: HashMap::new(),
            cached_indices: Vec::new(),
            evict_anchor: (0, 0),
            evict_furthest_idx: None,
            evict_furthest_dist: 0,
            max_size,
        }
    }

    pub fn insert(
        &mut self,
        index: usize,
        handle: egui::TextureHandle,
        params: TextureCacheInsert,
    ) -> Option<usize> {
        let is_new_key = !self.textures.contains_key(&index);
        self.textures.insert(index, handle);
        self.original_res
            .insert(index, (params.orig_w, params.orig_h));
        self.needs_tile_manager
            .insert(index, params.needs_tile_manager);
        if is_new_key {
            self.cached_indices.push(index);
        }
        self.refresh_evict_candidate(params.current_index, params.total_count, index);
        self.evict(params.current_index, params.total_count)
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
        self.drop_cached_index(index);
        if self.evict_furthest_idx == Some(index) {
            self.evict_furthest_idx = None;
        }
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
        if let Some(slot) = self.cached_indices.iter_mut().find(|i| **i == from) {
            *slot = to;
        }
        self.evict_furthest_idx = None;
    }

    pub fn permute(&mut self, old_to_new: &[usize]) {
        permute_usize_hashmap(&mut self.textures, old_to_new);
        permute_usize_hashmap(&mut self.original_res, old_to_new);
        permute_usize_hashmap(&mut self.needs_tile_manager, old_to_new);
        for idx in &mut self.cached_indices {
            if *idx < old_to_new.len() {
                *idx = old_to_new[*idx];
            }
        }
        self.evict_furthest_idx = None;
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
        self.cached_indices.clear();
        self.evict_furthest_idx = None;
        self.evict_furthest_dist = 0;
    }

    fn drop_cached_index(&mut self, index: usize) {
        self.cached_indices.retain(|&i| i != index);
    }

    fn consider_evict_candidate(&mut self, current_index: usize, total_count: usize, idx: usize) {
        let dist = circular_distance(current_index, total_count, idx);
        if self.evict_furthest_idx.is_none() || dist > self.evict_furthest_dist {
            self.evict_furthest_dist = dist;
            self.evict_furthest_idx = Some(idx);
        }
    }

    fn rebuild_evict_candidate(&mut self, current_index: usize, total_count: usize) {
        self.evict_anchor = (current_index, total_count);
        self.evict_furthest_idx = None;
        self.evict_furthest_dist = 0;
        let indices = self.cached_indices.clone();
        for idx in indices {
            self.consider_evict_candidate(current_index, total_count, idx);
        }
    }

    fn refresh_evict_candidate(
        &mut self,
        current_index: usize,
        total_count: usize,
        inserted: usize,
    ) {
        if self.evict_anchor != (current_index, total_count) || self.evict_furthest_idx.is_none() {
            self.rebuild_evict_candidate(current_index, total_count);
        } else {
            self.consider_evict_candidate(current_index, total_count, inserted);
        }
    }

    fn evict(&mut self, current_index: usize, total_count: usize) -> Option<usize> {
        if self.textures.len() <= self.max_size {
            return None;
        }
        // Evict the texture with the greatest CIRCULAR distance from current_index.
        // In a 100-image list, index 99 is distance 1 from index 0 (wrapping around).
        let idx = self.evict_furthest_idx?;
        self.textures.remove(&idx);
        self.original_res.remove(&idx);
        self.needs_tile_manager.remove(&idx);
        self.drop_cached_index(idx);
        self.rebuild_evict_candidate(current_index, total_count);
        Some(idx)
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
        cache.insert(
            3,
            handle,
            TextureCacheInsert {
                orig_w: 100,
                orig_h: 200,
                needs_tile_manager: true,
                current_index: 3,
                total_count: 10,
            },
        );

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

    #[test]
    fn texture_cache_evicts_furthest_circular_neighbor() {
        let ctx = egui::Context::default();
        let color_image = egui::ColorImage::from_rgba_unmultiplied([1, 1], &[0, 0, 0, 255]);
        let mut cache = TextureCache::new(2);
        let total = 10;
        let current = 0;

        for idx in [0usize, 1, 5] {
            let handle = ctx.load_texture(
                format!("tex_{idx}"),
                color_image.clone(),
                egui::TextureOptions::LINEAR,
            );
            if let Some(evicted) = cache.insert(
                idx,
                handle,
                TextureCacheInsert {
                    orig_w: 1,
                    orig_h: 1,
                    needs_tile_manager: false,
                    current_index: current,
                    total_count: total,
                },
            ) {
                assert_eq!(evicted, 5, "index 5 is furthest from current 0 in a ring of 10");
            }
        }

        assert!(cache.contains(0));
        assert!(cache.contains(1));
        assert!(!cache.contains(5));
    }

    #[test]
    fn texture_cache_wraparound_distance_prefers_nearby_over_wrapped() {
        let ctx = egui::Context::default();
        let color_image = egui::ColorImage::from_rgba_unmultiplied([1, 1], &[0, 0, 0, 255]);
        let mut cache = TextureCache::new(2);
        let total = 10;
        let current = 9;

        for idx in [9usize, 0, 4] {
            let handle = ctx.load_texture(
                format!("tex_{idx}"),
                color_image.clone(),
                egui::TextureOptions::LINEAR,
            );
            if let Some(evicted) = cache.insert(
                idx,
                handle,
                TextureCacheInsert {
                    orig_w: 1,
                    orig_h: 1,
                    needs_tile_manager: false,
                    current_index: current,
                    total_count: total,
                },
            ) {
                assert_eq!(
                    evicted, 4,
                    "index 4 is distance 5 from 9; 0 and 9 are distance 1"
                );
            }
        }

        assert!(cache.contains(9));
        assert!(cache.contains(0));
        assert!(!cache.contains(4));
    }
}
