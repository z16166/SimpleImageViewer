use eframe::egui::{self, Color32, Rect, TextureId};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaneBackendKind {
    Sdr,
    Hdr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaneDrawSourceKind {
    SdrTexture,
    HdrImage,
    HdrTile,
}

impl PlaneDrawSourceKind {
    pub(crate) fn backend(self) -> PlaneBackendKind {
        match self {
            Self::SdrTexture => PlaneBackendKind::Sdr,
            Self::HdrImage | Self::HdrTile => PlaneBackendKind::Hdr,
        }
    }
}

pub(crate) enum PlaneDrawSource {
    SdrTexture {
        texture_id: TextureId,
        color: Color32,
    },
    HdrImage {
        image: Arc<crate::hdr::types::HdrImageBuffer>,
        tone_map: crate::hdr::types::HdrToneMapSettings,
        target_format: wgpu::TextureFormat,
        output_mode: crate::hdr::renderer::HdrRenderOutputMode,
        rotation_steps: u32,
        alpha: f32,
    },
    HdrTile {
        tile: Arc<crate::hdr::tiled::HdrTileBuffer>,
        tone_map: crate::hdr::types::HdrToneMapSettings,
        target_format: wgpu::TextureFormat,
        output_mode: crate::hdr::renderer::HdrRenderOutputMode,
        rotation_steps: u32,
        alpha: f32,
    },
}

impl PlaneDrawSource {
    pub(crate) fn kind(&self) -> PlaneDrawSourceKind {
        match self {
            Self::SdrTexture { .. } => PlaneDrawSourceKind::SdrTexture,
            Self::HdrImage { .. } => PlaneDrawSourceKind::HdrImage,
            Self::HdrTile { .. } => PlaneDrawSourceKind::HdrTile,
        }
    }
}

pub(crate) fn draw_plane(
    ui: &egui::Ui,
    clip_rect: Rect,
    rect: Rect,
    uv: Rect,
    layout: &crate::app::rendering::geometry::PlaneLayout,
    source: PlaneDrawSource,
) {
    let _backend = source.kind().backend();
    match source {
        PlaneDrawSource::SdrTexture { texture_id, color } => {
            draw_sdr_texture_plane(ui, clip_rect, texture_id, rect, uv, color, layout);
        }
        PlaneDrawSource::HdrImage {
            image,
            tone_map,
            target_format,
            output_mode,
            rotation_steps,
            alpha,
        } => {
            let Some((clipped_rect, uv_rect)) = clipped_plane_rect_and_uv(rect, clip_rect) else {
                return;
            };
            ui.painter()
                .add(crate::hdr::renderer::hdr_image_plane_callback_with_uv(
                    clipped_rect,
                    image,
                    tone_map,
                    target_format,
                    output_mode,
                    rotation_steps,
                    alpha,
                    uv_subrect(uv, uv_rect),
                ));
        }
        PlaneDrawSource::HdrTile {
            tile,
            tone_map,
            target_format,
            output_mode,
            rotation_steps,
            alpha,
        } => {
            let Some((clipped_rect, uv_rect)) = clipped_plane_rect_and_uv(rect, clip_rect) else {
                return;
            };
            ui.painter()
                .add(crate::hdr::renderer::hdr_tile_plane_callback_with_uv(
                    clipped_rect,
                    tile,
                    tone_map,
                    target_format,
                    output_mode,
                    rotation_steps,
                    alpha,
                    uv_subrect(uv, uv_rect),
                ));
        }
    }
}

fn uv_subrect(base: Rect, clipped: Rect) -> Rect {
    let width = base.width();
    let height = base.height();
    Rect::from_min_max(
        egui::pos2(
            base.min.x + clipped.min.x * width,
            base.min.y + clipped.min.y * height,
        ),
        egui::pos2(
            base.min.x + clipped.max.x * width,
            base.min.y + clipped.max.y * height,
        ),
    )
}

pub(crate) fn sdr_texture_mesh(
    texture_id: TextureId,
    rect: Rect,
    uv: Rect,
    color: Color32,
    layout: &crate::app::rendering::geometry::PlaneLayout,
) -> egui::Mesh {
    let mut mesh = egui::Mesh::with_texture(texture_id);
    mesh.add_rect_with_uv(rect, uv, color);
    if layout.rotation_steps != 0 {
        let rot = egui::emath::Rot2::from_angle(layout.angle);
        for vertex in &mut mesh.vertices {
            vertex.pos = layout.pivot + rot * (vertex.pos - layout.pivot);
        }
    }
    mesh
}

pub(crate) fn draw_sdr_texture_plane(
    ui: &egui::Ui,
    clip_rect: Rect,
    texture_id: TextureId,
    rect: Rect,
    uv: Rect,
    color: Color32,
    layout: &crate::app::rendering::geometry::PlaneLayout,
) {
    ui.painter()
        .with_clip_rect(clip_rect)
        .add(egui::Shape::mesh(sdr_texture_mesh(
            texture_id, rect, uv, color, layout,
        )));
}

pub(crate) fn hdr_image_plane_rect(layout: &crate::app::rendering::geometry::PlaneLayout) -> Rect {
    layout.dest
}

pub(crate) fn clipped_plane_rect_and_uv(rect: Rect, clip_rect: Rect) -> Option<(Rect, Rect)> {
    let clipped = rect.intersect(clip_rect);
    if clipped.width() <= 0.0 || clipped.height() <= 0.0 {
        return None;
    }

    let uv_min_x = ((clipped.min.x - rect.min.x) / rect.width()).clamp(0.0, 1.0);
    let uv_max_x = ((clipped.max.x - rect.min.x) / rect.width()).clamp(0.0, 1.0);
    let uv_min_y = ((clipped.min.y - rect.min.y) / rect.height()).clamp(0.0, 1.0);
    let uv_max_y = ((clipped.max.y - rect.min.y) / rect.height()).clamp(0.0, 1.0);

    Some((
        clipped,
        Rect::from_min_max(
            egui::pos2(uv_min_x, uv_min_y),
            egui::pos2(uv_max_x, uv_max_y),
        ),
    ))
}

#[cfg(test)]
mod tests {
    use crate::app::rendering::plan::{RenderPlan, RenderShape};
    use crate::hdr::renderer::HdrRenderOutputMode;

    #[test]
    fn render_plan_selects_tiled_hdr_only_for_native_hdr_sources() {
        assert_eq!(
            RenderPlan::new(
                RenderShape::Tiled,
                true,
                Some(wgpu::TextureFormat::Rgba16Float),
                HdrRenderOutputMode::NativeHdr
            )
            .backend,
            super::PlaneBackendKind::Hdr
        );
        assert_eq!(
            RenderPlan::new(
                RenderShape::Tiled,
                true,
                Some(wgpu::TextureFormat::Rgba16Float),
                HdrRenderOutputMode::SdrToneMapped
            )
            .backend,
            super::PlaneBackendKind::Sdr
        );
        assert_eq!(
            RenderPlan::new(
                RenderShape::Tiled,
                false,
                Some(wgpu::TextureFormat::Rgba16Float),
                HdrRenderOutputMode::NativeHdr
            )
            .backend,
            super::PlaneBackendKind::Sdr
        );
    }

    #[test]
    fn sdr_texture_mesh_uses_shared_plane_rotation() {
        let layout = crate::app::rendering::geometry::PlaneLayout::from_dest(
            eframe::egui::vec2(10.0, 20.0),
            1,
            eframe::egui::Rect::from_min_size(
                eframe::egui::pos2(0.0, 0.0),
                eframe::egui::vec2(20.0, 10.0),
            ),
        );
        let mesh = super::sdr_texture_mesh(
            eframe::egui::TextureId::User(1),
            layout.unrotated_dest,
            eframe::egui::Rect::from_min_max(
                eframe::egui::Pos2::ZERO,
                eframe::egui::pos2(1.0, 1.0),
            ),
            eframe::egui::Color32::WHITE,
            &layout,
        );

        assert_eq!(mesh.texture_id, eframe::egui::TextureId::User(1));
        assert_eq!(mesh.vertices.len(), 4);
        assert!((mesh.vertices[0].pos.x - 20.0).abs() < 0.001);
        assert!(mesh.vertices[0].pos.y.abs() < 0.001);
    }

    #[test]
    fn hdr_image_plane_uses_rotated_display_rect() {
        let layout = crate::app::rendering::geometry::PlaneLayout::from_dest(
            eframe::egui::vec2(400.0, 200.0),
            1,
            eframe::egui::Rect::from_min_size(
                eframe::egui::pos2(10.0, 20.0),
                eframe::egui::vec2(100.0, 200.0),
            ),
        );

        assert_eq!(super::hdr_image_plane_rect(&layout), layout.dest);
        assert_ne!(super::hdr_image_plane_rect(&layout), layout.unrotated_dest);
    }

    #[test]
    fn clipped_plane_rect_preserves_uv_subrect() {
        let rect = eframe::egui::Rect::from_min_max(
            eframe::egui::pos2(-100.0, -50.0),
            eframe::egui::pos2(100.0, 150.0),
        );
        let clip = eframe::egui::Rect::from_min_max(
            eframe::egui::pos2(0.0, 0.0),
            eframe::egui::pos2(50.0, 100.0),
        );

        let (clipped, uv) = super::clipped_plane_rect_and_uv(rect, clip).unwrap();

        assert_eq!(clipped, clip);
        assert_eq!(
            uv,
            eframe::egui::Rect::from_min_max(
                eframe::egui::pos2(0.5, 0.25),
                eframe::egui::pos2(0.75, 0.75)
            )
        );
    }

    #[test]
    fn plane_draw_source_reports_backend_for_shared_dispatch() {
        assert_eq!(
            super::PlaneDrawSourceKind::SdrTexture.backend(),
            super::PlaneBackendKind::Sdr
        );
        assert_eq!(
            super::PlaneDrawSourceKind::HdrImage.backend(),
            super::PlaneBackendKind::Hdr
        );
        assert_eq!(
            super::PlaneDrawSourceKind::HdrTile.backend(),
            super::PlaneBackendKind::Hdr
        );
    }
}
