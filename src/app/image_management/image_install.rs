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

pub(super) struct StaticHdrInstall<'a> {
    pub(super) hdr: Arc<crate::hdr::types::HdrImageBuffer>,
    pub(super) fallback: &'a DecodedImage,
    pub(super) sdr_fallback_is_placeholder: bool,
    pub(super) ultra_hdr_capacity_sensitive: bool,
    pub(super) defer_sdr_upload: bool,
    pub(super) ctx: &'a egui::Context,
}

pub(super) struct TiledImageInstall<'a> {
    pub(super) idx: usize,
    pub(super) decode_profile: crate::loader::DecodeProfile,
    pub(super) source: Arc<dyn crate::loader::TiledImageSource>,
    pub(super) hdr_source: Option<Arc<dyn crate::hdr::tiled::HdrTiledSource>>,
    pub(super) sdr_preview: Option<&'a DecodedImage>,
    pub(super) hdr_preview: Option<Arc<crate::hdr::types::HdrImageBuffer>>,
    pub(super) hdr_sdr_fallback: bool,
    pub(super) ultra_hdr_capacity_sensitive: bool,
    pub(super) ctx: &'a egui::Context,
}

struct HdrAnimatedRemainderMergeState {
    preserved_textures: std::sync::Arc<Vec<egui::TextureHandle>>,
    preserved_delays: std::sync::Arc<Vec<std::time::Duration>>,
    next_frame: usize,
}

impl ImageViewerApp {
    /// True when the canvas is actually drawing through the HDR float plane for `index`,
    /// not merely when an HDR buffer exists in cache (e.g. ISO gain-map embedded SDR master on SDR output).
    pub(super) fn active_hdr_plane_displays_index(&self, index: usize) -> bool {
        self.render_plan_for_index(index)
            .is_some_and(|plan| plan.backend == crate::app::rendering::plane::PlaneBackendKind::Hdr)
    }

    fn render_plan_for_index(
        &self,
        index: usize,
    ) -> Option<crate::app::rendering::plan::RenderPlan> {
        if index != self.current_index {
            return None;
        }
        self.effective_hdr_display_output()?;
        let has_hdr_plane = self
            .current_hdr_image
            .as_ref()
            .is_some_and(|current| current.image_for_index(index).is_some())
            || self.hdr_image_cache.contains_key(&index)
            || self.hdr_tiled_source_cache.contains_key(&index);
        if !has_hdr_plane {
            return None;
        }
        let has_sdr_fallback = self.hdr_sdr_fallback_indices.contains(&index);
        let shape = if self.should_draw_tiled_canvas()
            || self.hdr_tiled_source_cache.contains_key(&index)
        {
            crate::app::rendering::plan::RenderShape::Tiled
        } else {
            crate::app::rendering::plan::RenderShape::Static
        };
        Some(self.build_render_plan(shape, has_hdr_plane, has_sdr_fallback))
    }

    pub(super) fn hdr_prefers_embedded_sdr_master_on_output(
        &self,
        hdr: &crate::hdr::types::HdrImageBuffer,
    ) -> bool {
        let output_mode = crate::hdr::monitor::effective_render_output_mode(
            self.effective_hdr_target_format(),
            self.effective_hdr_monitor_selection().as_ref(),
        );
        crate::loader::prefer_embedded_iso_gain_map_sdr_on_sdr_output(
            &self.settings,
            output_mode,
            Some(hdr),
        )
    }

    fn installed_hdr_directory_tree_strip_preview(
        &self,
        hdr: &crate::hdr::types::HdrImageBuffer,
        fallback: &DecodedImage,
        _sdr_fallback_is_placeholder: bool,
    ) -> Option<DecodedImage> {
        let max_side = self
            .settings
            .directory_tree_list_preview_size
            .strip_max_side();
        crate::loader::directory_tree_strip_from_hdr_or_fallback(hdr, fallback, max_side).ok()
    }

    fn should_eager_cache_install_hdr_strip(&self, idx: usize) -> bool {
        if !self.directory_tree_list_previews_active() {
            return false;
        }
        if idx == self.current_index {
            return true;
        }
        let total = self.image_files.len();
        if total == 0 {
            return false;
        }
        if self.directory_tree_strip_bootstrap_after_scan
            && idx < total.min(crate::app::directory_tree::BOOTSTRAP_STRIP_VISIBLE_ROW_CAP)
        {
            return true;
        }
        super::prefetch_circular_distance(self.current_index, total, idx)
            <= crate::app::directory_tree::DIRECTORY_TREE_COLD_NEIGHBOR_RADIUS
    }

    pub(crate) fn insert_deferred_sdr_upload(
        &mut self,
        idx: usize,
        decoded: crate::loader::DecodedImage,
    ) {
        self.store_deferred_sdr_upload_tracked(idx, decoded);
    }

    pub(super) fn upload_static_sdr_texture(
        &mut self,
        idx: usize,
        decoded: &DecodedImage,
        texture_name: String,
        buffer_tag: crate::loader::TexturePreviewBufferTag,
        stage: crate::loader::PreviewStage,
        ctx: &egui::Context,
    ) {
        let color_image = ColorImage::from_rgba_unmultiplied(
            [decoded.width as usize, decoded.height as usize],
            decoded.rgba(),
        );
        let handle = ctx.load_texture(texture_name, color_image, TextureOptions::LINEAR);
        self.insert_texture_cache_tracked(
            idx,
            handle,
            crate::loader::TextureCacheInsert {
                orig_w: decoded.width,
                orig_h: decoded.height,
                needs_tile_manager: false,
                buffer_tag,
                stage,
                current_index: self.current_index,
                total_count: self.image_files.len(),
            },
        );
        // Preload may have queued pixels for this index; GPU upload makes them redundant.
        self.deferred_sdr_uploads.remove(&idx);
    }

    pub(super) fn upload_raw_gpu_bootstrap_texture(
        &mut self,
        idx: usize,
        decoded: &DecodedImage,
        ctx: &egui::Context,
    ) {
        self.upload_static_sdr_texture(
            idx,
            decoded,
            raw_gpu_bootstrap_texture_name(idx),
            crate::loader::TexturePreviewBufferTag::RawGpuBootstrap,
            crate::loader::PreviewStage::Initial,
            ctx,
        );
        self.raw_gpu_embedded_bootstrap_indices.insert(idx);
    }

    pub(super) fn upload_hdr_sdr_fallback_texture(
        &mut self,
        idx: usize,
        decoded: &DecodedImage,
        ctx: &egui::Context,
    ) {
        self.upload_static_sdr_texture(
            idx,
            decoded,
            hdr_sdr_fallback_texture_name(idx),
            crate::loader::TexturePreviewBufferTag::HdrSdrFallback,
            crate::loader::PreviewStage::Refined,
            ctx,
        );
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
            self.upload_static_sdr_texture(
                idx,
                decoded,
                texture_name,
                crate::loader::TexturePreviewBufferTag::MainWindowSdr,
                crate::loader::PreviewStage::Refined,
                ctx,
            );
        } else {
            self.insert_deferred_sdr_upload(idx, decoded.clone());
        }
    }

    pub(super) fn index_within_prefetch_window(&self, index: usize) -> bool {
        let count = self.image_files.len();
        if count == 0 || index >= count {
            return false;
        }
        super::prefetch_window_contains(
            self.current_index,
            count,
            index,
            self.prefetch_window_max_distance,
        )
    }

    /// Upload deferred neighbor SDR pixels to `texture_cache` once preload completes.
    pub(super) fn flush_deferred_sdr_for_completed_prefetch_neighbor(
        &mut self,
        index: usize,
        ctx: &egui::Context,
    ) -> bool {
        if index == self.current_index || !self.index_within_prefetch_window(index) {
            return false;
        }
        if !self.deferred_sdr_uploads.contains_key(&index) {
            return false;
        }
        self.flush_deferred_sdr_upload_for_index(index, ctx);
        self.texture_cache.contains(index)
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
            self.upload_static_sdr_texture(
                index,
                &decoded,
                format!("img_{index}"),
                crate::loader::TexturePreviewBufferTag::MainWindowSdr,
                crate::loader::PreviewStage::Refined,
                ctx,
            );
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
        self.record_installed_display_mode(idx, crate::loader::RenderShape::Static);
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
            crate::app::directory_tree_strip_cache::StripThumbnailCacheRequest {
                index: idx,
                job_key: None,
                decoded,
                stage: crate::loader::PreviewStage::Refined,
                logical_size: Some((decoded.width, decoded.height)),
                buffer_tag:
                    crate::app::directory_tree_strip_cache::StripPreviewBufferTag::StripDecodedPixels,
                strip_max_side_used: None,
                ctx,
                bypass_detach_queue: false,
            },
        );
    }

    pub(super) fn install_static_hdr_image(&mut self, idx: usize, install: StaticHdrInstall<'_>) {
        let StaticHdrInstall {
            hdr,
            fallback,
            sdr_fallback_is_placeholder,
            ultra_hdr_capacity_sensitive,
            defer_sdr_upload,
            ctx,
        } = install;
        let gpu_demosaic_pending = crate::loader::hdr_raw_gpu_demosaic_pending(&hdr);
        self.record_installed_display_mode(idx, crate::loader::RenderShape::Static);
        self.remove_hdr_image_resources(idx);
        self.insert_hdr_image_cache_tracked(idx, Arc::clone(&hdr));
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
        }
        if self.should_eager_cache_install_hdr_strip(idx)
            && let Some(strip_preview) = self.installed_hdr_directory_tree_strip_preview(
                &hdr,
                fallback,
                sdr_fallback_is_placeholder,
            )
        {
            let strip_stage = crate::loader::PreviewStage::Refined;
            #[cfg(feature = "preload-debug")]
            crate::preload_debug!(
                "[PreloadDebug][Strip] install cache idx={} stage={strip_stage:?} decoded={}x{}",
                idx,
                strip_preview.width,
                strip_preview.height
            );
            let strip_logical = crate::loader::directory_tree_strip_logical_for_preview(
                hdr.width,
                hdr.height,
                fallback.width,
                fallback.height,
                strip_preview.width,
                strip_preview.height,
                !hdr.rgba_f32.is_empty(),
            );
            let strip_tag = if crate::loader::hdr_has_iso_deferred_gain_map(hdr.as_ref())
                && hdr.rgba_f32.is_empty()
            {
                crate::app::directory_tree_strip_cache::StripPreviewBufferTag::IsoGainMapBaseline
            } else {
                crate::app::directory_tree_strip_cache::strip_buffer_tag_for_hdr_preview(
                    !hdr.rgba_f32.is_empty(),
                    strip_preview.is_sdr_deferred_placeholder(),
                )
            };
            self.cache_directory_tree_strip_thumbnail(
                crate::app::directory_tree_strip_cache::StripThumbnailCacheRequest {
                    index: idx,
                    job_key: None,
                    decoded: &strip_preview,
                    stage: strip_stage,
                    logical_size: Some(strip_logical),
                    buffer_tag: strip_tag,
                    strip_max_side_used: None,
                    ctx,
                    bypass_detach_queue: false,
                },
            );
        }
    }

    pub(super) fn install_tiled_image(&mut self, install: TiledImageInstall<'_>) {
        let TiledImageInstall {
            idx,
            decode_profile,
            source,
            hdr_source,
            sdr_preview,
            hdr_preview,
            hdr_sdr_fallback,
            ultra_hdr_capacity_sensitive,
            ctx,
        } = install;
        self.record_installed_display_mode(idx, crate::loader::RenderShape::Tiled);
        self.remove_hdr_image_resources(idx);
        #[cfg(feature = "preload-debug")]
        let bootstrap_dims = sdr_preview.map(|p| (p.width, p.height));
        #[cfg(feature = "preload-debug")]
        let bootstrap_hdr_dims = hdr_preview.as_ref().map(|h| (h.width, h.height));
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

        if !source.defers_loader_hq_preview() {
            self.hq_tiled_preview_pending_indices.insert(idx);
            #[cfg(feature = "preload-debug")]
            crate::preload_debug!(
                "[PreloadDebug][Install] hq_pending_set idx={} current={} bootstrap={:?} hdr_preview={:?}",
                idx,
                self.current_index,
                bootstrap_dims,
                bootstrap_hdr_dims,
            );
        }

        let mut tm = build_tiled_manager_with_best_preview(
            idx,
            decode_profile,
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
            source.request_refinement(idx, self.decode_profile_for_index(idx));
            self.note_cpu_raw_refinement_requested(idx);
            self.pixel_data_source = Some(crate::pixel_inspector::PixelDataSource::Tiled(
                Arc::clone(&source),
            ));
        } else {
            self.insert_prefetched_tiles_tracked(idx, tm);
        }

        if crate::preload_debug::path_is_raw(&self.image_files[idx]) {
            crate::preload_debug!(
                "[PreloadDebug][RAW] install_tiled idx={} current={} logical={}x{} hdr={} sdr_preview={}",
                idx,
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

    fn build_hdr_animation_pending_parts(
        frames: &[crate::loader::HdrAnimationFrame],
    ) -> (
        Vec<std::sync::Arc<crate::hdr::types::HdrImageBuffer>>,
        Vec<crate::loader::AnimationFrame>,
    ) {
        let hdr_frames: Vec<std::sync::Arc<crate::hdr::types::HdrImageBuffer>> = frames
            .iter()
            .map(|frame| std::sync::Arc::new(frame.hdr.clone()))
            .collect();
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
        (hdr_frames, sdr_frames)
    }

    fn cache_eager_hdr_animated_strip_preview(
        &mut self,
        idx: usize,
        hdr: &crate::hdr::types::HdrImageBuffer,
        fallback: &DecodedImage,
        sdr_fallback_is_placeholder: bool,
        ctx: &egui::Context,
    ) {
        if !self.should_eager_cache_install_hdr_strip(idx) {
            return;
        }
        let Some(strip_preview) = self.installed_hdr_directory_tree_strip_preview(
            hdr,
            fallback,
            sdr_fallback_is_placeholder,
        ) else {
            return;
        };
        let strip_stage = crate::loader::PreviewStage::Refined;
        let strip_logical = crate::loader::directory_tree_strip_logical_for_preview(
            hdr.width,
            hdr.height,
            fallback.width,
            fallback.height,
            strip_preview.width,
            strip_preview.height,
            !hdr.rgba_f32.is_empty(),
        );
        let strip_tag =
            if crate::loader::hdr_has_iso_deferred_gain_map(hdr) && hdr.rgba_f32.is_empty() {
                crate::app::directory_tree_strip_cache::StripPreviewBufferTag::IsoGainMapBaseline
            } else {
                crate::app::directory_tree_strip_cache::strip_buffer_tag_for_hdr_preview(
                    !hdr.rgba_f32.is_empty(),
                    strip_preview.is_sdr_deferred_placeholder(),
                )
            };
        self.cache_directory_tree_strip_thumbnail(
            crate::app::directory_tree_strip_cache::StripThumbnailCacheRequest {
                index: idx,
                job_key: None,
                decoded: &strip_preview,
                stage: strip_stage,
                logical_size: Some(strip_logical),
                buffer_tag: strip_tag,
                strip_max_side_used: None,
                ctx,
                bypass_detach_queue: false,
            },
        );
    }

    fn hdr_animated_install_is_redundant(&self, idx: usize, frame_count: usize) -> bool {
        if frame_count <= 1 {
            return false;
        }
        if let Some(cached) = self.animation_cache.get(&idx)
            && cached.hdr_frames.is_some()
            && cached.textures.len() == frame_count
        {
            return true;
        }
        if let Some(pending) = self.pending_anim_frames.get(&idx)
            && pending.hdr_frames.is_some()
            && pending.frames.len() == frame_count
        {
            return true;
        }
        false
    }

    fn sdr_animated_install_is_redundant(&self, idx: usize, frame_count: usize) -> bool {
        if frame_count <= 1 {
            return false;
        }
        if let Some(cached) = self.animation_cache.get(&idx)
            && cached.hdr_frames.is_none()
            && cached.textures.len() == frame_count
        {
            return true;
        }
        if let Some(pending) = self.pending_anim_frames.get(&idx)
            && pending.hdr_frames.is_none()
            && pending.frames.len() == frame_count
        {
            return true;
        }
        false
    }

    fn apply_hdr_animated_remainder_merge(
        &mut self,
        idx: usize,
        frames: &[crate::loader::HdrAnimationFrame],
        ultra_hdr_capacity_sensitive: bool,
        preserved: HdrAnimatedRemainderMergeState,
        ctx: &egui::Context,
    ) {
        let (hdr_frames, sdr_frames) = Self::build_hdr_animation_pending_parts(frames);
        if let Some(first_hdr) = hdr_frames.first() {
            self.insert_hdr_image_cache_tracked(idx, std::sync::Arc::clone(first_hdr));
        }
        self.hdr_sdr_fallback_indices.insert(idx);
        if ultra_hdr_capacity_sensitive {
            self.ultra_hdr_capacity_sensitive_indices.insert(idx);
        }
        if let Some(first) = frames.first() {
            if first.fallback.is_sdr_deferred_placeholder() {
                self.hdr_placeholder_fallback_indices.insert(idx);
            } else {
                self.hdr_placeholder_fallback_indices.remove(&idx);
            }
            if idx == self.current_index {
                self.set_current_image_resolution(Some((first.width(), first.height())));
                if let Some(hdr) = hdr_frames.first() {
                    self.current_hdr_image = Some(crate::app::CurrentHdrImage::new(
                        idx,
                        std::sync::Arc::clone(hdr),
                    ));
                }
                self.refresh_hdr_view_status();
            }
        } else {
            self.hdr_placeholder_fallback_indices.remove(&idx);
        }

        let HdrAnimatedRemainderMergeState {
            preserved_textures,
            preserved_delays,
            next_frame,
        } = preserved;

        self.pending_anim_frames.insert(
            idx,
            PendingAnimUpload {
                image_index: idx,
                hdr_frames: Some(hdr_frames.clone()),
                frames: sdr_frames.clone(),
                textures: preserved_textures,
                delays: preserved_delays,
                next_frame,
            },
        );
        if idx == self.current_index {
            self.sync_active_animation_after_remainder_merge(idx, &hdr_frames, &sdr_frames);
        }
        crate::preload_debug!(
            "[PreloadDebug] merge hdr animation remainder: idx={} current={} frames={} uploaded={}",
            idx,
            self.current_index,
            frames.len(),
            next_frame,
        );
        ctx.request_repaint();
        if idx == self.current_index {
            self.ensure_current_animation_playback();
        }
    }

    fn try_merge_hdr_animated_remainder(
        &mut self,
        idx: usize,
        frames: &[crate::loader::HdrAnimationFrame],
        ultra_hdr_capacity_sensitive: bool,
        ctx: &egui::Context,
    ) -> bool {
        let new_count = frames.len();
        if new_count <= 1 {
            return false;
        }
        if self.hdr_animated_install_is_redundant(idx, new_count) {
            crate::preload_debug!(
                "[PreloadDebug] skip hdr animation reinstall: idx={} frames={}",
                idx,
                new_count,
            );
            return true;
        }

        if let Some(existing) = self.pending_anim_frames.get(&idx) {
            if existing.hdr_frames.is_none() {
                return false;
            }
            let existing_count = existing.frames.len();
            if !super::animation_remainder_extends_existing(existing_count, new_count) {
                return false;
            }
            let first = &existing.frames[0];
            if frames[0].width() != first.width || frames[0].height() != first.height {
                return false;
            }
            let preserved = HdrAnimatedRemainderMergeState {
                preserved_textures: std::sync::Arc::clone(&existing.textures),
                preserved_delays: std::sync::Arc::clone(&existing.delays),
                next_frame: existing.next_frame,
            };
            self.apply_hdr_animated_remainder_merge(
                idx,
                frames,
                ultra_hdr_capacity_sensitive,
                preserved,
                ctx,
            );
            return true;
        }

        if let Some(cached) = self.animation_cache.get(&idx) {
            if cached.hdr_frames.is_none() {
                return false;
            }
            let existing_count = cached.textures.len();
            if !super::animation_remainder_extends_existing(existing_count, new_count) {
                return false;
            }
            let Some(hdr_first) = cached.hdr_frames.as_ref().and_then(|frames| frames.first())
            else {
                return false;
            };
            if frames[0].width() != hdr_first.width || frames[0].height() != hdr_first.height {
                return false;
            }
            let cached = self
                .animation_cache
                .remove(&idx)
                .expect("cache entry present");
            let preserved = HdrAnimatedRemainderMergeState {
                preserved_textures: std::sync::Arc::clone(&cached.textures),
                preserved_delays: std::sync::Arc::clone(&cached.delays),
                next_frame: cached.textures.len(),
            };
            self.apply_hdr_animated_remainder_merge(
                idx,
                frames,
                ultra_hdr_capacity_sensitive,
                preserved,
                ctx,
            );
            return true;
        }

        false
    }

    fn apply_sdr_animated_remainder_merge(
        &mut self,
        idx: usize,
        frames: &[crate::loader::AnimationFrame],
        preserved_textures: std::sync::Arc<Vec<egui::TextureHandle>>,
        preserved_delays: std::sync::Arc<Vec<std::time::Duration>>,
        next_frame: usize,
        ctx: &egui::Context,
    ) {
        if idx == self.current_index
            && let Some(first) = frames.first()
        {
            self.set_current_image_resolution(Some((first.width, first.height)));
            self.tile_manager = None;
            self.pixel_data_source = Some(crate::pixel_inspector::PixelDataSource::Static {
                width: first.width,
                height: first.height,
                pixels: first.arc_pixels(),
            });
        }
        self.pending_anim_frames.insert(
            idx,
            PendingAnimUpload {
                image_index: idx,
                hdr_frames: None,
                frames: frames.to_vec(),
                textures: preserved_textures,
                delays: preserved_delays,
                next_frame,
            },
        );
        if idx == self.current_index {
            self.sync_active_animation_after_remainder_merge(idx, &[], frames);
        }
        crate::preload_debug!(
            "[PreloadDebug] merge animation remainder: idx={} current={} frames={} uploaded={}",
            idx,
            self.current_index,
            frames.len(),
            next_frame,
        );
        ctx.request_repaint();
        if idx == self.current_index {
            self.ensure_current_animation_playback();
        }
    }

    fn try_merge_sdr_animated_remainder(
        &mut self,
        idx: usize,
        frames: &[crate::loader::AnimationFrame],
        ctx: &egui::Context,
    ) -> bool {
        let new_count = frames.len();
        if new_count <= 1 {
            return false;
        }
        if self.sdr_animated_install_is_redundant(idx, new_count) {
            crate::preload_debug!(
                "[PreloadDebug] skip animation reinstall: idx={} frames={}",
                idx,
                new_count,
            );
            return true;
        }

        if let Some(existing) = self.pending_anim_frames.get(&idx) {
            if existing.hdr_frames.is_some() {
                return false;
            }
            let existing_count = existing.frames.len();
            if !super::animation_remainder_extends_existing(existing_count, new_count) {
                return false;
            }
            let first = &existing.frames[0];
            if frames[0].width != first.width || frames[0].height != first.height {
                return false;
            }
            let preserved_textures = std::sync::Arc::clone(&existing.textures);
            let preserved_delays = std::sync::Arc::clone(&existing.delays);
            let next_frame = existing.next_frame;
            self.apply_sdr_animated_remainder_merge(
                idx,
                frames,
                preserved_textures,
                preserved_delays,
                next_frame,
                ctx,
            );
            return true;
        }

        if let Some(cached) = self.animation_cache.get(&idx) {
            if cached.hdr_frames.is_some() {
                return false;
            }
            let existing_count = cached.textures.len();
            if !super::animation_remainder_extends_existing(existing_count, new_count) {
                return false;
            }
            let cached = self
                .animation_cache
                .remove(&idx)
                .expect("cache entry present");
            let preserved_textures = std::sync::Arc::clone(&cached.textures);
            let preserved_delays = std::sync::Arc::clone(&cached.delays);
            let next_frame = cached.textures.len();
            self.apply_sdr_animated_remainder_merge(
                idx,
                frames,
                preserved_textures,
                preserved_delays,
                next_frame,
                ctx,
            );
            return true;
        }

        false
    }

    pub(super) fn install_animated_image(
        &mut self,
        idx: usize,
        frames: &[crate::loader::AnimationFrame],
        ctx: &egui::Context,
    ) {
        if self.try_merge_sdr_animated_remainder(idx, frames, ctx) {
            return;
        }
        self.record_installed_display_mode(idx, crate::loader::RenderShape::Animated);
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
            // Same as install_static_sdr_image: main texture may exceed strip max_side
            // (e.g. 320x320 anim vs 256 strip) so texture_cache sync cannot fill the strip.
            // Without this handoff, cold strip deferred to main loader stays on the placeholder.
            self.cache_directory_tree_strip_thumbnail(
                crate::app::directory_tree_strip_cache::StripThumbnailCacheRequest {
                    index: idx,
                    job_key: None,
                    decoded: &decoded,
                    stage: crate::loader::PreviewStage::Refined,
                    logical_size: Some((decoded.width, decoded.height)),
                    buffer_tag: crate::app::directory_tree_strip_cache::StripPreviewBufferTag::StripDecodedPixels,
                    strip_max_side_used: None,
                    ctx,
                    bypass_detach_queue: false,
                },
            );
        }

        self.pending_anim_frames.insert(
            idx,
            PendingAnimUpload {
                image_index: idx,
                hdr_frames: None,
                frames: frames.to_vec(),
                textures: std::sync::Arc::new(Vec::new()),
                delays: std::sync::Arc::new(Vec::new()),
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
        if idx == self.current_index {
            self.ensure_current_animation_playback();
        }
    }

    pub(super) fn install_hdr_animated_image(
        &mut self,
        idx: usize,
        frames: &[crate::loader::HdrAnimationFrame],
        ultra_hdr_capacity_sensitive: bool,
        ctx: &egui::Context,
    ) {
        if self.try_merge_hdr_animated_remainder(idx, frames, ultra_hdr_capacity_sensitive, ctx) {
            return;
        }
        self.record_installed_display_mode(idx, crate::loader::RenderShape::Animated);
        self.remove_hdr_image_resources(idx);
        let hdr_frames: Vec<Arc<crate::hdr::types::HdrImageBuffer>> = frames
            .iter()
            .map(|frame| Arc::new(frame.hdr.clone()))
            .collect();
        if let Some(first_hdr) = hdr_frames.first() {
            // Preload / first navigation reads `hdr_image_cache` before deferred anim uploads
            // finish populating `animation_cache`. Without this, HDR displays fall back to the
            // black SDR placeholder until `pending_anim_frames` completes (dark → bright flash).
            self.insert_hdr_image_cache_tracked(idx, Arc::clone(first_hdr));
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
                textures: std::sync::Arc::new(Vec::new()),
                delays: std::sync::Arc::new(Vec::new()),
                next_frame: 0,
            },
        );
        crate::preload_debug!(
            "[PreloadDebug] queue hdr animation upload: idx={} current={} frames={}",
            idx,
            self.current_index,
            frames.len()
        );
        if let Some(first) = frames.first() {
            self.cache_eager_hdr_animated_strip_preview(
                idx,
                &first.hdr,
                &first.fallback,
                first.fallback.is_sdr_deferred_placeholder(),
                ctx,
            );
        }
        ctx.request_repaint();
        if idx == self.current_index {
            self.ensure_current_animation_playback();
        }
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
        self.main_loader_failed_indices.insert(idx);
        self.main_loader_failed_errors
            .insert(idx, error.to_string());
        // Async PSD/PSB installs a tiled shell before pixels exist; tear it down on
        // failure so navigation does not restore a black placeholder with no error.
        self.uninstall_failed_main_loader_asset(idx);
        let message = t!("status.load_failed", path = path_str, err = error).to_string();
        if idx == self.current_index {
            self.error_message = Some(message);
        }
    }

    /// Drop any "successful" shell left behind after an async main-loader failure.
    fn uninstall_failed_main_loader_asset(&mut self, idx: usize) {
        self.prefetched_tiles.remove(&idx);
        self.deferred_sdr_uploads.remove(&idx);
        self.hq_tiled_preview_pending_indices.remove(&idx);
        self.texture_cache.remove(idx);
        self.clear_installed_display_mode(idx);
        crate::tile_cache::PIXEL_CACHE.write().remove_image(idx);
        self.directory_tree_strip_cache.remove_index(idx);
        self.directory_tree_strip_tiled_attempted.remove(&idx);
        self.directory_tree_strip_cold_attempted.remove(&idx);
        self.directory_tree_strip_cold_awaiting_main_loader
            .remove(&idx);
        if self
            .tile_manager
            .as_ref()
            .is_some_and(|tm| tm.image_index == idx)
        {
            self.tile_manager = None;
            self.pixel_data_source = None;
        }
        self.sync_prefetch_resource_index(idx);
    }

    pub(super) fn note_main_loader_install_success(&mut self, idx: usize) {
        self.main_loader_failed_indices.remove(&idx);
        self.main_loader_failed_errors.remove(&idx);
    }

    pub(super) fn surface_main_loader_failure_for_current(&mut self) {
        let idx = self.current_index;
        let Some(error) = self.main_loader_failed_errors.get(&idx).cloned() else {
            return;
        };
        let path_str = self
            .image_files
            .get(idx)
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| format!("[index {idx} absent after rescan]"));
        self.error_message =
            Some(t!("status.load_failed", path = path_str, err = error).to_string());
    }
}
