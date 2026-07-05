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

use super::*;

fn release_tile_texture(pool: &mut Option<&mut GpuTexturePool>, binding: &HdrTileBinding) {
    if let (Some(pool), Some(texture)) = (pool.as_mut(), binding._texture.as_ref()) {
        pool.release(Arc::clone(texture));
    }
}

pub(crate) struct HdrTileBindings {
    pub(super) entries: HashMap<HdrTileKey, HdrTileBinding>,
    evictable_lru: crate::lru_order::LruOrder<HdrTileKey>,
    protected_recent: HashSet<HdrTileKey>,
    protected_order: crate::lru_order::LruOrder<HdrTileKey>,
    pub(super) current_bytes: usize,
    pub(super) max_bytes: usize,
}

pub(crate) struct HdrTileInsert {
    pub(crate) texture: Arc<wgpu::Texture>,
    pub(crate) view: wgpu::TextureView,
    pub(crate) compose_storage_view: Option<wgpu::TextureView>,
    pub(crate) tone_map_buffer: wgpu::Buffer,
    pub(crate) bind_group: wgpu::BindGroup,
    pub(crate) jpeg_compose_bind_group: Option<wgpu::BindGroup>,
    pub(crate) baked_jpeg_weight_bits: Option<u32>,
}

pub(super) const HDR_TILE_BINDING_RECENT_PROTECTION_COUNT: usize = 512;

impl Default for HdrTileBindings {
    fn default() -> Self {
        Self::with_budget(crate::hdr::tiled::configured_hdr_tile_cache_max_bytes())
    }
}

impl HdrTileBindings {
    pub(crate) fn with_budget(max_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            evictable_lru: crate::lru_order::LruOrder::default(),
            protected_recent: HashSet::new(),
            protected_order: crate::lru_order::LruOrder::default(),
            current_bytes: 0,
            max_bytes,
        }
    }

    pub(crate) fn contains(&mut self, key: HdrTileKey) -> bool {
        if self.entries.contains_key(&key) {
            self.touch(key);
            self.protect_recent(key);
            true
        } else {
            false
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn insert(
        &mut self,
        key: HdrTileKey,
        tile: HdrTileInsert,
        mut texture_pool: Option<&mut GpuTexturePool>,
    ) {
        let HdrTileInsert {
            texture,
            view,
            compose_storage_view,
            tone_map_buffer,
            bind_group,
            jpeg_compose_bind_group,
            baked_jpeg_weight_bits,
        } = tile;
        self.protect_recent(key);
        self.insert_binding(
            key,
            HdrTileBinding {
                _texture: Some(texture),
                _view: Some(view),
                compose_storage_view,
                tone_map_buffer: Some(tone_map_buffer),
                bind_group: Some(bind_group),
                jpeg_compose_bind_group,
                estimated_bytes: 0,
                baked_jpeg_weight_bits,
            },
            texture_pool,
        );
    }

    pub(crate) fn insert_binding(
        &mut self,
        key: HdrTileKey,
        binding: HdrTileBinding,
        mut texture_pool: Option<&mut GpuTexturePool>,
    ) {
        if let Some(old_binding) = self.entries.remove(&key) {
            self.current_bytes = self
                .current_bytes
                .saturating_sub(old_binding.estimated_bytes);
            self.evictable_lru.remove(key);
            release_tile_texture(&mut texture_pool, &old_binding);
        }

        let bytes = hdr_tile_key_bytes(key);
        while !self.evictable_lru.is_empty()
            && self.current_bytes.saturating_add(bytes) > self.max_bytes
        {
            let Some(evicted_key) = self.evictable_lru.pop_oldest() else {
                break;
            };
            self.protected_recent.remove(&evicted_key);
            self.protected_order.remove(evicted_key);
            if let Some(evicted_binding) = self.entries.remove(&evicted_key) {
                self.current_bytes = self
                    .current_bytes
                    .saturating_sub(evicted_binding.estimated_bytes);
                release_tile_texture(&mut texture_pool, &evicted_binding);
            }
        }

        if self.current_bytes.saturating_add(bytes) <= self.max_bytes
            || self.protected_recent.contains(&key)
        {
            let mut binding = binding;
            binding.estimated_bytes = bytes;
            self.entries.insert(key, binding);
            if !self.protected_recent.contains(&key) {
                self.evictable_lru.touch(key);
            }
            self.current_bytes += bytes;
        }
    }

    pub(crate) fn protect_recent(&mut self, key: HdrTileKey) {
        self.evictable_lru.remove(key);
        self.protected_order.touch(key);
        self.protected_recent.insert(key);
        while self.protected_order.len() > HDR_TILE_BINDING_RECENT_PROTECTION_COUNT {
            if let Some(expired) = self.protected_order.pop_oldest() {
                self.protected_recent.remove(&expired);
                if self.entries.contains_key(&expired) {
                    self.evictable_lru.touch(expired);
                }
            }
        }
    }

    pub(crate) fn touch(&mut self, key: HdrTileKey) {
        if self.protected_recent.contains(&key) {
            self.protected_order.touch(key);
        } else {
            self.evictable_lru.touch(key);
        }
    }

    #[cfg(test)]
    pub(crate) fn current_bytes(&self) -> usize {
        self.current_bytes
    }

    #[cfg(test)]
    pub(crate) fn insert_placeholder(&mut self, key: HdrTileKey) {
        self.insert_binding(
            key,
            HdrTileBinding {
                _texture: None,
                _view: None,
                compose_storage_view: None,
                tone_map_buffer: None,
                bind_group: None,
                jpeg_compose_bind_group: None,
                estimated_bytes: 0,
                baked_jpeg_weight_bits: None,
            },
            None,
        );
    }

    #[cfg(test)]
    pub(crate) fn insert_protected_placeholder(&mut self, key: HdrTileKey) {
        self.protect_recent(key);
        self.insert_placeholder(key);
    }

    pub(crate) fn remove(&mut self, key: HdrTileKey) {
        if let Some(binding) = self.entries.remove(&key) {
            self.current_bytes = self.current_bytes.saturating_sub(binding.estimated_bytes);
        }
        self.evictable_lru.remove(key);
        self.protected_recent.remove(&key);
        self.protected_order.remove(key);
    }

    pub(crate) fn bind_group(&self, key: HdrTileKey) -> Option<&wgpu::BindGroup> {
        self.entries
            .get(&key)
            .and_then(|entry| entry.bind_group.as_ref())
    }

    pub(crate) fn binding(&self, key: HdrTileKey) -> Option<&HdrTileBinding> {
        self.entries.get(&key)
    }

    pub(crate) fn binding_mut(&mut self, key: HdrTileKey) -> Option<&mut HdrTileBinding> {
        self.entries.get_mut(&key)
    }
}

pub(crate) struct HdrTileBinding {
    pub(super) _texture: Option<Arc<wgpu::Texture>>,
    pub(super) _view: Option<wgpu::TextureView>,
    /// Storage view for ISO deferred tile GPU compose; reused across rebakes at the same tile size.
    pub(super) compose_storage_view: Option<wgpu::TextureView>,
    pub(super) tone_map_buffer: Option<wgpu::Buffer>,
    pub(super) bind_group: Option<wgpu::BindGroup>,
    pub(super) jpeg_compose_bind_group: Option<wgpu::BindGroup>,
    pub(super) estimated_bytes: usize,
    pub(super) baked_jpeg_weight_bits: Option<u32>,
}

pub(crate) fn iso_deferred_tile_compose_views_reusable(
    binding: &HdrTileBinding,
    width: u32,
    height: u32,
) -> Option<(wgpu::TextureView, wgpu::TextureView)> {
    let hdr_view = binding._view.as_ref()?;
    let storage_view = binding.compose_storage_view.as_ref()?;
    if binding._texture.is_none() || width == 0 || height == 0 {
        return None;
    }
    Some((hdr_view.clone(), storage_view.clone()))
}

pub(crate) fn hdr_tile_key_bytes(key: HdrTileKey) -> usize {
    if key.rgba_len > 0 {
        key.rgba_len * std::mem::size_of::<f32>()
    } else {
        key.width as usize * key.height as usize * 4 * std::mem::size_of::<f32>()
    }
}
