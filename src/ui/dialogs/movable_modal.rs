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

//! A moveable, resizable dialog wrapper that paints a themed backdrop and
//! renders its content inside a floating `egui::Window`.
//!
//! # Usage
//!
//! ```rust
//! MovableModal::new("my_dialog", "Dialog Title")
//!     .resizable(true)
//!     .default_size([480.0, 320.0])
//!     .min_size([320.0, 200.0])
//!     .show(ctx, palette, |ui| {
//!         ui.label("Hello from a modal!");
//!     });
//! ```

use eframe::egui;
use crate::theme::ThemePalette;

/// Builder for a moveable, resizable modal dialog.
///
/// Internally it:
/// 1. Paints a semi-transparent backdrop at `Order::Background` — tinted for
///    both dark and light themes.
/// 2. Opens an `egui::Window` at `Order::Foreground` so the dialog floats
///    above all normal panels and windows.
///
/// The dialog starts centered on screen by default and remembers its position
/// (and size, when resizable) between open/close cycles within the same session.
pub(crate) struct MovableModal {
    id: egui::Id,
    title: String,
    resizable: bool,
    default_size: egui::Vec2,
    min_size: egui::Vec2,
}

// ── Constants ────────────────────────────────────────────────────────────────

const FALLBACK_WIN_SIZE: egui::Vec2 = egui::Vec2::new(1280.0, 720.0);

impl MovableModal {
    /// Create a new builder.
    ///
    /// `id` must be unique across all concurrently-open dialogs.
    /// `title` is shown in the window title bar.
    pub fn new(id: impl std::hash::Hash, title: impl Into<String>) -> Self {
        Self {
            id: egui::Id::new(id),
            title: title.into(),
            resizable: true,
            default_size: egui::vec2(420.0, 300.0),
            min_size: egui::vec2(200.0, 100.0),
        }
    }

    /// Whether the window frame can be dragged to resize. Default: `true`.
    pub fn resizable(mut self, resizable: bool) -> Self {
        self.resizable = resizable;
        self
    }

    /// Preferred size when the dialog is first shown. Default: 420 × 300.
    pub fn default_size(mut self, size: impl Into<egui::Vec2>) -> Self {
        self.default_size = size.into();
        self
    }

    /// Minimum allowed size (only relevant when `resizable = true`). Default: 200 × 100.
    pub fn min_size(mut self, size: impl Into<egui::Vec2>) -> Self {
        self.min_size = size.into();
        self
    }

    /// Render the backdrop and the dialog window for this frame.
    ///
    /// `add_contents` receives the inner `Ui`. Return values from the closure
    /// are discarded; callers communicate results via captured mutable variables.
    pub fn show(
        self,
        ctx: &egui::Context,
        palette: &ThemePalette,
        add_contents: impl FnOnce(&mut egui::Ui),
    ) {
        self.draw_backdrop(ctx, palette);

        // Use window-local size (starting at Pos2::ZERO) so that default_pos
        // is expressed in egui's coordinate system, not monitor screen coords.
        let win_size = ctx.input(|i| {
            i.viewport().inner_rect
                .map(|r| r.size())
                .unwrap_or(FALLBACK_WIN_SIZE)
        });
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, win_size);

        // For accurate vertical centering we need the window's TOTAL height
        // (title bar + content).  On the first open we estimate from default_size;
        // on every subsequent open we use the actual measured height that was
        // recorded at the end of the previous open — eliminating the error for
        // non-resizable dialogs whose content auto-sizes beyond default_size.
        let measured_h_key = self.id.with(super::modal_state::ID_MEASURED_HEIGHT);
        let measured_h: f32 = ctx.data(|d| d.get_temp(measured_h_key).unwrap_or(0.0));

        // egui's title bar height ≈ interact_size.y (row height) + 2× item_spacing.y.
        let title_bar_h = ctx.global_style().spacing.interact_size.y
            + ctx.global_style().spacing.item_spacing.y * 2.0;
        let total_h = if measured_h > 0.0 {
            measured_h          // actual height from previous open (includes title bar)
        } else {
            self.default_size.y + title_bar_h   // first-open estimate
        };

        let default_pos = egui::pos2(
            screen.center().x - self.default_size.x * 0.5,
            screen.center().y - total_h * 0.5,
        );

        // Unique Id per modal opening so egui has no position memory from
        // previous openings → dialog always appears at the freshly-computed center.
        let modal_gen: u32 = ctx.data(|d| d.get_temp(egui::Id::new(super::modal_state::ID_MODAL_GENERATION)).unwrap_or(0));
        let unique_id = self.id.with(modal_gen);

        let resp = egui::Window::new(self.title.as_str())
            .id(unique_id)
            .order(egui::Order::Foreground)
            .collapsible(false)
            .resizable([self.resizable, self.resizable])
            .default_pos([default_pos.x, default_pos.y])
            .default_size([self.default_size.x, self.default_size.y])
            .min_size([self.min_size.x, self.min_size.y])
            .show(ctx, |ui| {
                ui.visuals_mut().override_text_color = Some(palette.text_normal);
                add_contents(ui);
            });

        // Record the actual rendered window height for next open's centering.
        if let Some(inner) = resp {
            ctx.data_mut(|d| d.insert_temp(measured_h_key, inner.response.rect.height()));
        }
    }

    // ── Private ──────────────────────────────────────────────────────────────

    /// Paint a full-screen dimming overlay at `Order::Background`.
    ///
    /// Alpha is tuned per-theme so the overlay reads naturally in both dark
    /// and light modes:
    ///  • Dark theme  → darker overlay (higher alpha) to create contrast.
    ///  • Light theme → lighter grey tint (lower alpha, warm-ish hue) so the
    ///    bright background is dimmed without looking like a night-mode flash.
    fn draw_backdrop(&self, ctx: &egui::Context, palette: &ThemePalette) {
        // Same normalization: use window-local size, not monitor-absolute coords.
        let win_size = ctx.input(|i| {
            i.viewport().inner_rect
                .map(|r| r.size())
                .unwrap_or(FALLBACK_WIN_SIZE)
        });
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, win_size);
        let color = if palette.is_dark {
            egui::Color32::from_black_alpha(150)
        } else {
            egui::Color32::from_rgba_unmultiplied(30, 30, 40, 110)
        };

        ctx.layer_painter(egui::LayerId::new(
            egui::Order::Background,
            egui::Id::new("movable_modal_backdrop"),
        ))
        .rect_filled(screen, 0.0, color);
    }
}
