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
        HotkeyActionId::ExitFullscreen => AppAction::ExitFullscreen,
    }
}

pub(super) fn text_event_to_hotkey_logical_key(text: &str) -> Option<HotkeyLogicalKey> {
    crate::hotkeys::model::parse_logical_key_name(text)
}
