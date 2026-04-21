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

use eframe::egui::Color32;
use serde::{Deserialize, Serialize};
use std::time::Instant;

// ---------------------------------------------------------------------------
// AppTheme enum (persisted to settings.yaml)
// ---------------------------------------------------------------------------

/// The user-visible theme preference.
///
/// Adding a new theme in the future only requires:
/// 1. Adding a variant here.
/// 2. Implementing `ThemePalette::for_variant()` for it.
/// 3. Adding a UI entry in the settings dropdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AppTheme {
    /// Deep-dark purple theme (original style).
    #[default]
    Dark,
    /// Clean light theme.
    Light,
    /// Follows the OS dark/light preference. Falls back to Dark on
    /// non-Windows platforms (where detection is not yet implemented).
    System,
}

// ---------------------------------------------------------------------------
// ThemePalette — all semantic colour slots used across the app
// ---------------------------------------------------------------------------

/// A fully resolved set of colours for one frame.
///
/// Every colour used by the UI should come from here.
/// `setup_visuals` and all draw helpers accept a `&ThemePalette`.
#[derive(Clone)]
pub struct ThemePalette {
    // ── Background layers ────────────────────────────────────────────────────
    /// Main canvas background (behind the image).
    pub canvas_bg: Color32,
    /// Settings panel / popup window background.
    pub panel_bg: Color32,
    /// Widget interiors: textboxes, comboboxes, sliders.
    pub widget_bg: Color32,
    /// Widget background when hovered.
    pub widget_hover: Color32,
    /// Widget background when active/pressed.
    pub widget_active: Color32,
    /// Deepest dark — used for extreme_bg_color.
    pub extreme_bg: Color32,

    // ── Borders ──────────────────────────────────────────────────────────────
    pub widget_border: Color32,
    pub widget_border_hover: Color32,

    // ── Scrollbars ───────────────────────────────────────────────────────────
    pub scrollbar_handle: Color32,

    // ── Text ─────────────────────────────────────────────────────────────────
    pub text_normal: Color32,
    pub text_muted: Color32,

    // ── Accent ───────────────────────────────────────────────────────────────
    /// Primary action colour (buttons, selection, active widgets).
    pub accent: Color32,
    /// Secondary accent used for section headings.
    pub accent2: Color32,

    // ── Empty-state hint ─────────────────────────────────────────────────────
    pub hint_icon: Color32,
    pub hint_text: Color32,

    // ── OSD (On-Screen Display) ──────────────────────────────────────────────
    pub osd_text: Color32,
    pub osd_hint: Color32,

    pub is_dark: bool,
}

impl ThemePalette {
    // ------------------------------------------------------------------
    // Dark palette (original deep-purple style)
    // ------------------------------------------------------------------
    pub fn dark() -> Self {
        Self {
            canvas_bg: Color32::from_rgb(13, 13, 15), // Obsidian
            panel_bg: Color32::from_rgb(24, 25, 27),  // Charcoal Black
            widget_bg: Color32::from_gray(65),        // Increased from 48 for better contrast
            widget_hover: Color32::from_gray(80),
            widget_active: Color32::from_gray(100),
            extreme_bg: Color32::from_gray(10),

            widget_border: Color32::from_gray(75),
            widget_border_hover: Color32::from_gray(110),

            scrollbar_handle: Color32::from_gray(160), // Increased from 100 for better contrast

            text_normal: Color32::from_rgb(240, 240, 240),
            text_muted: Color32::from_gray(210), // Significantly lightened for small-text legibility

            accent: Color32::from_rgb(74, 144, 226), // Steel Blue
            accent2: Color32::from_rgb(0, 212, 180), // Cool Silver

            hint_icon: Color32::from_gray(60),
            hint_text: Color32::from_gray(100),

            osd_text: Color32::from_rgb(240, 240, 240), // Fully opaque and brighter
            osd_hint: Color32::from_rgb(180, 180, 185), // Fully opaque and brighter

            is_dark: true,
        }
    }

    // ------------------------------------------------------------------
    // Light palette (clean, Apple-inspired)
    // ------------------------------------------------------------------
    pub fn light() -> Self {
        Self {
            canvas_bg: Color32::from_rgb(242, 239, 233), // Soft Sand
            panel_bg: Color32::from_rgb(252, 250, 247),  // Warm Bone White
            widget_bg: Color32::from_rgb(241, 243, 244),
            widget_hover: Color32::from_rgb(232, 234, 237),
            widget_active: Color32::from_rgb(218, 220, 224),
            extreme_bg: Color32::from_rgb(242, 239, 233),

            widget_border: Color32::from_rgb(218, 220, 224),
            widget_border_hover: Color32::from_rgb(154, 160, 166),

            scrollbar_handle: Color32::from_rgb(218, 220, 224),

            text_normal: Color32::from_rgb(27, 38, 59), // Midnight Blue Ink
            text_muted: Color32::from_rgb(95, 99, 104),

            accent: Color32::from_rgb(27, 38, 59), // Midnight Blue
            accent2: Color32::from_rgb(74, 93, 78), // Deep Sage

            hint_icon: Color32::from_rgb(218, 220, 224),
            hint_text: Color32::from_rgb(128, 134, 139),

            osd_text: Color32::from_rgba_unmultiplied(27, 38, 59, 200),
            osd_hint: Color32::from_rgba_unmultiplied(95, 99, 104, 160),

            is_dark: false,
        }
    }
}

// ---------------------------------------------------------------------------
// System dark-mode detection (Windows only, 5-second poll)
// ---------------------------------------------------------------------------

/// Opaque state used by `AppTheme::resolve` to avoid polling every frame.
pub struct SystemThemeCache {
    /// When we last checked the registry.
    last_check: Instant,
    /// The cached result: `true` = system is in dark mode.
    is_dark: bool,
    /// The theme that was last resolved (to detect external changes).
    last_resolved: Option<(AppTheme, bool)>,
}

impl Default for SystemThemeCache {
    fn default() -> Self {
        Self {
            // Set to a very old time so the first call always refreshes
            last_check: Instant::now()
                .checked_sub(std::time::Duration::from_secs(60))
                .unwrap_or_else(Instant::now),
            is_dark: true,
            last_resolved: None,
        }
    }
}

impl AppTheme {
    /// Return the resolved `ThemePalette` for this theme.
    ///
    /// For `System`, polls the OS at most every 5 s using `cache` to avoid
    /// any per-frame overhead. The caller should pass in a long-lived
    /// `SystemThemeCache` stored in `ImageViewerApp`.
    pub fn resolve(&self, cache: &mut SystemThemeCache) -> ThemePalette {
        match self {
            AppTheme::Dark => ThemePalette::dark(),
            AppTheme::Light => ThemePalette::light(),
            AppTheme::System => {
                let now = Instant::now();
                if now.duration_since(cache.last_check).as_secs() >= 5 {
                    cache.last_check = now;
                    cache.is_dark = detect_system_dark_mode();
                }
                if cache.is_dark {
                    ThemePalette::dark()
                } else {
                    ThemePalette::light()
                }
            }
        }
    }

    /// Like `resolve`, but returns `Some` only when the palette has actually
    /// changed since the last call (theme switch or OS dark/light toggle).
    /// Returns `None` on no-change frames, avoiding struct construction overhead.
    pub fn resolve_if_changed(&self, cache: &mut SystemThemeCache) -> Option<ThemePalette> {
        // For System theme, refresh the OS detection periodically
        if *self == AppTheme::System {
            let now = Instant::now();
            if now.duration_since(cache.last_check).as_secs() >= 5 {
                cache.last_check = now;
                cache.is_dark = detect_system_dark_mode();
            }
        }

        let effective_dark = match self {
            AppTheme::Dark => true,
            AppTheme::Light => false,
            AppTheme::System => cache.is_dark,
        };

        let key = (*self, effective_dark);
        if cache.last_resolved == Some(key) {
            return None; // Nothing changed
        }
        cache.last_resolved = Some(key);
        Some(match self {
            AppTheme::Dark => ThemePalette::dark(),
            AppTheme::Light => ThemePalette::light(),
            AppTheme::System => {
                if cache.is_dark {
                    ThemePalette::dark()
                } else {
                    ThemePalette::light()
                }
            }
        })
    }

    /// Returns the effective boolean "is dark?" for the *current* state.
    /// Used to decide whether to call `setup_visuals` after a change.
    pub fn effective_is_dark(&self, cache: &mut SystemThemeCache) -> bool {
        match self {
            AppTheme::Dark => true,
            AppTheme::Light => false,
            AppTheme::System => {
                // Reuse cached value (resolve() keeps it up to date)
                cache.is_dark
            }
        }
    }
}

/// Detect whether the OS is in dark mode.
///
/// On Windows: reads `HKCU\Software\Microsoft\Windows\CurrentVersion\
/// Themes\Personalize\AppsUseLightTheme`
/// (0 = dark, 1 = light).
///
/// On non-Windows: always returns `true` (fall back to Dark theme).
fn detect_system_dark_mode() -> bool {
    #[cfg(target_os = "windows")]
    {
        return windows_is_dark_mode();
    }
    #[cfg(target_os = "macos")]
    {
        return macos_is_dark_mode();
    }
    #[cfg(target_os = "linux")]
    {
        return linux_is_dark_mode();
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        true
    }
}

#[cfg(target_os = "linux")]
fn linux_is_dark_mode() -> bool {
    use std::process::Command;

    // 1. Try XDG Settings Portal (Modern GNOME, KDE, and others)
    // Values: 0 = no preference, 1 = prefer-dark, 2 = prefer-light
    if let Ok(output) = Command::new("gdbus")
        .args(&[
            "call", "--session", 
            "--dest", "org.freedesktop.portal.Desktop", 
            "--object-path", "/org/freedesktop/portal/desktop", 
            "--method", "org.freedesktop.portal.Settings.Read", 
            "org.freedesktop.appearance", "color-scheme"
        ])
        .output() 
    {
        let s = String::from_utf8_lossy(&output.stdout);
        if s.contains("uint32 1") {
            return true;
        } else if s.contains("uint32 2") {
            return false;
        }
    }

    // 2. Try GNOME-specific setting (gsettings)
    if let Ok(output) = Command::new("gsettings")
        .args(&["get", "org.gnome.desktop.interface", "color-scheme"])
        .output() 
    {
        let s = String::from_utf8_lossy(&output.stdout);
        if s.contains("prefer-dark") {
            return true;
        } else if s.contains("prefer-light") {
            return false;
        }
    }

    // 3. Try KDE-specific setting (kreadconfig)
    // Check kreadconfig6 first, then kreadconfig5
    for cmd in &["kreadconfig6", "kreadconfig5"] {
        if let Ok(output) = Command::new(cmd)
            .args(&["--group", "General", "--key", "ColorScheme"])
            .output() 
        {
            let s = String::from_utf8_lossy(&output.stdout).to_lowercase();
            if !s.is_empty() {
                return s.contains("dark");
            }
        }
    }

    // Default to dark if detection fails
    true
}

#[cfg(target_os = "macos")]
fn macos_is_dark_mode() -> bool {
    use std::process::Command;
    // defaults read -g AppleInterfaceStyle returns "Dark"
    // If it's not set, the command fails with exit code 1, which means Light mode.
    if let Ok(output) = Command::new("defaults")
        .args(&["read", "-g", "AppleInterfaceStyle"])
        .output() 
    {
        let s = String::from_utf8_lossy(&output.stdout);
        return s.trim() == "Dark";
    }
    false
}

#[cfg(target_os = "windows")]
fn windows_is_dark_mode() -> bool {
    // We call RegGetValueW directly to avoid a heavy dependency.
    // The registry value is a DWORD (u32): 0 = dark mode, 1 = light mode.
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    fn to_wide(s: &str) -> Vec<u16> {
        OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    unsafe {
        unsafe extern "system" {
            fn RegGetValueW(
                h_key: *mut std::ffi::c_void,
                lp_sub_key: *const u16,
                lp_value: *const u16,
                dw_flags: u32,
                pdw_type: *mut u32,
                pv_data: *mut std::ffi::c_void,
                pcb_data: *mut u32,
            ) -> i32;
        }

        // HKEY_CURRENT_USER predefined handle
        const HKCU: *mut std::ffi::c_void = 0x80000001u64 as *mut std::ffi::c_void;
        // RRF_RT_REG_DWORD = 0x10
        const RRF_RT_REG_DWORD: u32 = 0x10;

        let sub_key = to_wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
        let value = to_wide("AppsUseLightTheme");

        let mut data: u32 = 0;
        let mut data_size: u32 = std::mem::size_of::<u32>() as u32;

        let result = RegGetValueW(
            HKCU,
            sub_key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            &mut data as *mut u32 as *mut std::ffi::c_void,
            &mut data_size,
        );

        if result == 0 {
            // 0 = dark mode, 1 = light mode
            data == 0
        } else {
            // On error, assume dark (safe default)
            true
        }
    }
}
