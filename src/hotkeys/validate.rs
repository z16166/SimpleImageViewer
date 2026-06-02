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

use crate::hotkeys::model::{
    HotkeyActionId, HotkeyBindingEntry, HotkeyConfigFile, HotkeyConflict, RuntimeHotkeyBinding,
    ValidationOutput, action_id_from_str, action_id_to_str, all_action_descriptors,
    default_hotkey_config_file, normalized_bindings_map,
};
use std::collections::{BTreeMap, HashMap, HashSet};

pub fn validate_hotkey_config(config: &HotkeyConfigFile) -> ValidationOutput {
    let mut normalized = default_hotkey_config_file();
    let mut warnings = Vec::new();
    let mut runtime_bindings = Vec::new();
    let mut by_chord: BTreeMap<String, Vec<HotkeyActionId>> = BTreeMap::new();
    let mut seen_actions = HashSet::new();

    let incoming = normalized_bindings_map(config);
    for desc in all_action_descriptors() {
        let action_id = desc.id;
        let fallback = normalized
            .bindings
            .iter()
            .find(|it| it.action_id == action_id_to_str(action_id))
            .cloned()
            .unwrap_or_else(|| HotkeyBindingEntry {
                action_id: action_id_to_str(action_id).to_string(),
                keys: Vec::new(),
                enabled: true,
                comment: String::new(),
            });

        let source = incoming.get(&action_id).unwrap_or(&fallback);
        if incoming.get(&action_id).is_none() {
            warnings.push(format!(
                "hotkeys missing action '{}', fallback to default",
                action_id_to_str(action_id)
            ));
        }

        let mut normalized_entry = HotkeyBindingEntry {
            action_id: action_id_to_str(action_id).to_string(),
            keys: Vec::new(),
            enabled: source.enabled,
            comment: source.comment.clone(),
        };

        if source.enabled {
            for key_text in &source.keys {
                if key_text.trim().is_empty() {
                    normalized_entry.keys.push(String::new());
                    continue;
                }
                match crate::hotkeys::model::KeyChord::parse(key_text) {
                    Some(chord) => {
                        if chord.requires_modifier() && chord.modifiers == 0 {
                            warnings.push(format!(
                                "mouse click key '{}' for action '{}' requires Ctrl, Alt, or Shift",
                                key_text,
                                action_id_to_str(action_id)
                            ));
                            continue;
                        }
                        let display = chord.display_string();
                        normalized_entry.keys.push(display.clone());
                        runtime_bindings.push(RuntimeHotkeyBinding { action_id, chord });
                        by_chord.entry(display).or_default().push(action_id);
                    }
                    None => warnings.push(format!(
                        "invalid key '{}' for action '{}', ignored",
                        key_text,
                        action_id_to_str(action_id)
                    )),
                }
            }
        }

        if normalized_entry.keys.is_empty() {
            warnings.push(format!(
                "action '{}' has no valid keys, fallback to defaults",
                action_id_to_str(action_id)
            ));
            let defaults = default_hotkey_config_file();
            if let Some(default_entry) = defaults
                .bindings
                .into_iter()
                .find(|it| it.action_id == action_id_to_str(action_id))
            {
                for key_text in default_entry.keys {
                    if let Some(chord) = crate::hotkeys::model::KeyChord::parse(&key_text) {
                        let display = chord.display_string();
                        normalized_entry.keys.push(display.clone());
                        runtime_bindings.push(RuntimeHotkeyBinding { action_id, chord });
                        by_chord.entry(display).or_default().push(action_id);
                    }
                }
            }
            normalized_entry.enabled = true;
        }

        seen_actions.insert(action_id);
        if let Some(slot) = normalized
            .bindings
            .iter_mut()
            .find(|it| it.action_id == action_id_to_str(action_id))
        {
            *slot = normalized_entry;
        } else {
            normalized.bindings.push(normalized_entry);
        }
    }

    for entry in &config.bindings {
        if action_id_from_str(&entry.action_id).is_none() {
            warnings.push(format!(
                "unknown action id '{}', this entry is ignored",
                entry.action_id
            ));
        }
    }

    let mut conflicts = Vec::new();
    for (key, actions) in by_chord {
        let unique_actions = unique_actions(actions);
        if unique_actions.len() > 1 {
            conflicts.push(HotkeyConflict {
                key,
                actions: unique_actions,
            });
        }
    }

    ValidationOutput {
        normalized,
        runtime_bindings,
        warnings,
        conflicts,
    }
}

fn unique_actions(actions: Vec<HotkeyActionId>) -> Vec<HotkeyActionId> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for action in actions {
        if seen.insert(action) {
            out.push(action);
        }
    }
    out
}

pub fn bindings_to_map(
    bindings: &[RuntimeHotkeyBinding],
) -> HashMap<crate::hotkeys::model::KeyChord, HotkeyActionId> {
    let mut out = HashMap::new();
    for binding in bindings {
        out.insert(binding.chord, binding.action_id);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hotkeys::model::{HOTKEYS_FILE_VERSION, HotkeyBindingEntry};

    #[test]
    fn invalid_key_falls_back_to_default() {
        let config = HotkeyConfigFile {
            version: HOTKEYS_FILE_VERSION,
            bindings: vec![HotkeyBindingEntry {
                action_id: "next_image".to_string(),
                keys: vec!["NotARealKey".to_string()],
                enabled: true,
                comment: String::new(),
            }],
        };
        let out = validate_hotkey_config(&config);
        let next = out
            .normalized
            .bindings
            .iter()
            .find(|it| it.action_id == "next_image")
            .expect("next_image binding exists");
        assert!(!next.keys.is_empty());
    }

    #[test]
    fn conflict_is_reported() {
        let config = HotkeyConfigFile {
            version: HOTKEYS_FILE_VERSION,
            bindings: vec![
                HotkeyBindingEntry {
                    action_id: "next_image".to_string(),
                    keys: vec!["Right".to_string()],
                    enabled: true,
                    comment: String::new(),
                },
                HotkeyBindingEntry {
                    action_id: "prev_image".to_string(),
                    keys: vec!["Right".to_string()],
                    enabled: true,
                    comment: String::new(),
                },
            ],
        };
        let out = validate_hotkey_config(&config);
        assert!(!out.conflicts.is_empty());
    }

    #[test]
    fn single_letter_hotkey_is_preserved() {
        let config = HotkeyConfigFile {
            version: HOTKEYS_FILE_VERSION,
            bindings: vec![HotkeyBindingEntry {
                action_id: "toggle_goto".to_string(),
                keys: vec!["D".to_string()],
                enabled: true,
                comment: String::new(),
            }],
        };

        let out = validate_hotkey_config(&config);
        let toggle_goto = out
            .normalized
            .bindings
            .iter()
            .find(|it| it.action_id == "toggle_goto")
            .expect("toggle_goto binding exists");

        assert_eq!(toggle_goto.keys, vec!["D".to_string()]);
    }

    #[test]
    fn modified_letter_hotkey_is_preserved() {
        let config = HotkeyConfigFile {
            version: HOTKEYS_FILE_VERSION,
            bindings: vec![HotkeyBindingEntry {
                action_id: "toggle_goto".to_string(),
                keys: vec!["Ctrl+Alt+Shift+D".to_string()],
                enabled: true,
                comment: String::new(),
            }],
        };

        let out = validate_hotkey_config(&config);
        let toggle_goto = out
            .normalized
            .bindings
            .iter()
            .find(|it| it.action_id == "toggle_goto")
            .expect("toggle_goto binding exists");

        assert_eq!(toggle_goto.keys, vec!["Ctrl+Shift+Alt+D".to_string()]);
    }

    #[test]
    fn duplicate_action_entries_are_merged() {
        let config = HotkeyConfigFile {
            version: HOTKEYS_FILE_VERSION,
            bindings: vec![
                HotkeyBindingEntry {
                    action_id: "next_image".to_string(),
                    keys: vec!["Right".to_string(), "Down".to_string()],
                    enabled: true,
                    comment: String::new(),
                },
                HotkeyBindingEntry {
                    action_id: "next_image".to_string(),
                    keys: vec!["D".to_string()],
                    enabled: true,
                    comment: String::new(),
                },
            ],
        };

        let out = validate_hotkey_config(&config);
        let next = out
            .normalized
            .bindings
            .iter()
            .find(|it| it.action_id == "next_image")
            .expect("next_image binding exists");

        assert_eq!(next.keys, vec!["Right", "Down", "D"]);
    }

    #[test]
    fn empty_hotkey_row_is_preserved_without_runtime_binding() {
        let config = HotkeyConfigFile {
            version: HOTKEYS_FILE_VERSION,
            bindings: vec![HotkeyBindingEntry {
                action_id: "next_image".to_string(),
                keys: vec![String::new()],
                enabled: true,
                comment: String::new(),
            }],
        };

        let out = validate_hotkey_config(&config);
        let next = out
            .normalized
            .bindings
            .iter()
            .find(|it| it.action_id == "next_image")
            .expect("next_image binding exists");

        assert_eq!(next.keys, vec![String::new()]);
        assert!(
            out.runtime_bindings
                .iter()
                .all(|binding| binding.action_id != HotkeyActionId::NextImage)
        );
    }

    #[test]
    fn wheel_hotkeys_are_supported() {
        let config = HotkeyConfigFile {
            version: HOTKEYS_FILE_VERSION,
            bindings: vec![
                HotkeyBindingEntry {
                    action_id: "next_image".to_string(),
                    keys: vec!["WheelDown".to_string()],
                    enabled: true,
                    comment: String::new(),
                },
                HotkeyBindingEntry {
                    action_id: "zoom_in".to_string(),
                    keys: vec!["Ctrl+WheelDown".to_string()],
                    enabled: true,
                    comment: String::new(),
                },
            ],
        };

        let out = validate_hotkey_config(&config);
        let next = out
            .normalized
            .bindings
            .iter()
            .find(|it| it.action_id == "next_image")
            .expect("next_image binding exists");
        let zoom_in = out
            .normalized
            .bindings
            .iter()
            .find(|it| it.action_id == "zoom_in")
            .expect("zoom_in binding exists");

        assert!(next.keys.iter().any(|key| key == "WheelDown"));
        assert!(zoom_in.keys.iter().any(|key| key == "Ctrl+WheelDown"));
    }

    #[test]
    fn mouse_click_hotkeys_require_modifiers_for_left_and_right() {
        let config = HotkeyConfigFile {
            version: HOTKEYS_FILE_VERSION,
            bindings: vec![
                HotkeyBindingEntry {
                    action_id: "toggle_goto".to_string(),
                    keys: vec!["LeftClick".to_string()],
                    enabled: true,
                    comment: String::new(),
                },
                HotkeyBindingEntry {
                    action_id: "toggle_settings".to_string(),
                    keys: vec!["Ctrl+LeftClick".to_string()],
                    enabled: true,
                    comment: String::new(),
                },
                HotkeyBindingEntry {
                    action_id: "print_current".to_string(),
                    keys: vec!["MiddleClick".to_string()],
                    enabled: true,
                    comment: String::new(),
                },
            ],
        };

        let out = validate_hotkey_config(&config);
        let toggle_goto = out
            .normalized
            .bindings
            .iter()
            .find(|it| it.action_id == "toggle_goto")
            .expect("toggle_goto binding exists");
        let toggle_settings = out
            .normalized
            .bindings
            .iter()
            .find(|it| it.action_id == "toggle_settings")
            .expect("toggle_settings binding exists");
        let print_current = out
            .normalized
            .bindings
            .iter()
            .find(|it| it.action_id == "print_current")
            .expect("print_current binding exists");

        assert!(!toggle_goto.keys.contains(&"LeftClick".to_string()));
        assert!(toggle_settings.keys.contains(&"Ctrl+LeftClick".to_string()));
        assert!(print_current.keys.contains(&"MiddleClick".to_string()));
    }
}
