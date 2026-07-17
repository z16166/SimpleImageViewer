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

//! Persistent screensaver configuration (`siv_screensaver.yaml`).
//!
//! Kept separate from `siv_settings.yaml` so Run/Preview never pollute normal
//! browse directory / fullscreen / slideshow state.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::startup::run_mode::{
    OrderedF32, ScreensaverCliOverrides, ScreensaverDisplayPolicy as CliDisplayPolicy,
};

const DEFAULT_INTERVAL_SECS: f32 = 8.0;
const DEFAULT_INPUT_GRACE_MS: u64 = 400;
const DEFAULT_MOVE_THRESHOLD_PX: f32 = 8.0;
const DEFAULT_MAX_FPS: f32 = 24.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ScreensaverDisplayPolicy {
    #[default]
    All,
    Primary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ScreensaverPerformanceProfile {
    /// Prefer lower GPU/CPU use for idle hang (default for screensaver).
    #[default]
    PowerSave,
    /// Closer to normal viewer quality (HDR optional, deeper preload).
    Quality,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreensaverSettings {
    /// Image root directories (first existing wins for session start; extras reserved).
    #[serde(default)]
    pub sources: Vec<PathBuf>,
    #[serde(default = "default_true")]
    pub recursive: bool,
    #[serde(default = "default_interval")]
    pub interval_secs: f32,
    #[serde(default = "default_true")]
    pub random_order: bool,
    #[serde(default)]
    pub display: ScreensaverDisplayPolicy,
    #[serde(default = "default_true")]
    pub exit_on_input: bool,
    /// Ignore residual input for this long after the run window appears.
    #[serde(default = "default_input_grace_ms")]
    pub input_grace_ms: u64,
    /// Pointer movement must exceed this many logical pixels to count as exit input.
    #[serde(default = "default_move_threshold_px")]
    pub pointer_move_threshold_px: f32,
    #[serde(default)]
    pub performance: ScreensaverPerformanceProfile,
    /// Optional target FPS cap when `performance` is PowerSave (repaint pacing).
    #[serde(default = "default_max_fps")]
    pub max_fps: f32,
    /// When false, force SDR / disable native HDR request in screensaver run.
    #[serde(default)]
    pub allow_hdr: bool,
    /// Preload neighbor count hint (soft); PowerSave clamps lower at runtime.
    #[serde(default = "default_preload_neighbors")]
    pub preload_neighbors: u32,
}

fn default_true() -> bool {
    true
}

fn default_interval() -> f32 {
    DEFAULT_INTERVAL_SECS
}

fn default_input_grace_ms() -> u64 {
    DEFAULT_INPUT_GRACE_MS
}

fn default_move_threshold_px() -> f32 {
    DEFAULT_MOVE_THRESHOLD_PX
}

fn default_max_fps() -> f32 {
    DEFAULT_MAX_FPS
}

fn default_preload_neighbors() -> u32 {
    1
}

impl Default for ScreensaverSettings {
    fn default() -> Self {
        Self {
            sources: Vec::new(),
            recursive: true,
            interval_secs: DEFAULT_INTERVAL_SECS,
            random_order: true,
            display: ScreensaverDisplayPolicy::All,
            exit_on_input: true,
            input_grace_ms: DEFAULT_INPUT_GRACE_MS,
            pointer_move_threshold_px: DEFAULT_MOVE_THRESHOLD_PX,
            performance: ScreensaverPerformanceProfile::PowerSave,
            max_fps: DEFAULT_MAX_FPS,
            allow_hdr: false,
            preload_neighbors: 1,
        }
    }
}

pub fn screensaver_settings_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("siv_screensaver.yaml")
}

impl ScreensaverSettings {
    pub fn load() -> Self {
        let path = screensaver_settings_path();
        if let Ok(text) = std::fs::read_to_string(&path) {
            match serde_yaml::from_str::<Self>(&text) {
                Ok(s) => return s.normalized(),
                Err(e) => {
                    log::warn!("[screensaver] failed to parse {}: {e}", path.display());
                }
            }
        }
        Self::default()
    }

    pub fn save(&self) -> Result<(), String> {
        let path = screensaver_settings_path();
        let payload = self.clone().normalized();
        match serde_yaml::to_string(&payload) {
            Ok(text) => std::fs::write(&path, text).map_err(|e| e.to_string()),
            Err(e) => Err(format!("[screensaver] serialize error: {e}")),
        }
    }

    pub fn normalized(mut self) -> Self {
        self.interval_secs = self.interval_secs.clamp(0.5, 3600.0);
        if !self.interval_secs.is_finite() {
            self.interval_secs = DEFAULT_INTERVAL_SECS;
        }
        self.input_grace_ms = self.input_grace_ms.min(5_000);
        self.pointer_move_threshold_px = self.pointer_move_threshold_px.clamp(0.0, 200.0);
        if !self.pointer_move_threshold_px.is_finite() {
            self.pointer_move_threshold_px = DEFAULT_MOVE_THRESHOLD_PX;
        }
        self.max_fps = self.max_fps.clamp(5.0, 60.0);
        if !self.max_fps.is_finite() {
            self.max_fps = DEFAULT_MAX_FPS;
        }
        self.preload_neighbors = self.preload_neighbors.min(8);
        self.sources.retain(|p| !p.as_os_str().is_empty());
        self
    }

    /// First configured source directory that exists (or the first entry even if missing).
    pub fn primary_source(&self) -> Option<&PathBuf> {
        self.sources
            .iter()
            .find(|p| p.is_dir())
            .or_else(|| self.sources.first())
    }

    pub fn apply_cli_overrides(&mut self, cli: &ScreensaverCliOverrides) {
        if let Some(source) = &cli.source {
            self.sources = vec![source.clone()];
        }
        if let Some(OrderedF32(secs)) = cli.interval_secs {
            self.interval_secs = secs;
        }
        if let Some(random) = cli.random {
            self.random_order = random;
        }
        if let Some(recursive) = cli.recursive {
            self.recursive = recursive;
        }
        if let Some(display) = cli.display {
            self.display = match display {
                CliDisplayPolicy::All => ScreensaverDisplayPolicy::All,
                CliDisplayPolicy::Primary => ScreensaverDisplayPolicy::Primary,
            };
        }
        if let Some(exit_on_input) = cli.exit_on_input {
            self.exit_on_input = exit_on_input;
        }
        if let Some(power_save) = cli.power_save {
            self.performance = if power_save {
                ScreensaverPerformanceProfile::PowerSave
            } else {
                ScreensaverPerformanceProfile::Quality
            };
        }
        *self = self.clone().normalized();
    }

    pub fn uses_power_save(&self) -> bool {
        matches!(self.performance, ScreensaverPerformanceProfile::PowerSave)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_clamps_interval() {
        let s = ScreensaverSettings {
            interval_secs: 0.01,
            ..Default::default()
        }
        .normalized();
        assert_eq!(s.interval_secs, 0.5);
    }

    #[test]
    fn apply_cli_source_replaces_list() {
        let mut s = ScreensaverSettings {
            sources: vec![PathBuf::from("a"), PathBuf::from("b")],
            ..Default::default()
        };
        let cli = ScreensaverCliOverrides {
            source: Some(PathBuf::from("c")),
            ..Default::default()
        };
        s.apply_cli_overrides(&cli);
        assert_eq!(s.sources, vec![PathBuf::from("c")]);
    }
}
