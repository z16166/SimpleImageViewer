use super::{AppAction, app_action_from_hotkey_action_id};
use crate::app::ImageViewerApp;
use crate::hotkeys::model::KeyChord;
use eframe::egui::{self, Context, Event};

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
