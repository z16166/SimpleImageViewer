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

use crate::hotkeys::model::{HOTKEYS_FILE_NAME, HotkeyConfigFile, default_hotkey_config_file};
use std::path::PathBuf;

pub fn hotkeys_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
        .join(HOTKEYS_FILE_NAME)
}

pub fn load_or_init_hotkeys_file() -> Result<HotkeyConfigFile, String> {
    let path = hotkeys_path();
    if !path.exists() {
        let defaults = default_hotkey_config_file();
        save_hotkeys_file(&defaults)?;
        return Ok(defaults);
    }
    let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    serde_yaml::from_str::<HotkeyConfigFile>(&text).map_err(|e| e.to_string())
}

pub fn save_hotkeys_file(config: &HotkeyConfigFile) -> Result<(), String> {
    let path = hotkeys_path();
    let text = serde_yaml::to_string(config).map_err(|e| e.to_string())?;
    std::fs::write(path, text).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_file_contains_bindings() {
        let default_cfg = default_hotkey_config_file();
        assert!(!default_cfg.bindings.is_empty());
    }
}
