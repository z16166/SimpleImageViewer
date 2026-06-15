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

use crate::app::ImageViewerApp;
use crate::app::rendering::plane::PlaneBackendKind;
use crate::hdr::monitor::HdrMonitorSelection;
use crate::hdr::renderer::HdrRenderOutputMode;
use crate::loader::PixelPlaneKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RenderShape {
    Static,
    Tiled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RenderTransitionPolicy {
    SdrOnly,
    StaticHdrWithSdrComplexFallback,
    TiledHdrWithSdrPreviewFallback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RenderPlan {
    pub(crate) shape: RenderShape,
    pub(crate) backend: PlaneBackendKind,
    pub(crate) output_mode: HdrRenderOutputMode,
    pub(crate) target_format: Option<wgpu::TextureFormat>,
    pub(crate) active_plane: PixelPlaneKind,
    pub(crate) transition_policy: RenderTransitionPolicy,
}

impl RenderPlan {
    /// Convenience constructor used by unit tests — defaults `has_sdr_fallback = true` so
    /// existing test matrices still exercise the "cached SDR fast path" branch of
    /// [`select_render_backend`]. Production code uses
    /// [`RenderPlan::new_with_sdr_fallback`] directly.
    #[cfg(test)]
    pub(crate) fn new(
        shape: RenderShape,
        has_hdr_plane: bool,
        target_format: Option<wgpu::TextureFormat>,
        output_mode: HdrRenderOutputMode,
    ) -> Self {
        Self::new_with_sdr_fallback(
            shape,
            has_hdr_plane,
            true,
            target_format,
            output_mode,
            false,
        )
    }

    pub(crate) fn new_with_sdr_fallback(
        shape: RenderShape,
        has_hdr_plane: bool,
        has_sdr_fallback: bool,
        target_format: Option<wgpu::TextureFormat>,
        output_mode: HdrRenderOutputMode,
        prefer_sdr_for_pending_gpu_demosaic: bool,
    ) -> Self {
        let backend = select_render_backend(
            has_hdr_plane,
            has_sdr_fallback,
            target_format.is_some(),
            output_mode,
            prefer_sdr_for_pending_gpu_demosaic,
        );
        let active_plane = match backend {
            PlaneBackendKind::Sdr => PixelPlaneKind::Sdr,
            PlaneBackendKind::Hdr => PixelPlaneKind::Hdr,
        };
        let transition_policy = transition_policy_for(shape, backend);

        Self {
            shape,
            backend,
            output_mode,
            target_format,
            active_plane,
            transition_policy,
        }
    }
}

pub(crate) fn build_render_plan_for_state(
    shape: RenderShape,
    has_hdr_plane: bool,
    has_sdr_fallback: bool,
    target_format: Option<wgpu::TextureFormat>,
    monitor_selection: Option<&HdrMonitorSelection>,
    prefer_sdr_for_pending_gpu_demosaic: bool,
) -> RenderPlan {
    let output_mode =
        crate::hdr::monitor::effective_render_output_mode(target_format, monitor_selection);
    RenderPlan::new_with_sdr_fallback(
        shape,
        has_hdr_plane,
        has_sdr_fallback,
        target_format,
        output_mode,
        prefer_sdr_for_pending_gpu_demosaic,
    )
}

impl ImageViewerApp {
    pub(crate) fn build_render_plan(
        &self,
        shape: RenderShape,
        has_hdr_plane: bool,
        has_sdr_fallback: bool,
    ) -> RenderPlan {
        let prefer_sdr_for_pending_gpu_demosaic = shape == RenderShape::Static
            && crate::app::image_management::prefer_sdr_bootstrap_while_raw_gpu_demosaic_pending(
                self.current_index,
                &self.hdr_raw_gpu_demosaic_pending_indices,
                &self.hdr_image_cache,
                has_sdr_fallback,
                self.texture_cache.contains(self.current_index),
            );
        if prefer_sdr_for_pending_gpu_demosaic {
            crate::preload_debug!(
                "[PreloadDebug][RAW-GPU] render backend=Sdr bootstrap cur={} pending=true",
                self.current_index
            );
        }
        build_render_plan_for_state(
            shape,
            has_hdr_plane,
            has_sdr_fallback,
            self.hdr_target_format,
            self.effective_hdr_monitor_selection().as_ref(),
            prefer_sdr_for_pending_gpu_demosaic,
        )
    }
}

/// Picks [`PlaneBackendKind`] given what content and display capabilities are available.
///
/// Normally native scRGB HDR output rides the `Hdr` backend and everything else rides the
/// cached `Sdr` texture fast path. Additional cases that upgrade to the **`Hdr`** backend so
/// the WGSL viewer path runs (live exposure / nits, `encode_native_hdr` vs `encode_sdr`):
///
/// 1. **`has_hdr_plane && !has_sdr_fallback`** — HDR plane exists but CPU SDR fallback is
///    missing (`Flowers.exr`-style tiling on SDR; otherwise blank until tiles arrive).
/// 2. **`has_hdr_plane && has_hdr_target && output_mode == SdrToneMapped`** — an HDR float
///    buffer is decoded but [`HdrRenderOutputMode::SdrToneMapped`] means we composite into an
///    SDR swap chain: the cached SDR texture is baked from CPU tone-map settings at load time
///    and **`set_hdr_tone_map_settings` does not re-upload it**. Routing through the HDR plane
///    keeps sliders / EV responsive on ordinary monitors instead of silently no-op-ing.
///
/// Ordinary 8‑bit albums stay on `PlaneBackendKind::Sdr` because `has_hdr_plane` is false.
pub(crate) fn select_render_backend(
    has_hdr_plane: bool,
    has_sdr_fallback: bool,
    has_hdr_target: bool,
    output_mode: HdrRenderOutputMode,
    prefer_sdr_for_pending_gpu_demosaic: bool,
) -> PlaneBackendKind {
    if prefer_sdr_for_pending_gpu_demosaic {
        return PlaneBackendKind::Sdr;
    }
    if has_hdr_plane && has_hdr_target && output_mode.is_native_hdr() {
        PlaneBackendKind::Hdr
    } else if has_hdr_plane && !has_sdr_fallback {
        PlaneBackendKind::Hdr
    } else if has_hdr_plane && has_hdr_target && output_mode == HdrRenderOutputMode::SdrToneMapped {
        PlaneBackendKind::Hdr
    } else {
        PlaneBackendKind::Sdr
    }
}

fn transition_policy_for(shape: RenderShape, backend: PlaneBackendKind) -> RenderTransitionPolicy {
    match (shape, backend) {
        (_, PlaneBackendKind::Sdr) => RenderTransitionPolicy::SdrOnly,
        (RenderShape::Static, PlaneBackendKind::Hdr) => {
            RenderTransitionPolicy::StaticHdrWithSdrComplexFallback
        }
        (RenderShape::Tiled, PlaneBackendKind::Hdr) => {
            RenderTransitionPolicy::TiledHdrWithSdrPreviewFallback
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::app::rendering::plane::PlaneBackendKind;
    use crate::hdr::renderer::HdrRenderOutputMode;

    #[test]
    fn render_plan_selects_hdr_backend_only_for_native_hdr_with_target_and_plane() {
        let plan = super::RenderPlan::new(
            super::RenderShape::Static,
            true,
            Some(wgpu::TextureFormat::Rgba16Float),
            HdrRenderOutputMode::NativeHdr,
        );

        assert_eq!(plan.shape, super::RenderShape::Static);
        assert_eq!(plan.backend, PlaneBackendKind::Hdr);
        assert_eq!(plan.active_plane, crate::loader::PixelPlaneKind::Hdr);
        assert_eq!(plan.output_mode, HdrRenderOutputMode::NativeHdr);
        assert_eq!(plan.target_format, Some(wgpu::TextureFormat::Rgba16Float));
    }

    #[test]
    fn render_plan_falls_back_to_sdr_without_hdr_plane_target_or_output() {
        for (has_hdr_plane, target_format, output_mode) in [
            (
                false,
                Some(wgpu::TextureFormat::Rgba16Float),
                HdrRenderOutputMode::NativeHdr,
            ),
            (true, None, HdrRenderOutputMode::NativeHdr),
        ] {
            let plan = super::RenderPlan::new(
                super::RenderShape::Tiled,
                has_hdr_plane,
                target_format,
                output_mode,
            );

            assert_eq!(plan.shape, super::RenderShape::Tiled);
            assert_eq!(plan.backend, PlaneBackendKind::Sdr);
            assert_eq!(plan.active_plane, crate::loader::PixelPlaneKind::Sdr);
        }
    }

    /// [`HdrRenderOutputMode::SdrToneMapped`] runs `encode_sdr` in WGSL; use [`PlaneBackendKind::Hdr`]
    /// when a float HDR plane exists so exposure / sliders are not masked by stale CPU cache.
    #[test]
    fn render_plan_promotes_hdr_backend_for_tone_mapped_when_float_plane_targets_surface() {
        let plan = super::RenderPlan::new(
            super::RenderShape::Tiled,
            true,
            Some(wgpu::TextureFormat::Rgba16Float),
            HdrRenderOutputMode::SdrToneMapped,
        );
        assert_eq!(plan.backend, PlaneBackendKind::Hdr);
        assert_eq!(plan.active_plane, crate::loader::PixelPlaneKind::Hdr);
    }

    /// Correctness safety net for the `Flowers.exr` (LuminanceChroma EXR) bug: when the
    /// content has an HDR plane but NO SDR fallback texture (e.g. HDR-only tiled source on
    /// an SDR display, or the transient window between a monitor HDR-off toggle and the
    /// next preview refinement), `select_render_backend` must route through the HDR plane
    /// shader's `SdrToneMapped` path rather than leave the canvas blank.
    #[test]
    fn render_plan_upgrades_to_hdr_backend_when_sdr_fallback_is_missing() {
        let plan = super::RenderPlan::new_with_sdr_fallback(
            super::RenderShape::Tiled,
            /* has_hdr_plane */ true,
            /* has_sdr_fallback */ false,
            Some(wgpu::TextureFormat::Bgra8Unorm),
            HdrRenderOutputMode::SdrToneMapped,
            false,
        );
        assert_eq!(plan.backend, PlaneBackendKind::Hdr);
        assert_eq!(plan.active_plane, crate::loader::PixelPlaneKind::Hdr);
        assert_eq!(plan.output_mode, HdrRenderOutputMode::SdrToneMapped);

        // Without an HDR plane the fallback cannot engage — we stay on the SDR backend and
        // accept a blank canvas instead of trying to shader-tone-map a non-existent image.
        let plan_no_hdr = super::RenderPlan::new_with_sdr_fallback(
            super::RenderShape::Tiled,
            /* has_hdr_plane */ false,
            /* has_sdr_fallback */ false,
            Some(wgpu::TextureFormat::Bgra8Unorm),
            HdrRenderOutputMode::SdrToneMapped,
            false,
        );
        assert_eq!(plan_no_hdr.backend, PlaneBackendKind::Sdr);

        // With an HDR float plane + SDR output, bake-time CPU cache would ignore slider changes;
        // [`select_render_backend`] upgrades to WGSL tone-map so exposure stays live.
        let plan_default = super::RenderPlan::new(
            super::RenderShape::Tiled,
            true,
            Some(wgpu::TextureFormat::Bgra8Unorm),
            HdrRenderOutputMode::SdrToneMapped,
        );
        assert_eq!(plan_default.backend, PlaneBackendKind::Hdr);
        assert_eq!(
            plan_default.active_plane,
            crate::loader::PixelPlaneKind::Hdr
        );
    }

    #[test]
    fn render_plan_builder_uses_monitor_output_policy_and_transition_policy() {
        let non_hdr_monitor = crate::hdr::monitor::HdrMonitorSelection {
            hdr_supported: false,
            label: "SDR monitor".to_string(),
            max_luminance_nits: None,
            max_full_frame_luminance_nits: None,
            max_hdr_capacity: None,
            hdr_capacity_source: None,
            native_surface_encoding: None,
        };
        let hdr_monitor = crate::hdr::monitor::HdrMonitorSelection {
            hdr_supported: true,
            label: "HDR monitor".to_string(),
            max_luminance_nits: Some(1000.0),
            max_full_frame_luminance_nits: Some(500.0),
            max_hdr_capacity: None,
            hdr_capacity_source: Some("test"),
            native_surface_encoding: Some(
                crate::hdr::monitor::HdrNativeSurfaceEncoding::LinearScRgb,
            ),
        };

        let sdr_plan = super::build_render_plan_for_state(
            super::RenderShape::Static,
            true,
            true,
            Some(wgpu::TextureFormat::Rgba16Float),
            Some(&non_hdr_monitor),
            false,
        );
        assert_eq!(sdr_plan.backend, PlaneBackendKind::Hdr);
        assert_eq!(
            sdr_plan.transition_policy,
            super::RenderTransitionPolicy::StaticHdrWithSdrComplexFallback
        );

        let hdr_plan = super::build_render_plan_for_state(
            super::RenderShape::Static,
            true,
            true,
            Some(wgpu::TextureFormat::Rgba16Float),
            Some(&hdr_monitor),
            false,
        );
        assert_eq!(hdr_plan.backend, PlaneBackendKind::Hdr);
        assert_eq!(
            hdr_plan.transition_policy,
            super::RenderTransitionPolicy::StaticHdrWithSdrComplexFallback
        );

        let tiled_plan = super::build_render_plan_for_state(
            super::RenderShape::Tiled,
            true,
            true,
            Some(wgpu::TextureFormat::Rgba16Float),
            Some(&hdr_monitor),
            false,
        );
        assert_eq!(tiled_plan.backend, PlaneBackendKind::Hdr);
        assert_eq!(
            tiled_plan.transition_policy,
            super::RenderTransitionPolicy::TiledHdrWithSdrPreviewFallback
        );

        // Unknown probe: conservative `effective_render_output_mode` is `SdrToneMapped`, so HDR
        // sources still composite through WGSL tone-map (`PlaneBackendKind::Hdr`) rather than
        // native HDR — not the naive "SDR wallpaper" GPU path (`PlaneBackendKind::Sdr`).
        let unknown_monitor_plan = super::build_render_plan_for_state(
            super::RenderShape::Static,
            true,
            true,
            Some(wgpu::TextureFormat::Rgba16Float),
            None,
            false,
        );
        assert_eq!(unknown_monitor_plan.backend, PlaneBackendKind::Hdr);
        assert_eq!(
            unknown_monitor_plan.transition_policy,
            super::RenderTransitionPolicy::StaticHdrWithSdrComplexFallback
        );
    }

    #[test]
    fn render_plan_prefers_sdr_while_gpu_raw_demosaic_is_pending() {
        let plan = super::RenderPlan::new_with_sdr_fallback(
            super::RenderShape::Static,
            true,
            true,
            Some(wgpu::TextureFormat::Rgba16Float),
            HdrRenderOutputMode::NativeHdr,
            true,
        );
        assert_eq!(plan.backend, PlaneBackendKind::Sdr);
        assert_eq!(plan.active_plane, crate::loader::PixelPlaneKind::Sdr);
    }
}
