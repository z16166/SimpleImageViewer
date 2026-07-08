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

use crate::app::rendering::plan::{RenderPlan, RenderShape};
use crate::app::rendering::plane::PlaneBackendKind;
use crate::app::{ImageViewerApp, TransitionStyle};
use crate::hdr::status::HdrRenderPath;
use crate::hdr::types::HdrColorSpace;
use crate::loader::{RawDemosaicBackend, RawOsdInfo, RawRenderPixels};
use crate::ui::osd::{HdrOsdFrame, ImageOsdFrame};
use eframe::egui;

impl ImageViewerApp {
    /// Upload deferred bootstrap SDR and seed HQ RAW OSD while GPU extract/demosaic is in flight.
    pub(crate) fn ensure_raw_inflight_bootstrap_present(
        &mut self,
        index: usize,
        ctx: &egui::Context,
    ) {
        if !self.raw_hq_index_requires_hdr_plane(index) {
            return;
        }
        if self.hdr_image_cache.contains_key(&index)
            || self.hdr_tiled_source_cache.contains_key(&index)
        {
            return;
        }
        let bootstrap_dims = self
            .deferred_sdr_uploads
            .get(&index)
            .map(|decoded| (decoded.width, decoded.height))
            .or_else(|| self.texture_cache.get_original_res(index))
            .filter(|(width, height)| *width > 0 && *height > 0);
        if self.deferred_sdr_uploads.contains_key(&index) && !self.texture_cache.contains(index) {
            self.flush_deferred_sdr_upload_for_index(index, ctx);
        }
        let bootstrap_dims = bootstrap_dims.or_else(|| {
            self.texture_cache
                .get_original_res(index)
                .filter(|(width, height)| *width > 0 && *height > 0)
        });
        let Some((bootstrap_w, bootstrap_h)) = bootstrap_dims else {
            return;
        };
        let sensor = self
            .texture_cache
            .get_original_res(index)
            .unwrap_or((bootstrap_w, bootstrap_h));
        let demosaic_backend =
            if self.raw_demosaic_mode_for_index(index) == crate::settings::RawDemosaicMode::Gpu {
                Some(RawDemosaicBackend::Video)
            } else {
                Some(RawDemosaicBackend::Host)
            };
        self.set_raw_metadata_for_index(
            index,
            Some(RawOsdInfo {
                sensor_size: sensor,
                embedded_preview: Some((bootstrap_w, bootstrap_h)),
                render_pixels: RawRenderPixels::HqBootstrap {
                    width: bootstrap_w,
                    height: bootstrap_h,
                },
                demosaic_backend,
                cpu_demosaic_ms: None,
                gpu_extract_ms: None,
                gpu_demosaic_ms: None,
            }),
            ctx,
        );
    }

    pub(crate) fn set_raw_metadata_for_index(
        &mut self,
        index: usize,
        raw_metadata: Option<crate::loader::RawOsdInfo>,
        ctx: &egui::Context,
    ) -> bool {
        let changed = match raw_metadata {
            Some(raw_metadata) => {
                self.raw_metadata.insert_or_update(index, raw_metadata);
                true
            }
            None => self.raw_metadata.remove(index),
        };
        if index == self.current_index {
            self.osd.sync_events();
            ctx.request_repaint();
        }
        changed
    }

    pub(crate) fn note_cpu_raw_refinement_requested(&mut self, index: usize) {
        if self
            .image_files
            .get(index)
            .is_some_and(|path| crate::preload_debug::path_is_raw(path))
        {
            self.cpu_raw_refinement_pending_indices.insert(index);
        }
    }

    pub(crate) fn clear_cpu_raw_refinement_pending(&mut self, index: usize) {
        self.cpu_raw_refinement_pending_indices.remove(&index);
    }

    /// Promote the current RAW OSD to full develop as soon as CPU refine completes.
    pub(crate) fn promote_current_raw_osd_after_cpu_refine(
        &mut self,
        index: usize,
        ctx: &egui::Context,
    ) {
        if index != self.current_index {
            return;
        }
        if !self
            .image_files
            .get(index)
            .is_some_and(|path| crate::preload_debug::path_is_raw(path))
        {
            return;
        }
        let (width, height) = self
            .tile_manager
            .as_ref()
            .filter(|tm| tm.image_index == index)
            .map(|tm| (tm.full_width, tm.full_height))
            .or(self.current_image_res)
            .unwrap_or((0, 0));
        if width == 0 || height == 0 {
            return;
        }
        let osd = crate::loader::RawOsdInfo::refine_complete_without_timing(width, height);
        self.set_raw_metadata_for_index(index, Some(osd), ctx);
    }

    pub(crate) fn current_hdr_render_path(&self) -> Option<HdrRenderPath> {
        if let Some(path) = self.cached_frame_hdr_render_path {
            return Some(path);
        }
        self.compute_hdr_render_path()
    }

    pub(crate) fn clear_frame_render_plan_cache(&mut self) {
        self.cached_frame_render_plan = None;
        self.cached_frame_hdr_render_path = None;
    }

    pub(crate) fn record_frame_render_plan(
        &mut self,
        plan: RenderPlan,
        shape: RenderShape,
        prefer_sdr_for_pending_gpu_demosaic: bool,
        has_hdr_content: bool,
    ) {
        let complex_transition_active = self.transition_start.is_some()
            && matches!(
                self.active_transition,
                TransitionStyle::PageFlip | TransitionStyle::Ripple | TransitionStyle::Curtain
            );
        let effective_selection = self
            .frame_effective_hdr_monitor_selection
            .clone()
            .or_else(|| self.effective_hdr_monitor_selection());
        let prefer_embedded_iso_gain_map_sdr_master = self
            .prefer_embedded_iso_gain_map_sdr_master_for_selection(effective_selection.as_ref());
        self.cached_frame_hdr_render_path = hdr_render_path_for_render_plan(
            &plan,
            shape,
            complex_transition_active,
            prefer_sdr_for_pending_gpu_demosaic,
            has_hdr_content,
            prefer_embedded_iso_gain_map_sdr_master,
        );
        self.cached_frame_render_plan = Some(plan);
    }

    fn compute_hdr_render_path(&self) -> Option<HdrRenderPath> {
        // Keep OSD [`HdrRenderPath`] aligned with [`Self::build_render_plan`] / draw path inputs.
        let idx = self.current_index;
        let tiled_canvas_active = self.tiled_canvas_matches_current_index();
        let has_hdr_tiled_source = self
            .current_hdr_tiled_image
            .as_ref()
            .is_some_and(|current| current.source_for_index(idx).is_some())
            || self.hdr_tiled_source_cache.contains_key(&idx);
        let has_sdr_fallback = self.hdr_sdr_fallback_indices.contains(&idx);
        let has_hdr_image = self
            .current_hdr_image
            .as_ref()
            .is_some_and(|current| current.image_for_index(idx).is_some())
            || self.hdr_image_cache.contains_key(&idx);
        let shape = if tiled_canvas_active {
            RenderShape::Tiled
        } else {
            RenderShape::Static
        };
        let has_hdr_plane = if shape == RenderShape::Tiled {
            has_hdr_tiled_source
        } else {
            has_hdr_image
        };
        let prefer_sdr_for_pending_gpu_demosaic = shape == RenderShape::Static
            && crate::app::image_management::prefer_sdr_bootstrap_while_raw_gpu_demosaic_pending(
                idx,
                &self.hdr_raw_gpu_demosaic_pending_indices,
                &self.hdr_raw_gpu_demosaic_baked_indices,
                &self.hdr_image_cache,
                has_sdr_fallback,
                self.texture_cache.contains(idx),
            );
        if prefer_sdr_for_pending_gpu_demosaic {
            crate::preload_debug!(
                "[PreloadDebug][RAW-GPU] render backend=Sdr bootstrap cur={} pending=true (osd)",
                idx
            );
        }

        let complex_transition_active = self.transition_start.is_some()
            && matches!(
                self.active_transition,
                TransitionStyle::PageFlip | TransitionStyle::Ripple | TransitionStyle::Curtain
            );

        let effective_selection = self
            .frame_effective_hdr_monitor_selection
            .clone()
            .or_else(|| self.effective_hdr_monitor_selection());

        let prefer_embedded_iso_gain_map_sdr_master = self
            .prefer_embedded_iso_gain_map_sdr_master_for_selection(effective_selection.as_ref());

        let plan = crate::app::rendering::plan::build_render_plan_for_state(
            crate::app::rendering::plan::RenderPlanStateInput {
                shape,
                has_hdr_plane,
                has_sdr_fallback,
                target_format: self.effective_hdr_target_format(),
                monitor_selection: effective_selection.as_ref(),
                prefer_sdr_for_pending_gpu_demosaic,
                force_hdr_plane_after_raw_demosaic: self.raw_gpu_demosaic_await_hdr_present,
                prefer_embedded_iso_gain_map_sdr_master,
            },
        );
        hdr_render_path_for_render_plan(
            &plan,
            shape,
            complex_transition_active,
            prefer_sdr_for_pending_gpu_demosaic,
            has_hdr_image || has_hdr_tiled_source || has_sdr_fallback,
            prefer_embedded_iso_gain_map_sdr_master,
        )
    }

    pub(crate) fn update_view_status_for_paint(&mut self, image: &ImageOsdFrame) {
        let file_name = self.current_file_name.as_str();
        self.raw_metadata.set_current_index(self.current_index);
        let effective_selection = self
            .frame_effective_hdr_monitor_selection
            .clone()
            .or_else(|| self.effective_hdr_monitor_selection());
        let hdr = HdrOsdFrame {
            render_path: self.current_hdr_render_path(),
            color_space: self.current_hdr_color_space(),
            output_mode: self.hdr_capabilities.output_mode,
            native_presentation_enabled: self.hdr_capabilities.native_presentation_enabled,
            ultra_hdr_decode_capacity: Some(self.ultra_hdr_decode_capacity),
            monitor_label: effective_selection
                .as_ref()
                .map(|selection| selection.label.as_str()),
            exposure_ev: self.effective_hdr_tone_map_settings().exposure_ev,
        };
        self.image_status.set_image_frame(image, file_name);
        self.push_hdr_view_status(&hdr);
        self.osd.sync_events();
    }

    pub(crate) fn refresh_hdr_view_status(&mut self) {
        let monitor_selection = self.effective_hdr_monitor_selection();
        let hdr = HdrOsdFrame {
            render_path: self.current_hdr_render_path(),
            color_space: self.current_hdr_color_space(),
            output_mode: self.hdr_capabilities.output_mode,
            native_presentation_enabled: self.hdr_capabilities.native_presentation_enabled,
            ultra_hdr_decode_capacity: Some(self.ultra_hdr_decode_capacity),
            monitor_label: monitor_selection
                .as_ref()
                .map(|selection| selection.label.as_str()),
            exposure_ev: self.effective_hdr_tone_map_settings().exposure_ev,
        };
        self.push_hdr_view_status(&hdr);
        self.osd.sync_events();
    }

    fn push_hdr_view_status(&mut self, hdr: &HdrOsdFrame<'_>) {
        self.image_status.set_hdr_frame(hdr);
    }

    pub(crate) fn invalidate_view_text_layout(&mut self) {
        self.osd.invalidate();
    }

    pub(crate) fn current_image_frame_status(
        &self,
        zoom_pct: u32,
    ) -> Option<crate::ui::osd::ImageOsdFrame> {
        let mut res_w = 0u32;
        let mut res_h = 0u32;
        let mut osd_mode = crate::ui::osd::ImageOsdMode::Static;

        if self.tiled_canvas_matches_current_index() {
            if let Some(tm) = &self.tile_manager {
                res_w = tm.full_width;
                res_h = tm.full_height;
                osd_mode = crate::ui::osd::ImageOsdMode::Tiled;
            }
        } else if let Some((w, h)) = self.current_image_res {
            res_w = w;
            res_h = h;
            let threshold = crate::tile_cache::get_tiled_threshold();
            if w as u64 * h as u64 > threshold {
                osd_mode = crate::ui::osd::ImageOsdMode::Tiled;
            }
        }

        if res_w == 0 {
            return None;
        }
        let file_size_bytes = self
            .file_byte_len_by_index
            .get(self.current_index)
            .copied()
            .unwrap_or(0);
        Some(crate::ui::osd::ImageOsdFrame {
            index: self.current_index,
            total: self.image_files.len(),
            zoom_pct,
            res: (res_w, res_h),
            file_size_bytes,
            mode: osd_mode,
        })
    }

    fn current_hdr_color_space(&self) -> Option<HdrColorSpace> {
        if let Some(source) = self
            .current_hdr_tiled_image
            .as_ref()
            .and_then(|current| current.source_for_index(self.current_index))
        {
            return Some(source.color_space());
        }

        self.current_hdr_image
            .as_ref()
            .and_then(|current| current.image_for_index(self.current_index))
            .map(|image| image.color_space)
            .or_else(|| {
                self.hdr_image_cache
                    .get(&self.current_index)
                    .map(|image| image.color_space)
            })
    }
}

fn hdr_render_path_for_render_plan(
    plan: &RenderPlan,
    shape: RenderShape,
    complex_transition_active: bool,
    prefer_sdr_for_pending_gpu_demosaic: bool,
    has_hdr_content: bool,
    prefer_embedded_iso_gain_map_sdr_master: bool,
) -> Option<HdrRenderPath> {
    // While GPU RAW demosaic is pending the canvas draws the cached SDR bootstrap texture.
    // Suppress the HDR supplemental line so OSD does not advertise float-plane / native HDR
    // output while EV and tone-mapping are inert on the baked preview.
    if prefer_sdr_for_pending_gpu_demosaic {
        return None;
    }

    if plan.backend == PlaneBackendKind::Hdr {
        return match shape {
            RenderShape::Tiled => Some(HdrRenderPath::FloatTilePlane),
            RenderShape::Static if !complex_transition_active => {
                Some(HdrRenderPath::FloatImagePlane)
            }
            RenderShape::Static => Some(HdrRenderPath::SdrFallback),
        };
    }

    if has_hdr_content {
        if prefer_embedded_iso_gain_map_sdr_master {
            Some(HdrRenderPath::EmbeddedSdrMaster)
        } else {
            Some(HdrRenderPath::SdrFallback)
        }
    } else {
        None
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Default)]
struct HdrRenderPathViewerPlanInput<'a> {
    tiled_canvas_active: bool,
    has_hdr_tiled_source: bool,
    has_hdr_image: bool,
    has_sdr_fallback: bool,
    hdr_target_format: Option<wgpu::TextureFormat>,
    complex_transition_active: bool,
    monitor_selection: Option<&'a crate::hdr::monitor::HdrMonitorSelection>,
    prefer_sdr_for_pending_gpu_demosaic: bool,
    force_hdr_plane_after_raw_demosaic: bool,
    prefer_embedded_iso_gain_map_sdr_master: bool,
}

#[cfg(test)]
fn hdr_render_path_for_viewer_plan(
    input: HdrRenderPathViewerPlanInput<'_>,
) -> Option<HdrRenderPath> {
    use crate::app::rendering::plan::{RenderPlanStateInput, build_render_plan_for_state};

    let HdrRenderPathViewerPlanInput {
        tiled_canvas_active,
        has_hdr_tiled_source,
        has_hdr_image,
        has_sdr_fallback,
        hdr_target_format,
        complex_transition_active,
        monitor_selection,
        prefer_sdr_for_pending_gpu_demosaic,
        force_hdr_plane_after_raw_demosaic,
        prefer_embedded_iso_gain_map_sdr_master,
    } = input;

    let shape = if tiled_canvas_active {
        RenderShape::Tiled
    } else {
        RenderShape::Static
    };
    let has_hdr_plane = if shape == RenderShape::Tiled {
        has_hdr_tiled_source
    } else {
        has_hdr_image
    };
    let plan = build_render_plan_for_state(RenderPlanStateInput {
        shape,
        has_hdr_plane,
        has_sdr_fallback,
        target_format: hdr_target_format,
        monitor_selection,
        prefer_sdr_for_pending_gpu_demosaic,
        force_hdr_plane_after_raw_demosaic,
        prefer_embedded_iso_gain_map_sdr_master,
    });
    hdr_render_path_for_render_plan(
        &plan,
        shape,
        complex_transition_active,
        prefer_sdr_for_pending_gpu_demosaic,
        has_hdr_image || has_hdr_tiled_source || has_sdr_fallback,
        prefer_embedded_iso_gain_map_sdr_master,
    )
}

#[cfg(test)]
mod tests {
    use super::{HdrRenderPathViewerPlanInput, hdr_render_path_for_viewer_plan};
    use crate::hdr::monitor::HdrMonitorSelection;
    use crate::hdr::status::HdrRenderPath;

    fn hdr_capable_monitor() -> HdrMonitorSelection {
        HdrMonitorSelection {
            hdr_supported: true,
            label: "HDR".to_string(),
            max_luminance_nits: Some(1000.0),
            max_full_frame_luminance_nits: Some(500.0),
            max_hdr_capacity: None,
            hdr_capacity_source: Some("test"),
            native_surface_encoding: Some(
                crate::hdr::monitor::HdrNativeSurfaceEncoding::LinearScRgb,
            ),
            ..HdrMonitorSelection::new("", false)
        }
    }

    #[test]
    fn hdr_tiled_source_reports_tile_plane_before_sdr_fallback() {
        let monitor = hdr_capable_monitor();
        assert_eq!(
            hdr_render_path_for_viewer_plan(HdrRenderPathViewerPlanInput {
                tiled_canvas_active: true,
                has_hdr_tiled_source: true,
                has_sdr_fallback: true,
                hdr_target_format: Some(wgpu::TextureFormat::Rgba16Float),
                monitor_selection: Some(&monitor),
                ..Default::default()
            }),
            Some(HdrRenderPath::FloatTilePlane)
        );
    }

    #[test]
    fn hdr_tiled_source_reports_sdr_fallback_without_hdr_target() {
        assert_eq!(
            hdr_render_path_for_viewer_plan(HdrRenderPathViewerPlanInput {
                tiled_canvas_active: true,
                has_hdr_tiled_source: true,
                has_sdr_fallback: true,
                ..Default::default()
            }),
            Some(HdrRenderPath::SdrFallback)
        );
    }

    #[test]
    fn complex_transition_keeps_full_image_hdr_on_sdr_fallback() {
        let monitor = hdr_capable_monitor();
        assert_eq!(
            hdr_render_path_for_viewer_plan(HdrRenderPathViewerPlanInput {
                has_hdr_image: true,
                hdr_target_format: Some(wgpu::TextureFormat::Rgba16Float),
                complex_transition_active: true,
                monitor_selection: Some(&monitor),
                ..Default::default()
            }),
            Some(HdrRenderPath::SdrFallback)
        );
    }

    #[test]
    fn gpu_raw_demosaic_pending_hides_hdr_supplemental_line() {
        let monitor = hdr_capable_monitor();
        assert_eq!(
            hdr_render_path_for_viewer_plan(HdrRenderPathViewerPlanInput {
                has_hdr_image: true,
                has_sdr_fallback: true,
                hdr_target_format: Some(wgpu::TextureFormat::Rgba16Float),
                monitor_selection: Some(&monitor),
                prefer_sdr_for_pending_gpu_demosaic: true,
                ..Default::default()
            }),
            None
        );
    }

    #[test]
    fn tone_mapped_sdr_surface_matches_render_plan_float_plane_osd() {
        assert_eq!(
            hdr_render_path_for_viewer_plan(HdrRenderPathViewerPlanInput {
                has_hdr_image: true,
                has_sdr_fallback: true,
                hdr_target_format: Some(wgpu::TextureFormat::Bgra8Unorm),
                ..Default::default()
            }),
            Some(HdrRenderPath::FloatImagePlane)
        );
    }

    #[test]
    fn unknown_monitor_capability_uses_float_plane_for_hdr_on_sdr_output() {
        // Unknown probe still forces `SdrToneMapped`, but an HDR float buffer now routes through the
        // WGSL path (not stale CPU bake) so sliders work; OSD must match [`RenderPlan`] backend `Hdr`.
        assert_eq!(
            hdr_render_path_for_viewer_plan(HdrRenderPathViewerPlanInput {
                has_hdr_image: true,
                has_sdr_fallback: true,
                hdr_target_format: Some(wgpu::TextureFormat::Rgba16Float),
                ..Default::default()
            }),
            Some(HdrRenderPath::FloatImagePlane)
        );
    }

    #[test]
    fn embedded_iso_gain_map_sdr_master_reports_embedded_render_path() {
        assert_eq!(
            hdr_render_path_for_viewer_plan(HdrRenderPathViewerPlanInput {
                has_hdr_image: true,
                has_sdr_fallback: true,
                hdr_target_format: Some(wgpu::TextureFormat::Bgra8Unorm),
                prefer_embedded_iso_gain_map_sdr_master: true,
                ..Default::default()
            }),
            Some(HdrRenderPath::EmbeddedSdrMaster)
        );
    }

    #[test]
    fn hdr_render_path_matrix_aligns_with_render_plan() {
        let monitor = hdr_capable_monitor();
        let cases = [
            (
                "static HDR native target",
                false,
                false,
                true,
                true,
                Some(wgpu::TextureFormat::Rgba16Float),
                false,
                Some(&monitor),
                Some(HdrRenderPath::FloatImagePlane),
            ),
            (
                "tiled HDR native target",
                true,
                true,
                false,
                true,
                Some(wgpu::TextureFormat::Rgba16Float),
                false,
                Some(&monitor),
                Some(HdrRenderPath::FloatTilePlane),
            ),
            (
                "static HDR on Bgra SDR framebuffer (tone-mapped WGSL)",
                false,
                false,
                true,
                true,
                Some(wgpu::TextureFormat::Bgra8Unorm),
                false,
                Some(&monitor),
                Some(HdrRenderPath::FloatImagePlane),
            ),
            (
                "tiled HDR without render target",
                true,
                true,
                false,
                true,
                None,
                false,
                Some(&monitor),
                Some(HdrRenderPath::SdrFallback),
            ),
        ];

        for (
            label,
            tiled_canvas_active,
            has_hdr_tiled_source,
            has_hdr_image,
            has_sdr_fallback,
            hdr_target_format,
            complex_transition_active,
            monitor_selection,
            expected,
        ) in cases
        {
            assert_eq!(
                hdr_render_path_for_viewer_plan(HdrRenderPathViewerPlanInput {
                    tiled_canvas_active,
                    has_hdr_tiled_source,
                    has_hdr_image,
                    has_sdr_fallback,
                    hdr_target_format,
                    complex_transition_active,
                    monitor_selection,
                    ..Default::default()
                }),
                expected,
                "{label}"
            );
        }
    }
}
