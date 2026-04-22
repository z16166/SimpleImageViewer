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

use std::path::PathBuf;
use serde::{Deserialize, Serialize};
use crate::theme::AppTheme;

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
    #[serde(default = "default_true")]
    pub loop_playback: bool,

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

    // Language (locale code: "en", "zh-CN", "zh-HK")
    #[serde(default)]
    pub language: String,

    // Theme
    #[serde(default)]
    pub theme: AppTheme,

    // Debug & Logging (Manual config only)
    #[serde(default = "default_true")]
    pub enable_log_file: bool,
    #[serde(default = "default_log_level")]
    pub log_level: String,
}

fn default_interval() -> f32 { 5.0 }
fn default_true()     -> bool { true }
fn default_volume()   -> f32  { 1.0 }
fn default_font_family() -> String { "System Default".to_string() }
fn default_font_size()   -> f32  { 16.0 }
fn default_transition_style() -> TransitionStyle { TransitionStyle::None }
fn default_transition_ms() -> u32 { 800 }
fn default_log_level() -> String { "info".to_string() }

impl Default for ScaleMode {
    fn default() -> Self { Self::FitToWindow }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            recursive: false,
            fullscreen: false,
            last_image_dir: None,
            auto_switch: false,
            auto_switch_interval: default_interval(),
            loop_playback: true,
            scale_mode: ScaleMode::FitToWindow,
            transition_style: default_transition_style(),
            transition_ms: default_transition_ms(),
            play_music: false,
            music_path: None,
            volume: default_volume(),
            font_family: default_font_family(),
            font_size: default_font_size(),
            preload: true,
            resume_last_image: false,
            last_viewed_image: None,
            show_osd: true,
            show_music_osd: true,
            music_paused: false,
            last_music_file: None,
            last_music_cue_track: None,
            audio_device: None,
            language: String::new(),
            theme: AppTheme::Dark,
            enable_log_file: true,
            log_level: default_log_level(),
        }
    }
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
    pub fn load() -> Self {
        let path = settings_path();
        if let Ok(text) = std::fs::read_to_string(&path) {
            match serde_yaml::from_str::<Self>(&text) {
                Ok(s) => {
                    return Self {
                        auto_switch_interval: s.auto_switch_interval.clamp(0.5, 300.0),
                        volume: s.volume.clamp(0.0, 1.0),
                        font_size: s.font_size.clamp(12.0, 72.0),
                        transition_ms: s.transition_ms.clamp(50, 5000),
                        ..s
                    };
                }
                Err(e) => eprintln!("[settings] parse error: {e}"),
            }
        }
        Self::default()
    }

    pub fn save(&self) -> Result<(), String> {
        let path = settings_path();
        match serde_yaml::to_string(self) {
            Ok(text) => {
                std::fs::write(&path, text).map_err(|e| e.to_string())
            }
            Err(e) => Err(format!("[settings] serialize error: {e}")),
        }
    }
}
