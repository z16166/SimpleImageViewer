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

//! Per-index display pipeline recorded at image install time.

use crate::loader::RenderShape;
use std::path::PathBuf;

use super::ImageViewerApp;

fn index_path_is_maybe_animated(image_files: &[PathBuf], index: usize) -> bool {
    image_files
        .get(index)
        .and_then(|path| path.extension())
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| crate::loader::is_maybe_animated(&ext.to_ascii_lowercase()))
}

impl ImageViewerApp {
    pub(crate) fn record_installed_display_mode(&mut self, index: usize, mode: RenderShape) {
        debug_assert!(mode != RenderShape::Unknown);
        self.installed_display_modes.insert(index, mode);
    }

    pub(crate) fn installed_display_mode(&self, index: usize) -> Option<RenderShape> {
        self.installed_display_modes.get(&index).copied()
    }

    pub(crate) fn clear_installed_display_mode(&mut self, index: usize) {
        self.installed_display_modes.remove(&index);
    }

    pub(crate) fn index_uses_tiled_pipeline(&self, index: usize) -> bool {
        self.installed_display_mode(index) == Some(RenderShape::Tiled)
    }

    /// True when an index needs a live [`TileManager`], including after prefetch eviction
    /// cleared `installed_display_modes` but the texture cache still marks tiled pyramids.
    pub(crate) fn index_requires_tile_manager(&self, index: usize) -> bool {
        self.index_uses_tiled_pipeline(index)
            || self.texture_cache.needs_tile_manager(index)
            || self.hdr_tiled_source_cache.contains_key(&index)
    }

    /// Whether tiled HQ sync can stop (loader refine satisfied, not on-demand SDR alone).
    pub(crate) fn tiled_hq_preview_requirement_met(&self, index: usize) -> bool {
        let display = self.display_requirements_for_index(index);
        if crate::loader::output_mode_is_hdr(display.output_mode) {
            return self.hdr_tiled_preview_cache.contains_key(&index)
                && self.texture_cache.satisfies_tiled_sdr_hq(index);
        }
        if self.texture_cache.satisfies_tiled_sdr_hq(index) {
            return true;
        }
        let Some(tm) = self
            .tile_manager
            .as_ref()
            .filter(|tm| tm.image_index == index)
        else {
            return false;
        };
        if tm.preview_texture.is_none() {
            return false;
        }
        match self.texture_cache.cached_preview_stage(index) {
            Some(crate::loader::PreviewStage::Refined) => true,
            None => {
                crate::preload_debug!(
                    "[PreloadDebug][SyncHq] tm_preview_without_cache_stage idx={} cache_tag={:?} hq_pending={} current={}",
                    index,
                    self.texture_cache.cached_buffer_tag(index),
                    self.hq_tiled_preview_pending_indices.contains(&index),
                    self.current_index,
                );
                !self.hq_tiled_preview_pending_indices.contains(&index)
            }
            Some(crate::loader::PreviewStage::Initial) => {
                crate::preload_debug!(
                    "[PreloadDebug][SyncHq] tm_preview_cache_stage_initial idx={} cache_tag={:?} current={}",
                    index,
                    self.texture_cache.cached_buffer_tag(index),
                    self.current_index,
                );
                false
            }
        }
    }

    pub(crate) fn index_uses_animated_pipeline(&self, index: usize) -> bool {
        self.installed_display_mode(index) == Some(RenderShape::Animated)
    }

    pub(crate) fn should_draw_tiled_canvas(&self) -> bool {
        self.index_uses_tiled_pipeline(self.current_index)
            && self.tiled_canvas_matches_current_index()
    }

    pub(crate) fn animation_needs_repaint_wake(&self) -> bool {
        self.index_uses_animated_pipeline(self.current_index)
            && self.animation.as_ref().is_some_and(|anim| {
                anim.image_index == self.current_index && anim.textures.len() > 1
            })
    }

    /// Time until the active animation frame should advance; used to schedule repaints.
    pub(crate) fn next_animation_repaint_after(&self) -> Option<std::time::Duration> {
        let anim = self.animation.as_ref()?;
        if anim.image_index != self.current_index || anim.textures.len() <= 1 {
            return None;
        }
        Some(anim.repaint_after())
    }

    /// Animated HDR planes use synchronous GPU upload on cache miss (main-branch behavior).
    pub(crate) fn hdr_plane_sync_upload_on_cache_miss(&self) -> bool {
        self.animation_needs_repaint_wake()
    }

    pub(crate) fn animation_upload_pending_for_current(&self) -> bool {
        self.index_uses_animated_pipeline(self.current_index)
            && self.pending_anim_frames.contains_key(&self.current_index)
    }

    /// True when a preloaded animated file has only its first-frame SDR texture cached.
    pub(crate) fn needs_stale_animated_first_frame_reload(&self) -> bool {
        let current_index = self.current_index;
        if self.installed_display_mode(current_index) == Some(RenderShape::Static) {
            return false;
        }
        if self.animation_cache.contains_key(&current_index)
            || self.pending_anim_frames.contains_key(&current_index)
        {
            return false;
        }
        if !self.texture_cache.contains(current_index) {
            return false;
        }
        self.index_uses_animated_pipeline(current_index)
            || index_path_is_maybe_animated(&self.image_files, current_index)
    }

    /// Restore or promote animation playback for the current index.
    pub(crate) fn ensure_current_animation_playback(&mut self) {
        let idx = self.current_index;
        if !self.index_uses_animated_pipeline(idx) {
            return;
        }
        self.tile_manager = None;

        if self
            .animation
            .as_ref()
            .is_some_and(|anim| anim.image_index == idx && anim.textures.len() > 1)
        {
            return;
        }

        if let Some(cached) = self.animation_cache.get(&idx) {
            if cached.textures.len() > 1 {
                if let Some(hdr_frames) = &cached.hdr_frames
                    && let Some(hdr) = hdr_frames.first()
                {
                    self.current_hdr_image = Some(crate::app::CurrentHdrImage::new(
                        idx,
                        std::sync::Arc::clone(hdr),
                    ));
                }
                self.animation = Some(crate::app::AnimationPlayback {
                    image_index: cached.image_index,
                    textures: std::sync::Arc::clone(&cached.textures),
                    hdr_frames: cached.hdr_frames.clone(),
                    delays: std::sync::Arc::clone(&cached.delays),
                    current_frame: 0,
                    frame_start: std::time::Instant::now(),
                    cpu_frames: cached.cpu_frames.clone(),
                });
            }
            return;
        }

        let Some(pending) = self.pending_anim_frames.get(&idx) else {
            return;
        };
        if pending.textures.len() <= 1 {
            return;
        }
        let uploaded = pending.textures.len();
        self.animation = Some(crate::app::AnimationPlayback {
            image_index: pending.image_index,
            textures: std::sync::Arc::clone(&pending.textures),
            hdr_frames: pending.hdr_frames.clone(),
            delays: std::sync::Arc::clone(&pending.delays),
            current_frame: 0,
            frame_start: std::time::Instant::now(),
            cpu_frames: Some(
                pending
                    .frames
                    .iter()
                    .take(uploaded)
                    .map(|frame| frame.arc_pixels())
                    .collect(),
            ),
        });
    }

    /// Extend active playback as deferred SDR textures finish uploading.
    pub(crate) fn sync_active_animation_from_pending(&mut self, idx: usize) {
        let Some(pending) = self.pending_anim_frames.get(&idx) else {
            return;
        };
        if pending.textures.len() <= 1 {
            return;
        }
        let Some(anim) = self.animation.as_mut() else {
            return;
        };
        if anim.image_index != idx || pending.textures.len() <= anim.textures.len() {
            return;
        }
        anim.textures = std::sync::Arc::clone(&pending.textures);
        anim.delays = std::sync::Arc::clone(&pending.delays);
        match anim.cpu_frames.as_mut() {
            Some(cpu_frames) => {
                cpu_frames.extend(
                    pending
                        .frames
                        .iter()
                        .skip(cpu_frames.len())
                        .map(|frame| frame.arc_pixels()),
                );
            }
            None => {
                anim.cpu_frames = Some(
                    pending
                        .frames
                        .iter()
                        .take(pending.textures.len())
                        .map(|frame| frame.arc_pixels())
                        .collect(),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::{DecodeProfile, LoadIntent, RenderShape};
    use crate::settings::RawDemosaicMode;
    use crate::tile_cache::TileManager;
    use std::path::PathBuf;
    use std::sync::Arc;

    struct EmptySource;
    impl crate::loader::TiledImageSource for EmptySource {
        fn width(&self) -> u32 {
            1
        }
        fn height(&self) -> u32 {
            1
        }
        fn extract_tile(&self, _: u32, _: u32, w: u32, h: u32) -> Arc<Vec<u8>> {
            Arc::new(vec![0; (w * h * 4) as usize])
        }
        fn generate_preview(&self, max_w: u32, max_h: u32) -> (u32, u32, Vec<u8>) {
            (max_w, max_h, vec![0; (max_w * max_h * 4) as usize])
        }
        fn full_pixels(&self) -> Option<Arc<Vec<u8>>> {
            None
        }
    }

    fn sample_tiled_profile() -> DecodeProfile {
        DecodeProfile {
            raw_high_quality: false,
            raw_demosaic_mode: RawDemosaicMode::Cpu,
            output_mode: crate::hdr::types::HdrOutputMode::SdrToneMapped,
            ultra_hdr_decode_capacity: 1.0,
            render_shape: RenderShape::Tiled,
            load_intent: LoadIntent::Current,
            profile_epoch: 0,
        }
    }

    fn app_with_mode(index: usize, mode: RenderShape) -> ImageViewerApp {
        let mut app = crate::app::image_management::tests::make_test_app();
        app.image_files = vec![PathBuf::from("test.gif")];
        app.set_current_index(index);
        app.record_installed_display_mode(index, mode);
        app
    }

    #[test]
    fn tiled_pipeline_requires_recorded_mode_and_tile_manager() {
        let mut app = app_with_mode(0, RenderShape::Tiled);
        assert!(!app.should_draw_tiled_canvas());
        app.tile_manager = Some(TileManager::with_source(
            0,
            sample_tiled_profile(),
            Arc::new(EmptySource) as Arc<dyn crate::loader::TiledImageSource>,
        ));
        assert!(app.should_draw_tiled_canvas());
    }

    #[test]
    fn animated_pipeline_blocks_tiled_draw_even_with_tile_manager() {
        let mut app = app_with_mode(0, RenderShape::Animated);
        app.tile_manager = Some(TileManager::with_source(
            0,
            sample_tiled_profile(),
            Arc::new(EmptySource) as Arc<dyn crate::loader::TiledImageSource>,
        ));
        assert!(!app.should_draw_tiled_canvas());
    }

    #[test]
    fn hdr_tiled_hq_requires_tone_mapped_sdr_texture() {
        use crate::hdr::types::{HdrColorSpace, HdrImageBuffer, HdrImageMetadata, HdrPixelFormat};
        use crate::loader::{PreviewStage, TextureCacheInsert, TexturePreviewBufferTag};
        use std::sync::Arc;

        let mut app = app_with_mode(0, RenderShape::Tiled);
        app.hdr_capabilities.output_mode = crate::hdr::types::HdrOutputMode::WindowsScRgb;
        app.hdr_tiled_preview_cache.insert(
            0,
            Arc::new(HdrImageBuffer {
                width: 64,
                height: 32,
                format: HdrPixelFormat::Rgba32Float,
                color_space: HdrColorSpace::LinearSrgb,
                metadata: HdrImageMetadata::default(),
                rgba_f32: Arc::new(vec![1.0; 64 * 32 * 4]),
            }),
        );
        assert!(
            !app.tiled_hq_preview_requirement_met(0),
            "HDR cache alone must not satisfy HQ gate"
        );

        let ctx = eframe::egui::Context::default();
        let color_image =
            eframe::egui::ColorImage::from_rgba_unmultiplied([64, 32], &vec![128u8; 64 * 32 * 4]);
        let handle = ctx.load_texture("hq", color_image, eframe::egui::TextureOptions::LINEAR);
        app.texture_cache.insert(
            0,
            handle,
            TextureCacheInsert {
                orig_w: 4096,
                orig_h: 2048,
                needs_tile_manager: true,
                buffer_tag: TexturePreviewBufferTag::TiledRefinedLoader,
                stage: PreviewStage::Refined,
                current_index: 0,
                total_count: 1,
            },
        );
        assert!(app.tiled_hq_preview_requirement_met(0));
    }

    #[test]
    fn tm_preview_without_cache_stage_counts_as_hq_when_not_pending() {
        use crate::loader::{PreviewStage, TextureCacheInsert, TexturePreviewBufferTag};
        use crate::tile_cache::TileManager;
        use std::sync::Arc;

        let mut app = app_with_mode(0, RenderShape::Tiled);
        let ctx = eframe::egui::Context::default();
        let color_image =
            eframe::egui::ColorImage::from_rgba_unmultiplied([64, 32], &vec![128u8; 64 * 32 * 4]);
        let handle = ctx.load_texture("tm_only", color_image, eframe::egui::TextureOptions::LINEAR);
        let mut tm = TileManager::with_source(
            0,
            sample_tiled_profile(),
            Arc::new(EmptySource) as Arc<dyn crate::loader::TiledImageSource>,
        );
        tm.preview_texture = Some(handle);
        app.tile_manager = Some(tm);
        assert!(
            app.tiled_hq_preview_requirement_met(0),
            "live tile-manager preview without cache stage should satisfy HQ when not pending"
        );

        app.texture_cache.insert(
            0,
            ctx.load_texture(
                "bootstrap",
                eframe::egui::ColorImage::from_rgba_unmultiplied([8, 8], &vec![0u8; 256]),
                eframe::egui::TextureOptions::LINEAR,
            ),
            TextureCacheInsert {
                orig_w: 4096,
                orig_h: 2048,
                needs_tile_manager: true,
                buffer_tag: TexturePreviewBufferTag::TiledBootstrap,
                stage: PreviewStage::Initial,
                current_index: 0,
                total_count: 1,
            },
        );
        assert!(
            !app.tiled_hq_preview_requirement_met(0),
            "bootstrap cache stage should block HQ until refined"
        );
    }

    #[test]
    fn stale_animated_reload_only_for_animated_mode_with_texture_only() {
        let mut app = app_with_mode(0, RenderShape::Animated);
        let ctx = eframe::egui::Context::default();
        let color_image = eframe::egui::ColorImage::from_rgba_unmultiplied([1, 1], &[0, 0, 0, 255]);
        let handle = ctx.load_texture("f0", color_image, eframe::egui::TextureOptions::LINEAR);
        app.texture_cache.insert(
            0,
            handle,
            crate::loader::TextureCacheInsert {
                orig_w: 1,
                orig_h: 1,
                needs_tile_manager: false,
                buffer_tag: crate::loader::TexturePreviewBufferTag::MainWindowSdr,
                stage: crate::loader::PreviewStage::Refined,
                current_index: 0,
                total_count: 1,
            },
        );
        assert!(app.needs_stale_animated_first_frame_reload());

        app.record_installed_display_mode(0, RenderShape::Static);
        assert!(!app.needs_stale_animated_first_frame_reload());
    }

    #[test]
    fn pending_animation_uploads_keep_process_loaded_images_active() {
        use crate::app::types::PendingAnimUpload;
        use crate::loader::AnimationFrame;
        use std::time::Duration;

        let mut app = app_with_mode(0, RenderShape::Animated);
        app.pending_anim_frames.insert(
            0,
            PendingAnimUpload {
                image_index: 0,
                hdr_frames: None,
                frames: vec![
                    AnimationFrame::new(1, 1, vec![0; 4], Duration::from_millis(100)),
                    AnimationFrame::new(1, 1, vec![1; 4], Duration::from_millis(100)),
                ],
                textures: std::sync::Arc::new(Vec::new()),
                delays: std::sync::Arc::new(Vec::new()),
                next_frame: 0,
            },
        );
        assert!(app.needs_process_loaded_images());
    }

    #[test]
    fn animation_upload_completes_after_loader_idle() {
        use crate::app::types::PendingAnimUpload;
        use crate::loader::AnimationFrame;
        use std::time::Duration;

        let mut app = app_with_mode(0, RenderShape::Animated);
        let frames: Vec<AnimationFrame> = (0..10)
            .map(|i| AnimationFrame::new(1, 1, vec![i; 4], Duration::from_millis(100)))
            .collect();
        app.pending_anim_frames.insert(
            0,
            PendingAnimUpload {
                image_index: 0,
                hdr_frames: None,
                frames,
                textures: std::sync::Arc::new(Vec::new()),
                delays: std::sync::Arc::new(Vec::new()),
                next_frame: 0,
            },
        );
        let ctx = eframe::egui::Context::default();
        app.process_loaded_images(&ctx, &mut None);
        assert!(app.pending_anim_frames.contains_key(&0));
        assert!(!app.loader.is_loading(0));

        app.process_loaded_images(&ctx, &mut None);
        assert!(!app.pending_anim_frames.contains_key(&0));
        assert!(app.animation_cache.contains_key(&0));
        assert!(
            app.animation
                .as_ref()
                .is_some_and(|anim| anim.textures.len() == 10)
        );
    }

    #[test]
    fn animation_upload_runs_while_scanning() {
        use crate::app::types::PendingAnimUpload;
        use crate::loader::AnimationFrame;
        use std::time::Duration;

        let mut app = app_with_mode(0, RenderShape::Animated);
        app.scanning = true;
        let frames: Vec<AnimationFrame> = (0..4)
            .map(|i| AnimationFrame::new(1, 1, vec![i; 4], Duration::from_millis(100)))
            .collect();
        app.pending_anim_frames.insert(
            0,
            PendingAnimUpload {
                image_index: 0,
                hdr_frames: None,
                frames,
                textures: std::sync::Arc::new(Vec::new()),
                delays: std::sync::Arc::new(Vec::new()),
                next_frame: 0,
            },
        );
        let ctx = eframe::egui::Context::default();
        app.process_pending_animation_uploads(&ctx);
        assert!(!app.pending_anim_frames.contains_key(&0));
        assert!(app.animation_cache.contains_key(&0));
    }
}
