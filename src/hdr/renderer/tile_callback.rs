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

pub(crate) struct HdrTilePlaneCallback {
    pub(super) tile: Arc<crate::hdr::tiled::HdrTileBuffer>,
    pub(super) tone_map: HdrToneMapSettings,
    pub(super) target_format: wgpu::TextureFormat,
    pub(super) output_mode: HdrRenderOutputMode,
    pub(super) rotation_steps: u32,
    pub(super) alpha: f32,
    pub(super) uv_rect: egui::Rect,
}

impl CallbackTrait for HdrTilePlaneCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        _egui_encoder: &mut wgpu::CommandEncoder,
        callback_resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        if !ensure_hdr_callback_resources(device, self.target_format, callback_resources) {
            return Vec::new();
        }

        let Some(resources) = callback_resources.get_mut::<HdrCallbackResources>() else {
            return Vec::new();
        };

        let native_display_scale = libavif_tone_map_native_display_scale(
            &self.tile.metadata,
            self.tile.color_space,
            &self.tone_map,
        );
        let tile_key = HdrTileKey::from_tile_with_uv(&self.tile, self.uv_rect);
        let iso_deferred = iso_deferred_from_metadata(&self.tile.metadata);
        let tile_ctx = self.tile.iso_deferred_tile;
        let iso_deferred_tile = iso_deferred.is_some() && tile_ctx.is_some();
        let target_capacity_bits = self.tone_map.target_hdr_capacity().to_bits();
        let binding_baked = resources
            .tile_bindings
            .binding(tile_key)
            .and_then(|binding| binding.baked_jpeg_weight_bits);
        let needs_compose = iso_deferred_tile && binding_baked != Some(target_capacity_bits);
        let jpeg_gpu_composed =
            iso_deferred_tile && (needs_compose || binding_baked == Some(target_capacity_bits));
        let uniform = hdr_tile_tone_map_uniform(HdrTileToneMapUniformParams {
            common: ToneMapCommonParams {
                settings: self.tone_map,
                rotation_steps: self.rotation_steps,
                alpha: self.alpha,
                output_mode: self.output_mode,
                framebuffer_format: self.target_format,
                uv_rect: self.uv_rect,
                native_display_scale,
            },
            tile: &self.tile,
            jpeg_gpu_composed,
        });

        if let (Some(deferred), Some(ctx)) = (iso_deferred, tile_ctx) {
            let upload_key = JpegTiledUploadKey {
                sdr_ptr: std::sync::Arc::as_ptr(&deferred.sdr_rgba) as usize,
                gain_ptr: std::sync::Arc::as_ptr(&deferred.gain_rgba) as usize,
            };
            if resources.jpeg_tiled_upload_key != Some(upload_key) {
                match upload_jpeg_tiled_source_textures(
                    device,
                    queue,
                    deferred,
                    ctx.physical_width,
                    ctx.physical_height,
                    device.limits().max_texture_dimension_2d,
                ) {
                    Ok((sdr, gain)) => {
                        resources.jpeg_tiled_upload_key = Some(upload_key);
                        resources.jpeg_tiled_sdr_texture = Some(sdr.texture);
                        resources.jpeg_tiled_sdr_view = Some(sdr.view);
                        resources.jpeg_tiled_gain_texture = Some(gain.texture);
                        resources.jpeg_tiled_gain_view = Some(gain.view);
                    }
                    Err(err) => {
                        log::warn!("[HDR] Skipping JPEG tiled source upload: {err}");
                        resources.tile_bindings.remove(tile_key);
                        return Vec::new();
                    }
                }
            }

            let needs_compose = resources
                .tile_bindings
                .binding(tile_key)
                .and_then(|binding| binding.baked_jpeg_weight_bits)
                != Some(target_capacity_bits);

            if needs_compose {
                let gpu_compose_ready = resources.jpeg_compose_bind_group_layout.is_some()
                    && resources.jpeg_compose_pipeline.is_some()
                    && resources.jpeg_compose_tile_pipeline.is_some()
                    && resources.jpeg_compose_uniform_buffer.is_some();

                let reused_compose =
                    resources
                        .tile_bindings
                        .binding(tile_key)
                        .and_then(|binding| {
                            iso_deferred_tile_compose_views_reusable(
                                binding,
                                self.tile.width,
                                self.tile.height,
                            )
                        });

                if let Some((hdr_view, display_storage)) = reused_compose {
                    if gpu_compose_ready {
                        let Some(sdr_view) = resources.jpeg_tiled_sdr_view.as_ref() else {
                            return Vec::new();
                        };
                        let Some(gain_view) = resources.jpeg_tiled_gain_view.as_ref() else {
                            return Vec::new();
                        };
                        let compose_command = jpeg_compose_gpu::encode_tile_compose_compute_pass(
                            jpeg_compose_gpu::JpegTileComposePass {
                                device,
                                queue,
                                resources,
                                deferred,
                                tile_ctx: &ctx,
                                tile_width: self.tile.width,
                                tile_height: self.tile.height,
                                tone_map: &self.tone_map,
                                sdr_view,
                                gain_view,
                                display_storage_view: &display_storage,
                            },
                        );
                        if let Some(binding) = resources.tile_bindings.binding_mut(tile_key) {
                            binding.baked_jpeg_weight_bits = Some(target_capacity_bits);
                            if let Some(buffer) = binding.tone_map_buffer.as_ref() {
                                queue.write_buffer(buffer, 0, bytemuck::bytes_of(&uniform));
                            }
                        }
                        return vec![compose_command];
                    }

                    if let Some(binding) = resources.tile_bindings.binding_mut(tile_key) {
                        let Some(texture) = binding._texture.as_ref() else {
                            resources.tile_bindings.remove(tile_key);
                            return Vec::new();
                        };
                        // CPU fallback composes synchronously in the render callback; large tiles
                        // can stall a frame. Move this work to a background task if it becomes visible.
                        let composed = match crate::hdr::jpeg_gain_map_gpu::compose_iso_deferred_tile_cpu_pixels(
                            deferred,
                            &ctx,
                            self.tile.width,
                            self.tile.height,
                            self.tone_map.target_hdr_capacity(),
                        ) {
                            Ok(composed) => composed,
                            Err(err) => {
                                log::warn!("[HDR] ISO tile CPU compose failed: {err}");
                                resources.tile_bindings.remove(tile_key);
                                return Vec::new();
                            }
                        };
                        if let Err(err) = write_rgba32f_to_texture(
                            queue,
                            texture,
                            self.tile.width,
                            self.tile.height,
                            &composed,
                        ) {
                            log::warn!("[HDR] ISO tile CPU compose upload failed: {err}");
                            resources.tile_bindings.remove(tile_key);
                            return Vec::new();
                        }
                        binding.baked_jpeg_weight_bits = Some(target_capacity_bits);
                        if let Some(buffer) = binding.tone_map_buffer.as_ref() {
                            queue.write_buffer(buffer, 0, bytemuck::bytes_of(&uniform));
                        }
                    }
                    let _ = hdr_view;
                    // CPU compose writes directly into the existing texture, so no GPU command buffer is needed.
                    return Vec::new();
                }

                let Some(sdr_view) = resources.jpeg_tiled_sdr_view.as_ref() else {
                    return Vec::new();
                };
                let Some(gain_view) = resources.jpeg_tiled_gain_view.as_ref() else {
                    return Vec::new();
                };

                match create_empty_rgba32f_texture(device, self.tile.width, self.tile.height) {
                    Ok(uploaded) => {
                        if gpu_compose_ready {
                            let Some(display_storage) = uploaded.storage_view.as_ref() else {
                                return Vec::new();
                            };
                            let compose_command =
                                jpeg_compose_gpu::encode_tile_compose_compute_pass(
                                    jpeg_compose_gpu::JpegTileComposePass {
                                        device,
                                        queue,
                                        resources,
                                        deferred,
                                        tile_ctx: &ctx,
                                        tile_width: self.tile.width,
                                        tile_height: self.tile.height,
                                        tone_map: &self.tone_map,
                                        sdr_view,
                                        gain_view,
                                        display_storage_view: display_storage,
                                    },
                                );
                            if !resources.image_bindings.is_empty() {
                                resources.image_bindings.clear();
                            }
                            let tone_map_buffer =
                                device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                    label: Some(
                                        "simple-image-viewer-hdr-tile-plane-tone-map-buffer",
                                    ),
                                    contents: bytemuck::bytes_of(&uniform),
                                    usage: wgpu::BufferUsages::UNIFORM
                                        | wgpu::BufferUsages::COPY_DST,
                                });
                            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                                label: Some("simple-image-viewer-hdr-tile-plane-bind-group"),
                                layout: &resources.bind_group_layout,
                                entries: &[
                                    wgpu::BindGroupEntry {
                                        binding: 0,
                                        resource: wgpu::BindingResource::TextureView(
                                            &uploaded.view,
                                        ),
                                    },
                                    wgpu::BindGroupEntry {
                                        binding: 1,
                                        resource: wgpu::BindingResource::TextureView(
                                            &resources.dummy_gain_view,
                                        ),
                                    },
                                    wgpu::BindGroupEntry {
                                        binding: 2,
                                        resource: tone_map_buffer.as_entire_binding(),
                                    },
                                ],
                            });
                            resources.tile_bindings.insert(
                                tile_key,
                                HdrTileInsert {
                                    texture: uploaded.texture,
                                    view: uploaded.view,
                                    compose_storage_view: uploaded.storage_view,
                                    tone_map_buffer,
                                    bind_group,
                                    baked_jpeg_weight_bits: Some(target_capacity_bits),
                                },
                            );
                            return vec![compose_command];
                        }

                        // CPU fallback composes synchronously in the render callback; large tiles
                        // can stall a frame. Move this work to a background task if it becomes visible.
                        let composed = match crate::hdr::jpeg_gain_map_gpu::compose_iso_deferred_tile_cpu_pixels(
                            deferred,
                            &ctx,
                            self.tile.width,
                            self.tile.height,
                            self.tone_map.target_hdr_capacity(),
                        ) {
                            Ok(composed) => composed,
                            Err(err) => {
                                log::warn!("[HDR] ISO tile CPU compose failed: {err}");
                                resources.tile_bindings.remove(tile_key);
                                return Vec::new();
                            }
                        };
                        if let Err(err) = write_rgba32f_to_texture(
                            queue,
                            &uploaded.texture,
                            self.tile.width,
                            self.tile.height,
                            &composed,
                        ) {
                            log::warn!("[HDR] ISO tile CPU compose upload failed: {err}");
                            resources.tile_bindings.remove(tile_key);
                            return Vec::new();
                        }
                        if !resources.image_bindings.is_empty() {
                            resources.image_bindings.clear();
                        }
                        let tone_map_buffer =
                            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                label: Some("simple-image-viewer-hdr-tile-plane-tone-map-buffer"),
                                contents: bytemuck::bytes_of(&uniform),
                                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                            });
                        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                            label: Some("simple-image-viewer-hdr-tile-plane-bind-group"),
                            layout: &resources.bind_group_layout,
                            entries: &[
                                wgpu::BindGroupEntry {
                                    binding: 0,
                                    resource: wgpu::BindingResource::TextureView(&uploaded.view),
                                },
                                wgpu::BindGroupEntry {
                                    binding: 1,
                                    resource: wgpu::BindingResource::TextureView(
                                        &resources.dummy_gain_view,
                                    ),
                                },
                                wgpu::BindGroupEntry {
                                    binding: 2,
                                    resource: tone_map_buffer.as_entire_binding(),
                                },
                            ],
                        });
                        resources.tile_bindings.insert(
                            tile_key,
                            HdrTileInsert {
                                texture: uploaded.texture,
                                view: uploaded.view,
                                compose_storage_view: uploaded.storage_view,
                                tone_map_buffer,
                                bind_group,
                                baked_jpeg_weight_bits: Some(target_capacity_bits),
                            },
                        );
                        return Vec::new();
                    }
                    Err(err) => {
                        log::warn!("[HDR] Skipping JPEG deferred tile compose: {err}");
                        resources.tile_bindings.remove(tile_key);
                        return Vec::new();
                    }
                }
            }

            if !resources.tile_bindings.contains(tile_key) {
                return Vec::new();
            }
        } else if !resources.tile_bindings.contains(tile_key) {
            match upload_callback_tile(device, queue, &self.tile) {
                Ok(uploaded) => {
                    if !resources.image_bindings.is_empty() {
                        resources.image_bindings.clear();
                    }
                    let tone_map_buffer =
                        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                            label: Some("simple-image-viewer-hdr-tile-plane-tone-map-buffer"),
                            contents: bytemuck::bytes_of(&uniform),
                            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                        });
                    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("simple-image-viewer-hdr-tile-plane-bind-group"),
                        layout: &resources.bind_group_layout,
                        entries: &[
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: wgpu::BindingResource::TextureView(&uploaded.view),
                            },
                            wgpu::BindGroupEntry {
                                binding: 1,
                                resource: wgpu::BindingResource::TextureView(
                                    &resources.dummy_gain_view,
                                ),
                            },
                            wgpu::BindGroupEntry {
                                binding: 2,
                                resource: tone_map_buffer.as_entire_binding(),
                            },
                        ],
                    });
                    resources.tile_bindings.insert(
                        tile_key,
                        HdrTileInsert {
                            texture: uploaded.texture,
                            view: uploaded.view,
                            compose_storage_view: None,
                            tone_map_buffer,
                            bind_group,
                            baked_jpeg_weight_bits: None,
                        },
                    );
                }
                Err(err) => {
                    log::warn!("[HDR] Skipping HDR tile plane upload: {err}");
                    resources.tile_bindings.remove(tile_key);
                }
            }
        }
        if let Some(binding) = resources.tile_bindings.binding_mut(tile_key)
            && let Some(buffer) = binding.tone_map_buffer.as_ref()
        {
            let binding_baked = binding.baked_jpeg_weight_bits;
            let jpeg_gpu_composed =
                iso_deferred_tile && binding_baked == Some(target_capacity_bits);
            let uniform = hdr_tile_tone_map_uniform(HdrTileToneMapUniformParams {
                common: ToneMapCommonParams {
                    settings: self.tone_map,
                    rotation_steps: self.rotation_steps,
                    alpha: self.alpha,
                    output_mode: self.output_mode,
                    framebuffer_format: self.target_format,
                    uv_rect: self.uv_rect,
                    native_display_scale,
                },
                tile: &self.tile,
                jpeg_gpu_composed,
            });
            queue.write_buffer(buffer, 0, bytemuck::bytes_of(&uniform));
        }

        Vec::new()
    }

    fn paint(
        &self,
        info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        callback_resources: &CallbackResources,
    ) {
        let Some(resources) = callback_resources.get::<HdrCallbackResources>() else {
            return;
        };
        let tile_key = HdrTileKey::from_tile_with_uv(&self.tile, self.uv_rect);
        let Some(bind_group) = resources.tile_bindings.bind_group(tile_key) else {
            return;
        };

        let viewport = info.viewport_in_pixels();
        render_pass.set_viewport(
            viewport.left_px as f32,
            viewport.top_px as f32,
            viewport.width_px as f32,
            viewport.height_px as f32,
            0.0,
            1.0,
        );
        let scissor = info.clip_rect_in_pixels();
        render_pass.set_scissor_rect(
            scissor.left_px.max(0) as u32,
            scissor.top_px.max(0) as u32,
            scissor.width_px.max(0) as u32,
            scissor.height_px.max(0) as u32,
        );
        render_pass.set_pipeline(&resources.pipeline);
        render_pass.set_bind_group(0, bind_group, &[]);
        render_pass.draw(0..3, 0..1);
    }
}
