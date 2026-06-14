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
use std::sync::Arc;
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

/// HDR inputs that determine the supplemental OSD line.
#[derive(Copy, Clone, PartialEq)]
pub struct HdrOsdFrame<'a> {
    pub render_path: Option<crate::hdr::status::HdrRenderPath>,
    pub color_space: Option<crate::hdr::types::HdrColorSpace>,
    pub output_mode: crate::hdr::types::HdrOutputMode,
    pub native_presentation_enabled: bool,
    pub ultra_hdr_decode_capacity: Option<f32>,
    pub monitor_label: Option<&'a str>,
    pub exposure_ev: f32,
}

impl Default for HdrOsdFrame<'_> {
    fn default() -> Self {
        Self {
            render_path: None,
            color_space: None,
            output_mode: crate::hdr::types::HdrOutputMode::SdrToneMapped,
            native_presentation_enabled: false,
            ultra_hdr_decode_capacity: None,
            monitor_label: None,
            exposure_ev: 0.0,
        }
    }
}

#[derive(Clone, PartialEq)]
pub enum OsdEvent {
    CurrentIndex(usize),
    TotalImages(usize),
    ZoomPct(u32),
    ImageResolution((u32, u32)),
    FileSizeBytes(u64),
    ImageMode(ImageOsdMode),
    FileName(Arc<str>),
    RawSensorSize((u32, u32)),
    RawEmbeddedPreview(Option<(u32, u32)>),
    RawRenderPixels(crate::loader::RawRenderPixels),
    RawDemosaicBackend(Option<crate::loader::RawDemosaicBackend>),
    RawCpuDemosaicMs(Option<u32>),
    RawGpuExtractMs(Option<u32>),
    RawGpuDemosaicMs(Option<u32>),
    HdrRenderPath(Option<crate::hdr::status::HdrRenderPath>),
    HdrColorSpace(Option<crate::hdr::types::HdrColorSpace>),
    HdrOutputMode(crate::hdr::types::HdrOutputMode),
    HdrNativePresentationEnabled(bool),
    UltraHdrDecodeCapacity(Option<f32>),
    HdrMonitorLabel(Option<Arc<str>>),
    HdrExposureEv(f32),
}

impl OsdEvent {
    pub fn current_index(value: &usize) -> Self {
        Self::CurrentIndex(*value)
    }

    pub fn total_images(value: &usize) -> Self {
        Self::TotalImages(*value)
    }

    pub fn zoom_pct(value: &u32) -> Self {
        Self::ZoomPct(*value)
    }

    pub fn image_resolution(value: &(u32, u32)) -> Self {
        Self::ImageResolution(*value)
    }

    pub fn file_size_bytes(value: &u64) -> Self {
        Self::FileSizeBytes(*value)
    }

    pub fn image_mode(value: &ImageOsdMode) -> Self {
        Self::ImageMode(*value)
    }

    pub fn file_name(value: &String) -> Self {
        Self::FileName(Arc::from(value.as_str()))
    }

    pub fn raw_sensor_size(value: &(u32, u32)) -> Self {
        Self::RawSensorSize(*value)
    }

    pub fn raw_embedded_preview(value: &Option<(u32, u32)>) -> Self {
        Self::RawEmbeddedPreview(*value)
    }

    pub fn raw_render_pixels(value: &crate::loader::RawRenderPixels) -> Self {
        Self::RawRenderPixels(*value)
    }

    pub fn raw_demosaic_backend(value: &Option<crate::loader::RawDemosaicBackend>) -> Self {
        Self::RawDemosaicBackend(*value)
    }

    pub fn raw_cpu_demosaic_ms(value: &Option<u32>) -> Self {
        Self::RawCpuDemosaicMs(*value)
    }

    pub fn raw_gpu_extract_ms(value: &Option<u32>) -> Self {
        Self::RawGpuExtractMs(*value)
    }

    pub fn raw_gpu_demosaic_ms(value: &Option<u32>) -> Self {
        Self::RawGpuDemosaicMs(*value)
    }

    pub fn hdr_render_path(value: &Option<crate::hdr::status::HdrRenderPath>) -> Self {
        Self::HdrRenderPath(*value)
    }

    pub fn hdr_color_space(value: &Option<crate::hdr::types::HdrColorSpace>) -> Self {
        Self::HdrColorSpace(*value)
    }

    pub fn hdr_output_mode(value: &crate::hdr::types::HdrOutputMode) -> Self {
        Self::HdrOutputMode(*value)
    }

    pub fn hdr_native_presentation_enabled(value: &bool) -> Self {
        Self::HdrNativePresentationEnabled(*value)
    }

    pub fn ultra_hdr_decode_capacity(value: &Option<f32>) -> Self {
        Self::UltraHdrDecodeCapacity(*value)
    }

    pub fn hdr_monitor_label(value: &Option<String>) -> Self {
        Self::HdrMonitorLabel(value.as_deref().map(Arc::from))
    }

    pub fn hdr_exposure_ev(value: &f32) -> Self {
        Self::HdrExposureEv(*value)
    }
}

#[derive(Clone, PartialEq)]
struct SupplementalOsdInputs {
    current_index: usize,
    total_images: usize,
    zoom_pct: u32,
    image_resolution: (u32, u32),
    file_size_bytes: u64,
    image_mode: ImageOsdMode,
    file_name: Arc<str>,
    raw_sensor_size: (u32, u32),
    raw_embedded_preview: Option<(u32, u32)>,
    raw_render_pixels: crate::loader::RawRenderPixels,
    raw_demosaic_backend: Option<crate::loader::RawDemosaicBackend>,
    raw_cpu_demosaic_ms: Option<u32>,
    raw_gpu_extract_ms: Option<u32>,
    raw_gpu_demosaic_ms: Option<u32>,
    hdr_render_path: Option<crate::hdr::status::HdrRenderPath>,
    hdr_color_space: Option<crate::hdr::types::HdrColorSpace>,
    hdr_output_mode: crate::hdr::types::HdrOutputMode,
    hdr_native_presentation_enabled: bool,
    ultra_hdr_decode_capacity: Option<f32>,
    hdr_monitor_label: Option<Arc<str>>,
    hdr_exposure_ev: f32,
}

impl Default for SupplementalOsdInputs {
    fn default() -> Self {
        Self {
            current_index: 0,
            total_images: 0,
            zoom_pct: 0,
            image_resolution: (0, 0),
            file_size_bytes: 0,
            image_mode: ImageOsdMode::Static,
            file_name: Arc::from(""),
            raw_sensor_size: (0, 0),
            raw_embedded_preview: None,
            raw_render_pixels: crate::loader::RawRenderPixels::Embedded {
                width: 0,
                height: 0,
            },
            raw_demosaic_backend: None,
            raw_cpu_demosaic_ms: None,
            raw_gpu_extract_ms: None,
            raw_gpu_demosaic_ms: None,
            hdr_render_path: None,
            hdr_color_space: None,
            hdr_output_mode: crate::hdr::types::HdrOutputMode::SdrToneMapped,
            hdr_native_presentation_enabled: false,
            ultra_hdr_decode_capacity: None,
            hdr_monitor_label: None,
            hdr_exposure_ev: 0.0,
        }
    }
}

impl SupplementalOsdInputs {
    fn hdr_line(hdr: &HdrOsdFrame<'_>) -> Option<String> {
        let render_path = hdr.render_path?;
        crate::hdr::status::hdr_osd_tag_from_parts(
            true,
            render_path,
            hdr.color_space,
            hdr.output_mode,
            hdr.native_presentation_enabled,
            hdr.ultra_hdr_decode_capacity,
            hdr.monitor_label,
            hdr.exposure_ev,
        )
    }

    fn hdr_line_from_state(&self) -> Option<String> {
        let hdr = HdrOsdFrame {
            render_path: self.hdr_render_path,
            color_space: self.hdr_color_space,
            output_mode: self.hdr_output_mode,
            native_presentation_enabled: self.hdr_native_presentation_enabled,
            ultra_hdr_decode_capacity: self.ultra_hdr_decode_capacity,
            monitor_label: self.hdr_monitor_label.as_deref(),
            exposure_ev: self.hdr_exposure_ev,
        };
        Self::hdr_line(&hdr)
    }

    fn apply_event(&mut self, event: OsdEvent) {
        match event {
            OsdEvent::CurrentIndex(value) => self.current_index = value,
            OsdEvent::TotalImages(value) => self.total_images = value,
            OsdEvent::ZoomPct(value) => self.zoom_pct = value,
            OsdEvent::ImageResolution(value) => self.image_resolution = value,
            OsdEvent::FileSizeBytes(value) => self.file_size_bytes = value,
            OsdEvent::ImageMode(value) => self.image_mode = value,
            OsdEvent::FileName(value) => self.file_name = value,
            OsdEvent::RawSensorSize(value) => self.raw_sensor_size = value,
            OsdEvent::RawEmbeddedPreview(value) => self.raw_embedded_preview = value,
            OsdEvent::RawRenderPixels(value) => self.raw_render_pixels = value,
            OsdEvent::RawDemosaicBackend(value) => self.raw_demosaic_backend = value,
            OsdEvent::RawCpuDemosaicMs(value) => self.raw_cpu_demosaic_ms = value,
            OsdEvent::RawGpuExtractMs(value) => self.raw_gpu_extract_ms = value,
            OsdEvent::RawGpuDemosaicMs(value) => self.raw_gpu_demosaic_ms = value,
            OsdEvent::HdrRenderPath(value) => self.hdr_render_path = value,
            OsdEvent::HdrColorSpace(value) => self.hdr_color_space = value,
            OsdEvent::HdrOutputMode(value) => self.hdr_output_mode = value,
            OsdEvent::HdrNativePresentationEnabled(value) => {
                self.hdr_native_presentation_enabled = value;
            }
            OsdEvent::UltraHdrDecodeCapacity(value) => self.ultra_hdr_decode_capacity = value,
            OsdEvent::HdrMonitorLabel(value) => self.hdr_monitor_label = value,
            OsdEvent::HdrExposureEv(value) => self.hdr_exposure_ev = value,
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
    osd_event_rx: crossbeam_channel::Receiver<OsdEvent>,
    cached_hud: String,
    cached_hdr_line_display: String,
    cached_raw_line: String,
    has_raw_line: bool,
    has_hdr_line: bool,
    layout: Option<LayoutLines>,
    layout_content_stamp: u64,
    layout_built_stamp: u64,
    measure_scratch: String,
    cached_loading_hint: String,
    cached_save_error: String,
    last_save_error_message: String,
    last_music_state: Option<OsdState>,
    supplemental_state: SupplementalOsdInputs,
}

impl OsdRenderer {
    pub fn new(osd_event_rx: crossbeam_channel::Receiver<OsdEvent>) -> Self {
        Self {
            osd_event_rx,
            cached_hud: String::new(),
            cached_hdr_line_display: String::new(),
            cached_raw_line: String::new(),
            has_raw_line: false,
            has_hdr_line: false,
            layout: None,
            layout_content_stamp: 0,
            layout_built_stamp: 0,
            measure_scratch: String::new(),
            cached_loading_hint: t!("status.loading").to_string(),
            cached_save_error: String::new(),
            last_save_error_message: String::new(),
            last_music_state: None,
            supplemental_state: SupplementalOsdInputs::default(),
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

    /// RAW/HDR supplemental lines are derived from a compact input snapshot.
    /// Callers update source state; this method handles dirty checking and cache rebuilds.
    pub fn sync_events(&mut self) {
        let mut changed = false;
        for event in self.osd_event_rx.try_iter() {
            self.supplemental_state.apply_event(event);
            changed = true;
        }
        if !changed {
            return;
        }

        self.rebuild_hud_from_state();
        self.refresh_cached_raw_line();
        self.refresh_cached_hdr_line();
        self.bump_content();
    }

    pub fn invalidate(&mut self) {
        self.last_music_state = None;
        self.bump_content();
    }

    /// Re-translate all cached OSD strings after a locale change.
    pub fn on_language_changed(&mut self) {
        self.cached_loading_hint = t!("status.loading").to_string();
        self.rebuild_hud_from_state();
        self.refresh_cached_raw_line();
        self.refresh_cached_hdr_line();
        self.bump_content();
    }

    fn refresh_cached_raw_line(&mut self) {
        let raw = crate::loader::RawOsdInfo::compose_osd_line(
            self.supplemental_state.raw_sensor_size,
            self.supplemental_state.raw_embedded_preview,
            self.supplemental_state.raw_render_pixels,
            self.supplemental_state.raw_demosaic_backend,
            self.supplemental_state.raw_cpu_demosaic_ms,
            self.supplemental_state.raw_gpu_extract_ms,
            self.supplemental_state.raw_gpu_demosaic_ms,
        );
        self.has_raw_line = raw.is_some();
        self.cached_raw_line.clear();
        if let Some(raw) = raw {
            self.cached_raw_line.push_str(&raw);
        }
    }

    fn rebuild_hud_from_state(&mut self) {
        let mode_label = match self.supplemental_state.image_mode {
            ImageOsdMode::Static => t!("osd.mode.static"),
            ImageOsdMode::Tiled => t!("osd.mode.tiled"),
        };
        let file_size_text = format_file_size(self.supplemental_state.file_size_bytes);
        self.cached_hud.clear();
        use std::fmt::Write as _;
        let _ = write!(
            self.cached_hud,
            "{} / {}    {}    {}    {}%    {}×{}    [{}]",
            self.supplemental_state.current_index + 1,
            self.supplemental_state.total_images,
            self.supplemental_state.file_name,
            file_size_text,
            self.supplemental_state.zoom_pct,
            self.supplemental_state.image_resolution.0,
            self.supplemental_state.image_resolution.1,
            mode_label,
        );
        self.bump_content();
    }

    fn refresh_cached_hdr_line(&mut self) {
        let hdr = self.supplemental_state.hdr_line_from_state();
        self.has_hdr_line = hdr.is_some();
        self.cached_hdr_line_display.clear();
        if let Some(hdr) = hdr {
            self.cached_hdr_line_display.push('[');
            self.cached_hdr_line_display.push_str(&hdr);
            self.cached_hdr_line_display.push(']');
        }
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
        palette: &ThemePalette,
        save_error: &Option<(String, Instant)>,
    ) {
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
    use super::{
        HdrOsdFrame, ImageOsdFrame, ImageOsdMode, OsdEvent, OsdRenderer, format_file_size,
        quantize_osd_zoom_pct,
    };
    use crate::hdr::status::HdrRenderPath;
    use crate::hdr::types::{HdrColorSpace, HdrOutputMode};
    use std::sync::Arc;

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

    #[test]
    fn supplemental_inputs_rebuild_only_when_derived_state_changes() {
        rust_i18n::set_locale("en");
        let (osd_event_tx, osd_event_rx) = crossbeam_channel::unbounded();
        let mut osd = OsdRenderer::new(osd_event_rx);
        let hdr = HdrOsdFrame {
            render_path: Some(HdrRenderPath::FloatImagePlane),
            color_space: Some(HdrColorSpace::Rec2020Linear),
            output_mode: HdrOutputMode::SdrToneMapped,
            native_presentation_enabled: false,
            ultra_hdr_decode_capacity: Some(1.25),
            monitor_label: Some("DISPLAY1"),
            exposure_ev: 0.0,
        };
        let image = ImageOsdFrame {
            index: 3,
            total: 9,
            zoom_pct: 102,
            res: (4000, 3000),
            file_size_bytes: 1536,
            mode: ImageOsdMode::Static,
        };

        let _ = osd_event_tx.send(OsdEvent::CurrentIndex(3));
        let _ = osd_event_tx.send(OsdEvent::TotalImages(image.total));
        let _ = osd_event_tx.send(OsdEvent::ZoomPct(image.cache_key().zoom_pct));
        let _ = osd_event_tx.send(OsdEvent::ImageResolution(image.res));
        let _ = osd_event_tx.send(OsdEvent::FileSizeBytes(image.file_size_bytes));
        let _ = osd_event_tx.send(OsdEvent::ImageMode(image.mode));
        let _ = osd_event_tx.send(OsdEvent::FileName(Arc::from("image.jpg")));
        let _ = osd_event_tx.send(OsdEvent::RawSensorSize((6000, 4000)));
        let _ = osd_event_tx.send(OsdEvent::RawEmbeddedPreview(Some((1920, 1280))));
        let _ = osd_event_tx.send(OsdEvent::RawRenderPixels(
            crate::loader::RawRenderPixels::HqBootstrap {
                width: 1920,
                height: 1280,
            },
        ));
        let _ = osd_event_tx.send(OsdEvent::RawDemosaicBackend(Some(
            crate::loader::RawDemosaicBackend::Host,
        )));
        let _ = osd_event_tx.send(OsdEvent::HdrRenderPath(hdr.render_path));
        let _ = osd_event_tx.send(OsdEvent::HdrColorSpace(hdr.color_space));
        let _ = osd_event_tx.send(OsdEvent::HdrOutputMode(hdr.output_mode));
        let _ = osd_event_tx.send(OsdEvent::HdrNativePresentationEnabled(
            hdr.native_presentation_enabled,
        ));
        let _ = osd_event_tx.send(OsdEvent::UltraHdrDecodeCapacity(
            hdr.ultra_hdr_decode_capacity,
        ));
        let _ = osd_event_tx.send(OsdEvent::HdrMonitorLabel(hdr.monitor_label.map(Arc::from)));
        let _ = osd_event_tx.send(OsdEvent::HdrExposureEv(hdr.exposure_ev));
        osd.sync_events();
        let first_stamp = osd.layout_content_stamp;
        assert!(osd.cached_hud.contains("4 / 9"));
        assert!(osd.cached_hud.contains("image.jpg"));
        assert!(osd.cached_hud.contains("100%"));
        assert!(osd.has_raw_line());
        assert!(osd.has_hdr_line());
        assert!(osd.cached_raw_line.contains("6000x4000"));
        assert!(osd.cached_raw_line.contains("1920x1280"));
        assert!(osd.cached_hdr_line_display.contains("+0.0 EV"));

        osd.sync_events();
        assert_eq!(osd.layout_content_stamp, first_stamp);

        let _ = osd_event_tx.send(OsdEvent::HdrExposureEv(1.5));
        osd.sync_events();
        assert!(osd.layout_content_stamp > first_stamp);
        assert!(osd.cached_hdr_line_display.contains("+1.5 EV"));
    }
}
