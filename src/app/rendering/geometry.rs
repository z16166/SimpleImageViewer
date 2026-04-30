use crate::app::ImageViewerApp;
use crate::app::ScaleMode;
use eframe::egui::{Context, Rect, Vec2};

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
        let screen_rect = ctx.input(|i| i.content_rect());

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

        // Invalidate tiled caches to re-request tiles in new orientation
        self.generation = self.generation.wrapping_add(1);
        self.loader.set_generation(self.generation);
        if let Some(tm) = &mut self.tile_manager {
            tm.generation = self.generation;
            tm.pending_tiles.clear();
        }
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
}
