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

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;

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
}

const MAX_IDLE_TEXTURES_PER_KEY: usize = 4;

#[derive(Default)]
pub(crate) struct GpuTexturePool {
    idle: HashMap<TexturePoolKey, Vec<Arc<wgpu::Texture>>>,
}

impl GpuTexturePool {
    pub(crate) fn acquire(
        &mut self,
        device: &wgpu::Device,
        desc: &wgpu::TextureDescriptor<'_>,
    ) -> Arc<wgpu::Texture> {
        let key = TexturePoolKey::from_descriptor(desc);
        if let Some(stack) = self.idle.get_mut(&key)
            && let Some(texture) = stack.pop()
        {
            return texture;
        }
        Arc::new(device.create_texture(desc))
    }

    pub(crate) fn release(&mut self, texture: Arc<wgpu::Texture>) {
        let key = TexturePoolKey {
            width: texture.width(),
            height: texture.height(),
            format: texture.format(),
            usage: texture.usage(),
        };
        let stack = self.idle.entry(key).or_default();
        if stack.len() < MAX_IDLE_TEXTURES_PER_KEY {
            stack.push(texture);
        }
    }
}

pub(crate) type SharedGpuTexturePool = Mutex<GpuTexturePool>;
