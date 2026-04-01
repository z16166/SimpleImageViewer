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
    pub fn label(self) -> &'static str {
        match self {
            Self::FitToWindow => "Fit to Window",
            Self::OriginalSize => "Original Size",
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

    // Music
    #[serde(default)]
    pub play_music: bool,
    #[serde(default)]
    pub music_path: Option<PathBuf>,
    #[serde(default = "default_volume")]
    pub volume: f32,
}

fn default_interval() -> f32 { 3.0 }
fn default_true()     -> bool { true }
fn default_volume()   -> f32  { 1.0 }

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
            play_music: false,
            music_path: None,
            volume: default_volume(),
        }
    }
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

/// Returns the path where settings are saved.
/// Stored next to the executable so settings follow the binary.
pub fn settings_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("siv_settings.yaml")
}

impl Settings {
    /// Load settings from disk, falling back to defaults on any error.
    pub fn load() -> Self {
        let path = settings_path();
        if let Ok(text) = std::fs::read_to_string(&path) {
            match serde_yaml::from_str::<Self>(&text) {
                Ok(s) => {
                    // Clamp values to sane ranges after loading
                    return Self {
                        auto_switch_interval: s.auto_switch_interval.clamp(0.5, 300.0),
                        volume: s.volume.clamp(0.0, 1.0),
                        ..s
                    };
                }
                Err(e) => eprintln!("[settings] parse error: {e}"),
            }
        }
        Self::default()
    }

    /// Save settings to disk. Errors are printed but not propagated.
    pub fn save(&self) {
        let path = settings_path();
        match serde_yaml::to_string(self) {
            Ok(text) => {
                if let Err(e) = std::fs::write(&path, text) {
                    eprintln!("[settings] write error: {e}");
                }
            }
            Err(e) => eprintln!("[settings] serialize error: {e}"),
        }
    }
}
