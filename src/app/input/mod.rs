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

use crate::hotkeys::model::{HotkeyActionId, HotkeyLogicalKey};
use eframe::egui;

mod actions;
mod keyboard;
mod pointer;
mod ui;
mod wheel;

#[cfg(test)]
mod tests;

pub(crate) use actions::AppAction;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AutoSwitchStep {
    Stop,
    NavigateTo(usize),
    ShuffleToFirst,
}

pub(crate) fn auto_switch_step(
    image_count: usize,
    current_index: usize,
    random_order: bool,
    random_order_ready: bool,
) -> AutoSwitchStep {
    if image_count <= 1 {
        return AutoSwitchStep::Stop;
    }
    if random_order && !random_order_ready {
        return AutoSwitchStep::ShuffleToFirst;
    }

    let last = image_count - 1;
    if current_index >= last {
        if random_order {
            return AutoSwitchStep::ShuffleToFirst;
        }
    }

    AutoSwitchStep::NavigateTo((current_index + 1) % image_count)
}

#[cfg(test)]
struct HotkeyBinding {
    modifiers: u8,
    key: egui::Key,
}

#[cfg(test)]
const M_NONE: u8 = 0;
const M_CTRL: u8 = 1;
const M_SHIFT: u8 = 2;
const M_ALT: u8 = 4;

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

pub(super) fn get_modifiers_mask(m: egui::Modifiers) -> u8 {
    let mut mask = 0;
    if m.ctrl || m.command {
        mask |= M_CTRL;
    }
    if m.shift {
        mask |= M_SHIFT;
    }
    if m.alt {
        mask |= M_ALT;
    }
    mask
}

pub(super) fn app_action_from_hotkey_action_id(action: HotkeyActionId) -> AppAction {
    match action {
        HotkeyActionId::NextImage => AppAction::Next,
        HotkeyActionId::PrevImage => AppAction::Prev,
        HotkeyActionId::FirstImage => AppAction::First,
        HotkeyActionId::LastImage => AppAction::Last,
        HotkeyActionId::ZoomIn => AppAction::ZoomIn,
        HotkeyActionId::ZoomOut => AppAction::ZoomOut,
        HotkeyActionId::ZoomReset => AppAction::ZoomReset,
        HotkeyActionId::ToggleSettings => AppAction::ToggleSettings,
        HotkeyActionId::ToggleFullscreen => AppAction::ToggleFullscreen,
        HotkeyActionId::ToggleScaleMode => AppAction::ToggleScaleMode,
        HotkeyActionId::ToggleOsd => AppAction::ToggleOSD,
        HotkeyActionId::RotateCw => AppAction::RotateCW,
        HotkeyActionId::RotateCcw => AppAction::RotateCCW,
        HotkeyActionId::HdrExposureUp => AppAction::HdrExposureUp,
        HotkeyActionId::HdrExposureDown => AppAction::HdrExposureDown,
        HotkeyActionId::DeleteToRecycleBin => AppAction::Delete,
        HotkeyActionId::PermanentDelete => AppAction::PermanentDelete,
        HotkeyActionId::PrintCurrent => AppAction::Print,
        HotkeyActionId::ToggleGoto => AppAction::ToggleGoto,
        HotkeyActionId::ToggleSlideshow => AppAction::ToggleAutoSwitch,
        HotkeyActionId::RefreshFileList => AppAction::RefreshFileList,
        #[cfg(not(target_os = "windows"))]
        HotkeyActionId::Quit => AppAction::Quit,
        HotkeyActionId::SelectPixelRegion => AppAction::SelectPixelRegion,
        HotkeyActionId::ExitFullscreen => AppAction::ExitFullscreen,
        HotkeyActionId::CopyTo => AppAction::CopyTo,
        HotkeyActionId::CutTo => AppAction::CutTo,
        HotkeyActionId::ToggleTray => AppAction::ToggleTray,
    }
}

pub(super) fn text_event_to_hotkey_logical_key(text: &str) -> Option<HotkeyLogicalKey> {
    crate::hotkeys::model::parse_logical_key_name(text)
}
