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

use crate::theme::ThemePalette;
use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, RichText, Stroke, StrokeKind, Vec2,
};
use rust_i18n::t;
use std::time::Instant;

const OSD_FONT: FontId = FontId::proportional(crate::constants::OSD_TEXT_SIZE);
const OSD_ERROR_FONT: FontId = FontId::proportional(crate::constants::OSD_ERROR_TEXT_SIZE);
const LOADING_HINT_FONT: FontId = FontId::proportional(crate::constants::LOADING_HINT_TEXT_SIZE);

/// Zoom shown on the OSD is bucketed so wheel-zoom does not rebuild strings every frame.
const OSD_ZOOM_BUCKET_PCT: u32 = 5;

pub fn quantize_osd_zoom_pct(zoom_pct: u32) -> u32 {
    if zoom_pct == 0 {
        return 0;
    }
    ((zoom_pct + OSD_ZOOM_BUCKET_PCT / 2) / OSD_ZOOM_BUCKET_PCT * OSD_ZOOM_BUCKET_PCT)
        .max(OSD_ZOOM_BUCKET_PCT)
}

fn layout_width(ui: &egui::Ui, text: &str) -> f32 {
    ui.painter()
        .layout_no_wrap(text.to_owned(), OSD_FONT, Color32::PLACEHOLDER)
        .size()
        .x
}

fn truncate_into(ui: &egui::Ui, dst: &mut String, src: &str, max_width: f32, scratch: &mut String) {
    dst.clear();
    if max_width <= 0.0 {
        dst.push('…');
        return;
    }
    if layout_width(ui, src) <= max_width {
        dst.push_str(src);
        return;
    }
    const ELLIPSIS: char = '…';
    let n = src.chars().count();
    let mut lo = 0usize;
    let mut hi = n;
    while lo < hi {
        let mid = (lo + hi + 1) / 2;
        scratch.clear();
        for ch in src.chars().take(mid) {
            scratch.push(ch);
        }
        scratch.push(ELLIPSIS);
        if layout_width(ui, scratch) <= max_width {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    if lo == 0 {
        dst.push(ELLIPSIS);
        return;
    }
    for ch in src.chars().take(lo) {
        dst.push(ch);
    }
    dst.push(ELLIPSIS);
}

/// Static vs tiled path tag for the main image OSD line.
#[derive(Copy, Clone, PartialEq, Eq)]
pub enum ImageOsdMode {
    Static,
    Tiled,
}

/// Per-frame image OSD inputs (Copy — no heap allocs on the hot path).
#[derive(Copy, Clone, PartialEq, Eq)]
pub struct ImageOsdFrame {
    pub index: usize,
    pub total: usize,
    pub zoom_pct: u32,
    pub res: (u32, u32),
    pub file_size_bytes: u64,
    pub mode: ImageOsdMode,
}

impl ImageOsdFrame {
    pub fn cache_key(self) -> Self {
        Self {
            zoom_pct: quantize_osd_zoom_pct(self.zoom_pct),
            ..self
        }
    }
}

/// Parameters that affect the music HUD.
#[derive(PartialEq, Clone)]
pub struct OsdState {
    pub index: usize,
    pub total: usize,
    pub zoom_pct: u32,
    pub res: (u32, u32),
    pub file_size_bytes: u64,
    pub mode: String,
    pub current_track: Option<String>,
    pub metadata: Option<String>,
    pub current_cue_track: Option<usize>,
    pub current_pos_ms: u64,
    pub total_duration_ms: u64,
    pub cue_markers: Vec<u64>,
}

struct LayoutLines {
    width_px: u32,
    main: String,
    raw: String,
    hdr: String,
}

pub struct OsdRenderer {
    cached_hud: String,
    cached_hdr_line_display: String,
    cached_raw_line: String,
    has_raw_line: bool,
    has_hdr_line: bool,
    layout: Option<LayoutLines>,
    layout_content_stamp: u64,
    layout_built_stamp: u64,
    measure_scratch: String,
    last_hud_key: Option<ImageOsdFrame>,
    last_hud_file_name: String,
    cached_loading_hint: String,
    cached_save_error: String,
    last_save_error_message: String,
    last_music_state: Option<OsdState>,
}

impl OsdRenderer {
    pub fn new() -> Self {
        Self {
            cached_hud: String::new(),
            cached_hdr_line_display: String::new(),
            cached_raw_line: String::new(),
            has_raw_line: false,
            has_hdr_line: false,
            layout: None,
            layout_content_stamp: 0,
            layout_built_stamp: 0,
            measure_scratch: String::new(),
            last_hud_key: None,
            last_hud_file_name: String::new(),
            cached_loading_hint: t!("status.loading").to_string(),
            cached_save_error: String::new(),
            last_save_error_message: String::new(),
            last_music_state: None,
        }
    }

    pub fn has_raw_line(&self) -> bool {
        self.has_raw_line
    }

    pub fn has_hdr_line(&self) -> bool {
        self.has_hdr_line
    }

    fn bump_content(&mut self) {
        self.layout_content_stamp = self.layout_content_stamp.wrapping_add(1);
    }

    /// RAW/HDR supplemental lines — updated when loader or HDR settings change, not each frame.
    pub fn set_supplemental_lines(&mut self, raw: Option<String>, hdr: Option<String>) {
        self.has_raw_line = raw.is_some();
        self.cached_raw_line = raw.unwrap_or_default();
        self.has_hdr_line = hdr.is_some();
        self.cached_hdr_line_display.clear();
        if let Some(hdr) = hdr {
            self.cached_hdr_line_display.push('[');
            self.cached_hdr_line_display.push_str(&hdr);
            self.cached_hdr_line_display.push(']');
        }
        self.bump_content();
    }

    pub fn invalidate(&mut self) {
        self.last_hud_key = None;
        self.last_hud_file_name.clear();
        self.last_music_state = None;
        self.bump_content();
    }

    fn rebuild_hud_if_needed(&mut self, frame: ImageOsdFrame, file_name: &str) {
        let key = frame.cache_key();
        if self.last_hud_key == Some(key) && self.last_hud_file_name == file_name {
            return;
        }
        let mode_label = match key.mode {
            ImageOsdMode::Static => t!("osd.mode.static"),
            ImageOsdMode::Tiled => t!("osd.mode.tiled"),
        };
        let file_size_text = format_file_size(key.file_size_bytes);
        self.cached_hud.clear();
        use std::fmt::Write as _;
        let _ = write!(
            self.cached_hud,
            "{} / {}    {}    {}    {}%    {}×{}    [{}]",
            key.index + 1,
            key.total,
            file_name,
            file_size_text,
            key.zoom_pct,
            key.res.0,
            key.res.1,
            mode_label,
        );
        self.last_hud_key = Some(key);
        self.last_hud_file_name.clear();
        self.last_hud_file_name.push_str(file_name);
        self.bump_content();
    }

    fn ensure_layout(&mut self, ui: &egui::Ui, max_width: f32) {
        let width_px = max_width.max(0.0) as u32;
        if self.layout_built_stamp == self.layout_content_stamp
            && self.layout.as_ref().is_some_and(|l| l.width_px == width_px)
        {
            return;
        }

        let mut lines = LayoutLines {
            width_px,
            main: String::new(),
            raw: String::new(),
            hdr: String::new(),
        };
        truncate_into(
            ui,
            &mut lines.main,
            &self.cached_hud,
            max_width,
            &mut self.measure_scratch,
        );
        if self.has_raw_line {
            truncate_into(
                ui,
                &mut lines.raw,
                &self.cached_raw_line,
                max_width,
                &mut self.measure_scratch,
            );
        }
        if self.has_hdr_line {
            truncate_into(
                ui,
                &mut lines.hdr,
                &self.cached_hdr_line_display,
                max_width,
                &mut self.measure_scratch,
            );
        }
        self.layout = Some(lines);
        self.layout_built_stamp = self.layout_content_stamp;
    }

    fn sync_save_error(&mut self, save_error: &Option<(String, Instant)>) {
        let message = save_error
            .as_ref()
            .map(|(msg, _)| msg.as_str())
            .unwrap_or("");
        if message == self.last_save_error_message {
            return;
        }
        self.last_save_error_message.clear();
        self.last_save_error_message.push_str(message);
        self.cached_save_error.clear();
        if !message.is_empty() {
            self.cached_save_error = t!("error.settings_save_failed", error = message).to_string();
        }
    }

    pub fn render_image(
        &mut self,
        ui: &mut egui::Ui,
        screen_rect: egui::Rect,
        frame: ImageOsdFrame,
        file_name: &str,
        palette: &ThemePalette,
        save_error: &Option<(String, Instant)>,
    ) {
        self.rebuild_hud_if_needed(frame, file_name);
        self.sync_save_error(save_error);

        let max_w = (screen_rect.width() - crate::constants::OSD_MARGIN * 2.0).max(64.0);
        self.ensure_layout(ui, max_w);

        let Some(layout) = self.layout.as_ref() else {
            return;
        };

        let base_pos = screen_rect.left_bottom()
            + Vec2::new(crate::constants::OSD_MARGIN, -crate::constants::OSD_MARGIN);
        ui.painter().text(
            base_pos,
            Align2::LEFT_BOTTOM,
            layout.main.as_str(),
            OSD_FONT,
            palette.osd_text,
        );

        let mut line_offset = crate::constants::OSD_TEXT_SIZE + crate::constants::OSD_HDR_LINE_GAP;

        if self.has_raw_line {
            let raw_pos = base_pos + Vec2::new(0.0, -line_offset);
            ui.painter().text(
                raw_pos,
                Align2::LEFT_BOTTOM,
                layout.raw.as_str(),
                OSD_FONT,
                palette.osd_text,
            );
            line_offset += crate::constants::OSD_TEXT_SIZE + crate::constants::OSD_HDR_LINE_GAP;
        }

        if self.has_hdr_line {
            let hdr_pos = base_pos + Vec2::new(0.0, -line_offset);
            ui.painter().text(
                hdr_pos,
                Align2::LEFT_BOTTOM,
                layout.hdr.as_str(),
                OSD_FONT,
                palette.osd_text,
            );
        }

        if !self.cached_save_error.is_empty() {
            let mut err_offset_y = crate::constants::OSD_ERROR_OFFSET;
            if self.has_raw_line {
                err_offset_y += crate::constants::OSD_ERROR_EXTRA_WHEN_HDR_LINE;
            }
            if self.has_hdr_line {
                err_offset_y += crate::constants::OSD_ERROR_EXTRA_WHEN_HDR_LINE;
            }
            let err_pos =
                screen_rect.left_bottom() + Vec2::new(crate::constants::OSD_MARGIN, -err_offset_y);
            ui.painter().text(
                err_pos,
                Align2::LEFT_BOTTOM,
                self.cached_save_error.as_str(),
                OSD_ERROR_FONT,
                Color32::from_rgb(255, 100, 100),
            );
        }
    }

    /// Renders a modern music HUD at the bottom center of the screen.
    pub fn render_music_hud(
        &mut self,
        ui: &mut egui::Ui,
        _screen_rect: egui::Rect,
        state: &OsdState,
        palette: &ThemePalette,
    ) -> egui::Rect {
        if state.total_duration_ms == 0 || state.current_track.is_none() {
            return egui::Rect::NOTHING;
        }

        let hud_rect = ui.max_rect();

        ui.painter().add(egui::Shape::rect_filled(
            hud_rect,
            CornerRadius::same(8),
            Color32::from_black_alpha(160),
        ));
        ui.painter().rect_stroke(
            hud_rect,
            CornerRadius::same(8),
            Stroke::new(1.0_f32, palette.accent2.linear_multiply(0.3)),
            StrokeKind::Outside,
        );

        let inner_rect = hud_rect.shrink(10.0);
        ui.scope_builder(egui::UiBuilder::new().max_rect(inner_rect), |ui| {
            ui.vertical(|ui| {
                let display_text = state.metadata.as_deref().or(state.current_track.as_deref());
                if let Some(text) = display_text {
                    let short_text = if text.chars().count() > crate::constants::MUSIC_HUD_MAX_CHARS
                    {
                        format!(
                            "{}...",
                            text.chars()
                                .take(crate::constants::MUSIC_HUD_TRUNCATE_LEN)
                                .collect::<String>()
                        )
                    } else {
                        text.to_string()
                    };

                    ui.label(
                        RichText::new(format!("♪ {}", short_text))
                            .color(
                                palette
                                    .accent2
                                    .linear_multiply(crate::constants::MUSIC_HUD_CONTRAST_BOOST)
                                    .to_opaque(),
                            )
                            .small()
                            .strong(),
                    );
                }

                ui.add_space(2.0);

                ui.horizontal(|ui| {
                    let mut pos = state.current_pos_ms as f32 / 1000.0;
                    let total = state.total_duration_ms as f32 / 1000.0;

                    let cur_str = format!("{:02}:{:02}", (pos as u32) / 60, (pos as u32) % 60);
                    let tot_str = format!("{:02}:{:02}", (total as u32) / 60, (total as u32) % 60);

                    ui.label(RichText::new(cur_str).small().color(palette.text_muted));

                    ui.spacing_mut().slider_width =
                        ui.available_width() - crate::constants::SLIDER_WIDTH_LABEL_OFFSET;
                    let resp = ui.add(
                        egui::Slider::new(&mut pos, 0.0..=total)
                            .show_value(false)
                            .trailing_fill(true),
                    );

                    if state.total_duration_ms > 0 && !state.cue_markers.is_empty() {
                        let painter = ui.painter();
                        let slider_rect = resp.rect;

                        for (idx, &marker_ms) in state.cue_markers.iter().enumerate() {
                            if marker_ms >= state.total_duration_ms {
                                continue;
                            }
                            let ratio =
                                (marker_ms as f32 / state.total_duration_ms as f32).clamp(0.0, 1.0);
                            let x = slider_rect.left() + ratio * slider_rect.width();
                            let center = egui::pos2(x, slider_rect.center().y);

                            let is_current = state.current_cue_track == Some(idx);
                            let color = if is_current {
                                palette.accent2
                            } else {
                                palette.text_muted.gamma_multiply(0.6)
                            };
                            let radius = if is_current { 2.5 } else { 1.5 };

                            painter.circle_filled(center, radius, color);
                        }
                    }

                    ui.label(RichText::new(tot_str).small().color(palette.text_muted));

                    if resp.drag_stopped() {
                        ui.memory_mut(|mem| {
                            mem.data
                                .insert_temp(egui::Id::new(crate::constants::ID_PENDING_SEEK), pos)
                        });
                    }
                });
            });
        });

        hud_rect
    }

    pub fn render_loading_hint(
        &self,
        ui: &egui::Ui,
        screen_rect: egui::Rect,
        palette: &ThemePalette,
    ) {
        ui.painter().text(
            screen_rect.center() - Vec2::new(0.0, 20.0),
            Align2::CENTER_BOTTOM,
            self.cached_loading_hint.as_str(),
            LOADING_HINT_FONT,
            palette.text_muted,
        );
    }
}

pub fn format_file_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * KB;
    const GB: f64 = 1024.0 * MB;
    if bytes < 1024 {
        format!("{bytes} bytes")
    } else if (bytes as f64) < MB {
        format!("{:.1} KB", bytes as f64 / KB)
    } else if (bytes as f64) < GB {
        format!("{:.1} MB", bytes as f64 / MB)
    } else {
        format!("{:.1} GB", bytes as f64 / GB)
    }
}

#[cfg(test)]
mod tests {
    use super::{format_file_size, quantize_osd_zoom_pct};

    #[test]
    fn formats_file_sizes_with_binary_units() {
        assert_eq!(format_file_size(42), "42 bytes");
        assert_eq!(format_file_size(1536), "1.5 KB");
        assert_eq!(format_file_size(2 * 1024 * 1024), "2.0 MB");
        assert_eq!(format_file_size(3 * 1024 * 1024 * 1024), "3.0 GB");
    }

    #[test]
    fn quantize_osd_zoom_pct_buckets_to_five_percent() {
        assert_eq!(quantize_osd_zoom_pct(0), 0);
        assert_eq!(quantize_osd_zoom_pct(102), 100);
        assert_eq!(quantize_osd_zoom_pct(103), 105);
        assert_eq!(quantize_osd_zoom_pct(153), 155);
    }
}
