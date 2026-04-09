use std::path::PathBuf;
use serde::{Deserialize, Serialize};

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
    pub fn label(self) -> String {
        match self {
            Self::FitToWindow => rust_i18n::t!("scale.fit").to_string(),
            Self::OriginalSize => rust_i18n::t!("scale.original").to_string(),
        }
    }

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitionStyle {
    None,
    Fade,
    ZoomFade,
    Slide,
    Push,
    PageFlip,
    Ripple,
    Curtain,
}

impl TransitionStyle {
    pub fn label(self) -> String {
        match self {
            Self::None     => rust_i18n::t!("transition.none").to_string(),
            Self::Fade     => rust_i18n::t!("transition.fade").to_string(),
            Self::ZoomFade => rust_i18n::t!("transition.zoom_fade").to_string(),
            Self::Slide    => rust_i18n::t!("transition.slide").to_string(),
            Self::Push     => rust_i18n::t!("transition.push").to_string(),
            Self::PageFlip => rust_i18n::t!("transition.page_flip").to_string(),
            Self::Ripple   => rust_i18n::t!("transition.ripple").to_string(),
            Self::Curtain  => rust_i18n::t!("transition.curtain").to_string(),
        }
    }
}

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
    pub last_music_track: Option<PathBuf>,

    // Font & Appearance
    #[serde(default = "default_font_family")]
    pub font_family: String,
    #[serde(default = "default_font_size")]
    pub font_size: f32,

    // Overlay (OSD)
    #[serde(default = "default_true")]
    pub show_osd: bool,

    // Language (locale code: "en", "zh-CN", "zh-HK")
    #[serde(default)]
    pub language: String,
}

fn default_interval() -> f32 { 5.0 }
fn default_true()     -> bool { true }
fn default_volume()   -> f32  { 1.0 }
fn default_font_family() -> String { "System Default".to_string() }
fn default_font_size()   -> f32  { 16.0 }
fn default_transition_style() -> TransitionStyle { TransitionStyle::None }
fn default_transition_ms() -> u32 { 800 }

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
            music_paused: false,
            last_music_track: None,
            language: String::new(), // empty = auto-detect on first launch
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
            }
        }
        "en".to_string()
    }
}

#[cfg(target_os = "windows")]
fn get_windows_locale() -> String {
    let mut buf = [0u16; 85]; // LOCALE_NAME_MAX_LENGTH
    let ret = unsafe {
        extern "system" {
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

    pub fn save(&self) {
        let path = settings_path();
        match serde_yaml::to_string(self) {
            Ok(text) => {
                let _ = std::fs::write(&path, text);
            }
            Err(e) => eprintln!("[settings] serialize error: {e}"),
        }
    }
}
