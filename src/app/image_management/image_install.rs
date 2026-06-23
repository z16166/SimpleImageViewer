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

pub(super) const RAW_GPU_BOOTSTRAP_TEXTURE_PREFIX: &str = "img_raw_gpu_bootstrap_";
pub(super) const HDR_SDR_FALLBACK_TEXTURE_PREFIX: &str = "img_hdr_fallback_";

pub(super) fn raw_gpu_bootstrap_texture_name(idx: usize) -> String {
    format!("{RAW_GPU_BOOTSTRAP_TEXTURE_PREFIX}{idx}")
}

pub(super) fn hdr_sdr_fallback_texture_name(idx: usize) -> String {
    format!("{HDR_SDR_FALLBACK_TEXTURE_PREFIX}{idx}")
}

impl ImageViewerApp {
    pub(super) fn active_hdr_plane_displays_index(&self, index: usize) -> bool {
        index == self.current_index
            && self.effective_hdr_display_output().is_some()
            && self
                .current_hdr_image
                .as_ref()
                .is_some_and(|current| current.image_for_index(index).is_some())
    }

    pub(super) fn insert_deferred_sdr_upload(
        &mut self,
        idx: usize,
        decoded: crate::loader::DecodedImage,
    ) {
        use std::collections::hash_map::Entry;

        if let Entry::Occupied(mut slot) = self.deferred_sdr_uploads.entry(idx) {
            *slot.get_mut() = decoded;
            return;
        }
        if self.deferred_sdr_uploads.len() >= crate::app::MAX_DEFERRED_SDR_UPLOADS {
            let current = self.current_index;
            let total = self.image_files.len();
            if let Some(evict_idx) = self
                .deferred_sdr_uploads
                .keys()
                .copied()
                .max_by_key(|&i| super::prefetch_circular_distance(current, total, i))
            {
                self.deferred_sdr_uploads.remove(&evict_idx);
            }
        }
        self.deferred_sdr_uploads.insert(idx, decoded);
    }

    pub(super) fn upload_static_sdr_texture(
        &mut self,
        idx: usize,
        decoded: &DecodedImage,
        texture_name: String,
        ctx: &egui::Context,
    ) {
        let color_image = ColorImage::from_rgba_unmultiplied(
            [decoded.width as usize, decoded.height as usize],
            decoded.rgba(),
        );
        let handle = ctx.load_texture(texture_name, color_image, TextureOptions::LINEAR);
        if let Some(evicted_idx) = self.texture_cache.insert(
            idx,
            handle,
            decoded.width,
            decoded.height,
            false,
            self.current_index,
            self.image_files.len(),
        ) {
            self.handle_texture_cache_eviction(evicted_idx);
        }
        // Preload may have queued pixels for this index; GPU upload makes them redundant.
        self.deferred_sdr_uploads.remove(&idx);
    }

    pub(super) fn upload_raw_gpu_bootstrap_texture(
        &mut self,
        idx: usize,
        decoded: &DecodedImage,
        ctx: &egui::Context,
    ) {
        self.upload_static_sdr_texture(idx, decoded, raw_gpu_bootstrap_texture_name(idx), ctx);
        self.raw_gpu_embedded_bootstrap_indices.insert(idx);
    }

    pub(super) fn upload_hdr_sdr_fallback_texture(
        &mut self,
        idx: usize,
        decoded: &DecodedImage,
        ctx: &egui::Context,
    ) {
        self.upload_static_sdr_texture(idx, decoded, hdr_sdr_fallback_texture_name(idx), ctx);
        self.raw_gpu_embedded_bootstrap_indices.remove(&idx);
    }

    pub(super) fn queue_or_upload_raw_gpu_bootstrap_texture(
        &mut self,
        idx: usize,
        decoded: &DecodedImage,
        ctx: &egui::Context,
    ) {
        if idx == self.current_index {
            self.upload_raw_gpu_bootstrap_texture(idx, decoded, ctx);
        } else {
            self.insert_deferred_sdr_upload(idx, decoded.clone());
        }
    }

    pub(super) fn queue_or_upload_hdr_sdr_fallback_texture(
        &mut self,
        idx: usize,
        decoded: &DecodedImage,
        ctx: &egui::Context,
    ) {
        if idx == self.current_index {
            self.upload_hdr_sdr_fallback_texture(idx, decoded, ctx);
        } else {
            self.insert_deferred_sdr_upload(idx, decoded.clone());
        }
    }

    pub(super) fn queue_or_upload_static_sdr_texture(
        &mut self,
        idx: usize,
        decoded: &DecodedImage,
        texture_name: String,
        ctx: &egui::Context,
    ) {
        if idx == self.current_index {
            self.upload_static_sdr_texture(idx, decoded, texture_name, ctx);
        } else {
            self.insert_deferred_sdr_upload(idx, decoded.clone());
        }
    }

    pub(crate) fn flush_deferred_sdr_upload_for_index(
        &mut self,
        index: usize,
        ctx: &egui::Context,
    ) {
        if !self.deferred_sdr_uploads.contains_key(&index) {
            return;
        }
        let hdr_fallback_upload = self.hdr_sdr_fallback_indices.contains(&index);
        if self.active_hdr_plane_displays_index(index) {
            return;
        }
        if self.texture_cache.contains(index) && !hdr_fallback_upload {
            self.deferred_sdr_uploads.remove(&index);
            return;
        }
        let Some(decoded) = self.deferred_sdr_uploads.remove(&index) else {
            return;
        };
        let is_hdr_fallback = self.hdr_sdr_fallback_indices.contains(&index);
        if is_hdr_fallback {
            self.upload_hdr_sdr_fallback_texture(index, &decoded, ctx);
        } else {
            self.upload_static_sdr_texture(index, &decoded, format!("img_{index}"), ctx);
        }
        if index == self.current_index {
            self.set_current_image_resolution(Some((decoded.width, decoded.height)));
        }
    }

    pub(super) fn flush_deferred_sdr_upload_for_current(&mut self, ctx: &egui::Context) {
        let index = self.current_index;
        self.flush_deferred_sdr_upload_for_index(index, ctx);
    }

    pub(super) fn clear_current_animation_for_index(&mut self, idx: usize) {
        if self
            .animation
            .as_ref()
            .is_some_and(|animation| animation.image_index == idx)
        {
            self.animation = None;
        }
    }

    pub(super) fn install_static_sdr_image(
        &mut self,
        idx: usize,
        decoded: &DecodedImage,
        ctx: &egui::Context,
    ) {
        self.remove_hdr_image_resources(idx);
        self.queue_or_upload_static_sdr_texture(idx, decoded, format!("img_{idx}"), ctx);
        if idx == self.current_index {
            self.set_current_image_resolution(Some((decoded.width, decoded.height)));
            self.tile_manager = None;
            self.clear_current_animation_for_index(idx);
            self.pixel_data_source = Some(crate::pixel_inspector::PixelDataSource::Static {
                width: decoded.width,
                height: decoded.height,
                pixels: decoded.arc_pixels(),
            });
        }
        if self
            .image_files
            .get(idx)
            .is_some_and(|p| crate::preload_debug::path_is_raw(p))
        {
            crate::preload_debug!(
                "[PreloadDebug][RAW] install_static_sdr idx={} current={} size={}x{}",
                idx,
                idx == self.current_index,
                decoded.width,
                decoded.height
            );
        }
        self.cache_directory_tree_strip_thumbnail(
            idx,
            decoded,
            crate::loader::PreviewStage::Refined,
            Some((decoded.width, decoded.height)),
            ctx,
        );
    }

    pub(super) fn install_static_hdr_image(
        &mut self,
        idx: usize,
        hdr: Arc<crate::hdr::types::HdrImageBuffer>,
        fallback: &DecodedImage,
        sdr_fallback_is_placeholder: bool,
        ultra_hdr_capacity_sensitive: bool,
        defer_sdr_upload: bool,
        ctx: &egui::Context,
    ) {
        let gpu_demosaic_pending = crate::loader::hdr_raw_gpu_demosaic_pending(&hdr);
        self.remove_hdr_image_resources(idx);
        self.hdr_image_cache.insert(idx, Arc::clone(&hdr));
        self.hdr_sdr_fallback_indices.insert(idx);
        if sdr_fallback_is_placeholder {
            self.hdr_placeholder_fallback_indices.insert(idx);
        } else {
            self.hdr_placeholder_fallback_indices.remove(&idx);
        }
        if gpu_demosaic_pending {
            self.hdr_raw_gpu_demosaic_pending_indices.insert(idx);
            let key = crate::hdr::renderer::HdrImageKey::from_image(hdr.as_ref());
            self.hdr_raw_gpu_demosaic_pending_key_index.insert(key, idx);
            let bootstrap = if sdr_fallback_is_placeholder {
                self.raw_metadata.embedded_preview_dims(idx)
            } else {
                Some((fallback.width, fallback.height))
            };
            crate::preload_debug!(
                "[PreloadDebug][RAW-GPU] pending set idx={idx} key={key:?} bootstrap={bootstrap:?} cur={}",
                idx == self.current_index
            );
            self.raw_metadata.note_gpu_demosaic_pending(idx, bootstrap);
        } else {
            self.hdr_raw_gpu_demosaic_pending_indices.remove(&idx);
            self.hdr_raw_gpu_demosaic_pending_key_index
                .retain(|_, pending_idx| *pending_idx != idx);
            if crate::loader::raw_gpu_source_has_bootstrap_preview(hdr.as_ref()) {
                self.on_raw_hdr_plane_ready(idx);
            }
        }
        if gpu_demosaic_pending && self.texture_cache.contains(idx) {
            self.texture_cache
                .set_original_res(idx, hdr.width, hdr.height);
            self.texture_cache.set_preview_placeholder(idx, false);
        }
        if ultra_hdr_capacity_sensitive {
            self.ultra_hdr_capacity_sensitive_indices.insert(idx);
        }

        let bootstrap_already_uploaded = gpu_demosaic_pending
            && self.texture_cache.contains(idx)
            && !sdr_fallback_is_placeholder;
        let skip_current_sdr_upload = idx == self.current_index
            && (sdr_fallback_is_placeholder || bootstrap_already_uploaded);
        if !skip_current_sdr_upload {
            if defer_sdr_upload && idx != self.current_index {
                let mut deferred = fallback.clone();
                if sdr_fallback_is_placeholder {
                    deferred.mark_sdr_deferred_placeholder();
                }
                self.insert_deferred_sdr_upload(idx, deferred);
            } else {
                self.queue_or_upload_hdr_sdr_fallback_texture(idx, fallback, ctx);
            }
        }

        if idx == self.current_index {
            self.set_current_image_resolution(Some((hdr.width, hdr.height)));
            self.current_hdr_image = Some(crate::app::CurrentHdrImage::new(idx, Arc::clone(&hdr)));
            self.refresh_hdr_view_status();
            if gpu_demosaic_pending {
                ctx.request_repaint();
            }
            self.tile_manager = None;
            self.clear_current_animation_for_index(idx);
            self.pixel_data_source = Some(crate::pixel_inspector::PixelDataSource::Static {
                width: hdr.width,
                height: hdr.height,
                pixels: fallback.arc_pixels(),
            });
            if sdr_fallback_is_placeholder
                && !crate::loader::hdr_raw_gpu_refinement_is_pointless(&hdr)
            {
                if !self.hdr_in_flight_fallback_refinements.contains(&idx) {
                    let source_key = source_key_for_path(&self.image_files[idx]);
                    self.hdr_in_flight_fallback_refinements.insert(idx);
                    self.loader.trigger_hdr_sdr_fallback_refinement(
                        idx,
                        self.generation,
                        Arc::clone(&hdr),
                        source_key,
                    );
                }
            }
        }
        if !sdr_fallback_is_placeholder {
            self.cache_directory_tree_strip_thumbnail(
                idx,
                fallback,
                crate::loader::PreviewStage::Refined,
                Some((hdr.width, hdr.height)),
                ctx,
            );
        }
    }

    pub(super) fn handle_hdr_sdr_fallback_update(
        &mut self,
        update: crate::loader::HdrSdrFallbackResult,
        ctx: &egui::Context,
    ) {
        let idx = update.index;
        if !self.hdr_image_cache.contains_key(&idx) {
            return;
        }
        let Some(fallback_image) = update.fallback else {
            return;
        };
        if idx == self.current_index {
            self.pixel_data_source = Some(crate::pixel_inspector::PixelDataSource::Static {
                width: fallback_image.width,
                height: fallback_image.height,
                pixels: fallback_image.arc_pixels(),
            });
        }
        let active_hdr_plane_displays_current = self.active_hdr_plane_displays_index(idx);
        self.hdr_sdr_fallback_indices.insert(idx);
        self.hdr_placeholder_fallback_indices.remove(&idx);
        let logical_size = self.texture_cache.get_original_res(idx).or_else(|| {
            self.hdr_image_cache
                .get(&idx)
                .map(|hdr| (hdr.width, hdr.height))
        });
        if active_hdr_plane_displays_current {
            // The float HDR plane is the displayed source; applying the refined SDR fallback here
            // changes render-plan bookkeeping and can retrigger GPU compose right after page-flip.
            self.insert_deferred_sdr_upload(idx, fallback_image.clone());
            self.cache_directory_tree_strip_thumbnail(
                idx,
                &fallback_image,
                crate::loader::PreviewStage::Refined,
                logical_size,
                ctx,
            );
            return;
        }
        self.queue_or_upload_hdr_sdr_fallback_texture(idx, &fallback_image, ctx);
        self.cache_directory_tree_strip_thumbnail(
            idx,
            &fallback_image,
            crate::loader::PreviewStage::Refined,
            logical_size,
            ctx,
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn install_tiled_image(
        &mut self,
        idx: usize,
        generation: u64,
        source: Arc<dyn crate::loader::TiledImageSource>,
        hdr_source: Option<Arc<dyn crate::hdr::tiled::HdrTiledSource>>,
        sdr_preview: Option<&DecodedImage>,
        hdr_preview: Option<Arc<crate::hdr::types::HdrImageBuffer>>,
        hdr_sdr_fallback: bool,
        ultra_hdr_capacity_sensitive: bool,
        ctx: &egui::Context,
    ) {
        self.remove_hdr_image_resources(idx);
        if let Some(hdr_source) = hdr_source.as_ref() {
            self.hdr_tiled_source_cache
                .insert(idx, Arc::clone(hdr_source));
            self.cache_hdr_tiled_preview(idx, hdr_preview);
        }
        if hdr_sdr_fallback {
            self.hdr_sdr_fallback_indices.insert(idx);
        }
        if ultra_hdr_capacity_sensitive {
            self.ultra_hdr_capacity_sensitive_indices.insert(idx);
        }

        self.upload_tiled_bootstrap_preview(ctx, idx, sdr_preview, source.width(), source.height());

        let mut tm = build_tiled_manager_with_best_preview(
            idx,
            generation,
            Arc::clone(&source),
            self.texture_cache.get(idx).cloned(),
        );
        self.attach_initial_preview_if_needed(ctx, idx, &mut tm, sdr_preview);

        let _hdr_plane_active = hdr_source.is_some();
        if idx == self.current_index {
            if let Some(hdr_source) = hdr_source {
                self.current_hdr_tiled_image =
                    Some(crate::app::CurrentHdrTiledImage::new(idx, hdr_source));
                self.refresh_hdr_view_status();
            }
            self.set_current_image_resolution(Some((source.width(), source.height())));
            crate::tile_cache::set_tile_size_for_image(source.width(), source.height());
            self.tile_manager = Some(tm);
            self.animation = None;
            self.log_large_image(idx, source.width(), source.height());
            source.request_refinement(idx, self.generation);
            self.pixel_data_source = Some(crate::pixel_inspector::PixelDataSource::Tiled(
                Arc::clone(&source),
            ));
        } else {
            self.prefetched_tiles.insert(idx, tm);
        }

        if crate::preload_debug::path_is_raw(&self.image_files[idx]) {
            crate::preload_debug!(
                "[PreloadDebug][RAW] install_tiled idx={} gen={} current={} logical={}x{} hdr={} sdr_preview={}",
                idx,
                generation,
                idx == self.current_index,
                source.width(),
                source.height(),
                _hdr_plane_active,
                sdr_preview
                    .map(|p| format!("{}x{}", p.width, p.height))
                    .unwrap_or_else(|| "none".into())
            );
        }
    }

    pub(super) fn install_animated_image(
        &mut self,
        idx: usize,
        frames: &[crate::loader::AnimationFrame],
        ctx: &egui::Context,
    ) {
        self.remove_hdr_image_resources(idx);
        if let Some(first) = frames.first() {
            let decoded = DecodedImage::from_arc(first.width, first.height, first.arc_pixels());
            self.queue_or_upload_static_sdr_texture(idx, &decoded, format!("img_{idx}"), ctx);
            if idx == self.current_index {
                self.set_current_image_resolution(Some((first.width, first.height)));
                self.tile_manager = None;
                self.pixel_data_source = Some(crate::pixel_inspector::PixelDataSource::Static {
                    width: first.width,
                    height: first.height,
                    pixels: first.arc_pixels(),
                });
            }
        }

        self.pending_anim_frames.insert(
            idx,
            PendingAnimUpload {
                image_index: idx,
                hdr_frames: None,
                frames: frames.to_vec(),
                textures: Vec::new(),
                delays: Vec::new(),
                next_frame: 0,
            },
        );
        crate::preload_debug!(
            "[PreloadDebug] queue animation upload: idx={} current={} frames={}",
            idx,
            self.current_index,
            frames.len()
        );
        ctx.request_repaint();
    }

    pub(super) fn install_hdr_animated_image(
        &mut self,
        idx: usize,
        frames: &[crate::loader::HdrAnimationFrame],
        ultra_hdr_capacity_sensitive: bool,
        ctx: &egui::Context,
    ) {
        self.remove_hdr_image_resources(idx);
        let hdr_frames: Vec<Arc<crate::hdr::types::HdrImageBuffer>> = frames
            .iter()
            .map(|frame| Arc::new(frame.hdr.clone()))
            .collect();
        if let Some(first_hdr) = hdr_frames.first() {
            // Preload / first navigation reads `hdr_image_cache` before deferred anim uploads
            // finish populating `animation_cache`. Without this, HDR displays fall back to the
            // black SDR placeholder until `pending_anim_frames` completes (dark → bright flash).
            self.hdr_image_cache.insert(idx, Arc::clone(first_hdr));
        }
        self.hdr_sdr_fallback_indices.insert(idx);
        if ultra_hdr_capacity_sensitive {
            self.ultra_hdr_capacity_sensitive_indices.insert(idx);
        }

        if let Some(first) = frames.first() {
            let fallback_is_placeholder = first.fallback.is_sdr_deferred_placeholder();
            if fallback_is_placeholder {
                self.hdr_placeholder_fallback_indices.insert(idx);
                self.invalidate_directory_tree_strip_preview_for_index(idx);
            } else {
                self.hdr_placeholder_fallback_indices.remove(&idx);
            }
            self.queue_or_upload_hdr_sdr_fallback_texture(idx, &first.fallback, ctx);
            if idx == self.current_index {
                self.set_current_image_resolution(Some((first.width(), first.height())));
                self.current_hdr_image = Some(crate::app::CurrentHdrImage::new(
                    idx,
                    Arc::clone(&hdr_frames[0]),
                ));
                self.refresh_hdr_view_status();
                self.tile_manager = None;
                self.clear_current_animation_for_index(idx);
                self.pixel_data_source = Some(crate::pixel_inspector::PixelDataSource::Static {
                    width: first.width(),
                    height: first.height(),
                    pixels: first.fallback.arc_pixels(),
                });
            }
        } else {
            self.hdr_placeholder_fallback_indices.remove(&idx);
        }

        let sdr_frames: Vec<crate::loader::AnimationFrame> = frames
            .iter()
            .map(|frame| {
                crate::loader::AnimationFrame::from_arc(
                    frame.width(),
                    frame.height(),
                    frame.fallback.arc_pixels(),
                    frame.delay,
                )
            })
            .collect();

        self.pending_anim_frames.insert(
            idx,
            PendingAnimUpload {
                image_index: idx,
                hdr_frames: Some(hdr_frames),
                frames: sdr_frames,
                textures: Vec::new(),
                delays: Vec::new(),
                next_frame: 0,
            },
        );
        crate::preload_debug!(
            "[PreloadDebug] queue hdr animation upload: idx={} current={} frames={}",
            idx,
            self.current_index,
            frames.len()
        );
        ctx.request_repaint();
    }

    pub(super) fn install_image_error(&mut self, idx: usize, error: &str) {
        let path_str = self
            .image_files
            .get(idx)
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| format!("[index {idx} absent after rescan]"));
        log::error!(
            "Failed to load image at index {} ({}): {error}",
            idx,
            path_str
        );
        if idx == self.current_index {
            self.error_message =
                Some(t!("status.load_failed", path = path_str, err = error).to_string());
        }
    }
}
