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

use crate::theme::AppTheme;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// ScaleMode
// ---------------------------------------------------------------------------

/// How the image is scaled for display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScaleMode {
    /// Fit the image inside the current window, preserving aspect ratio.
    FitToWindow,
    /// Display at the image's natural pixel size (1 logical unit per pixel).
    OriginalSize,
}

impl ScaleMode {
    pub fn toggled(self) -> Self {
        match self {
            Self::FitToWindow => Self::OriginalSize,
            Self::OriginalSize => Self::FitToWindow,
        }
    }
}

// ---------------------------------------------------------------------------
// TransitionStyle
// ---------------------------------------------------------------------------

macro_rules! define_transition_styles {
    ($($variant:ident => $key:expr),*) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum TransitionStyle {
            None,
            $($variant),*,
            Random,
        }

        impl TransitionStyle {
            /// List of styles used for random selection, automatically synced with the enum
            pub const RANDOM_POOL: &[TransitionStyle] = &[
                $(Self::$variant),*
            ];

            pub fn label(self) -> String {
                match self {
                    Self::None => rust_i18n::t!("transition.none").to_string(),
                    Self::Random => rust_i18n::t!("transition.random").to_string(),
                    $(Self::$variant => rust_i18n::t!($key).to_string()),*
                }
            }
        }
    }
}

define_transition_styles!(
    Fade     => "transition.fade",
    ZoomFade => "transition.zoom_fade",
    Slide    => "transition.slide",
    Push     => "transition.push",
    PageFlip => "transition.page_flip",
    Ripple   => "transition.ripple",
    Curtain  => "transition.curtain"
);

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    // Image browsing
    #[serde(default)]
    pub recursive: bool,
    /// Not persisted — app always starts windowed so the OS title bar is visible.
    #[serde(skip)]
    pub fullscreen: bool,
    #[serde(default)]
    pub last_image_dir: Option<PathBuf>,
    #[serde(default = "default_true")]
    pub preload: bool,
    #[serde(default)]
    pub skip_raw_if_jpeg_exists: bool,

    // Session resumption
    #[serde(default)]
    pub resume_last_image: bool,
    #[serde(default)]
    pub last_viewed_image: Option<PathBuf>,

    // Auto-switch
    #[serde(default)]
    pub auto_switch: bool,
    #[serde(default = "default_interval")]
    pub auto_switch_interval: f32,
    #[serde(default)]
    pub random_slideshow_order: bool,

    // Scale / view
    #[serde(default)]
    pub scale_mode: ScaleMode,

    // Transitions
    #[serde(default = "default_transition_style")]
    pub transition_style: TransitionStyle,
    #[serde(default = "default_transition_ms")]
    pub transition_ms: u32,

    // Music
    #[serde(default)]
    pub play_music: bool,
    #[serde(default)]
    pub music_path: Option<PathBuf>,
    #[serde(default = "default_volume")]
    pub volume: f32,
    #[serde(default)]
    pub music_paused: bool,
    #[serde(default)]
    pub last_music_file: Option<PathBuf>,
    #[serde(default)]
    pub last_music_cue_track: Option<usize>,
    #[serde(default)]
    pub audio_device: Option<String>,

    // Font & Appearance
    #[serde(default = "default_font_family")]
    pub font_family: String,
    #[serde(default = "default_font_size")]
    pub font_size: f32,

    // Overlay (OSD)
    #[serde(default = "default_true")]
    pub show_osd: bool,
    #[serde(default = "default_true")]
    pub show_music_osd: bool,

    // RAW Processing
    #[serde(default)]
    pub raw_high_quality: bool,

    // HDR tone mapping
    /// Request a native HDR swap chain (Windows scRGB / macOS EDR / Wayland HDR10).
    /// On Linux X11 this is ignored; use [`Settings::hdr_native_surface_enabled_effective`].
    #[serde(default = "default_hdr_native_surface_enabled")]
    pub hdr_native_surface_enabled: bool,
    /// EV scale for **native HDR** presentation (`Rgba16Float` / PQ / gamma‑2.2 HDR paths).
    /// `hdr_exposure_ev` in persisted YAML aliases here for backwards compatibility.
    #[serde(default, alias = "hdr_exposure_ev")]
    pub hdr_exposure_ev_native: f32,
    /// EV scale when tone‑mapping into an **SDR swap chain** (8‑bit etc.) or matching CPU previews.
    #[serde(default)]
    pub hdr_exposure_ev_sdr: f32,
    #[serde(default = "default_hdr_sdr_white_nits")]
    pub hdr_sdr_white_nits: f32,
    #[serde(default = "default_hdr_max_display_nits")]
    pub hdr_max_display_nits: f32,

    // Language (locale code: "en", "zh-CN", "zh-HK")
    #[serde(default)]
    pub language: String,

    // Theme
    #[serde(default)]
    pub theme: AppTheme,

    // Window placement (persisted so the app reopens on the same monitor it
    // last closed on — important on multi-monitor systems where the user has
    // mixed HDR + SDR displays and wants to control which one HDR rendering
    // is exercised on).
    #[serde(default)]
    pub window_outer_position: Option<[i32; 2]>,
    #[serde(default)]
    pub window_inner_size: Option<[u32; 2]>,
    /// Last non-maximized outer top-left. Kept when closing maximized so the
    /// next session can recreate at restore size/position before maximizing.
    #[serde(default)]
    pub window_restore_outer_position: Option<[i32; 2]>,
    /// Last non-maximized client size. Same role as [`Self::window_restore_outer_position`].
    #[serde(default)]
    pub window_restore_inner_size: Option<[u32; 2]>,
    /// Last observed client size while maximized. Used only to size the hidden
    /// first frame so maximized startup does not redraw the image at a new size
    /// immediately after the window becomes visible.
    #[serde(default)]
    pub window_maximized_inner_size: Option<[u32; 2]>,
    #[serde(default)]
    pub window_maximized: bool,
    /// Screen-space center of the window when last closed maximized. Used to
    /// recreate on the same monitor when Windows reports a maximized-position
    /// artifact (e.g. `[-7,-7]`) instead of a restorable outer top-left.
    #[serde(default)]
    pub window_maximized_screen_center: Option<[i32; 2]>,
}

fn default_interval() -> f32 {
    5.0
}
fn default_true() -> bool {
    true
}

fn default_hdr_native_surface_enabled() -> bool {
    if cfg!(target_os = "linux") {
        crate::hdr::platform::linux_native_hdr_platform_eligible()
    } else {
        true
    }
}
fn default_volume() -> f32 {
    1.0
}
fn default_font_family() -> String {
    "System Default".to_string()
}
fn default_font_size() -> f32 {
    14.0
}
fn default_transition_style() -> TransitionStyle {
    TransitionStyle::None
}
fn default_transition_ms() -> u32 {
    800
}
fn default_hdr_sdr_white_nits() -> f32 {
    crate::hdr::types::DEFAULT_SDR_WHITE_NITS
}
fn default_hdr_max_display_nits() -> f32 {
    crate::hdr::types::DEFAULT_MAX_DISPLAY_NITS
}
impl Default for ScaleMode {
    fn default() -> Self {
        Self::FitToWindow
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            recursive: false,
            fullscreen: false,
            last_image_dir: None,
            auto_switch: false,
            auto_switch_interval: default_interval(),
            random_slideshow_order: false,
            scale_mode: ScaleMode::FitToWindow,
            transition_style: default_transition_style(),
            transition_ms: default_transition_ms(),
            play_music: false,
            music_path: None,
            volume: default_volume(),
            font_family: default_font_family(),
            font_size: default_font_size(),
            preload: true,
            skip_raw_if_jpeg_exists: false,
            resume_last_image: false,
            last_viewed_image: None,
            show_osd: true,
            show_music_osd: true,
            music_paused: false,
            last_music_file: None,
            last_music_cue_track: None,
            audio_device: None,
            raw_high_quality: false,
            hdr_native_surface_enabled: default_hdr_native_surface_enabled(),
            hdr_exposure_ev_native: 0.0,
            hdr_exposure_ev_sdr: 0.0,
            hdr_sdr_white_nits: default_hdr_sdr_white_nits(),
            hdr_max_display_nits: default_hdr_max_display_nits(),
            language: String::new(),
            theme: AppTheme::Dark,
            window_outer_position: None,
            window_inner_size: None,
            window_restore_outer_position: None,
            window_restore_inner_size: None,
            window_maximized_inner_size: None,
            window_maximized: false,
            window_maximized_screen_center: None,
        }
    }
}

impl Settings {
    /// Windows reports negative outer positions (e.g. `[-7,-7]`, `[-8,-8]`) while
    /// maximized; these are not valid restore coordinates. `(0, 0)` is valid.
    pub fn is_maximized_position_artifact([x, y]: [i32; 2]) -> bool {
        x < 0 && y < 0
    }

    pub(crate) fn valid_outer_position(pos: [i32; 2]) -> Option<[i32; 2]> {
        (!Self::is_maximized_position_artifact(pos)).then_some(pos)
    }

    /// Map a maximized window's screen center to a restore outer top-left on
    /// the monitor that contains the center (clamped to the work area).
    pub fn restore_outer_top_left_for_screen_center(
        center: [i32; 2],
        inner_size: [u32; 2],
    ) -> Option<[i32; 2]> {
        restore_outer_top_left_for_screen_center_impl(center, inner_size)
    }

    /// Outer top-left used when spawning the native window.
    pub fn startup_outer_position(&self) -> Option<[f32; 2]> {
        if self.window_maximized {
            if let Some(pos) = self.window_restore_outer_position {
                return Some([pos[0] as f32, pos[1] as f32]);
            }
            let restore_inner = self
                .window_restore_inner_size
                .or(self.window_inner_size)
                .unwrap_or([1280, 800]);
            if let Some(center) = self.window_maximized_screen_center
                && let Some(top_left) =
                    Self::restore_outer_top_left_for_screen_center(center, restore_inner)
            {
                return Some([top_left[0] as f32, top_left[1] as f32]);
            }
            return self
                .window_outer_position
                .and_then(Self::valid_outer_position)
                .map(|[x, y]| [x as f32, y as f32]);
        }
        let pos = self.window_outer_position?;
        Some([pos[0] as f32, pos[1] as f32])
    }

    /// Client size used when spawning the native window.
    pub fn startup_inner_size(&self) -> [f32; 2] {
        if self.window_maximized {
            self.window_maximized_inner_size
                .or(self.window_inner_size)
                .or(self.window_restore_inner_size)
                .map(|[w, h]| [w as f32, h as f32])
                .unwrap_or([1280.0, 800.0])
        } else {
            self.window_inner_size
                .map(|[w, h]| [w as f32, h as f32])
                .unwrap_or([1280.0, 800.0])
        }
    }

    /// Monitor hint for spawn-time HDR probing (prefers restore placement).
    pub fn window_spawn_top_left_for_hdr(&self) -> Option<[i32; 2]> {
        self.startup_outer_position()
            .map(|[x, y]| [x.round() as i32, y.round() as i32])
    }
}

#[cfg(target_os = "windows")]
fn restore_outer_top_left_for_screen_center_impl(
    center: [i32; 2],
    inner_size: [u32; 2],
) -> Option<[i32; 2]> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
    };

    unsafe {
        let monitor = MonitorFromPoint(
            POINT {
                x: center[0],
                y: center[1],
            },
            MONITOR_DEFAULTTONEAREST,
        );
        if monitor.is_invalid() {
            return None;
        }
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(monitor, &mut info).as_bool() {
            return None;
        }
        let work = info.rcWork;
        let w = inner_size[0] as i32;
        let h = inner_size[1] as i32;
        let mut x = center[0] - w / 2;
        let mut y = center[1] - h / 2;
        x = x.clamp(work.left, work.right.saturating_sub(w));
        y = y.clamp(work.top, work.bottom.saturating_sub(h));
        Some([x, y])
    }
}

#[cfg(not(target_os = "windows"))]
fn restore_outer_top_left_for_screen_center_impl(
    center: [i32; 2],
    inner_size: [u32; 2],
) -> Option<[i32; 2]> {
    Some([
        center[0] - inner_size[0] as i32 / 2,
        center[1] - inner_size[1] as i32 / 2,
    ])
}

// ---------------------------------------------------------------------------
// Language detection
// ---------------------------------------------------------------------------

/// Detect the system UI language and map it to one of our supported locales.
/// Falls back to "en" if no match is found.
pub fn detect_system_language() -> String {
    #[cfg(target_os = "windows")]
    {
        return get_windows_locale();
    }

    // On non-Windows platforms, try the LANG / LANGUAGE env var
    #[cfg(not(target_os = "windows"))]
    {
        for var in &["LANGUAGE", "LANG", "LC_ALL", "LC_MESSAGES"] {
            if let Ok(val) = std::env::var(var) {
                let v = val.to_lowercase();
                if v.starts_with("zh_cn") || v.starts_with("zh-cn") || v.starts_with("zh_hans") {
                    return "zh-CN".to_string();
                }
                if v.starts_with("zh_hk") || v.starts_with("zh-hk") || v.starts_with("zh_mo") {
                    return "zh-HK".to_string();
                }
                if v.starts_with("zh_tw") || v.starts_with("zh-tw") {
                    return "zh-TW".to_string();
                }
            }
        }
        "en".to_string()
    }
}

#[cfg(target_os = "windows")]
fn get_windows_locale() -> String {
    let mut buf = [0u16; 85]; // LOCALE_NAME_MAX_LENGTH
    let ret = unsafe {
        unsafe extern "system" {
            fn GetUserDefaultLocaleName(lp_locale_name: *mut u16, cch_locale_name: i32) -> i32;
        }
        GetUserDefaultLocaleName(buf.as_mut_ptr(), buf.len() as i32)
    };
    if ret <= 1 {
        return "en".to_string();
    }
    let locale = String::from_utf16_lossy(&buf[..(ret - 1) as usize]);
    if locale.starts_with("zh-CN") || locale.starts_with("zh-Hans") {
        "zh-CN".to_string()
    } else if locale.starts_with("zh-HK") || locale.starts_with("zh-MO") {
        "zh-HK".to_string()
    } else if locale.starts_with("zh-TW") {
        "zh-TW".to_string()
    } else {
        "en".to_string()
    }
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

pub fn settings_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("siv_settings.yaml")
}

impl Settings {
    /// Native HDR swap-chain requests (runtime). False on Linux X11; on Linux Wayland
    /// follows [`Settings::hdr_native_surface_enabled`].
    #[inline]
    pub fn hdr_native_surface_enabled_effective(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            if !crate::hdr::platform::linux_native_hdr_platform_eligible() {
                return false;
            }
        }
        self.hdr_native_surface_enabled
    }

    /// Applies probed panel peak luminance when available so native HDR output
    /// is scaled to the active display rather than a generic 1000 nit default.
    pub fn hdr_tone_map_settings_for_monitor(
        &self,
        monitor: Option<&crate::hdr::monitor::HdrMonitorSelection>,
        render_output_mode: crate::hdr::renderer::HdrRenderOutputMode,
    ) -> crate::hdr::types::HdrToneMapSettings {
        let mut max_display_nits = self
            .hdr_max_display_nits
            .max(self.hdr_sdr_white_nits.max(1.0));
        if let Some(probed_peak) = monitor
            .and_then(|selection| selection.max_luminance_nits)
            .filter(|value| value.is_finite() && *value > 0.0)
        {
            max_display_nits = max_display_nits
                .min(probed_peak)
                .max(self.hdr_sdr_white_nits);
        }
        let exposure_ev = match render_output_mode {
            crate::hdr::renderer::HdrRenderOutputMode::SdrToneMapped => self.hdr_exposure_ev_sdr,
            _ => self.hdr_exposure_ev_native,
        };
        crate::hdr::types::HdrToneMapSettings {
            exposure_ev,
            sdr_white_nits: self.hdr_sdr_white_nits,
            max_display_nits,
        }
    }

    pub fn load() -> Self {
        let path = settings_path();
        if let Ok(text) = std::fs::read_to_string(&path) {
            match serde_yaml::from_str::<Self>(&text) {
                Ok(s) => {
                    let hdr_sdr_white_nits = s.hdr_sdr_white_nits.clamp(80.0, 400.0);
                    let hdr_max_display_nits = s
                        .hdr_max_display_nits
                        .clamp(100.0, 10_000.0)
                        .max(hdr_sdr_white_nits);
                    let merged = Self {
                        auto_switch_interval: s.auto_switch_interval.clamp(0.5, 300.0),
                        volume: s.volume.clamp(0.0, 1.0),
                        font_size: s.font_size.clamp(12.0, 72.0),
                        transition_ms: s.transition_ms.clamp(50, 5000),
                        hdr_exposure_ev_native: s.hdr_exposure_ev_native.clamp(-8.0, 8.0),
                        hdr_exposure_ev_sdr: s.hdr_exposure_ev_sdr.clamp(-8.0, 8.0),
                        hdr_sdr_white_nits,
                        hdr_max_display_nits,
                        ..s
                    };
                    #[cfg(target_os = "linux")]
                    {
                        if !crate::hdr::platform::linux_native_hdr_platform_eligible() {
                            return Self {
                                hdr_native_surface_enabled: false,
                                ..merged
                            };
                        }
                        return merged;
                    }
                    #[cfg(not(target_os = "linux"))]
                    {
                        return merged;
                    }
                }
                Err(e) => eprintln!("[settings] parse error: {e}"),
            }
        }
        Self::default()
    }

    pub fn save(&self) -> Result<(), String> {
        let path = settings_path();
        let payload = {
            #[cfg(target_os = "linux")]
            {
                let mut s = self.clone();
                if !crate::hdr::platform::linux_native_hdr_platform_eligible() {
                    s.hdr_native_surface_enabled = false;
                }
                s
            }
            #[cfg(not(target_os = "linux"))]
            {
                self.clone()
            }
        };
        match serde_yaml::to_string(&payload) {
            Ok(text) => std::fs::write(&path, text).map_err(|e| e.to_string()),
            Err(e) => Err(format!("[settings] serialize error: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Settings;

    #[test]
    fn default_settings_expose_hdr_tone_map_controls() {
        let settings = Settings::default();

        assert_eq!(settings.hdr_exposure_ev_native, 0.0);
        assert_eq!(settings.hdr_exposure_ev_sdr, 0.0);
        assert_eq!(
            settings.hdr_sdr_white_nits,
            crate::hdr::types::DEFAULT_SDR_WHITE_NITS
        );
        assert_eq!(
            settings.hdr_max_display_nits,
            crate::hdr::types::DEFAULT_MAX_DISPLAY_NITS
        );
    }

    #[test]
    fn default_settings_native_hdr_surface_follows_platform() {
        let settings = Settings::default();

        #[cfg(target_os = "linux")]
        {
            assert_eq!(
                settings.hdr_native_surface_enabled,
                crate::hdr::platform::linux_native_hdr_platform_eligible()
            );
        }
        #[cfg(not(target_os = "linux"))]
        assert!(settings.hdr_native_surface_enabled);
    }

    #[test]
    fn hdr_native_surface_enabled_effective_respects_linux_session() {
        let settings = Settings {
            hdr_native_surface_enabled: true,
            ..Settings::default()
        };
        #[cfg(target_os = "linux")]
        {
            assert_eq!(
                settings.hdr_native_surface_enabled_effective(),
                crate::hdr::platform::linux_native_hdr_platform_eligible()
            );
        }
        #[cfg(not(target_os = "linux"))]
        assert!(settings.hdr_native_surface_enabled_effective());
    }

    #[test]
    fn missing_native_hdr_surface_setting_uses_platform_default() {
        let settings: Settings = serde_yaml::from_str("{}").expect("deserialize defaults");

        #[cfg(target_os = "linux")]
        assert_eq!(
            settings.hdr_native_surface_enabled,
            crate::hdr::platform::linux_native_hdr_platform_eligible()
        );
        #[cfg(not(target_os = "linux"))]
        assert!(settings.hdr_native_surface_enabled);
    }

    #[test]
    fn maximized_startup_inner_size_prefers_last_maximized_client_size() {
        let settings = Settings {
            window_maximized: true,
            window_inner_size: Some([2000, 1200]),
            window_restore_inner_size: Some([1280, 800]),
            window_maximized_inner_size: Some([3840, 2089]),
            ..Settings::default()
        };

        assert_eq!(settings.startup_inner_size(), [3840.0, 2089.0]);
    }

    #[test]
    fn maximized_position_artifact_rejects_negative_pairs_only() {
        assert!(!Settings::is_maximized_position_artifact([0, 0]));
        assert!(!Settings::is_maximized_position_artifact([320, 140]));
        assert!(Settings::is_maximized_position_artifact([-7, -7]));
        assert!(Settings::is_maximized_position_artifact([-8, -8]));
    }

    #[test]
    fn maximized_startup_outer_position_prefers_saved_restore() {
        let settings = Settings {
            window_maximized: true,
            window_restore_outer_position: Some([1920, 100]),
            window_maximized_screen_center: Some([9999, 9999]),
            ..Settings::default()
        };

        assert_eq!(settings.startup_outer_position(), Some([1920.0, 100.0]));
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn restore_outer_top_left_for_screen_center_offsets_by_half_inner_size() {
        let top_left = Settings::restore_outer_top_left_for_screen_center([2000, 1000], [800, 600])
            .expect("restore top-left");
        assert_eq!(top_left, [1600, 700]);
    }

    #[test]
    fn valid_outer_position_keeps_origin_but_not_maximized_artifact() {
        let settings = Settings {
            window_maximized: true,
            window_restore_outer_position: None,
            window_outer_position: Some([0, 0]),
            ..Settings::default()
        };

        assert_eq!(settings.startup_outer_position(), Some([0.0, 0.0]));
    }

    #[test]
    fn explicit_native_hdr_surface_setting_can_disable_request() {
        let settings: Settings =
            serde_yaml::from_str("hdr_native_surface_enabled: false").expect("deserialize setting");

        assert!(!settings.hdr_native_surface_enabled);
    }

    #[test]
    fn hdr_tone_map_settings_keep_display_nits_at_least_sdr_white() {
        let settings = Settings {
            hdr_sdr_white_nits: 300.0,
            hdr_max_display_nits: 200.0,
            ..Settings::default()
        };

        let tone_map = settings.hdr_tone_map_settings_for_monitor(
            None,
            crate::hdr::renderer::HdrRenderOutputMode::SdrToneMapped,
        );

        assert_eq!(tone_map.sdr_white_nits, 300.0);
        assert_eq!(tone_map.max_display_nits, 300.0);
    }

    #[test]
    fn hdr_tone_map_settings_cap_max_display_nits_to_probed_peak() {
        let settings = Settings {
            hdr_sdr_white_nits: 203.0,
            hdr_max_display_nits: 1000.0,
            ..Settings::default()
        };
        let monitor = crate::hdr::monitor::HdrMonitorSelection {
            hdr_supported: true,
            label: "eDP-1".to_string(),
            max_luminance_nits: Some(450.0),
            max_full_frame_luminance_nits: None,
            max_hdr_capacity: None,
            hdr_capacity_source: Some("Wayland wp_color_management"),
            native_surface_encoding: Some(crate::hdr::monitor::HdrNativeSurfaceEncoding::PqHdr10),
        };

        let tone_map = settings.hdr_tone_map_settings_for_monitor(
            Some(&monitor),
            crate::hdr::renderer::HdrRenderOutputMode::NativeHdrPq,
        );

        assert_eq!(tone_map.max_display_nits, 450.0);
    }

    #[test]
    fn hdr_tone_map_settings_split_exposure_by_render_output_mode() {
        let settings = Settings {
            hdr_exposure_ev_native: 3.0,
            hdr_exposure_ev_sdr: -1.0,
            ..Settings::default()
        };
        assert_eq!(
            settings
                .hdr_tone_map_settings_for_monitor(
                    None,
                    crate::hdr::renderer::HdrRenderOutputMode::NativeHdrPq,
                )
                .exposure_ev,
            3.0
        );
        assert_eq!(
            settings
                .hdr_tone_map_settings_for_monitor(
                    None,
                    crate::hdr::renderer::HdrRenderOutputMode::SdrToneMapped,
                )
                .exposure_ev,
            -1.0
        );
    }
}
