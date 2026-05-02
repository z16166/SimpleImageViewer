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
    pub(crate) fn new(
        shape: RenderShape,
        has_hdr_plane: bool,
        target_format: Option<wgpu::TextureFormat>,
        output_mode: HdrRenderOutputMode,
    ) -> Self {
        let backend = select_render_backend(has_hdr_plane, target_format.is_some(), output_mode);
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
    target_format: Option<wgpu::TextureFormat>,
    monitor_selection: Option<&HdrMonitorSelection>,
) -> RenderPlan {
    let output_mode =
        crate::hdr::monitor::effective_render_output_mode(target_format, monitor_selection);
    RenderPlan::new(shape, has_hdr_plane, target_format, output_mode)
}

impl ImageViewerApp {
    pub(crate) fn build_render_plan(&self, shape: RenderShape, has_hdr_plane: bool) -> RenderPlan {
        build_render_plan_for_state(
            shape,
            has_hdr_plane,
            self.hdr_target_format,
            self.hdr_monitor_state.selection(),
        )
    }
}

pub(crate) fn select_render_backend(
    has_hdr_plane: bool,
    has_hdr_target: bool,
    output_mode: HdrRenderOutputMode,
) -> PlaneBackendKind {
    if has_hdr_plane && has_hdr_target && output_mode == HdrRenderOutputMode::NativeHdr {
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

        let sdr_plan = super::build_render_plan_for_state(
            super::RenderShape::Static,
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
            Some(wgpu::TextureFormat::Rgba16Float),
            None,
        );
        assert_eq!(hdr_plan.backend, PlaneBackendKind::Hdr);
        assert_eq!(
            hdr_plan.transition_policy,
            super::RenderTransitionPolicy::StaticHdrWithSdrComplexFallback
        );

        let tiled_plan = super::build_render_plan_for_state(
            super::RenderShape::Tiled,
            true,
            Some(wgpu::TextureFormat::Rgba16Float),
            None,
        );
        assert_eq!(tiled_plan.backend, PlaneBackendKind::Hdr);
        assert_eq!(
            tiled_plan.transition_policy,
            super::RenderTransitionPolicy::TiledHdrWithSdrPreviewFallback
        );
    }
}
