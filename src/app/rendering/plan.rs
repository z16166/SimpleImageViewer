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
        Self::new_with_sdr_fallback(shape, has_hdr_plane, true, target_format, output_mode)
    }

    pub(crate) fn new_with_sdr_fallback(
        shape: RenderShape,
        has_hdr_plane: bool,
        has_sdr_fallback: bool,
        target_format: Option<wgpu::TextureFormat>,
        output_mode: HdrRenderOutputMode,
    ) -> Self {
        let backend = select_render_backend(
            has_hdr_plane,
            has_sdr_fallback,
            target_format.is_some(),
            output_mode,
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
) -> RenderPlan {
    let output_mode =
        crate::hdr::monitor::effective_render_output_mode(target_format, monitor_selection);
    RenderPlan::new_with_sdr_fallback(
        shape,
        has_hdr_plane,
        has_sdr_fallback,
        target_format,
        output_mode,
    )
}

impl ImageViewerApp {
    pub(crate) fn build_render_plan(
        &self,
        shape: RenderShape,
        has_hdr_plane: bool,
        has_sdr_fallback: bool,
    ) -> RenderPlan {
        build_render_plan_for_state(
            shape,
            has_hdr_plane,
            has_sdr_fallback,
            self.hdr_target_format,
            self.hdr_monitor_state.selection(),
        )
    }
}

/// Picks [`PlaneBackendKind`] given what content and display capabilities are available.
///
/// Normally native scRGB HDR output rides the `Hdr` backend and everything else rides the
/// cached `Sdr` texture fast path. The third branch — **`has_hdr_plane &&
/// !has_sdr_fallback`** — upgrades the SDR-tone-mapped case to the `Hdr` backend so the HDR
/// plane shader's `SdrToneMapped` output mode can stand in for the missing SDR texture.
/// This covers HDR-only tiled sources (e.g. subsampled / luminance-chroma EXR such as
/// `Flowers.exr`) on SDR panels, where otherwise `tile_manager.preview_texture` and
/// `texture_cache` would both be empty and the draw surface would render blank until SDR
/// tiles eventually arrive.
pub(crate) fn select_render_backend(
    has_hdr_plane: bool,
    has_sdr_fallback: bool,
    has_hdr_target: bool,
    output_mode: HdrRenderOutputMode,
) -> PlaneBackendKind {
    if has_hdr_plane && has_hdr_target && output_mode == HdrRenderOutputMode::NativeHdr {
        PlaneBackendKind::Hdr
    } else if has_hdr_plane && !has_sdr_fallback {
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
            (
                true,
                Some(wgpu::TextureFormat::Rgba16Float),
                HdrRenderOutputMode::SdrToneMapped,
            ),
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
        );
        assert_eq!(plan_no_hdr.backend, PlaneBackendKind::Sdr);

        // Default builder still defaults `has_sdr_fallback = true` so ordinary SDR image
        // content keeps the cached-texture fast path.
        let plan_default = super::RenderPlan::new(
            super::RenderShape::Tiled,
            true,
            Some(wgpu::TextureFormat::Bgra8Unorm),
            HdrRenderOutputMode::SdrToneMapped,
        );
        assert_eq!(plan_default.backend, PlaneBackendKind::Sdr);
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
        };
        let hdr_monitor = crate::hdr::monitor::HdrMonitorSelection {
            hdr_supported: true,
            label: "HDR monitor".to_string(),
            max_luminance_nits: Some(1000.0),
            max_full_frame_luminance_nits: Some(500.0),
            max_hdr_capacity: None,
            hdr_capacity_source: Some("test"),
        };

        let sdr_plan = super::build_render_plan_for_state(
            super::RenderShape::Static,
            true,
            true,
            Some(wgpu::TextureFormat::Rgba16Float),
            Some(&non_hdr_monitor),
        );
        assert_eq!(sdr_plan.backend, PlaneBackendKind::Sdr);
        assert_eq!(
            sdr_plan.transition_policy,
            super::RenderTransitionPolicy::SdrOnly
        );

        let hdr_plan = super::build_render_plan_for_state(
            super::RenderShape::Static,
            true,
            true,
            Some(wgpu::TextureFormat::Rgba16Float),
            Some(&hdr_monitor),
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
        );
        assert_eq!(tiled_plan.backend, PlaneBackendKind::Hdr);
        assert_eq!(
            tiled_plan.transition_policy,
            super::RenderTransitionPolicy::TiledHdrWithSdrPreviewFallback
        );

        // Defense-in-depth: when the monitor capability hasn't been probed yet (e.g. the
        // OS-side enumeration silently failed because the egui main window title was
        // localized), default to the SDR plane rather than optimistically routing through
        // the scRGB native HDR pipeline on a possibly SDR-only display.
        let unknown_monitor_plan = super::build_render_plan_for_state(
            super::RenderShape::Static,
            true,
            true,
            Some(wgpu::TextureFormat::Rgba16Float),
            None,
        );
        assert_eq!(unknown_monitor_plan.backend, PlaneBackendKind::Sdr);
        assert_eq!(
            unknown_monitor_plan.transition_policy,
            super::RenderTransitionPolicy::SdrOnly
        );
    }
}
