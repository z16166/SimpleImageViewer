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

use eframe::egui;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const HOTKEYS_FILE_NAME: &str = "siv_hotkeys.yaml";
pub const HOTKEYS_FILE_VERSION: u32 = 2;

pub const MOD_CTRL: u8 = 1;
pub const MOD_SHIFT: u8 = 2;
pub const MOD_ALT: u8 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HotkeyActionId {
    NextImage,
    PrevImage,
    FirstImage,
    LastImage,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    ToggleSettings,
    ToggleFullscreen,
    ToggleScaleMode,
    ToggleOsd,
    RotateCw,
    RotateCcw,
    HdrExposureUp,
    HdrExposureDown,
    DeleteToRecycleBin,
    PermanentDelete,
    PrintCurrent,
    ToggleGoto,
    ToggleSlideshow,
    RefreshFileList,
    SelectPixelRegion,
    #[cfg(not(target_os = "windows"))]
    Quit,
    ExitFullscreen,
    CopyTo,
    CutTo,
    ToggleTray,
    PickDirectory,
    ToggleDirectoryTreeNav,
}

#[derive(Debug, Clone, Copy)]
pub struct ActionDescriptor {
    pub id: HotkeyActionId,
    pub id_str: &'static str,
}

pub const ACTION_DESCRIPTORS: &[ActionDescriptor] = &[
    ActionDescriptor {
        id: HotkeyActionId::NextImage,
        id_str: "next_image",
    },
    ActionDescriptor {
        id: HotkeyActionId::PrevImage,
        id_str: "prev_image",
    },
    ActionDescriptor {
        id: HotkeyActionId::FirstImage,
        id_str: "first_image",
    },
    ActionDescriptor {
        id: HotkeyActionId::LastImage,
        id_str: "last_image",
    },
    ActionDescriptor {
        id: HotkeyActionId::ZoomIn,
        id_str: "zoom_in",
    },
    ActionDescriptor {
        id: HotkeyActionId::ZoomOut,
        id_str: "zoom_out",
    },
    ActionDescriptor {
        id: HotkeyActionId::ZoomReset,
        id_str: "zoom_reset",
    },
    ActionDescriptor {
        id: HotkeyActionId::ToggleSettings,
        id_str: "toggle_settings",
    },
    ActionDescriptor {
        id: HotkeyActionId::ToggleFullscreen,
        id_str: "toggle_fullscreen",
    },
    ActionDescriptor {
        id: HotkeyActionId::ToggleScaleMode,
        id_str: "toggle_scale_mode",
    },
    ActionDescriptor {
        id: HotkeyActionId::ToggleOsd,
        id_str: "toggle_osd",
    },
    ActionDescriptor {
        id: HotkeyActionId::RotateCw,
        id_str: "rotate_cw",
    },
    ActionDescriptor {
        id: HotkeyActionId::RotateCcw,
        id_str: "rotate_ccw",
    },
    ActionDescriptor {
        id: HotkeyActionId::HdrExposureUp,
        id_str: "hdr_exposure_up",
    },
    ActionDescriptor {
        id: HotkeyActionId::HdrExposureDown,
        id_str: "hdr_exposure_down",
    },
    ActionDescriptor {
        id: HotkeyActionId::DeleteToRecycleBin,
        id_str: "delete_to_recycle_bin",
    },
    ActionDescriptor {
        id: HotkeyActionId::PermanentDelete,
        id_str: "permanent_delete",
    },
    ActionDescriptor {
        id: HotkeyActionId::PrintCurrent,
        id_str: "print_current",
    },
    ActionDescriptor {
        id: HotkeyActionId::ToggleGoto,
        id_str: "toggle_goto",
    },
    ActionDescriptor {
        id: HotkeyActionId::ToggleSlideshow,
        id_str: "toggle_slideshow",
    },
    ActionDescriptor {
        id: HotkeyActionId::RefreshFileList,
        id_str: "refresh_file_list",
    },
    ActionDescriptor {
        id: HotkeyActionId::SelectPixelRegion,
        id_str: "select_pixel_region",
    },
    #[cfg(not(target_os = "windows"))]
    ActionDescriptor {
        id: HotkeyActionId::Quit,
        id_str: "quit_app",
    },
    ActionDescriptor {
        id: HotkeyActionId::ExitFullscreen,
        id_str: "exit_fullscreen",
    },
    ActionDescriptor {
        id: HotkeyActionId::CopyTo,
        id_str: "copy_to",
    },
    ActionDescriptor {
        id: HotkeyActionId::CutTo,
        id_str: "cut_to",
    },
    ActionDescriptor {
        id: HotkeyActionId::ToggleTray,
        id_str: "toggle_tray",
    },
    ActionDescriptor {
        id: HotkeyActionId::PickDirectory,
        id_str: "pick_directory",
    },
    ActionDescriptor {
        id: HotkeyActionId::ToggleDirectoryTreeNav,
        id_str: "toggle_directory_tree_nav",
    },
];

pub fn all_action_descriptors() -> &'static [ActionDescriptor] {
    ACTION_DESCRIPTORS
}

pub fn action_id_to_str(id: HotkeyActionId) -> &'static str {
    ACTION_DESCRIPTORS
        .iter()
        .find_map(|it| (it.id == id).then_some(it.id_str))
        .unwrap_or("unknown")
}

pub fn action_id_from_str(value: &str) -> Option<HotkeyActionId> {
    let trimmed = value.trim();
    ACTION_DESCRIPTORS
        .iter()
        .find_map(|it| (it.id_str == trimmed).then_some(it.id))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HotkeyBindingEntry {
    pub action_id: String,
    #[serde(default)]
    pub keys: Vec<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub comment: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HotkeyConfigFile {
    #[serde(default = "default_hotkeys_file_version")]
    pub version: u32,
    #[serde(default)]
    pub bindings: Vec<HotkeyBindingEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HotkeyLogicalKey {
    Egui(egui::Key),
    Text(&'static str),
    WheelUp,
    WheelDown,
    MouseLeft,
    MouseRight,
    MouseMiddle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyChord {
    pub modifiers: u8,
    pub key: HotkeyLogicalKey,
}

impl KeyChord {
    pub fn display_string(self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        if self.modifiers & MOD_CTRL != 0 {
            parts.push("Ctrl");
        }
        if self.modifiers & MOD_SHIFT != 0 {
            parts.push("Shift");
        }
        if self.modifiers & MOD_ALT != 0 {
            parts.push("Alt");
        }
        parts.push(logical_key_to_name(self.key));
        parts.join("+")
    }

    pub fn parse(input: &str) -> Option<Self> {
        let mut modifiers = 0_u8;
        let mut key_name = None::<String>;
        for token in input.split('+').map(str::trim).filter(|s| !s.is_empty()) {
            let normalized = token.to_ascii_lowercase();
            match normalized.as_str() {
                "ctrl" | "control" | "cmd" | "command" => modifiers |= MOD_CTRL,
                "shift" => modifiers |= MOD_SHIFT,
                "alt" | "option" => modifiers |= MOD_ALT,
                _ => key_name = Some(token.to_string()),
            }
        }
        let key_name = key_name?;
        let key = parse_logical_key_name(&key_name)?;
        Some(Self { modifiers, key })
    }

    pub fn from_input_event(key: egui::Key, mods: egui::Modifiers) -> Self {
        Self::from_logical_input(HotkeyLogicalKey::Egui(key), mods)
    }

    pub fn from_wheel_input(delta_y: f32, mods: egui::Modifiers) -> Option<Self> {
        let key = if delta_y > 0.0 {
            HotkeyLogicalKey::WheelUp
        } else if delta_y < 0.0 {
            HotkeyLogicalKey::WheelDown
        } else {
            return None;
        };
        Some(Self::from_logical_input(key, mods))
    }

    pub fn from_pointer_button(button: egui::PointerButton, mods: egui::Modifiers) -> Option<Self> {
        let key = match button {
            egui::PointerButton::Primary => HotkeyLogicalKey::MouseLeft,
            egui::PointerButton::Secondary => HotkeyLogicalKey::MouseRight,
            egui::PointerButton::Middle => HotkeyLogicalKey::MouseMiddle,
            egui::PointerButton::Extra1 | egui::PointerButton::Extra2 => return None,
        };
        Some(Self::from_logical_input(key, mods))
    }

    pub fn requires_modifier(self) -> bool {
        matches!(
            self.key,
            HotkeyLogicalKey::MouseLeft | HotkeyLogicalKey::MouseRight
        )
    }

    fn from_logical_input(key: HotkeyLogicalKey, mods: egui::Modifiers) -> Self {
        let mut mask = 0_u8;
        if mods.ctrl || mods.command {
            mask |= MOD_CTRL;
        }
        if mods.shift {
            mask |= MOD_SHIFT;
        }
        if mods.alt {
            mask |= MOD_ALT;
        }
        Self {
            modifiers: mask,
            key,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeHotkeyBinding {
    pub action_id: HotkeyActionId,
    pub chord: KeyChord,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotkeyConflict {
    pub key: String,
    pub actions: Vec<HotkeyActionId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotkeyWarning {
    InvalidKey {
        action_id: HotkeyActionId,
        key: String,
    },
    MouseClickRequiresModifier {
        action_id: HotkeyActionId,
        key: String,
    },
    NoValidKeys {
        action_id: HotkeyActionId,
    },
    UnknownAction {
        action_id: String,
    },
}

#[derive(Debug, Clone)]
pub struct ValidationOutput {
    pub normalized: HotkeyConfigFile,
    pub runtime_bindings: Vec<RuntimeHotkeyBinding>,
    pub warnings: Vec<HotkeyWarning>,
    pub conflicts: Vec<HotkeyConflict>,
}

pub fn default_hotkey_config_file() -> HotkeyConfigFile {
    let mut bindings = Vec::new();
    for desc in all_action_descriptors() {
        let keys = default_key_chords(desc.id)
            .iter()
            .map(|it| it.display_string())
            .collect::<Vec<_>>();
        bindings.push(HotkeyBindingEntry {
            action_id: desc.id_str.to_string(),
            keys,
            enabled: true,
            comment: String::new(),
        });
    }
    HotkeyConfigFile {
        version: HOTKEYS_FILE_VERSION,
        bindings,
    }
}

#[cfg(test)]
pub fn keychord_from_legacy_binding(modifiers: u8, key: egui::Key) -> KeyChord {
    let logical_key = match key {
        egui::Key::Plus => HotkeyLogicalKey::Text("+"),
        egui::Key::Minus => HotkeyLogicalKey::Text("-"),
        _ => HotkeyLogicalKey::Egui(key),
    };
    KeyChord {
        modifiers,
        key: logical_key,
    }
}

pub fn default_key_chords(action_id: HotkeyActionId) -> &'static [KeyChord] {
    match action_id {
        HotkeyActionId::NextImage => &[
            KeyChord {
                modifiers: 0,
                key: HotkeyLogicalKey::WheelDown,
            },
            KeyChord {
                modifiers: 0,
                key: HotkeyLogicalKey::Egui(egui::Key::ArrowRight),
            },
            KeyChord {
                modifiers: 0,
                key: HotkeyLogicalKey::Egui(egui::Key::ArrowDown),
            },
            KeyChord {
                modifiers: 0,
                key: HotkeyLogicalKey::Egui(egui::Key::PageDown),
            },
        ],
        HotkeyActionId::PrevImage => &[
            KeyChord {
                modifiers: 0,
                key: HotkeyLogicalKey::WheelUp,
            },
            KeyChord {
                modifiers: 0,
                key: HotkeyLogicalKey::Egui(egui::Key::ArrowLeft),
            },
            KeyChord {
                modifiers: 0,
                key: HotkeyLogicalKey::Egui(egui::Key::ArrowUp),
            },
            KeyChord {
                modifiers: 0,
                key: HotkeyLogicalKey::Egui(egui::Key::PageUp),
            },
        ],
        HotkeyActionId::FirstImage => &[KeyChord {
            modifiers: 0,
            key: HotkeyLogicalKey::Egui(egui::Key::Home),
        }],
        HotkeyActionId::LastImage => &[KeyChord {
            modifiers: 0,
            key: HotkeyLogicalKey::Egui(egui::Key::End),
        }],
        HotkeyActionId::ZoomIn => &[
            KeyChord {
                modifiers: MOD_CTRL,
                key: HotkeyLogicalKey::WheelDown,
            },
            KeyChord {
                modifiers: 0,
                key: HotkeyLogicalKey::Egui(egui::Key::Plus),
            },
            KeyChord {
                modifiers: 0,
                key: HotkeyLogicalKey::Egui(egui::Key::Equals),
            },
        ],
        HotkeyActionId::ZoomOut => &[
            KeyChord {
                modifiers: MOD_CTRL,
                key: HotkeyLogicalKey::WheelUp,
            },
            KeyChord {
                modifiers: 0,
                key: HotkeyLogicalKey::Egui(egui::Key::Minus),
            },
        ],
        HotkeyActionId::ZoomReset => &[
            KeyChord {
                modifiers: 0,
                key: HotkeyLogicalKey::Text("*"),
            },
            KeyChord {
                modifiers: MOD_CTRL,
                key: HotkeyLogicalKey::Egui(egui::Key::Num0),
            },
        ],
        HotkeyActionId::ToggleSettings => &[KeyChord {
            modifiers: 0,
            key: HotkeyLogicalKey::Egui(egui::Key::F1),
        }],
        HotkeyActionId::ToggleFullscreen => &[
            KeyChord {
                modifiers: 0,
                key: HotkeyLogicalKey::Egui(egui::Key::F11),
            },
            KeyChord {
                modifiers: 0,
                key: HotkeyLogicalKey::Egui(egui::Key::F),
            },
        ],
        HotkeyActionId::ToggleScaleMode => &[KeyChord {
            modifiers: 0,
            key: HotkeyLogicalKey::Egui(egui::Key::Z),
        }],
        HotkeyActionId::ToggleOsd => &[KeyChord {
            modifiers: 0,
            key: HotkeyLogicalKey::Egui(egui::Key::Tab),
        }],
        HotkeyActionId::RotateCw => &[
            KeyChord {
                modifiers: MOD_CTRL,
                key: HotkeyLogicalKey::Egui(egui::Key::ArrowRight),
            },
            KeyChord {
                modifiers: MOD_ALT,
                key: HotkeyLogicalKey::WheelDown,
            },
        ],
        HotkeyActionId::RotateCcw => &[
            KeyChord {
                modifiers: MOD_CTRL,
                key: HotkeyLogicalKey::Egui(egui::Key::ArrowLeft),
            },
            KeyChord {
                modifiers: MOD_ALT,
                key: HotkeyLogicalKey::WheelUp,
            },
        ],
        HotkeyActionId::HdrExposureUp => &[KeyChord {
            modifiers: MOD_CTRL,
            key: HotkeyLogicalKey::Egui(egui::Key::ArrowUp),
        }],
        HotkeyActionId::HdrExposureDown => &[KeyChord {
            modifiers: MOD_CTRL,
            key: HotkeyLogicalKey::Egui(egui::Key::ArrowDown),
        }],
        HotkeyActionId::DeleteToRecycleBin => &[KeyChord {
            modifiers: 0,
            key: HotkeyLogicalKey::Egui(egui::Key::Delete),
        }],
        HotkeyActionId::PermanentDelete => &[KeyChord {
            modifiers: MOD_SHIFT,
            key: HotkeyLogicalKey::Egui(egui::Key::Delete),
        }],
        HotkeyActionId::PrintCurrent => &[KeyChord {
            modifiers: MOD_CTRL,
            key: HotkeyLogicalKey::Egui(egui::Key::P),
        }],
        HotkeyActionId::ToggleGoto => &[KeyChord {
            modifiers: 0,
            key: HotkeyLogicalKey::Egui(egui::Key::G),
        }],
        HotkeyActionId::ToggleSlideshow => &[KeyChord {
            modifiers: 0,
            key: HotkeyLogicalKey::Egui(egui::Key::Space),
        }],
        HotkeyActionId::RefreshFileList => &[KeyChord {
            modifiers: 0,
            key: HotkeyLogicalKey::Egui(egui::Key::F5),
        }],
        #[cfg(not(target_os = "windows"))]
        HotkeyActionId::Quit => &[KeyChord {
            modifiers: MOD_CTRL,
            key: HotkeyLogicalKey::Egui(egui::Key::Q),
        }],
        HotkeyActionId::SelectPixelRegion => &[KeyChord {
            modifiers: MOD_SHIFT,
            key: HotkeyLogicalKey::MouseLeft,
        }],
        HotkeyActionId::ExitFullscreen => &[KeyChord {
            modifiers: 0,
            key: HotkeyLogicalKey::Egui(egui::Key::Escape),
        }],
        HotkeyActionId::CopyTo => &[KeyChord {
            modifiers: MOD_CTRL | MOD_SHIFT,
            key: HotkeyLogicalKey::Egui(egui::Key::C),
        }],
        HotkeyActionId::CutTo => &[KeyChord {
            modifiers: MOD_CTRL | MOD_SHIFT,
            key: HotkeyLogicalKey::Egui(egui::Key::X),
        }],
        HotkeyActionId::ToggleTray => &[KeyChord {
            modifiers: MOD_CTRL | MOD_SHIFT,
            key: HotkeyLogicalKey::Egui(egui::Key::T),
        }],
        HotkeyActionId::PickDirectory => &[KeyChord {
            modifiers: MOD_CTRL,
            key: HotkeyLogicalKey::Egui(egui::Key::O),
        }],
        HotkeyActionId::ToggleDirectoryTreeNav => &[KeyChord {
            modifiers: MOD_CTRL,
            key: HotkeyLogicalKey::Egui(egui::Key::T),
        }],
    }
}

pub fn parse_logical_key_name(value: &str) -> Option<HotkeyLogicalKey> {
    let trimmed = value.trim();
    let normalized = trimmed.to_ascii_lowercase();
    match normalized.as_str() {
        "wheelup" | "mousewheelup" | "mouse wheel up" => Some(HotkeyLogicalKey::WheelUp),
        "wheeldown" | "mousewheeldown" | "mouse wheel down" => Some(HotkeyLogicalKey::WheelDown),
        "leftclick" | "mouseleft" | "mouse left" | "mouse left click" => {
            Some(HotkeyLogicalKey::MouseLeft)
        }
        "rightclick" | "mouseright" | "mouse right" | "mouse right click" => {
            Some(HotkeyLogicalKey::MouseRight)
        }
        "middleclick" | "mousemiddle" | "mouse middle" | "mouse middle click" => {
            Some(HotkeyLogicalKey::MouseMiddle)
        }
        "plus" | "+" => Some(HotkeyLogicalKey::Text("+")),
        "minus" | "dash" | "-" => Some(HotkeyLogicalKey::Text("-")),
        "asterisk" | "*" => Some(HotkeyLogicalKey::Text("*")),
        "zero" => Some(HotkeyLogicalKey::Egui(egui::Key::Num0)),
        _ => egui::Key::from_name(trimmed).map(HotkeyLogicalKey::Egui),
    }
}

pub fn logical_key_to_name(key: HotkeyLogicalKey) -> &'static str {
    match key {
        HotkeyLogicalKey::Egui(key) => key.name(),
        HotkeyLogicalKey::Text("+") => "Plus",
        HotkeyLogicalKey::Text("-") => "Dash",
        HotkeyLogicalKey::Text("*") => "Asterisk",
        HotkeyLogicalKey::WheelUp => "WheelUp",
        HotkeyLogicalKey::WheelDown => "WheelDown",
        HotkeyLogicalKey::MouseLeft => "LeftClick",
        HotkeyLogicalKey::MouseRight => "RightClick",
        HotkeyLogicalKey::MouseMiddle => "MiddleClick",
        _ => "Unknown",
    }
}

pub fn normalized_bindings_map(
    config: &HotkeyConfigFile,
) -> HashMap<HotkeyActionId, HotkeyBindingEntry> {
    let mut out = HashMap::new();
    for entry in &config.bindings {
        if let Some(action_id) = action_id_from_str(&entry.action_id) {
            out.entry(action_id)
                .and_modify(|existing: &mut HotkeyBindingEntry| {
                    existing.keys.extend(entry.keys.iter().cloned());
                    existing.enabled |= entry.enabled;
                    if existing.comment.is_empty() {
                        existing.comment = entry.comment.clone();
                    }
                })
                .or_insert_with(|| entry.clone());
        }
    }
    out
}

fn default_true() -> bool {
    true
}

fn default_hotkeys_file_version() -> u32 {
    HOTKEYS_FILE_VERSION
}
