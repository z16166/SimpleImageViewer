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
use crate::app::ScaleMode;
use crate::app::directory_tree::DIRECTORY_TREE_EMBEDDED_SIDE_PANEL_ID;
use eframe::egui::{self, Context, Pos2, Rect, Vec2};

fn clamp_non_empty_canvas_rect(rect: Rect) -> Rect {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        Rect::NOTHING
    } else {
        rect
    }
}

/// Last frame's [`egui::PanelState::rect`] for the embedded navigation side panel, if any.
pub(crate) fn embedded_directory_tree_panel_rect(ctx: &Context) -> Option<Rect> {
    egui::PanelState::load(ctx, egui::Id::new(DIRECTORY_TREE_EMBEDDED_SIDE_PANEL_ID))
        .map(|state| state.rect)
}

/// Paint vs pointer rects for the main image canvas when an embedded nav panel is present.
///
/// ROOT `Ui` uses top-down layout, so `Panel::left().show_inside` advances the cursor but does
/// not shrink `available_rect_before_wrap` horizontally. Without this inset the canvas
/// `allocate_rect(click_and_drag)` covers the whole nav panel and blocks splitters/resizers.
pub(crate) fn main_window_canvas_rects(
    available: Rect,
    resize_grab_radius_side: f32,
    embedded_panel: Option<Rect>,
) -> (Rect, Rect) {
    let mut paint_rect = available;
    let mut interact_rect = available;
    if let Some(panel) = embedded_panel {
        paint_rect.min.x = paint_rect.min.x.max(panel.max.x);
        interact_rect.min.x = interact_rect
            .min
            .x
            .max(panel.max.x + resize_grab_radius_side);
    }
    (
        clamp_non_empty_canvas_rect(paint_rect),
        clamp_non_empty_canvas_rect(interact_rect),
    )
}

/// Image paint area within the main window (excludes embedded directory-tree side panel).
pub(crate) fn main_window_canvas_paint_rect(available: Rect, embedded_panel: Option<Rect>) -> Rect {
    main_window_canvas_rects(available, 0.0, embedded_panel).0
}

/// Pan offset update for zoom-about-point; `canvas_rect` must match [`ImageViewerApp::compute_display_rect`].
pub(crate) fn zoom_pan_offset_for_screen_point(
    mouse: Pos2,
    canvas_rect: Rect,
    ratio: f32,
    pan_offset: Vec2,
) -> Vec2 {
    let d = mouse - canvas_rect.center();
    d * (1.0 - ratio) + pan_offset * ratio
}

/// Union of outgoing/incoming image bounds, clamped to the main canvas paint area.
///
/// Geometric transitions (page flip, curtain, ripple) use this as their animation limit so effects
/// do not spill over embedded directory-tree navigation or other non-canvas UI.
pub(crate) fn transition_union_rect(
    canvas_rect: Rect,
    final_dest: Rect,
    prev_dest: Rect,
    has_prev: bool,
) -> Rect {
    let image_union = if has_prev {
        prev_dest.union(final_dest)
    } else {
        final_dest
    };
    let bounded = image_union.intersect(canvas_rect);
    if bounded.width() > 0.0 && bounded.height() > 0.0 {
        bounded
    } else {
        canvas_rect
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PlaneLayout {
    pub image_size: Vec2,
    pub rotated_image_size: Vec2,
    pub dest: Rect,
    pub unrotated_dest: Rect,
    pub pivot: Pos2,
    pub rotation_steps: i32,
    pub angle: f32,
    pub effective_scale: f32,
}

impl PlaneLayout {
    pub(crate) fn from_dest(image_size: Vec2, rotation_steps: i32, dest: Rect) -> Self {
        let rotation_steps = rotation_steps.rem_euclid(4);
        let rotated_image_size = rotated_image_size_for_display(image_size, rotation_steps);
        let unrotated_dest = unrotated_draw_rect_for_display(dest, rotation_steps);
        let effective_scale = if rotated_image_size.x > 0.0 {
            dest.width() / rotated_image_size.x
        } else {
            0.0
        };
        Self {
            image_size,
            rotated_image_size,
            dest,
            unrotated_dest,
            pivot: dest.center(),
            rotation_steps,
            angle: rotation_steps as f32 * (std::f32::consts::PI / 2.0),
            effective_scale,
        }
    }
}

pub(crate) fn rotated_image_size_for_display(img_size: Vec2, rotation_steps: i32) -> Vec2 {
    if rotation_steps.rem_euclid(4) % 2 != 0 {
        Vec2::new(img_size.y, img_size.x)
    } else {
        img_size
    }
}

pub(crate) fn unrotated_draw_rect_for_display(
    rotated_display_rect: Rect,
    rotation_steps: i32,
) -> Rect {
    let size = if rotation_steps.rem_euclid(4) % 2 != 0 {
        Vec2::new(rotated_display_rect.height(), rotated_display_rect.width())
    } else {
        rotated_display_rect.size()
    };
    Rect::from_center_size(rotated_display_rect.center(), size)
}

impl ImageViewerApp {
    /// Calculate current absolute display scale relative to image pixels (logical scale).
    pub(crate) fn calculate_effective_scale(&self, img_size: Vec2, screen_rect: Rect) -> f32 {
        match self.settings.scale_mode {
            ScaleMode::FitToWindow => {
                if img_size.x > 0.1 && img_size.y > 0.1 {
                    (screen_rect.width() / img_size.x).min(screen_rect.height() / img_size.y)
                        * self.zoom_factor
                } else {
                    self.zoom_factor
                }
            }
            ScaleMode::OriginalSize => self.zoom_factor / self.cached_pixels_per_point,
        }
    }

    /// Compute the display rect for an image texture within the screen.
    pub(crate) fn compute_display_rect(&self, img_size: Vec2, screen_rect: Rect) -> Rect {
        let scale = self.calculate_effective_scale(img_size, screen_rect);
        match self.settings.scale_mode {
            ScaleMode::FitToWindow => {
                let disp = img_size * scale;
                let off = (screen_rect.size() - disp) * 0.5;
                Rect::from_min_size(screen_rect.min + off + self.pan_offset, disp)
            }
            ScaleMode::OriginalSize => {
                // Divide by pixels_per_point so 1 image pixel = 1 physical screen pixel
                // on HiDPI/Retina displays (e.g. 4K at 200% scaling).
                let ppp = self.cached_pixels_per_point;
                let disp = img_size * (self.zoom_factor / ppp);
                let center = screen_rect.center() + self.pan_offset;
                Rect::from_center_size(center, disp)
            }
        }
    }

    pub(crate) fn compute_plane_layout(&self, img_size: Vec2, screen_rect: Rect) -> PlaneLayout {
        let rotation = self.current_rotation;
        let rotated_img_size = rotated_image_size_for_display(img_size, rotation);
        let dest = self.compute_display_rect(rotated_img_size, screen_rect);
        PlaneLayout::from_dest(img_size, rotation, dest)
    }

    /// Embedded side-panel bounds used to inset the main image canvas, if any.
    ///
    /// Returns `None` when navigation is hidden, detached, or auto-hidden so the canvas spans
    /// the full main-window content area.
    pub(crate) fn embedded_nav_panel_rect_for_area(
        &self,
        ctx: &Context,
        area: Rect,
    ) -> Option<Rect> {
        if !self.directory_tree_settings_active() || !self.directory_tree_nav_is_embedded() {
            return None;
        }
        embedded_directory_tree_panel_rect(ctx).or_else(|| {
            let width = self.embedded_nav_panel_width_estimate();
            (width > 0.0).then(|| {
                Rect::from_min_max(area.min, Pos2::new(area.min.x + width, area.max.y))
            })
        })
    }

    /// Layout rect for the image canvas. Prefer the last painted canvas area so navigation
    /// hold/transition geometry matches the embedded directory-tree side panel.
    pub(crate) fn canvas_rect_for_layout(&self, ctx: &Context) -> Rect {
        if let Some(rect) = self.last_canvas_rect {
            return rect;
        }
        let content = ctx.input(|i| i.content_rect());
        let panel = self.embedded_nav_panel_rect_for_area(ctx, content);
        main_window_canvas_paint_rect(content, panel)
    }

    /// Rotate the image while keeping the current screen center point fixed on the same image coordinate.
    pub(crate) fn apply_rotation_with_tracking(&mut self, clockwise: bool, ctx: &Context) {
        if self.image_files.is_empty() {
            return;
        }

        // 1. Get original image resolution
        let res = if let Some(r) = self.current_image_res {
            r
        } else {
            return;
        };
        let img_size = Vec2::new(res.0 as f32, res.1 as f32);
        let screen_rect = self.canvas_rect_for_layout(ctx);

        // 2. Calculate current scale
        let old_rotation = self.current_rotation;
        let old_needs_swap = old_rotation % 2 != 0;
        let old_rotated_size = if old_needs_swap {
            Vec2::new(img_size.y, img_size.x)
        } else {
            img_size
        };
        let old_scale = self.calculate_effective_scale(old_rotated_size, screen_rect);

        // 3. Update rotation state
        if clockwise {
            self.current_rotation = (self.current_rotation + 1) % 4;
        } else {
            self.current_rotation = (self.current_rotation + 3) % 4;
        }

        // 4. Calculate new scale (FitToWindow scale might change due to aspect ratio swap)
        let new_rotation = self.current_rotation;
        let new_needs_swap = new_rotation % 2 != 0;
        let new_rotated_size = if new_needs_swap {
            Vec2::new(img_size.y, img_size.x)
        } else {
            img_size
        };
        let new_scale = self.calculate_effective_scale(new_rotated_size, screen_rect);

        // 5. Transform pan_offset to maintain center alignment.
        // Rotation around image center maps (x, y) to (-y, x) for CW 90.
        // We also compensate for scale changes to keep the visual point fixed.
        let p = self.pan_offset;
        if clockwise {
            // Clockwise: (x, y) -> (-y, x)
            self.pan_offset = Vec2::new(-p.y, p.x);
        } else {
            // Counter-clockwise: (x, y) -> (y, -x)
            self.pan_offset = Vec2::new(p.y, -p.x);
        }

        if old_scale > 0.0001 {
            self.pan_offset *= new_scale / old_scale;
        }

        // Invalidate tiled caches to re-request tiles in new orientation.
        self.invalidate_tile_requests_for_view_change();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{Pos2, Rect, Vec2};

    #[test]
    fn rotated_image_size_swaps_only_for_quarter_turns() {
        let size = Vec2::new(400.0, 200.0);

        assert_eq!(rotated_image_size_for_display(size, 0), size);
        assert_eq!(
            rotated_image_size_for_display(size, 1),
            Vec2::new(200.0, 400.0)
        );
        assert_eq!(rotated_image_size_for_display(size, 2), size);
        assert_eq!(
            rotated_image_size_for_display(size, 3),
            Vec2::new(200.0, 400.0)
        );
    }

    #[test]
    fn unrotated_draw_rect_preserves_rotated_display_bounds() {
        let rotated_bounds = Rect::from_center_size(Pos2::new(100.0, 80.0), Vec2::new(40.0, 120.0));

        let quarter_turn_rect = unrotated_draw_rect_for_display(rotated_bounds, 1);
        assert_eq!(quarter_turn_rect.center(), rotated_bounds.center());
        assert_eq!(quarter_turn_rect.size(), Vec2::new(120.0, 40.0));

        let half_turn_rect = unrotated_draw_rect_for_display(rotated_bounds, 2);
        assert_eq!(half_turn_rect, rotated_bounds);
    }

    #[test]
    fn main_window_canvas_paint_rect_without_panel_matches_content() {
        let content = Rect::from_min_max(Pos2::ZERO, Pos2::new(1000.0, 800.0));
        assert_eq!(main_window_canvas_paint_rect(content, None), content);
    }

    #[test]
    fn main_window_canvas_paint_rect_with_embedded_panel_insets_left_edge() {
        let content = Rect::from_min_max(Pos2::ZERO, Pos2::new(1000.0, 800.0));
        let panel = Rect::from_min_max(Pos2::ZERO, Pos2::new(240.0, 800.0));
        let paint = main_window_canvas_paint_rect(content, Some(panel));
        assert_eq!(paint.min.x, 240.0);
        assert_eq!(paint.center().x, 620.0);
        assert_ne!(paint.center().x, content.center().x);
    }

    #[test]
    fn zoom_pan_offset_anchors_to_canvas_center_not_full_window() {
        let content = Rect::from_min_max(Pos2::ZERO, Pos2::new(1000.0, 800.0));
        let panel = Rect::from_min_max(Pos2::ZERO, Pos2::new(200.0, 800.0));
        let canvas = main_window_canvas_paint_rect(content, Some(panel));
        let mouse = Pos2::new(600.0, 400.0);
        let ratio = 2.0;
        let pan = Vec2::ZERO;

        let with_canvas_center = zoom_pan_offset_for_screen_point(mouse, canvas, ratio, pan);
        let with_window_center = zoom_pan_offset_for_screen_point(mouse, content, ratio, pan);
        assert_ne!(with_canvas_center, with_window_center);
        assert_eq!(
            zoom_pan_offset_for_screen_point(canvas.center(), canvas, ratio, pan),
            Vec2::ZERO
        );
    }

    #[test]
    fn transition_union_rect_is_clamped_to_canvas() {
        let canvas = Rect::from_min_max(Pos2::new(240.0, 0.0), Pos2::new(1000.0, 800.0));
        let final_dest = Rect::from_center_size(canvas.center(), Vec2::new(400.0, 300.0));
        let prev_dest = Rect::from_center_size(Pos2::new(500.0, 400.0), Vec2::new(400.0, 300.0));
        let union = transition_union_rect(canvas, final_dest, prev_dest, true);
        assert!(union.min.x >= canvas.min.x - f32::EPSILON);
        assert!(union.max.x <= canvas.max.x + f32::EPSILON);
        assert!(union.min.y >= canvas.min.y - f32::EPSILON);
        assert!(union.max.y <= canvas.max.y + f32::EPSILON);
    }

    #[test]
    fn plane_layout_preserves_shared_image_geometry() {
        let image_size = Vec2::new(400.0, 200.0);
        let dest = Rect::from_min_size(Pos2::new(10.0, 20.0), Vec2::new(100.0, 200.0));

        let layout = PlaneLayout::from_dest(image_size, 1, dest);

        assert_eq!(layout.image_size, image_size);
        assert_eq!(layout.rotated_image_size, Vec2::new(200.0, 400.0));
        assert_eq!(layout.dest, dest);
        assert_eq!(layout.unrotated_dest.size(), Vec2::new(200.0, 100.0));
        assert_eq!(layout.pivot, dest.center());
        assert_eq!(layout.rotation_steps, 1);
        assert_eq!(layout.effective_scale, 0.5);
    }
}
