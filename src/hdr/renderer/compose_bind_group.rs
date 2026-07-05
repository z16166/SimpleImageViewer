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

pub(super) enum ComposePrimaryBinding<'a> {
    TextureView(&'a wgpu::TextureView),
    StorageBuffer { buffer: &'a wgpu::Buffer, size: u64 },
}

pub(super) fn create_compose_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    label: &'static str,
    primary: ComposePrimaryBinding<'_>,
    gain_view: &wgpu::TextureView,
    uniform_buffer: &wgpu::Buffer,
    display_storage_view: &wgpu::TextureView,
) -> wgpu::BindGroup {
    let primary_entry = match primary {
        ComposePrimaryBinding::TextureView(view) => wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::TextureView(view),
        },
        ComposePrimaryBinding::StorageBuffer { buffer, size } => wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                buffer,
                offset: 0,
                size: std::num::NonZeroU64::new(size),
            }),
        },
    };
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(label),
        layout,
        entries: &[
            primary_entry,
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(gain_view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: uniform_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(display_storage_view),
            },
        ],
    })
}
