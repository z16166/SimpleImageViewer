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

use super::{
    AutoSwitchStep, HOTKEY_MAP, app_action_from_hotkey_action_id, auto_switch_step,
    text_event_to_hotkey_logical_key,
};
use crate::hotkeys::model::{HotkeyLogicalKey, keychord_from_legacy_binding};
use eframe::egui::Key;
use std::collections::HashSet;

#[test]
fn auto_switch_uses_existing_order_when_random_is_disabled() {
    assert_eq!(
        auto_switch_step(5, 1, false, false),
        AutoSwitchStep::NavigateTo(2)
    );
}

#[test]
fn auto_switch_stops_when_there_is_only_one_image() {
    assert_eq!(auto_switch_step(1, 0, true, false), AutoSwitchStep::Stop);
}

#[test]
fn random_auto_switch_starts_by_shuffling_to_first_image() {
    assert_eq!(
        auto_switch_step(5, 1, true, false),
        AutoSwitchStep::ShuffleToFirst
    );
}

#[test]
fn random_auto_switch_reshuffles_before_next_loop() {
    assert_eq!(
        auto_switch_step(5, 4, true, true),
        AutoSwitchStep::ShuffleToFirst
    );
}

#[test]
fn auto_switch_loops_at_end_when_random_is_disabled() {
    assert_eq!(
        auto_switch_step(5, 4, false, true),
        AutoSwitchStep::NavigateTo(0)
    );
}

#[test]
fn legacy_hotkey_map_has_no_conflicts() {
    let mut seen = HashSet::new();
    for binding in HOTKEY_MAP {
        let chord = keychord_from_legacy_binding(binding.modifiers, binding.key);
        assert!(
            seen.insert(chord),
            "duplicate legacy chord: {:?}",
            chord.display_string()
        );
    }
}

#[test]
fn all_runtime_actions_map_to_app_actions() {
    for desc in crate::hotkeys::model::all_action_descriptors() {
        let _app_action = app_action_from_hotkey_action_id(desc.id);
    }
}

#[test]
fn text_event_mapping_reuses_hotkey_key_parser() {
    assert_eq!(
        text_event_to_hotkey_logical_key("+"),
        Some(HotkeyLogicalKey::Text("+"))
    );
    assert_eq!(
        text_event_to_hotkey_logical_key("1"),
        Some(HotkeyLogicalKey::Egui(Key::Num1))
    );
    assert_eq!(
        text_event_to_hotkey_logical_key("M"),
        Some(HotkeyLogicalKey::Egui(Key::M))
    );
}
