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

use super::{AppAction, app_action_from_hotkey_action_id};
use crate::app::ImageViewerApp;
use crate::hotkeys::model::KeyChord;
use eframe::egui::{Context, Event};

impl ImageViewerApp {
    pub(crate) fn map_pointer_button_to_action(&self, ctx: &Context) -> Option<AppAction> {
        ctx.input(|i| {
            for event in &i.events {
                let Event::PointerButton {
                    button,
                    pressed: false,
                    modifiers,
                    ..
                } = event
                else {
                    continue;
                };
                let Some(chord) = KeyChord::from_pointer_button(*button, *modifiers) else {
                    continue;
                };
                if let Some(action_id) = self.hotkeys_runtime.map.get(&chord).copied() {
                    return Some(app_action_from_hotkey_action_id(action_id));
                }
            }
            None
        })
    }
}
