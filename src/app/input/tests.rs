#[cfg(test)]
const HOTKEY_MAP: &[HotkeyBinding] = &[
    // --- Group 1: High Priority (Complex Modifiers) ---
    HotkeyBinding {
        modifiers: M_SHIFT,
        key: egui::Key::Delete,
    },
    HotkeyBinding {
        modifiers: M_CTRL,
        key: egui::Key::ArrowLeft,
    },
    HotkeyBinding {
        modifiers: M_CTRL,
        key: egui::Key::ArrowRight,
    },
    HotkeyBinding {
        modifiers: M_CTRL,
        key: egui::Key::ArrowUp,
    },
    HotkeyBinding {
        modifiers: M_CTRL,
        key: egui::Key::ArrowDown,
    },
    HotkeyBinding {
        modifiers: M_CTRL,
        key: egui::Key::P,
    },
    #[cfg(not(target_os = "windows"))]
    HotkeyBinding {
        modifiers: M_CTRL,
        key: egui::Key::Q,
    },
    // --- Group 2: Simple Navigation / Control ---
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::ArrowRight,
    },
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::ArrowDown,
    },
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::PageDown,
    },
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::ArrowLeft,
    },
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::ArrowUp,
    },
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::PageUp,
    },
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::Home,
    },
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::End,
    },
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::Space,
    },
    // --- Group 3: Functional Keys ---
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::Tab,
    },
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::F1,
    },
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::F11,
    },
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::F,
    },
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::Z,
    },
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::G,
    },
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::Delete,
    },
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::Escape,
    },
    // Zoom
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::Plus,
    },
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::Equals,
    },
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::Minus,
    },
];
#[cfg(test)]
mod tests {
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
}
