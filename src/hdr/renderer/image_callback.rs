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

/// GPU bindings for static HDR image planes (AVIF/JXL ISO gain-map, etc.).
/// Page-flip transitions need prev + current + several preloaded neighbors resident at once.
const MAX_HDR_IMAGE_PLANE_BINDINGS: usize = 8;
/// Do not evict bindings touched within this window (covers one transition frame).
const HDR_IMAGE_BINDING_EVICTION_PROTECT: std::time::Duration =
    std::time::Duration::from_millis(50);
/// A keep-resident binding that has not been prepared again by this point is no longer on screen.
const HDR_IMAGE_BINDING_KEEP_RESIDENT_ABANDONED_AFTER: std::time::Duration =
    std::time::Duration::from_millis(100);

pub(super) fn hdr_image_binding_is_eviction_candidate(
    keep_resident: bool,
    last_use: std::time::Instant,
    now: std::time::Instant,
) -> bool {
    let age = now.duration_since(last_use);
    age > HDR_IMAGE_BINDING_EVICTION_PROTECT
        && (!keep_resident || age > HDR_IMAGE_BINDING_KEEP_RESIDENT_ABANDONED_AFTER)
}

pub(crate) struct HdrImagePlaneCallback {
    pub(super) image: Arc<HdrImageBuffer>,
    pub(super) tone_map: HdrToneMapSettings,
    pub(super) target_format: wgpu::TextureFormat,
    pub(super) output_mode: HdrRenderOutputMode,
    pub(super) rotation_steps: u32,
    pub(super) alpha: f32,
    pub(super) uv_rect: egui::Rect,
    pub(super) ripple: Option<(egui::Pos2, f32, f32, u32)>,
    /// When true, the GPU binding for this image is not evicted from the LRU cache.
    pub(super) keep_resident: bool,
    pub(super) raw_demosaic_baked_notify: Option<Arc<Mutex<Vec<HdrImageKey>>>>,
}

impl CallbackTrait for HdrImagePlaneCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        _egui_encoder: &mut wgpu::CommandEncoder,
        callback_resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let needs_resources = callback_resources
            .get::<HdrCallbackResources>()
            .map_or(true, |resources| {
                resources.target_format != self.target_format
            });
        if needs_resources {
            callback_resources.insert(create_callback_resources(device, self.target_format));
        }

        let Some(resources) = callback_resources.get_mut::<HdrCallbackResources>() else {
            return Vec::new();
        };

        let native_display_scale = libavif_tone_map_native_display_scale(
            &self.image.metadata,
            self.image.color_space,
            &self.tone_map,
        );

        let image_key = HdrImageKey::from_image(&self.image);
        let iso_deferred = iso_deferred_from_metadata(&self.image.metadata);
        #[cfg(feature = "heif-native")]
        let apple_deferred = apple_heic_deferred_from_metadata(&self.image.metadata);
        #[cfg(not(feature = "heif-native"))]
        let apple_deferred: Option<&crate::hdr::types::AppleHeicGainMapGpuSource> = None;
        let raw_source = self.image.metadata.raw_gpu_source.as_ref();
        let target_capacity_bits = self.tone_map.target_hdr_capacity().to_bits();
        let jpeg_failed = iso_deferred.is_some()
            && resources
                .failed_jpeg_image_compose
                .contains(&(image_key, target_capacity_bits));
        #[cfg(feature = "heif-native")]
        let apple_failed = apple_deferred.is_some()
            && resources
                .failed_apple_image_compose
                .contains(&(image_key, target_capacity_bits));
        #[cfg(not(feature = "heif-native"))]
        let apple_failed = false;
        let raw_failed = raw_source.is_some() && resources.failed_raw_demosaic.contains(&image_key);
        if jpeg_failed || apple_failed || raw_failed {
            return Vec::new();
        }

        if !resources.image_bindings.contains_key(&image_key) {
            match upload_image_plane(device, queue, &self.image) {
                Ok(uploaded) => {
                    let tone_map_buffer =
                        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                            label: Some("simple-image-viewer-hdr-image-plane-tone-map-buffer"),
                            contents: bytemuck::bytes_of(&ToneMapUniform::from_settings(
                                HdrToneMapSettings::default(),
                                0,
                                1.0,
                                HdrRenderOutputMode::SdrToneMapped,
                                self.target_format,
                                HdrColorSpace::LinearSrgb,
                                HdrTransferFunction::Linear,
                                HdrReference::Unknown,
                                egui::Rect::from_min_max(
                                    egui::Pos2::ZERO,
                                    egui::Pos2::new(1.0, 1.0),
                                ),
                                1.0,
                                None,
                                None,
                            )),
                            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                        });

                    let jpeg_compose_uniform_buffer = if iso_deferred.is_some() {
                        Some(device.create_buffer(&wgpu::BufferDescriptor {
                            label: Some("simple-image-viewer-hdr-jpeg-compose-uniform-buffer"),
                            size: 128,
                            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                            mapped_at_creation: false,
                        }))
                    } else {
                        None
                    };

                    #[cfg(feature = "heif-native")]
                    let (
                        compose_tone_map_buffer,
                        encoded_primary_buffer,
                        encoded_primary_buffer_bytes,
                    ) = if apple_deferred.is_some() {
                        let compose_buf = device.create_buffer(&wgpu::BufferDescriptor {
                            label: Some("simple-image-viewer-hdr-apple-compose-tone-map-buffer"),
                            size: std::mem::size_of::<ToneMapUniform>() as u64,
                            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                            mapped_at_creation: false,
                        });
                        (Some(compose_buf), None, 0)
                    } else {
                        (None, None, 0)
                    };

                    let (uploaded_texture, uploaded_view, uploaded_display_storage_view) = (
                        uploaded.base.texture,
                        uploaded.base.view,
                        uploaded.base.storage_view,
                    );
                    let (uploaded_gain_texture, uploaded_gain_view) =
                        if let Some(gain) = uploaded.gain {
                            (Some(gain.texture), Some(gain.view))
                        } else {
                            (None, None)
                        };
                    let (uploaded_sdr_texture, uploaded_sdr_view) =
                        if let Some(sdr) = uploaded.sdr_baseline {
                            (Some(sdr.texture), Some(sdr.view))
                        } else {
                            (None, None)
                        };
                    let (uploaded_raw_pixels_texture, uploaded_raw_pixels_view) =
                        if let Some(raw) = uploaded.raw_pixels {
                            (Some(raw.texture), Some(raw.view))
                        } else {
                            (None, None)
                        };
                    let uploaded_raw_green_plane_view =
                        uploaded.raw_green_plane.and_then(|g| g.storage_view);
                    let uploaded_raw_r_at_green_plane_view =
                        uploaded.raw_r_at_green_plane.and_then(|g| g.storage_view);
                    let uploaded_raw_b_at_green_plane_view =
                        uploaded.raw_b_at_green_plane.and_then(|g| g.storage_view);

                    let binding = HdrImageBinding {
                        uploaded_texture,
                        uploaded_view,
                        uploaded_gain_texture,
                        uploaded_gain_view,
                        uploaded_sdr_texture,
                        uploaded_sdr_view,
                        uploaded_display_storage_view,
                        uploaded_raw_pixels_texture,
                        uploaded_raw_pixels_view,
                        uploaded_raw_green_plane_view,
                        uploaded_raw_r_at_green_plane_view,
                        uploaded_raw_b_at_green_plane_view,
                        baked_jpeg_image_key: None,
                        baked_jpeg_weight_bits: None,
                        baked_apple_image_key: None,
                        baked_apple_weight_bits: None,
                        baked_raw_demosaic_key: None,
                        baked_raw_demosaic_method: None,
                        tone_map_buffer,
                        jpeg_compose_uniform_buffer,
                        #[cfg(feature = "heif-native")]
                        compose_tone_map_buffer,
                        #[cfg(feature = "heif-native")]
                        encoded_primary_buffer,
                        #[cfg(feature = "heif-native")]
                        encoded_primary_buffer_bytes,
                        #[cfg(feature = "heif-native")]
                        encoded_primary_source_ptr: None,
                        bind_group: None,
                        last_use: std::time::Instant::now(),
                        keep_resident: false,
                    };
                    resources.image_bindings.insert(image_key, binding);
                }
                Err(err) => {
                    log::warn!("[HDR] Skipping HDR image plane upload: {err}");
                    return Vec::new();
                }
            }
        }

        let Some(binding) = resources.image_bindings.get_mut(&image_key) else {
            return Vec::new();
        };
        binding.last_use = std::time::Instant::now();
        binding.keep_resident = self.keep_resident;

        let needs_raw_demosaic = raw_source.is_some()
            && (binding.baked_raw_demosaic_key != Some(image_key)
                || binding.baked_raw_demosaic_method != raw_source.map(|s| s.demosaic_method));
        let needs_jpeg_compose = iso_deferred.is_some()
            && (binding.baked_jpeg_image_key != Some(image_key)
                || binding.baked_jpeg_weight_bits != Some(target_capacity_bits));
        #[cfg(feature = "heif-native")]
        let needs_apple_compose = apple_deferred.is_some()
            && (binding.baked_apple_image_key != Some(image_key)
                || binding.baked_apple_weight_bits != Some(target_capacity_bits));
        #[cfg(not(feature = "heif-native"))]
        let needs_apple_compose = false;

        let mut compose_command_buffers = Vec::new();
        if needs_raw_demosaic {
            if let Some(source) = raw_source {
                if let (
                    Some(compose_layout),
                    Some(green_pipeline),
                    Some(rb_at_green_pipeline),
                    Some(rgb_pipeline),
                    Some(uniform_buf),
                ) = (
                    resources.raw_demosaic_bind_group_layout.as_ref(),
                    resources.raw_demosaic_green_pipeline.as_ref(),
                    resources.raw_demosaic_rb_at_green_pipeline.as_ref(),
                    resources.raw_demosaic_rgb_pipeline.as_ref(),
                    resources.raw_demosaic_uniform_buffer.as_ref(),
                ) {
                    #[cfg(feature = "preload-debug")]
                    let demosaic_bake_started = std::time::Instant::now();
                    log::debug!(
                        "[HDR] GPU RAW demosaicing path=GPU size={}x{} method={:?}",
                        self.image.width,
                        self.image.height,
                        source.demosaic_method
                    );
                    crate::preload_debug!(
                        "[PreloadDebug][RAW-GPU] demosaic bake {}x{} first={}",
                        self.image.width,
                        self.image.height,
                        binding.baked_raw_demosaic_key != Some(image_key)
                    );
                    let raw_pixels_view = binding
                        .uploaded_raw_pixels_view
                        .as_ref()
                        .expect("raw pixels view");
                    let green_plane_view = binding
                        .uploaded_raw_green_plane_view
                        .as_ref()
                        .expect("raw green plane view");
                    let r_at_green_plane_view = binding
                        .uploaded_raw_r_at_green_plane_view
                        .as_ref()
                        .expect("raw r-at-green plane view");
                    let b_at_green_plane_view = binding
                        .uploaded_raw_b_at_green_plane_view
                        .as_ref()
                        .expect("raw b-at-green plane view");
                    let output_view = binding
                        .uploaded_display_storage_view
                        .as_ref()
                        .expect("display storage view");
                    compose_command_buffers.push(
                        crate::hdr::raw_demosaic_gpu::encode_raw_demosaic_compute_pass(
                            device,
                            queue,
                            compose_layout,
                            green_pipeline,
                            rb_at_green_pipeline,
                            rgb_pipeline,
                            source,
                            raw_pixels_view,
                            green_plane_view,
                            r_at_green_plane_view,
                            b_at_green_plane_view,
                            output_view,
                            uniform_buf,
                        ),
                    );
                    binding.baked_raw_demosaic_key = Some(image_key);
                    binding.baked_raw_demosaic_method = Some(source.demosaic_method);
                    if let Some(notify) = self.raw_demosaic_baked_notify.as_ref()
                        && let Ok(mut pending) = notify.lock()
                    {
                        pending.push(image_key);
                    }
                    #[cfg(feature = "preload-debug")]
                    {
                        crate::preload_debug!(
                            "[PreloadDebug][RAW-GPU] demosaic encode queued {}x{} {:.0}ms",
                            self.image.width,
                            self.image.height,
                            demosaic_bake_started.elapsed().as_secs_f64() * 1000.0
                        );
                    }
                } else {
                    log::warn!(
                        "[HDR] GPU RAW demosaicing unavailable; falling back to CPU placeholder"
                    );
                    resources.failed_raw_demosaic.insert(image_key);
                    return Vec::new();
                }
            }
        }
        if needs_jpeg_compose {
            if let Some(deferred) = iso_deferred {
                if let (Some(compose_layout), Some(compose_pipeline)) = (
                    resources.jpeg_compose_bind_group_layout.as_ref(),
                    resources.jpeg_compose_pipeline.as_ref(),
                ) {
                    log::debug!(
                        "[HDR] ISO gain-map compose path=GPU size={}x{} gain={}x{} target_capacity={:.3} weight={:.3}",
                        self.image.width,
                        self.image.height,
                        deferred.gain_width,
                        deferred.gain_height,
                        self.tone_map.target_hdr_capacity(),
                        crate::hdr::gain_map::gain_map_weight(
                            deferred.metadata,
                            self.tone_map.target_hdr_capacity()
                        )
                    );
                    let sdr_view = binding.uploaded_sdr_view.as_ref().expect("jpeg sdr view");
                    let gain_view = binding.uploaded_gain_view.as_ref().expect("jpeg gain view");
                    let display_storage = binding
                        .uploaded_display_storage_view
                        .as_ref()
                        .expect("jpeg display storage view");
                    let uniform_buf = binding
                        .jpeg_compose_uniform_buffer
                        .as_ref()
                        .expect("jpeg compose uniform buffer");
                    compose_command_buffers.push(jpeg_compose_gpu::encode_compose_compute_pass(
                        device,
                        queue,
                        compose_layout,
                        compose_pipeline,
                        &self.image,
                        deferred,
                        &self.tone_map,
                        sdr_view,
                        gain_view,
                        display_storage,
                        uniform_buf,
                    ));
                    binding.baked_jpeg_image_key = Some(image_key);
                    binding.baked_jpeg_weight_bits = Some(target_capacity_bits);
                } else {
                    log::debug!(
                        "[HDR] ISO gain-map compose path=CPU size={}x{} gain={}x{} target_capacity={:.3} weight={:.3}",
                        self.image.width,
                        self.image.height,
                        deferred.gain_width,
                        deferred.gain_height,
                        self.tone_map.target_hdr_capacity(),
                        crate::hdr::gain_map::gain_map_weight(
                            deferred.metadata,
                            self.tone_map.target_hdr_capacity()
                        )
                    );
                    // CPU fallback composes synchronously in the render callback; large images can
                    // stall a frame. Move this work to a background task if it becomes user-visible.
                    match crate::hdr::jpeg_gain_map_gpu::compose_iso_deferred_cpu_pixels(
                        self.image.width,
                        self.image.height,
                        deferred,
                        self.tone_map.target_hdr_capacity(),
                    ) {
                        Ok(composed) => {
                            if let Err(err) = write_rgba32f_to_texture(
                                queue,
                                &binding.uploaded_texture,
                                self.image.width,
                                self.image.height,
                                &composed,
                            ) {
                                log::warn!("[HDR] ISO CPU compose upload failed: {err}");
                                resources
                                    .failed_jpeg_image_compose
                                    .insert((image_key, target_capacity_bits));
                                resources.image_bindings.remove(&image_key);
                                return Vec::new();
                            } else {
                                binding.baked_jpeg_image_key = Some(image_key);
                                binding.baked_jpeg_weight_bits = Some(target_capacity_bits);
                            }
                        }
                        Err(err) => {
                            log::warn!("[HDR] ISO CPU compose failed: {err}");
                            resources
                                .failed_jpeg_image_compose
                                .insert((image_key, target_capacity_bits));
                            resources.image_bindings.remove(&image_key);
                            return Vec::new();
                        }
                    }
                }
            }
        }

        #[cfg(feature = "heif-native")]
        if needs_apple_compose {
            if let Some(deferred) = apple_deferred {
                if let (Some(compose_layout), Some(compose_pipeline)) = (
                    resources.compose_bind_group_layout.as_ref(),
                    resources.compose_pipeline.as_ref(),
                ) {
                    let primary_ptr = std::sync::Arc::as_ptr(&self.image.rgba_f32) as usize;
                    let upload_primary = binding.encoded_primary_source_ptr != Some(primary_ptr);
                    let max_binding = device.limits().max_storage_buffer_binding_size;
                    if let Err(err) = apple_compose_gpu::ensure_encoded_primary_buffer(
                        device,
                        binding,
                        self.image.width,
                        max_binding,
                    ) {
                        log::warn!(
                            "[HDR] Apple GPU compose primary buffer allocation failed: {err}"
                        );
                        resources
                            .failed_apple_image_compose
                            .insert((image_key, target_capacity_bits));
                        resources.image_bindings.remove(&image_key);
                        return Vec::new();
                    } else {
                        let gain_view = binding.uploaded_gain_view.as_ref().expect("gain view");
                        let display_storage = binding
                            .uploaded_display_storage_view
                            .as_ref()
                            .expect("display storage view");
                        let encoded_primary_buffer = binding
                            .encoded_primary_buffer
                            .as_ref()
                            .expect("encoded primary buffer");
                        let compose_tone_map_buf = binding
                            .compose_tone_map_buffer
                            .as_ref()
                            .expect("apple compose tone map buffer");
                        compose_command_buffers.push(
                            apple_compose_gpu::encode_compose_compute_pass(
                                device,
                                queue,
                                compose_layout,
                                compose_pipeline,
                                &self.image,
                                deferred,
                                &self.tone_map,
                                encoded_primary_buffer,
                                gain_view,
                                display_storage,
                                upload_primary,
                                compose_tone_map_buf,
                            ),
                        );
                        if upload_primary {
                            binding.encoded_primary_source_ptr = Some(primary_ptr);
                        }
                        binding.baked_apple_image_key = Some(image_key);
                        binding.baked_apple_weight_bits = Some(target_capacity_bits);
                    }
                } else {
                    // CPU fallback composes synchronously in the render callback; large images can
                    // stall a frame. Move this work to a background task if it becomes user-visible.
                    match crate::hdr::heif_apple_gain_map_gpu::compose_apple_heic_deferred_cpu_pixels(
                        &self.image,
                        self.tone_map.target_hdr_capacity(),
                    ) {
                        Ok(composed) => {
                            if let Err(err) = write_rgba32f_to_texture(
                                queue,
                                &binding.uploaded_texture,
                                self.image.width,
                                self.image.height,
                                &composed,
                            ) {
                                log::warn!("[HDR] Apple CPU compose upload failed: {err}");
                                resources
                                    .failed_apple_image_compose
                                    .insert((image_key, target_capacity_bits));
                                resources.image_bindings.remove(&image_key);
                                return Vec::new();
                            } else {
                                binding.baked_apple_image_key = Some(image_key);
                                binding.baked_apple_weight_bits = Some(target_capacity_bits);
                            }
                        }
                        Err(err) => {
                            log::warn!("[HDR] Apple CPU compose failed: {err}");
                            resources
                                .failed_apple_image_compose
                                .insert((image_key, target_capacity_bits));
                            resources.image_bindings.remove(&image_key);
                            return Vec::new();
                        }
                    }
                }
            }
        }

        let apple_gpu_composed = apple_deferred.is_some()
            && binding.baked_apple_image_key == Some(image_key)
            && binding.baked_apple_weight_bits == Some(target_capacity_bits);
        let jpeg_gpu_composed = iso_deferred.is_some()
            && binding.baked_jpeg_image_key == Some(image_key)
            && binding.baked_jpeg_weight_bits == Some(target_capacity_bits);
        let raw_gpu_composed = raw_source.is_some()
            && binding.baked_raw_demosaic_key == Some(image_key)
            && binding.baked_raw_demosaic_method == raw_source.map(|s| s.demosaic_method);
        let deferred_gpu_composed = apple_gpu_composed || jpeg_gpu_composed || raw_gpu_composed;

        let uniform = image_tone_map_uniform(
            &self.image,
            self.tone_map,
            self.rotation_steps,
            self.alpha,
            self.output_mode,
            self.target_format,
            self.uv_rect,
            native_display_scale,
            apple_gpu_composed,
            self.ripple,
        );
        queue.write_buffer(&binding.tone_map_buffer, 0, bytemuck::bytes_of(&uniform));

        if binding.bind_group.is_none() {
            let gain_view = if deferred_gpu_composed {
                &resources.dummy_gain_view
            } else {
                binding
                    .uploaded_gain_view
                    .as_ref()
                    .unwrap_or(&resources.dummy_gain_view)
            };
            binding.bind_group = Some(create_hdr_image_plane_bind_group(
                device,
                &resources.bind_group_layout,
                &binding.uploaded_view,
                gain_view,
                &binding.tone_map_buffer,
            ));
        }

        while resources.image_bindings.len() > MAX_HDR_IMAGE_PLANE_BINDINGS {
            let now = std::time::Instant::now();
            let Some(oldest_key) = resources
                .image_bindings
                .iter()
                .filter(|(_, binding)| {
                    hdr_image_binding_is_eviction_candidate(
                        binding.keep_resident,
                        binding.last_use,
                        now,
                    )
                })
                .min_by_key(|(_, binding)| binding.last_use)
                .map(|(&key, _)| key)
            else {
                // Every binding was used this frame (typical during page-flip); allow a
                // temporary overflow rather than evicting prev/current mid-transition.
                break;
            };
            resources.image_bindings.remove(&oldest_key);
        }

        compose_command_buffers
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
        let image_key = HdrImageKey::from_image(&self.image);
        let Some(binding) = resources.image_bindings.get(&image_key) else {
            return;
        };
        let Some(bind_group) = binding.bind_group.as_ref() else {
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
