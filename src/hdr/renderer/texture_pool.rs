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

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use parking_lot::Mutex;

use crate::lru_order::LruOrder;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct TexturePoolKey {
    pub width: u32,
    pub height: u32,
    pub format: wgpu::TextureFormat,
    pub usage: wgpu::TextureUsages,
}

impl TexturePoolKey {
    pub(crate) fn from_descriptor(desc: &wgpu::TextureDescriptor<'_>) -> Self {
        Self {
            width: desc.size.width,
            height: desc.size.height,
            format: desc.format,
            usage: desc.usage,
        }
    }

    pub(crate) fn from_texture(texture: &wgpu::Texture) -> Self {
        Self {
            width: texture.width(),
            height: texture.height(),
            format: texture.format(),
            usage: texture.usage(),
        }
    }
}

const MAX_IDLE_TEXTURES_PER_KEY: usize = 4;

const BYTES_PER_MIB: usize = 1024 * 1024;
const MIN_GPU_TEXTURE_POOL_BYTES: usize = 128 * BYTES_PER_MIB;
const MAX_GPU_TEXTURE_POOL_BYTES: usize = 512 * BYTES_PER_MIB;
const GPU_TEXTURE_POOL_MEMORY_DIVISOR: usize = 32;

/// Max idle GPU texture bytes retained by the pool for a system RAM size.
///
/// A single oversized idle texture is allowed when the pool is otherwise empty so that very large
/// image planes can still be recycled across navigation; additional idle entries are LRU-evicted
/// once the budget is exceeded.
pub(crate) fn gpu_texture_pool_budget_for_memory(total_memory_bytes: usize) -> usize {
    (total_memory_bytes / GPU_TEXTURE_POOL_MEMORY_DIVISOR).clamp(
        MIN_GPU_TEXTURE_POOL_BYTES,
        MAX_GPU_TEXTURE_POOL_BYTES,
    )
}

fn configured_gpu_texture_pool_max_bytes() -> usize {
    let total_memory_bytes =
        (crate::system_memory::total_memory_mb() as usize).saturating_mul(BYTES_PER_MIB);
    gpu_texture_pool_budget_for_memory(total_memory_bytes)
}

fn texture_pool_key_bytes(key: &TexturePoolKey) -> usize {
    let block_bytes = key.format.block_copy_size(None).unwrap_or(0) as usize;
    if block_bytes == 0 {
        return 0;
    }
    let (block_w, block_h) = key.format.block_dimensions();
    let blocks_w = key.width.div_ceil(block_w) as usize;
    let blocks_h = key.height.div_ceil(block_h) as usize;
    blocks_w
        .checked_mul(blocks_h)
        .and_then(|product| product.checked_mul(block_bytes))
        .unwrap_or(usize::MAX)
}

#[derive(Default)]
pub(crate) struct GpuTexturePool {
    idle: HashMap<TexturePoolKey, Vec<Arc<wgpu::Texture>>>,
    idle_ptr_key: HashMap<usize, TexturePoolKey>,
    idle_lru: LruOrder<usize>,
    idle_bytes: usize,
    /// `Arc::as_ptr` addresses for textures handed out by [`Self::acquire`].
    issued: HashSet<usize>,
}

impl GpuTexturePool {
    pub(crate) fn acquire(
        &mut self,
        device: &wgpu::Device,
        desc: &wgpu::TextureDescriptor<'_>,
    ) -> Arc<wgpu::Texture> {
        let key = TexturePoolKey::from_descriptor(desc);
        let texture = if let Some(texture) = self.take_idle_texture(&key) {
            texture
        } else {
            Arc::new(device.create_texture(desc))
        };
        self.issued.insert(Arc::as_ptr(&texture) as usize);
        texture
    }

    pub(crate) fn release(&mut self, texture: Arc<wgpu::Texture>) {
        let ptr = Arc::as_ptr(&texture) as usize;
        if !self.issued.remove(&ptr) {
            return;
        }
        let key = TexturePoolKey::from_texture(&texture);
        self.try_cache_idle_texture(texture, key);
    }

    fn take_idle_texture(&mut self, key: &TexturePoolKey) -> Option<Arc<wgpu::Texture>> {
        let texture = self.idle.get_mut(key)?.pop()?;
        let ptr = Arc::as_ptr(&texture) as usize;
        self.idle_ptr_key.remove(&ptr);
        self.idle_lru.remove(ptr);
        let bytes = texture_pool_key_bytes(key);
        self.idle_bytes = self.idle_bytes.saturating_sub(bytes);
        if self.idle.get(key).is_some_and(|stack| stack.is_empty()) {
            self.idle.remove(key);
        }
        Some(texture)
    }

    fn try_cache_idle_texture(&mut self, texture: Arc<wgpu::Texture>, key: TexturePoolKey) {
        if self
            .idle
            .get(&key)
            .is_some_and(|stack| stack.len() >= MAX_IDLE_TEXTURES_PER_KEY)
        {
            return;
        }

        let bytes = texture_pool_key_bytes(&key);
        let budget = configured_gpu_texture_pool_max_bytes();
        let pool_was_empty = self.idle_bytes == 0;

        // Match pending GPU write backlog: one oversized idle texture when the pool is empty.
        if !pool_was_empty && bytes > budget {
            return;
        }

        let ptr = Arc::as_ptr(&texture) as usize;
        self.idle.entry(key).or_default().push(texture);
        self.idle_ptr_key.insert(ptr, key);
        self.idle_lru.touch(ptr);
        self.idle_bytes = self.idle_bytes.saturating_add(bytes);

        if !(pool_was_empty && bytes > budget) {
            self.evict_idle_over_budget(budget);
        }
    }

    fn evict_idle_over_budget(&mut self, budget: usize) {
        while self.idle_bytes > budget {
            let Some(ptr) = self.idle_lru.pop_oldest() else {
                break;
            };
            let Some(key) = self.idle_ptr_key.remove(&ptr) else {
                continue;
            };
            let bytes = texture_pool_key_bytes(&key);
            self.idle_bytes = self.idle_bytes.saturating_sub(bytes);
            if let Some(stack) = self.idle.get_mut(&key)
                && let Some(pos) = stack.iter().position(|texture| Arc::as_ptr(texture) as usize == ptr)
            {
                stack.remove(pos);
            }
            if self.idle.get(&key).is_some_and(|stack| stack.is_empty()) {
                self.idle.remove(&key);
            }
        }
    }
}

pub(crate) type SharedGpuTexturePool = Mutex<GpuTexturePool>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_texture_pool_budget_scales_with_system_memory() {
        const GIB: usize = 1024 * 1024 * 1024;

        assert_eq!(
            gpu_texture_pool_budget_for_memory(4 * GIB),
            128 * BYTES_PER_MIB
        );
        assert_eq!(
            gpu_texture_pool_budget_for_memory(32 * GIB),
            512 * BYTES_PER_MIB
        );
        assert_eq!(
            gpu_texture_pool_budget_for_memory(128 * GIB),
            512 * BYTES_PER_MIB
        );
    }

    #[test]
    fn texture_pool_key_bytes_uses_block_layout() {
        let key = TexturePoolKey {
            width: 512,
            height: 512,
            format: wgpu::TextureFormat::Rgba32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
        };
        assert_eq!(texture_pool_key_bytes(&key), 512 * 512 * 16);
    }
}
