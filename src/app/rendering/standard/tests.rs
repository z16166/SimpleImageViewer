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

#[cfg(test)]
mod tests {
    use super::helpers::{
        curtain_hdr_transition_rotation, pending_navigation_hold_params,
        resolve_transition_prev_layout, should_clear_transition_state_after_static_hdr_draw,
    };
    use super::{
        should_dispatch_standard_draw, should_draw_pending_navigation_hold_frame,
        should_draw_static_hdr_immediately, should_route_through_hdr_plane,
    };
    use crate::app::rendering::plan::{RenderPlan, RenderShape};
    use crate::app::rendering::plane::PlaneBackendKind;
    use crate::app::TransitionStyle;
    use crate::hdr::types::HdrRenderOutputMode;
    use eframe::egui::{Pos2, Rect, Vec2};

    fn static_plan(
        has_hdr_plane: bool,
        target: Option<wgpu::TextureFormat>,
        output_mode: HdrRenderOutputMode,
    ) -> RenderPlan {
        RenderPlan::new(RenderShape::Static, has_hdr_plane, target, output_mode)
    }

    #[test]
    fn standard_dispatch_allows_hdr_plane_without_sdr_texture() {
        assert!(should_dispatch_standard_draw(true, false, false));
        assert!(!should_dispatch_standard_draw(true, false, true));
        assert!(should_dispatch_standard_draw(false, true, true));
        assert!(!should_dispatch_standard_draw(false, false, false));
    }

    #[test]
    fn pending_navigation_hold_draws_previous_frame_opaque() {
        let params = pending_navigation_hold_params();
        assert_eq!(params.prev_alpha, 1.0);
        assert_eq!(params.prev_scale, 1.0);
        assert_eq!(params.prev_offset, Vec2::ZERO);
    }

    #[test]
    fn hdr_plane_routing_uses_shader_for_sdr_tone_mapped_when_float_plane_exists() {
        // When [`HdrRenderOutputMode::SdrToneMapped`] (SDR framebuffer or conservative probe),
        // the HDR float buffer must still flow through WGSL tone-map (`PlaneBackendKind::Hdr`)
        // so sliders / exposure update every frame instead of staring at stale CPU‑baked SDR textures.
        let tone_mapped_plan = static_plan(
            true,
            Some(wgpu::TextureFormat::Rgba16Float),
            HdrRenderOutputMode::SdrToneMapped,
        );
        assert_eq!(tone_mapped_plan.backend, PlaneBackendKind::Hdr);
        assert!(
            should_route_through_hdr_plane(&tone_mapped_plan),
            "`SdrToneMapped` must not mask the HDR plane shader when HDR float data exists"
        );

        let hdr_plan = static_plan(
            true,
            Some(wgpu::TextureFormat::Rgba16Float),
            HdrRenderOutputMode::NativeHdr,
        );
        assert_eq!(hdr_plan.backend, PlaneBackendKind::Hdr);
        assert!(
            should_route_through_hdr_plane(&hdr_plan),
            "Hdr backend must continue to stream the float buffer through the plane shader"
        );
    }

    #[test]
    fn hdr_plane_routing_uses_shader_for_ripple_animation_on_hdr_backend() {
        let hdr_plan = static_plan(
            true,
            Some(wgpu::TextureFormat::Rgba16Float),
            HdrRenderOutputMode::NativeHdr,
        );
        assert!(should_route_through_hdr_plane(&hdr_plan));
        assert!(should_route_through_hdr_plane(&hdr_plan));
    }

    #[test]
    fn native_static_hdr_draws_immediately_without_sdr_transition_phase() {
        assert!(should_draw_static_hdr_immediately(
            &static_plan(
                true,
                Some(wgpu::TextureFormat::Rgba16Float),
                HdrRenderOutputMode::NativeHdr
            ),
            TransitionStyle::None,
            false
        ));
        assert!(should_draw_static_hdr_immediately(
            &static_plan(
                true,
                Some(wgpu::TextureFormat::Rgba16Float),
                HdrRenderOutputMode::SdrToneMapped
            ),
            TransitionStyle::None,
            false
        ));
        assert!(!should_draw_static_hdr_immediately(
            &static_plan(
                false,
                Some(wgpu::TextureFormat::Rgba16Float),
                HdrRenderOutputMode::NativeHdr
            ),
            TransitionStyle::None,
            false
        ));
        assert!(!should_draw_static_hdr_immediately(
            &static_plan(
                true,
                Some(wgpu::TextureFormat::Rgba16Float),
                HdrRenderOutputMode::NativeHdr
            ),
            TransitionStyle::Curtain,
            true
        ));
        assert!(!should_draw_static_hdr_immediately(
            &static_plan(
                true,
                Some(wgpu::TextureFormat::Rgba16Float),
                HdrRenderOutputMode::NativeHdr
            ),
            TransitionStyle::Fade,
            true
        ));
        assert!(!should_draw_static_hdr_immediately(
            &static_plan(
                true,
                Some(wgpu::TextureFormat::Rgba16Float),
                HdrRenderOutputMode::NativeHdr
            ),
            TransitionStyle::Slide,
            true
        ));
        assert!(!should_draw_static_hdr_immediately(
            &static_plan(
                true,
                Some(wgpu::TextureFormat::Rgba16Float),
                HdrRenderOutputMode::NativeHdr
            ),
            TransitionStyle::Push,
            true
        ));
        assert!(!should_draw_static_hdr_immediately(
            &static_plan(
                true,
                Some(wgpu::TextureFormat::Rgba16Float),
                HdrRenderOutputMode::NativeHdr
            ),
            TransitionStyle::Ripple,
            true
        ));
    }

    #[test]
    fn hdr_curtain_transition_uses_image_rotation() {
        assert_eq!(curtain_hdr_transition_rotation(3), 3);
    }

    #[test]
    fn transition_prev_layout_uses_captured_outgoing_rect_for_tiled_previews() {
        let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(1024.0, 512.0));
        let wide_new_dest = Rect::from_center_size(screen.center(), Vec2::new(1024.0, 96.0));
        let captured_tall_old_dest =
            Rect::from_center_size(screen.center(), Vec2::new(48.0, 512.0));

        let (prev_dest, _, has_prev) = resolve_transition_prev_layout(
            screen,
            wide_new_dest,
            Some(Vec2::new(512.0, 48.0)),
            Some(captured_tall_old_dest),
            true,
            |size, rect| Rect::from_center_size(rect.center(), size),
        );

        assert!(has_prev);
        assert_eq!(prev_dest, captured_tall_old_dest);
        assert!(prev_dest.height() > prev_dest.width());
    }

    #[test]
    fn pending_navigation_hold_frame_waits_for_target_without_transition_animation() {
        assert!(should_draw_pending_navigation_hold_frame(
            None,
            Some(3),
            3,
            true
        ));
        assert!(!should_draw_pending_navigation_hold_frame(
            Some(std::time::Instant::now()),
            Some(3),
            3,
            true
        ));
        assert!(!should_draw_pending_navigation_hold_frame(
            None,
            Some(4),
            3,
            true
        ));
        assert!(!should_draw_pending_navigation_hold_frame(
            None,
            Some(3),
            3,
            false
        ));
    }

    #[test]
    fn pending_transition_keeps_previous_frame_state_on_static_hdr_draw() {
        assert!(!should_clear_transition_state_after_static_hdr_draw(
            true,
            Some(7),
            7
        ));
        assert!(should_clear_transition_state_after_static_hdr_draw(
            true,
            Some(8),
            7
        ));
        assert!(should_clear_transition_state_after_static_hdr_draw(
            true, None, 7
        ));
        assert!(!should_clear_transition_state_after_static_hdr_draw(
            false, None, 7
        ));
    }
}