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

pub mod io;
pub mod model;
pub mod validate;

use crate::hotkeys::model::{
    HotkeyActionId, HotkeyConfigFile, HotkeyConflict, HotkeyWarning, KeyChord, ValidationOutput,
};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct RuntimeHotkeyState {
    pub config: HotkeyConfigFile,
    pub map: HashMap<KeyChord, HotkeyActionId>,
    pub warnings: Vec<HotkeyWarning>,
    pub conflicts: Vec<HotkeyConflict>,
}

pub fn load_runtime_hotkeys_state() -> Result<RuntimeHotkeyState, String> {
    let source = io::load_or_init_hotkeys_file()?;
    let validation = validate::validate_hotkey_config(&source);
    Ok(build_runtime_state(validation))
}

pub fn rebuild_runtime_state(config: &HotkeyConfigFile) -> RuntimeHotkeyState {
    build_runtime_state(validate::validate_hotkey_config(config))
}

pub fn chords_for_action(
    map: &HashMap<KeyChord, HotkeyActionId>,
    action: HotkeyActionId,
) -> Vec<KeyChord> {
    map.iter()
        .filter_map(|(chord, &id)| (id == action).then_some(*chord))
        .collect()
}

fn build_runtime_state(validation: ValidationOutput) -> RuntimeHotkeyState {
    let map = validate::bindings_to_map(&validation.runtime_bindings);
    RuntimeHotkeyState {
        config: validation.normalized,
        map,
        warnings: validation.warnings,
        conflicts: validation.conflicts,
    }
}
