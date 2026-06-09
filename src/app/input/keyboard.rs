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

use super::{AppAction, app_action_from_hotkey_action_id, get_modifiers_mask, text_event_to_hotkey_logical_key};
use crate::app::ImageViewerApp;
use crate::hotkeys::model::KeyChord;
use eframe::egui::{self, Context, Key};

impl ImageViewerApp {
    pub(crate) fn handle_keyboard(&mut self, ctx: &Context) {
        // High-level layer detection
        if self.active_modal.is_some() {
            self.handle_modal_input(ctx);
        } else if self.show_settings {
            self.handle_settings_input(ctx);
        } else {
            self.handle_main_window_input(ctx);
        }
    }

    /// Layer 3: Input handling when a modal dialog is active.
    fn handle_modal_input(&mut self, ctx: &Context) {
        ctx.input(|i| {
            // Escape always dismisses any modal
            if i.key_pressed(Key::Escape) {
                self.active_modal = None;
                return;
            }
        });
    }

    /// Layer 2: Input handling when the non-modal settings panel is open.
    fn handle_settings_input(&mut self, ctx: &Context) {
        let mut action: Option<AppAction> = None;
        let capturing = self.is_hotkey_capture_active();
        ctx.input(|i| {
            if !capturing {
                action = self.map_key_to_action(i);
            }
            // Escape closes settings unless a hotkey capture session is active (allows ESC binding).
            if !capturing && i.key_pressed(Key::Escape) {
                self.show_settings = false;
            }
        });

        if let Some(act) = action {
            if act == AppAction::ToggleSettings {
                self.dispatch_action(act, ctx);
            }
        }
    }

    /// Layer 1: Input handling for the main window (normal operation).
    fn handle_main_window_input(&mut self, ctx: &Context) {
        let mut action: Option<AppAction> = None;

        ctx.input(|i| {
            action = self.map_key_to_action(i);
        });

        // If OSD was toggled via Tab, we also clear focus to prevent egui focus-trapping.
        if action == Some(AppAction::ToggleOSD) {
            ctx.memory_mut(|mem| mem.request_focus(egui::Id::NULL));
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Tab));
        }

        if let Some(act) = action {
            self.dispatch_action(act, ctx);
        }
    }

    fn map_key_to_action(&self, i: &egui::InputState) -> Option<AppAction> {
        for ev in &i.events {
            if let egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } = ev
            {
                let chord = KeyChord::from_input_event(*key, *modifiers);
                if let Some(action_id) = self.hotkeys_runtime.map.get(&chord).copied() {
                    return Some(app_action_from_hotkey_action_id(action_id));
                }
            }
        }

        // Some keyboard layouts report zoom keys as text input rather than plain key presses.
        let current_mods = get_modifiers_mask(i.modifiers);
        for ev in &i.events {
            if let egui::Event::Text(text) = ev {
                let logical = text_event_to_hotkey_logical_key(text);
                if let Some(logical) = logical {
                    let chord = KeyChord {
                        modifiers: current_mods,
                        key: logical,
                    };
                    if let Some(action_id) = self.hotkeys_runtime.map.get(&chord).copied() {
                        return Some(app_action_from_hotkey_action_id(action_id));
                    }
                }
            }
        }

        None
    }
}
