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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct JpegTiledUploadKey {
    pub(super) sdr_ptr: usize,
    pub(super) gain_ptr: usize,
}

#[allow(dead_code)]
pub(crate) struct HdrImageBinding {
    pub(super) uploaded_texture: Arc<wgpu::Texture>,
    pub(super) uploaded_view: wgpu::TextureView,
    pub(super) uploaded_gain_texture: Option<Arc<wgpu::Texture>>,
    pub(super) uploaded_gain_view: Option<wgpu::TextureView>,
    pub(super) uploaded_sdr_texture: Option<Arc<wgpu::Texture>>,
    pub(super) uploaded_sdr_view: Option<wgpu::TextureView>,
    pub(super) uploaded_display_storage_view: Option<wgpu::TextureView>,
    pub(super) uploaded_raw_pixels_texture: Option<Arc<wgpu::Texture>>,
    pub(super) uploaded_raw_pixels_view: Option<wgpu::TextureView>,
    pub(super) uploaded_raw_green_plane_write_view: Option<wgpu::TextureView>,
    pub(super) uploaded_raw_green_plane_read_view: Option<wgpu::TextureView>,

    pub(super) baked_jpeg_image_key: Option<HdrImageKey>,
    pub(super) baked_jpeg_weight_bits: Option<u32>,
    pub(super) baked_apple_image_key: Option<HdrImageKey>,
    pub(super) baked_apple_weight_bits: Option<u32>,
    pub(super) baked_raw_demosaic_key: Option<HdrImageKey>,
    pub(super) baked_raw_demosaic_method: Option<crate::settings::RawDemosaicMethod>,

    pub(super) tone_map_buffer: wgpu::Buffer,
    pub(super) jpeg_compose_uniform_buffer: Option<wgpu::Buffer>,
    /// Cached when SDR/gain/display views are stable; uniform updates do not require rebinding.
    pub(super) jpeg_compose_bind_group: Option<wgpu::BindGroup>,
    /// Texture view identities used to build `jpeg_compose_bind_group`.
    pub(super) jpeg_compose_bind_group_views: Option<(usize, usize, usize)>,
    #[cfg(feature = "heif-native")]
    pub(super) compose_tone_map_buffer: Option<wgpu::Buffer>,
    #[cfg(feature = "heif-native")]
    pub(super) encoded_primary_buffer: Option<wgpu::Buffer>,
    #[cfg(feature = "heif-native")]
    pub(super) encoded_primary_buffer_bytes: usize,
    #[cfg(feature = "heif-native")]
    pub(super) encoded_primary_source_ptr: Option<usize>,
    #[cfg(feature = "heif-native")]
    pub(super) apple_compose_bind_groups: HashMap<u64, wgpu::BindGroup>,

    pub(super) bind_group: Option<wgpu::BindGroup>,
    /// Last bytes written to `tone_map_buffer`; skip `write_buffer` when unchanged.
    pub(super) last_tone_map_uniform: Option<ToneMapUniform>,
    pub(super) last_use: std::time::Instant,
    pub(super) keep_resident: bool,
    /// [`ImageViewerApp::current_device_id`] epoch when GPU resources were created.
    pub(super) device_id: u64,
}

pub(crate) struct HdrCallbackResources {
    pub(super) target_format: wgpu::TextureFormat,
    pub(super) bind_group_layout: wgpu::BindGroupLayout,
    pub(super) pipeline: wgpu::RenderPipeline,
    #[allow(dead_code)]
    pub(super) dummy_gain_texture: wgpu::Texture,
    pub(super) dummy_gain_view: wgpu::TextureView,
    pub(super) tile_bindings: HdrTileBindings,
    pub(super) image_bindings: HashMap<HdrImageKey, HdrImageBinding>,
    pub(super) failed_jpeg_image_compose: HashSet<(HdrImageKey, u32)>,
    pub(super) failed_apple_image_compose: HashSet<(HdrImageKey, u32)>,
    pub(super) failed_raw_demosaic: HashSet<HdrImageKey>,
    pub(super) jpeg_compose_bind_group_layout: Option<wgpu::BindGroupLayout>,
    pub(super) jpeg_compose_pipeline: Option<wgpu::ComputePipeline>,
    pub(super) jpeg_compose_tile_pipeline: Option<wgpu::ComputePipeline>,
    pub(super) raw_demosaic_green_bind_group_layout: Option<wgpu::BindGroupLayout>,
    pub(super) raw_demosaic_rgb_bind_group_layout: Option<wgpu::BindGroupLayout>,
    pub(super) raw_demosaic_green_pipeline: Option<wgpu::ComputePipeline>,
    pub(super) raw_demosaic_rgb_pipeline: Option<wgpu::ComputePipeline>,
    pub(super) raw_demosaic_uniform_buffer: Option<wgpu::Buffer>,
    /// Single ISO gain-map compose uniform for tiled Ultra HDR via [`HdrTilePlaneCallback`].
    ///
    /// Static deferred JPEG via [`HdrImagePlaneCallback`] uses per-binding buffers
    /// (see `HdrImageBinding::jpeg_compose_uniform_buffer`) to avoid data races in concurrent drawing.
    pub(super) jpeg_compose_uniform_buffer: Option<wgpu::Buffer>,
    pub(super) jpeg_tiled_upload_key: Option<JpegTiledUploadKey>,
    pub(super) jpeg_tiled_sdr_texture: Option<Arc<wgpu::Texture>>,
    pub(super) jpeg_tiled_sdr_view: Option<wgpu::TextureView>,
    pub(super) jpeg_tiled_gain_texture: Option<Arc<wgpu::Texture>>,
    pub(super) jpeg_tiled_gain_view: Option<wgpu::TextureView>,
    #[cfg(feature = "heif-native")]
    pub(super) compose_bind_group_layout: Option<wgpu::BindGroupLayout>,
    #[cfg(feature = "heif-native")]
    pub(super) compose_pipeline: Option<wgpu::ComputePipeline>,
    pub(super) texture_pool: SharedGpuTexturePool,
}

const HDR_COMPOSE_WORKGROUP_SIZE: u32 = 16;
const HDR_COMPOSE_MIN_STORAGE_TEXTURES: u32 = 1;
#[cfg(feature = "heif-native")]
const HDR_COMPOSE_MIN_STORAGE_BUFFERS: u32 = 1;

pub(super) fn iso_gain_map_compose_compute_supported(limits: &wgpu::Limits) -> bool {
    limits.max_compute_invocations_per_workgroup
        >= HDR_COMPOSE_WORKGROUP_SIZE * HDR_COMPOSE_WORKGROUP_SIZE
        && limits.max_compute_workgroup_size_x >= HDR_COMPOSE_WORKGROUP_SIZE
        && limits.max_compute_workgroup_size_y >= HDR_COMPOSE_WORKGROUP_SIZE
        && limits.max_storage_textures_per_shader_stage >= HDR_COMPOSE_MIN_STORAGE_TEXTURES
}

#[cfg(feature = "heif-native")]
pub(super) fn apple_compose_compute_supported(limits: &wgpu::Limits) -> bool {
    iso_gain_map_compose_compute_supported(limits)
        && limits.max_storage_buffers_per_shader_stage >= HDR_COMPOSE_MIN_STORAGE_BUFFERS
}

pub(crate) struct CallbackUpload {
    pub(super) texture: Arc<wgpu::Texture>,
    pub(super) view: wgpu::TextureView,
    pub(super) storage_view: Option<wgpu::TextureView>,
}

pub(crate) struct ImagePlaneUpload {
    pub(super) base: CallbackUpload,
    pub(super) gain: Option<CallbackUpload>,
    pub(super) sdr_baseline: Option<CallbackUpload>,
    pub(super) raw_pixels: Option<CallbackUpload>,
    pub(super) raw_green_plane: Option<CallbackUpload>,
}

// `wgpu::Texture`, `wgpu::TextureView`, and `wgpu::Buffer` are `Send`; loader workers
// hand off completed uploads to the main thread via `LoadResult::uploaded_planes`, and
// `HdrImageBinding::from_uploaded` runs on the main thread before any paint callback uses them.

pub(crate) const MAX_HDR_IMAGE_PLANE_BINDINGS: usize = 16;
pub(crate) const HDR_IMAGE_BINDING_EVICTION_PROTECT: std::time::Duration =
    std::time::Duration::from_millis(50);
pub(crate) const HDR_IMAGE_BINDING_KEEP_RESIDENT_ABANDONED_AFTER: std::time::Duration =
    std::time::Duration::from_millis(100);

pub(crate) fn hdr_image_binding_is_eviction_candidate(
    keep_resident: bool,
    last_use: std::time::Instant,
    now: std::time::Instant,
) -> bool {
    let age = now.duration_since(last_use);
    age > HDR_IMAGE_BINDING_EVICTION_PROTECT
        && (!keep_resident || age > HDR_IMAGE_BINDING_KEEP_RESIDENT_ABANDONED_AFTER)
}

impl HdrImageBinding {
    /// Build a resident binding from a completed background [`ImagePlaneUpload`].
    ///
    /// `target_format` is the swap-chain / callback target format — the same role as
    /// `HdrImagePlaneCallback::target_format` on the synchronous `prepare()` cache-miss path.
    /// `tone_map` and `output_mode` seed the uniform buffer; `prepare()` refreshes it each frame.
    pub(crate) fn from_uploaded(
        device: &wgpu::Device,
        uploaded: ImagePlaneUpload,
        image: &HdrImageBuffer,
        tone_map: HdrToneMapSettings,
        target_format: wgpu::TextureFormat,
        output_mode: HdrRenderOutputMode,
        device_id: u64,
    ) -> Self {
        let native_display_scale =
            libavif_tone_map_native_display_scale(&image.metadata, image.color_space, &tone_map);
        let uniform = image_tone_map_uniform(
            image,
            ImageToneMapUniformParams {
                common: ToneMapCommonParams {
                    settings: tone_map,
                    rotation_steps: 0,
                    alpha: 1.0,
                    output_mode,
                    framebuffer_format: target_format,
                    uv_rect: egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
                    native_display_scale,
                },
                gpu_composed_scene_linear: false,
                ripple: None,
            },
        );
        let tone_map_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("simple-image-viewer-hdr-image-plane-tone-map-buffer"),
            contents: bytemuck::bytes_of(&uniform),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let iso_deferred = iso_deferred_from_metadata(&image.metadata);
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
        let apple_deferred = apple_heic_deferred_from_metadata(&image.metadata);
        #[cfg(feature = "heif-native")]
        let (compose_tone_map_buffer, encoded_primary_buffer, encoded_primary_buffer_bytes) =
            if apple_deferred.is_some() {
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
        let (uploaded_gain_texture, uploaded_gain_view) = if let Some(gain) = uploaded.gain {
            (Some(gain.texture), Some(gain.view))
        } else {
            (None, None)
        };
        let (uploaded_sdr_texture, uploaded_sdr_view) = if let Some(sdr) = uploaded.sdr_baseline {
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
        let (uploaded_raw_green_plane_write_view, uploaded_raw_green_plane_read_view) =
            if let Some(green) = uploaded.raw_green_plane {
                (green.storage_view, Some(green.view))
            } else {
                (None, None)
            };

        HdrImageBinding {
            uploaded_texture,
            uploaded_view,
            uploaded_gain_texture,
            uploaded_gain_view,
            uploaded_sdr_texture,
            uploaded_sdr_view,
            uploaded_display_storage_view,
            uploaded_raw_pixels_texture,
            uploaded_raw_pixels_view,
            uploaded_raw_green_plane_write_view,
            uploaded_raw_green_plane_read_view,
            baked_jpeg_image_key: None,
            baked_jpeg_weight_bits: None,
            baked_apple_image_key: None,
            baked_apple_weight_bits: None,
            baked_raw_demosaic_key: None,
            baked_raw_demosaic_method: None,
            tone_map_buffer,
            jpeg_compose_uniform_buffer,
            jpeg_compose_bind_group: None,
            jpeg_compose_bind_group_views: None,
            #[cfg(feature = "heif-native")]
            compose_tone_map_buffer,
            #[cfg(feature = "heif-native")]
            encoded_primary_buffer,
            #[cfg(feature = "heif-native")]
            encoded_primary_buffer_bytes,
            #[cfg(feature = "heif-native")]
            // Pre-upload path matches sync prepare(): encoded primary is filled on first paint.
            encoded_primary_source_ptr: None,
            #[cfg(feature = "heif-native")]
            apple_compose_bind_groups: HashMap::new(),
            bind_group: None,
            last_tone_map_uniform: None,
            last_use: std::time::Instant::now(),
            keep_resident: false,
            device_id,
        }
    }
}

impl HdrCallbackResources {
    /// Inserts a background pre-uploaded binding when its `device_id` matches the live epoch.
    ///
    /// Returns `false` when the binding is stale (e.g. `wgpu::Device` replaced after upload);
    /// the binding is dropped so orphan VRAM is not registered against the new device.
    pub(crate) fn register_preuploaded_binding(
        &mut self,
        image_key: HdrImageKey,
        binding: HdrImageBinding,
        expected_device_id: u64,
    ) -> bool {
        if binding.device_id != expected_device_id {
            log::debug!(
                "[HDR] Dropping stale pre-uploaded binding key={image_key:?}: \
                 binding device_id={} live device_id={expected_device_id}",
                binding.device_id
            );
            return false;
        }
        self.image_bindings.insert(image_key, binding);
        self.evict_old_bindings();
        true
    }

    pub(crate) fn raw_demosaic_baked_for(
        &self,
        image_key: HdrImageKey,
        method: crate::settings::RawDemosaicMethod,
    ) -> bool {
        self.image_bindings.get(&image_key).is_some_and(|binding| {
            binding.baked_raw_demosaic_key == Some(image_key)
                && binding.baked_raw_demosaic_method == Some(method)
        })
    }

    pub(crate) fn mark_raw_demosaic_failed(&mut self, image_key: HdrImageKey) {
        self.failed_raw_demosaic.insert(image_key);
    }

    /// Encode GPU RAW demosaic into an existing image binding and mark it baked.
    ///
    /// Returns `Ok(true)` when encoded, `Ok(false)` when the binding is not ready yet
    /// (caller should retry), or `Err` when GPU demosaic cannot proceed.
    pub(crate) fn encode_raw_demosaic_for_binding(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        image_key: HdrImageKey,
        method: crate::settings::RawDemosaicMethod,
        source: &crate::hdr::types::RawGpuSource,
    ) -> Result<bool, &'static str> {
        if self.raw_demosaic_baked_for(image_key, method) {
            return Ok(true);
        }
        if !self.image_bindings.contains_key(&image_key) {
            return Ok(false);
        }

        let pipelines_ready = self.raw_demosaic_green_bind_group_layout.is_some()
            && self.raw_demosaic_rgb_bind_group_layout.is_some()
            && self.raw_demosaic_green_pipeline.is_some()
            && self.raw_demosaic_rgb_pipeline.is_some()
            && self.raw_demosaic_uniform_buffer.is_some();
        if !pipelines_ready {
            return Err("GPU RAW demosaic pipelines unavailable");
        }

        let cloned_views = {
            let binding = self
                .image_bindings
                .get(&image_key)
                .expect("binding present after contains_key");
            match (
                binding.uploaded_raw_pixels_view.clone(),
                binding.uploaded_raw_green_plane_write_view.clone(),
                binding.uploaded_raw_green_plane_read_view.clone(),
                binding.uploaded_display_storage_view.clone(),
            ) {
                (Some(a), Some(b), Some(c), Some(d)) => Some((a, b, c, d)),
                _ => None,
            }
        };
        let Some((raw_pixels_view, green_plane_write_view, green_plane_read_view, output_view)) =
            cloned_views
        else {
            return Err("GPU RAW demosaic views missing");
        };

        let command = {
            let green_layout = self
                .raw_demosaic_green_bind_group_layout
                .as_ref()
                .expect("checked");
            let rgb_layout = self
                .raw_demosaic_rgb_bind_group_layout
                .as_ref()
                .expect("checked");
            let green_pipeline = self.raw_demosaic_green_pipeline.as_ref().expect("checked");
            let rgb_pipeline = self.raw_demosaic_rgb_pipeline.as_ref().expect("checked");
            let uniform_buf = self.raw_demosaic_uniform_buffer.as_ref().expect("checked");
            crate::hdr::raw_demosaic_gpu::encode_raw_demosaic_compute_pass(
                crate::hdr::raw_demosaic_gpu::RawDemosaicComputePass {
                    device,
                    queue,
                    green_bind_group_layout: green_layout,
                    rgb_bind_group_layout: rgb_layout,
                    green_pipeline,
                    rgb_pipeline,
                    source,
                    raw_pixels_view: &raw_pixels_view,
                    green_plane_write_view: &green_plane_write_view,
                    green_plane_read_view: &green_plane_read_view,
                    output_view: &output_view,
                    uniform_buffer: uniform_buf,
                },
            )
        };
        queue.submit(std::iter::once(command));

        if let Some(binding) = self.image_bindings.get_mut(&image_key) {
            binding.baked_raw_demosaic_key = Some(image_key);
            binding.baked_raw_demosaic_method = Some(method);
            // Force bind-group rebuild so paint uses dummy gain after GPU compose.
            binding.bind_group = None;
        }
        Ok(true)
    }

    #[cfg_attr(not(feature = "preload-debug"), allow(dead_code))]
    pub(crate) fn hdr_image_binding_present(&self, image_key: HdrImageKey) -> bool {
        self.image_bindings.contains_key(&image_key)
    }

    pub(crate) fn evict_old_bindings(&mut self) {
        while self.image_bindings.len() > MAX_HDR_IMAGE_PLANE_BINDINGS {
            let now = std::time::Instant::now();
            let Some(oldest_key) = self
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
                break;
            };
            if let Some(binding) = self.image_bindings.remove(&oldest_key) {
                self.release_binding_textures(binding);
            }
        }
    }

    fn release_binding_textures(&mut self, binding: HdrImageBinding) {
        let mut pool = self.texture_pool.lock();
        pool.release(binding.uploaded_texture);
        if let Some(texture) = binding.uploaded_gain_texture {
            pool.release(texture);
        }
        if let Some(texture) = binding.uploaded_sdr_texture {
            pool.release(texture);
        }
        if let Some(texture) = binding.uploaded_raw_pixels_texture {
            pool.release(texture);
        }
    }

    pub(crate) fn apply_iso_image_cpu_compose(
        &mut self,
        sink: super::pending_gpu_writes::GpuUploadSink<'_>,
        key: HdrImageKey,
        target_capacity_bits: u32,
        width: u32,
        height: u32,
        pixels: Arc<Vec<f32>>,
    ) -> Result<(), String> {
        let Some(binding) = self.image_bindings.get_mut(&key) else {
            return Err("HDR image binding missing for CPU compose".to_string());
        };
        super::write_rgba32f_to_texture(
            sink,
            Arc::clone(&binding.uploaded_texture),
            width,
            height,
            pixels,
        )?;
        binding.baked_jpeg_image_key = Some(key);
        binding.baked_jpeg_weight_bits = Some(target_capacity_bits);
        self.failed_jpeg_image_compose
            .remove(&(key, target_capacity_bits));
        Ok(())
    }

    #[cfg(feature = "heif-native")]
    pub(crate) fn apply_apple_image_cpu_compose(
        &mut self,
        sink: super::pending_gpu_writes::GpuUploadSink<'_>,
        key: HdrImageKey,
        target_capacity_bits: u32,
        width: u32,
        height: u32,
        pixels: Arc<Vec<f32>>,
    ) -> Result<(), String> {
        let Some(binding) = self.image_bindings.get_mut(&key) else {
            return Err("HDR image binding missing for CPU compose".to_string());
        };
        super::write_rgba32f_to_texture(
            sink,
            Arc::clone(&binding.uploaded_texture),
            width,
            height,
            pixels,
        )?;
        binding.baked_apple_image_key = Some(key);
        binding.baked_apple_weight_bits = Some(target_capacity_bits);
        self.failed_apple_image_compose
            .remove(&(key, target_capacity_bits));
        Ok(())
    }

    pub(crate) fn apply_iso_tile_cpu_compose(
        &mut self,
        sink: super::pending_gpu_writes::GpuUploadSink<'_>,
        tile_key: HdrTileKey,
        target_capacity_bits: u32,
        width: u32,
        height: u32,
        pixels: Arc<Vec<f32>>,
    ) -> Result<(), String> {
        let Some(binding) = self.tile_bindings.binding_mut(tile_key) else {
            return Err("HDR tile binding missing for CPU compose".to_string());
        };
        let Some(texture) = binding._texture.as_ref() else {
            self.tile_bindings.remove(tile_key);
            return Err("HDR tile texture missing for CPU compose".to_string());
        };
        super::write_rgba32f_to_texture(sink, Arc::clone(texture), width, height, pixels)?;
        binding.baked_jpeg_weight_bits = Some(target_capacity_bits);
        Ok(())
    }

    pub(crate) fn register_completed_tile_upload(
        &mut self,
        device: &wgpu::Device,
        item: super::pending_work::HdrCompletedTileUpload,
    ) -> bool {
        if self.tile_bindings.contains(item.tile_key) {
            return false;
        }
        let native_display_scale = libavif_tone_map_native_display_scale(
            &item.tile.metadata,
            item.tile.color_space,
            &item.tone_map,
        );
        let uniform = hdr_tile_tone_map_uniform(HdrTileToneMapUniformParams {
            common: ToneMapCommonParams {
                settings: item.tone_map,
                rotation_steps: item.rotation_steps,
                alpha: item.alpha,
                output_mode: item.output_mode,
                framebuffer_format: item.target_format,
                uv_rect: item.uv_rect,
                native_display_scale,
            },
            tile: &item.tile,
            jpeg_gpu_composed: false,
        });
        let tone_map_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("simple-image-viewer-hdr-tile-plane-tone-map-buffer"),
            contents: bytemuck::bytes_of(&uniform),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("simple-image-viewer-hdr-tile-plane-bind-group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&item.uploaded.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&self.dummy_gain_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: tone_map_buffer.as_entire_binding(),
                },
            ],
        });
        let mut pool = self.texture_pool.lock();
        self.tile_bindings.insert(
            item.tile_key,
            HdrTileInsert {
                texture: item.uploaded.texture,
                view: item.uploaded.view,
                compose_storage_view: item.uploaded.storage_view,
                tone_map_buffer,
                bind_group,
                jpeg_compose_bind_group: None,
                baked_jpeg_weight_bits: None,
            },
            Some(&mut *pool),
        );
        true
    }

    pub(crate) fn register_jpeg_tiled_source_upload(
        &mut self,
        upload_key: JpegTiledUploadKey,
        sdr: CallbackUpload,
        gain: CallbackUpload,
    ) {
        self.jpeg_tiled_upload_key = Some(upload_key);
        self.jpeg_tiled_sdr_texture = Some(sdr.texture);
        self.jpeg_tiled_sdr_view = Some(sdr.view);
        self.jpeg_tiled_gain_texture = Some(gain.texture);
        self.jpeg_tiled_gain_view = Some(gain.view);
    }

    pub(crate) fn set_image_binding_keep_resident(
        &mut self,
        key: HdrImageKey,
        keep_resident: bool,
    ) {
        if let Some(binding) = self.image_bindings.get_mut(&key) {
            binding.keep_resident = keep_resident;
        }
    }

    pub(crate) fn mark_iso_image_compose_failed(
        &mut self,
        key: HdrImageKey,
        target_capacity_bits: u32,
    ) {
        self.failed_jpeg_image_compose
            .insert((key, target_capacity_bits));
        self.image_bindings.remove(&key);
    }

    #[cfg(feature = "heif-native")]
    pub(crate) fn mark_apple_image_compose_failed(
        &mut self,
        key: HdrImageKey,
        target_capacity_bits: u32,
    ) {
        self.failed_apple_image_compose
            .insert((key, target_capacity_bits));
        self.image_bindings.remove(&key);
    }

    pub(crate) fn mark_iso_tile_compose_failed(&mut self, tile_key: HdrTileKey) {
        self.tile_bindings.remove(tile_key);
    }
}

pub(crate) const HDR_APPLE_GAIN_TEXTURE_FORMAT: wgpu::TextureFormat =
    wgpu::TextureFormat::Rgba8Unorm;

pub(super) fn create_dummy_gain_texture(
    device: &wgpu::Device,
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("simple-image-viewer-hdr-dummy-gain-texture"),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: HDR_APPLE_GAIN_TEXTURE_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

pub(crate) fn create_callback_resources(
    device: &wgpu::Device,
    target_format: wgpu::TextureFormat,
    pipeline_cache: Option<&wgpu::PipelineCache>,
) -> HdrCallbackResources {
    let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("simple-image-viewer-hdr-image-plane-bind-group-layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("simple-image-viewer-hdr-image-plane-pipeline-layout"),
        bind_group_layouts: &[Some(&bind_group_layout)],
        immediate_size: 0,
    });
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("simple-image-viewer-hdr-image-plane-shader"),
        source: wgpu::ShaderSource::Wgsl(HDR_IMAGE_PLANE_SHADER.into()),
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("simple-image-viewer-hdr-image-plane-pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            polygon_mode: wgpu::PolygonMode::Fill,
            unclipped_depth: false,
            conservative: false,
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: target_format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: pipeline_cache,
    });
    let (dummy_gain_texture, dummy_gain_view) = create_dummy_gain_texture(device);
    let adapter_info = device.adapter_info();
    let gl_backend = adapter_info.backend == wgpu::Backend::Gl;

    #[cfg(feature = "heif-native")]
    let (compose_bind_group_layout, compose_pipeline) = if gl_backend {
        log::warn!(
            "[HDR] GPU Apple HEIC gain-map compose disabled on OpenGL backend; using CPU fallback"
        );
        (None, None)
    } else if apple_compose_compute_supported(&device.limits()) {
        let (layout, pipeline, _compose_tone_map_buffer) =
            apple_compose_gpu::create_compose_compute_resources(device, pipeline_cache);
        (Some(layout), Some(pipeline))
    } else {
        log::warn!(
            "[HDR] GPU Apple HEIC gain-map compose unavailable \
                 (max_compute_invocations_per_workgroup={}, \
                 max_storage_buffers_per_shader_stage={}); using CPU fallback",
            device.limits().max_compute_invocations_per_workgroup,
            device.limits().max_storage_buffers_per_shader_stage
        );
        (None, None)
    };
    let jpeg_compose = if gl_backend {
        log::warn!("[HDR] GPU ISO gain-map compose disabled on OpenGL backend; using CPU fallback");
        None
    } else if iso_gain_map_compose_compute_supported(&device.limits()) {
        Some(jpeg_compose_gpu::create_jpeg_compose_compute_resources(
            device,
            pipeline_cache,
        ))
    } else {
        log::warn!(
            "[HDR] GPU ISO gain-map compose unavailable \
             (max_compute_invocations_per_workgroup={}); using CPU fallback",
            device.limits().max_compute_invocations_per_workgroup
        );
        None
    };
    let (
        jpeg_compose_bind_group_layout,
        jpeg_compose_pipeline,
        jpeg_compose_tile_pipeline,
        jpeg_compose_uniform_buffer,
    ) = match jpeg_compose {
        Some((
            jpeg_compose_bind_group_layout,
            jpeg_compose_pipeline,
            jpeg_compose_tile_pipeline,
            jpeg_compose_uniform_buffer,
        )) => (
            Some(jpeg_compose_bind_group_layout),
            Some(jpeg_compose_pipeline),
            Some(jpeg_compose_tile_pipeline),
            Some(jpeg_compose_uniform_buffer),
        ),
        None => (None, None, None, None),
    };

    let raw_demosaic_compute_supported = device.limits().max_compute_invocations_per_workgroup
        >= 256
        && device.limits().max_compute_workgroup_size_x
            >= crate::hdr::raw_demosaic_gpu::RAW_DEMOSAIC_WORKGROUP_SIZE
        && device.limits().max_compute_workgroup_size_y
            >= crate::hdr::raw_demosaic_gpu::RAW_DEMOSAIC_WORKGROUP_SIZE;

    let (
        raw_demosaic_green_bind_group_layout,
        raw_demosaic_rgb_bind_group_layout,
        raw_demosaic_green_pipeline,
        raw_demosaic_rgb_pipeline,
        raw_demosaic_uniform_buffer,
    ) = if gl_backend {
        log::warn!("[HDR] GPU RAW demosaicing disabled on OpenGL backend; using CPU fallback");
        (None, None, None, None, None)
    } else if raw_demosaic_compute_supported {
        let (green_layout, rgb_layout, green_pipeline, rgb_pipeline, buf) =
            crate::hdr::raw_demosaic_gpu::create_raw_demosaic_compute_resources(
                device,
                pipeline_cache,
            );
        (
            Some(green_layout),
            Some(rgb_layout),
            Some(green_pipeline),
            Some(rgb_pipeline),
            Some(buf),
        )
    } else {
        log::warn!(
            "[HDR] GPU RAW demosaicing unavailable \
             (max_compute_invocations_per_workgroup={}); using CPU fallback",
            device.limits().max_compute_invocations_per_workgroup
        );
        (None, None, None, None, None)
    };

    HdrCallbackResources {
        target_format,
        bind_group_layout,
        pipeline,
        dummy_gain_texture,
        dummy_gain_view,
        tile_bindings: HdrTileBindings::default(),
        image_bindings: HashMap::new(),
        failed_jpeg_image_compose: HashSet::new(),
        failed_apple_image_compose: HashSet::new(),
        failed_raw_demosaic: HashSet::new(),
        jpeg_compose_bind_group_layout,
        jpeg_compose_pipeline,
        jpeg_compose_tile_pipeline,
        raw_demosaic_green_bind_group_layout,
        raw_demosaic_rgb_bind_group_layout,
        raw_demosaic_green_pipeline,
        raw_demosaic_rgb_pipeline,
        raw_demosaic_uniform_buffer,
        jpeg_compose_uniform_buffer,
        jpeg_tiled_upload_key: None,
        jpeg_tiled_sdr_texture: None,
        jpeg_tiled_sdr_view: None,
        jpeg_tiled_gain_texture: None,
        jpeg_tiled_gain_view: None,
        #[cfg(feature = "heif-native")]
        compose_bind_group_layout,
        #[cfg(feature = "heif-native")]
        compose_pipeline,
        texture_pool: Mutex::new(GpuTexturePool::default()),
    }
}
