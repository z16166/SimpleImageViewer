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
use crate::ui::dialogs::MovableModal;
use crate::ui::dialogs::modal_state::{ModalAction, ModalResult};
use crate::ui::utils::styled_button;
use eframe::egui::{self, Context, RichText};
use rust_i18n::t;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorOption {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WallpaperTarget {
    AllMonitors,
    Monitor(String),
}

// ── Private state ─────────────────────────────────────────────────────────────

/// Runtime state for the wallpaper mode selector dialog.
///
/// Fields are private; other modules can only create an instance via
/// [`State::new`] and cannot inspect or mutate the internals directly.
pub struct State {
    /// The wallpaper fitting mode chosen by the radio buttons.
    pub selected_mode: String,
    /// Path of the currently active desktop wallpaper (display only).
    pub current_system_wallpaper: Option<String>,
    pub loading: bool,
    pub supports_per_monitor: bool,
    pub monitor_options: Vec<MonitorOption>,
    pub selected_target: WallpaperTarget,
}

impl State {
    /// Create state in loading mode.
    pub fn new_loading() -> Self {
        Self {
            selected_mode: "Crop".to_string(),
            current_system_wallpaper: None,
            loading: true,
            supports_per_monitor: false,
            monitor_options: Vec::new(),
            selected_target: WallpaperTarget::AllMonitors,
        }
    }

    pub fn apply_wallpaper_probe(
        &mut self,
        current: Option<String>,
        monitor_options: Vec<MonitorOption>,
        supports_per_monitor: bool,
    ) {
        self.current_system_wallpaper = current;
        self.monitor_options = monitor_options;
        self.supports_per_monitor = supports_per_monitor;
        if self.supports_per_monitor && self.monitor_options.len() > 1 {
            if let Some(first_monitor) = self.monitor_options.first() {
                self.selected_target = WallpaperTarget::Monitor(first_monitor.id.clone());
            } else {
                self.selected_target = WallpaperTarget::AllMonitors;
            }
        } else {
            self.selected_target = WallpaperTarget::AllMonitors;
        }
        self.loading = false;
    }
}

// ── Rendering ─────────────────────────────────────────────────────────────────

/// Render the wallpaper mode selector modal for one frame.
pub fn show(
    state: &mut State,
    current_image_path: &str,
    current_image_res: Option<(u32, u32)>,
    ctx: &Context,
    palette: &ThemePalette,
) -> ModalResult {
    let mut result = ModalResult::Pending;

    const WIDTH: f32 = 520.0;
    const HEIGHT: f32 = 320.0;

    MovableModal::new("wallpaper_dialog", t!("wallpaper.title"))
        .default_size([WIDTH, HEIGHT])
        .min_size([400.0, 240.0])
        .show(ctx, palette, |ui| {
            if state.loading {
                ui.add_space(20.0);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(t!("wallpaper.loading").to_string());
                });
                ui.add_space(20.0);
            } else if let Some(ref current) = state.current_system_wallpaper {
                ui.label(
                    RichText::new(t!("wallpaper.current"))
                        .color(palette.text_muted)
                        .small(),
                );
                egui::ScrollArea::horizontal()
                    .id_salt("curr_wp_scroll")
                    .min_scrolled_height(24.0)
                    .show(ui, |ui| {
                        ui.vertical(|ui| {
                            ui.add_space(2.0);
                            ui.add(
                                egui::Label::new(current)
                                    .selectable(true)
                                    .wrap_mode(egui::TextWrapMode::Extend),
                            );
                            ui.add_space(4.0);
                        });
                    });
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(8.0);
            }

            let show_target = state.supports_per_monitor && state.monitor_options.len() > 1;
            let single_monitor_target_selected =
                show_target && !matches!(state.selected_target, WallpaperTarget::AllMonitors);
            if show_target {
                ui.label(
                    RichText::new(t!("wallpaper.target"))
                        .color(palette.accent2)
                        .strong(),
                );
                ui.radio_value(
                    &mut state.selected_target,
                    WallpaperTarget::AllMonitors,
                    t!("wallpaper.target_all").to_string(),
                );
                for monitor in &state.monitor_options {
                    ui.radio_value(
                        &mut state.selected_target,
                        WallpaperTarget::Monitor(monitor.id.clone()),
                        monitor.label.clone(),
                    );
                }
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(8.0);
            }

            ui.label(
                RichText::new(t!("wallpaper.new_path"))
                    .color(palette.text_muted)
                    .small(),
            );
            egui::ScrollArea::horizontal()
                .id_salt("new_wp_scroll")
                .min_scrolled_height(24.0)
                .show(ui, |ui| {
                    ui.vertical(|ui| {
                        ui.add_space(2.0);
                        ui.add(
                            egui::Label::new(current_image_path)
                                .selectable(true)
                                .wrap_mode(egui::TextWrapMode::Extend),
                        );
                        ui.add_space(4.0);
                    });
                });

            if let Some((w, h)) = current_image_res {
                ui.add_space(4.0);
                ui.label(
                    RichText::new(t!("wallpaper.resolution"))
                        .color(palette.text_muted)
                        .small(),
                );
                ui.label(format!("{} × {}", w, h));
            }

            ui.add_space(12.0);
            ui.separator();
            ui.add_space(8.0);
            ui.label(
                RichText::new(t!("wallpaper.mode"))
                    .color(palette.accent2)
                    .strong(),
            );

            let mut mode_items = vec![
                ("Crop", t!("wallpaper.crop").to_string()),
                ("Fit", t!("wallpaper.fit").to_string()),
                ("Stretch", t!("wallpaper.stretch").to_string()),
                ("Tile", t!("wallpaper.tile").to_string()),
                ("Center", t!("wallpaper.center").to_string()),
            ];
            // Span is meaningful only when applying a single image across all monitors.
            if !single_monitor_target_selected {
                mode_items.push(("Span", t!("wallpaper.span").to_string()));
            }
            egui::Grid::new("wallpaper_mode_grid")
                .num_columns(3)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    for (idx, (value, label)) in mode_items.iter().enumerate() {
                        ui.radio_value(&mut state.selected_mode, (*value).to_string(), label);
                        if idx % 3 == 2 {
                            ui.end_row();
                        }
                    }
                });

            if single_monitor_target_selected && state.selected_mode == "Span" {
                state.selected_mode = "Crop".to_string();
            }

            ui.add_space(16.0);
            ui.horizontal(|ui| {
                if styled_button(ui, &t!("btn.set_wallpaper").to_string(), palette).clicked() {
                    result = ModalResult::Confirmed(ModalAction::SetWallpaper {
                        mode: state.selected_mode.clone(),
                        target: state.selected_target.clone(),
                    });
                }
                if styled_button(ui, &t!("btn.cancel").to_string(), palette).clicked() {
                    result = ModalResult::Dismissed;
                }
            });

            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                result = ModalResult::Dismissed;
            }
        });

    result
}

// ── Side-effect helper ────────────────────────────────────────────────────────

/// Spawn a background thread to actually change the desktop wallpaper.
///
/// This is kept here (not in the dispatch layer) because it is an
/// implementation detail of the wallpaper operation, not a generic concern.
pub fn apply(path: PathBuf, mode_str: &str, target: &WallpaperTarget) {
    let mode = str_to_mode(mode_str);
    let mode_str = mode_str.to_string();
    let target = target.clone();
    std::thread::spawn(move || {
        // TODO(wallpaper): Add a compatibility conversion fallback (e.g. PNG/JPG)
        // for formats that the OS wallpaper API cannot apply directly.
        if let Err(e) = apply_windows_per_monitor(path.clone(), &mode_str, &target) {
            log::debug!(
                "[wallpaper] per-monitor apply failed, falling back to global set: {}",
                e
            );
            let _ = wallpaper::set_mode(mode);
            if let Err(e) = wallpaper::set_from_path(path.to_string_lossy().as_ref()) {
                log::error!("Failed to set wallpaper: {e}");
            }
        }
    });
}

#[cfg(target_os = "windows")]
pub fn probe_windows_wallpaper_targets() -> (Vec<MonitorOption>, bool) {
    use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};
    use windows::Win32::UI::Shell::{DesktopWallpaper, IDesktopWallpaper};
    use windows::core::PWSTR;

    let _com = match crate::wic::ComGuard::new() {
        Ok(guard) => guard,
        Err(_) => return (Vec::new(), false),
    };

    unsafe {
        let mut monitors = Vec::new();
        let mut supports = false;
        let result = (|| -> windows::core::Result<()> {
            let desktop: IDesktopWallpaper = CoCreateInstance(&DesktopWallpaper, None, CLSCTX_ALL)?;
            let count = desktop.GetMonitorDevicePathCount()?;
            supports = count > 1;
            for i in 0..count {
                let id: PWSTR = desktop.GetMonitorDevicePathAt(i)?;
                let id_string = id.to_string()?;
                let rect = desktop.GetMonitorRECT(&windows::core::HSTRING::from(&id_string))?;
                let w = (rect.right - rect.left).max(0);
                let h = (rect.bottom - rect.top).max(0);
                let desc = windows_monitor_desc_from_rect(rect)
                    .unwrap_or_else(|| windows_monitor_desc_from_id(&id_string));
                let label = t!(
                    "wallpaper.target_monitor",
                    index = (i + 1).to_string(),
                    width = w.to_string(),
                    height = h.to_string(),
                    desc = desc
                )
                .to_string();
                monitors.push(MonitorOption {
                    id: id_string,
                    label,
                });
            }
            Ok(())
        })();

        if result.is_err() {
            return (Vec::new(), false);
        }
        (monitors, supports)
    }
}

#[cfg(target_os = "windows")]
fn windows_monitor_desc_from_id(id: &str) -> String {
    let trimmed = id.trim_matches(char::from(0));
    let last = trimmed.rsplit('#').next().unwrap_or(trimmed);
    if last.is_empty() {
        "Unknown".to_string()
    } else {
        last.to_string()
    }
}

#[cfg(target_os = "windows")]
fn windows_monitor_desc_from_rect(rect: windows::Win32::Foundation::RECT) -> Option<String> {
    use windows::Win32::Graphics::Gdi::{
        DISPLAY_DEVICEW, EnumDisplayDevicesW, GetMonitorInfoW, MONITOR_DEFAULTTONEAREST,
        MONITORINFOEXW, MonitorFromRect,
    };
    use windows::core::PCWSTR;

    unsafe {
        let hmonitor = MonitorFromRect(&rect, MONITOR_DEFAULTTONEAREST);
        if hmonitor.is_invalid() {
            return None;
        }
        let mut info = MONITORINFOEXW {
            monitorInfo: windows::Win32::Graphics::Gdi::MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFOEXW>() as u32,
                ..Default::default()
            },
            ..Default::default()
        };
        if !GetMonitorInfoW(hmonitor, &mut info.monitorInfo as *mut _ as *mut _).as_bool() {
            return None;
        }

        let mut dd = DISPLAY_DEVICEW {
            cb: std::mem::size_of::<DISPLAY_DEVICEW>() as u32,
            ..Default::default()
        };
        let ok = EnumDisplayDevicesW(PCWSTR(info.szDevice.as_ptr()), 0, &mut dd, 0);
        if !ok.as_bool() {
            return None;
        }
        let raw = String::from_utf16_lossy(&dd.DeviceString);
        let name = raw.trim_matches(char::from(0)).trim().to_string();
        if name.is_empty() { None } else { Some(name) }
    }
}

#[cfg(not(target_os = "windows"))]
pub fn probe_windows_wallpaper_targets() -> (Vec<MonitorOption>, bool) {
    (Vec::new(), false)
}

#[cfg(target_os = "windows")]
fn apply_windows_per_monitor(
    path: PathBuf,
    mode_str: &str,
    target: &WallpaperTarget,
) -> Result<(), String> {
    use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};
    use windows::Win32::UI::Shell::{
        DESKTOP_WALLPAPER_POSITION, DWPOS_CENTER, DWPOS_FILL, DWPOS_FIT, DWPOS_SPAN, DWPOS_STRETCH,
        DWPOS_TILE, DesktopWallpaper, IDesktopWallpaper,
    };
    use windows::core::HSTRING;

    let abs_path = std::fs::canonicalize(&path).unwrap_or(path);
    let path_str = abs_path.to_string_lossy().to_string();

    let _com = crate::wic::ComGuard::new().map_err(|e| format!("CoInitializeEx failed: {e}"))?;

    unsafe {
        let desktop: IDesktopWallpaper = CoCreateInstance(&DesktopWallpaper, None, CLSCTX_ALL)
            .map_err(|e| format!("CoCreateInstance failed: {e}"))?;

        let position: DESKTOP_WALLPAPER_POSITION = match mode_str {
            "Center" => DWPOS_CENTER,
            "Fit" => DWPOS_FIT,
            "Stretch" => DWPOS_STRETCH,
            "Tile" => DWPOS_TILE,
            "Span" => DWPOS_SPAN,
            _ => DWPOS_FILL,
        };
        let _ = desktop.SetPosition(position);

        let path_h = HSTRING::from(path_str);
        match target {
            WallpaperTarget::AllMonitors => {
                let count = desktop
                    .GetMonitorDevicePathCount()
                    .map_err(|e| format!("GetMonitorDevicePathCount failed: {e}"))?;
                for i in 0..count {
                    let id = desktop
                        .GetMonitorDevicePathAt(i)
                        .map_err(|e| format!("GetMonitorDevicePathAt failed: {e}"))?;
                    let id_h = HSTRING::from(
                        id.to_string()
                            .map_err(|e| format!("Monitor ID UTF-16 conversion failed: {e}"))?,
                    );
                    desktop
                        .SetWallpaper(&id_h, &path_h)
                        .map_err(|e| format!("SetWallpaper(all) failed: {e}"))?;
                }
            }
            WallpaperTarget::Monitor(monitor_id) => {
                let id_h = HSTRING::from(monitor_id);
                desktop
                    .SetWallpaper(&id_h, &path_h)
                    .map_err(|e| format!("SetWallpaper(one) failed: {e}"))?;
            }
        }
        Ok(())
    }
}

#[cfg(not(target_os = "windows"))]
fn apply_windows_per_monitor(
    _path: PathBuf,
    _mode_str: &str,
    _target: &WallpaperTarget,
) -> Result<(), String> {
    Err("per-monitor wallpaper is Windows-only".to_string())
}

fn str_to_mode(s: &str) -> wallpaper::Mode {
    match s {
        "Center" => wallpaper::Mode::Center,
        "Crop" => wallpaper::Mode::Crop,
        "Fit" => wallpaper::Mode::Fit,
        "Span" => wallpaper::Mode::Span,
        "Stretch" => wallpaper::Mode::Stretch,
        "Tile" => wallpaper::Mode::Tile,
        _ => wallpaper::Mode::Crop,
    }
}
