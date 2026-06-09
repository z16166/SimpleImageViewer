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

use rust_i18n::t;

pub(crate) fn build_hotkeys_issue_message(
    load_error: Option<&str>,
    conflicts: &[crate::hotkeys::model::HotkeyConflict],
    warnings: &[crate::hotkeys::model::HotkeyWarning],
) -> Option<String> {
    if load_error.is_none() && conflicts.is_empty() && warnings.is_empty() {
        return None;
    }

    let mut lines = Vec::new();
    if let Some(error) = load_error {
        lines.push(t!("hotkeys.load_failed", error = error).to_string());
    }
    if !conflicts.is_empty() {
        lines.push(t!("hotkeys.startup_conflicts", count = conflicts.len()).to_string());
        for conflict in conflicts.iter().take(3) {
            let actions = conflict
                .actions
                .iter()
                .map(|action| crate::hotkeys::model::action_id_to_str(*action))
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!("{}: {}", conflict.key, actions));
        }
    }
    if !warnings.is_empty() {
        lines.push(t!("hotkeys.startup_warnings", count = warnings.len()).to_string());
        lines.extend(warnings.iter().take(3).map(localized_hotkey_warning));
    }
    lines.push(t!("hotkeys.startup_open_settings_hint").to_string());
    Some(lines.join("\n"))
}

pub(crate) fn localized_hotkey_warning(warning: &crate::hotkeys::model::HotkeyWarning) -> String {
    use crate::hotkeys::model::{HotkeyWarning, action_id_to_str};
    match warning {
        HotkeyWarning::InvalidKey { action_id, key } => t!(
            "hotkeys.warning.invalid_key",
            key = key.as_str(),
            action = action_id_to_str(*action_id)
        )
        .to_string(),
        HotkeyWarning::MouseClickRequiresModifier { action_id, key } => t!(
            "hotkeys.warning.mouse_click_requires_modifier",
            key = key.as_str(),
            action = action_id_to_str(*action_id)
        )
        .to_string(),
        HotkeyWarning::NoValidKeys { action_id } => t!(
            "hotkeys.warning.no_valid_keys",
            action = action_id_to_str(*action_id)
        )
        .to_string(),
        HotkeyWarning::UnknownAction { action_id } => t!(
            "hotkeys.warning.unknown_action",
            action = action_id.as_str()
        )
        .to_string(),
    }
}
