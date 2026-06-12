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

use crate::hotkeys::model::HotkeyActionId;
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const CONTEXT_MENU_FILE_NAME: &str = "siv_context_menu.yaml";
pub const CONTEXT_MENU_FILE_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinActionSource {
    ExistingContextMenuOnly,
    HotkeyAction(HotkeyActionId),
}

#[derive(Debug, Clone, Copy)]
pub struct ContextMenuBuiltinDescriptor {
    pub id: &'static str,
    pub label_key: &'static str,
    #[allow(dead_code)]
    pub source: BuiltinActionSource,
    #[allow(dead_code)]
    pub hotkey_action: Option<HotkeyActionId>,
    pub default_enabled: bool,
}

pub const CONTEXT_MENU_BUILTINS: &[ContextMenuBuiltinDescriptor] = &[
    ContextMenuBuiltinDescriptor {
        id: "copy_path",
        label_key: "ctx.copy_path",
        source: BuiltinActionSource::ExistingContextMenuOnly,
        hotkey_action: None,
        default_enabled: true,
    },
    ContextMenuBuiltinDescriptor {
        id: "copy_file",
        label_key: "ctx.copy_file",
        source: BuiltinActionSource::ExistingContextMenuOnly,
        hotkey_action: None,
        default_enabled: true,
    },
    ContextMenuBuiltinDescriptor {
        id: "copy_to",
        label_key: "ctx.copy_to",
        source: BuiltinActionSource::HotkeyAction(HotkeyActionId::CopyTo),
        hotkey_action: Some(HotkeyActionId::CopyTo),
        default_enabled: true,
    },
    ContextMenuBuiltinDescriptor {
        id: "cut_to",
        label_key: "ctx.cut_to",
        source: BuiltinActionSource::HotkeyAction(HotkeyActionId::CutTo),
        hotkey_action: Some(HotkeyActionId::CutTo),
        default_enabled: true,
    },
    ContextMenuBuiltinDescriptor {
        id: "view_exif",
        label_key: "ctx.view_exif",
        source: BuiltinActionSource::ExistingContextMenuOnly,
        hotkey_action: None,
        default_enabled: true,
    },
    ContextMenuBuiltinDescriptor {
        id: "view_xmp",
        label_key: "ctx.view_xmp",
        source: BuiltinActionSource::ExistingContextMenuOnly,
        hotkey_action: None,
        default_enabled: true,
    },
    ContextMenuBuiltinDescriptor {
        id: "zoom_in",
        label_key: "hotkeys.action.zoom_in",
        source: BuiltinActionSource::HotkeyAction(HotkeyActionId::ZoomIn),
        hotkey_action: Some(HotkeyActionId::ZoomIn),
        default_enabled: false,
    },
    ContextMenuBuiltinDescriptor {
        id: "zoom_out",
        label_key: "hotkeys.action.zoom_out",
        source: BuiltinActionSource::HotkeyAction(HotkeyActionId::ZoomOut),
        hotkey_action: Some(HotkeyActionId::ZoomOut),
        default_enabled: false,
    },
    ContextMenuBuiltinDescriptor {
        id: "zoom_reset",
        label_key: "hotkeys.action.zoom_reset",
        source: BuiltinActionSource::HotkeyAction(HotkeyActionId::ZoomReset),
        hotkey_action: Some(HotkeyActionId::ZoomReset),
        default_enabled: false,
    },
    ContextMenuBuiltinDescriptor {
        id: "toggle_scale_mode",
        label_key: "hotkeys.action.toggle_scale_mode",
        source: BuiltinActionSource::HotkeyAction(HotkeyActionId::ToggleScaleMode),
        hotkey_action: Some(HotkeyActionId::ToggleScaleMode),
        default_enabled: false,
    },
    ContextMenuBuiltinDescriptor {
        id: "toggle_osd",
        label_key: "hotkeys.action.toggle_osd",
        source: BuiltinActionSource::HotkeyAction(HotkeyActionId::ToggleOsd),
        hotkey_action: Some(HotkeyActionId::ToggleOsd),
        default_enabled: false,
    },
    ContextMenuBuiltinDescriptor {
        id: "rotate_ccw",
        label_key: "ctx.rotate_ccw",
        source: BuiltinActionSource::HotkeyAction(HotkeyActionId::RotateCcw),
        hotkey_action: Some(HotkeyActionId::RotateCcw),
        default_enabled: true,
    },
    ContextMenuBuiltinDescriptor {
        id: "rotate_cw",
        label_key: "ctx.rotate_cw",
        source: BuiltinActionSource::HotkeyAction(HotkeyActionId::RotateCw),
        hotkey_action: Some(HotkeyActionId::RotateCw),
        default_enabled: true,
    },
    ContextMenuBuiltinDescriptor {
        id: "hdr_exposure_up",
        label_key: "hotkeys.action.hdr_exposure_up",
        source: BuiltinActionSource::HotkeyAction(HotkeyActionId::HdrExposureUp),
        hotkey_action: Some(HotkeyActionId::HdrExposureUp),
        default_enabled: false,
    },
    ContextMenuBuiltinDescriptor {
        id: "hdr_exposure_down",
        label_key: "hotkeys.action.hdr_exposure_down",
        source: BuiltinActionSource::HotkeyAction(HotkeyActionId::HdrExposureDown),
        hotkey_action: Some(HotkeyActionId::HdrExposureDown),
        default_enabled: false,
    },
    ContextMenuBuiltinDescriptor {
        id: "delete_to_recycle_bin",
        label_key: "hotkeys.action.delete_to_recycle_bin",
        source: BuiltinActionSource::HotkeyAction(HotkeyActionId::DeleteToRecycleBin),
        hotkey_action: Some(HotkeyActionId::DeleteToRecycleBin),
        default_enabled: false,
    },
    ContextMenuBuiltinDescriptor {
        id: "permanent_delete",
        label_key: "hotkeys.action.permanent_delete",
        source: BuiltinActionSource::HotkeyAction(HotkeyActionId::PermanentDelete),
        hotkey_action: Some(HotkeyActionId::PermanentDelete),
        default_enabled: false,
    },
    ContextMenuBuiltinDescriptor {
        id: "print_current",
        label_key: "ctx.print_full",
        source: BuiltinActionSource::HotkeyAction(HotkeyActionId::PrintCurrent),
        hotkey_action: Some(HotkeyActionId::PrintCurrent),
        default_enabled: true,
    },
    ContextMenuBuiltinDescriptor {
        id: "print_visible",
        label_key: "ctx.print_visible",
        source: BuiltinActionSource::ExistingContextMenuOnly,
        hotkey_action: None,
        default_enabled: true,
    },
    ContextMenuBuiltinDescriptor {
        id: "set_wallpaper",
        label_key: "ctx.set_wallpaper",
        source: BuiltinActionSource::ExistingContextMenuOnly,
        hotkey_action: None,
        default_enabled: true,
    },
    ContextMenuBuiltinDescriptor {
        id: "toggle_fullscreen",
        label_key: "hotkeys.action.toggle_fullscreen",
        source: BuiltinActionSource::HotkeyAction(HotkeyActionId::ToggleFullscreen),
        hotkey_action: Some(HotkeyActionId::ToggleFullscreen),
        default_enabled: true,
    },
    ContextMenuBuiltinDescriptor {
        id: "exit_fullscreen",
        label_key: "hotkeys.action.exit_fullscreen",
        source: BuiltinActionSource::HotkeyAction(HotkeyActionId::ExitFullscreen),
        hotkey_action: Some(HotkeyActionId::ExitFullscreen),
        default_enabled: false,
    },
];

pub fn builtin_descriptors() -> &'static [ContextMenuBuiltinDescriptor] {
    CONTEXT_MENU_BUILTINS
}

pub fn builtin_descriptor(id: &str) -> Option<&'static ContextMenuBuiltinDescriptor> {
    CONTEXT_MENU_BUILTINS.iter().find(|desc| desc.id == id)
}

#[cfg(test)]
pub fn hotkey_actions_allowed_in_context_menu() -> Vec<HotkeyActionId> {
    vec![
        HotkeyActionId::ZoomIn,
        HotkeyActionId::ZoomOut,
        HotkeyActionId::ZoomReset,
        HotkeyActionId::ToggleScaleMode,
        HotkeyActionId::ToggleOsd,
        HotkeyActionId::RotateCcw,
        HotkeyActionId::RotateCw,
        HotkeyActionId::HdrExposureUp,
        HotkeyActionId::HdrExposureDown,
        HotkeyActionId::DeleteToRecycleBin,
        HotkeyActionId::PermanentDelete,
        HotkeyActionId::PrintCurrent,
        HotkeyActionId::ToggleFullscreen,
        HotkeyActionId::ExitFullscreen,
        HotkeyActionId::CopyTo,
        HotkeyActionId::CutTo,
    ]
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextMenuItemKind {
    Builtin,
    Separator,
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextMenuConfigFile {
    #[serde(default = "default_context_menu_file_version")]
    pub version: u32,
    #[serde(default)]
    pub items: Vec<ContextMenuEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextMenuEntry {
    pub kind: ContextMenuItemKind,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub builtin_id: Option<String>,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub command: Option<ContextMenuCommand>,
}

impl ContextMenuEntry {
    pub fn builtin(id: &str, enabled: bool) -> Self {
        Self {
            kind: ContextMenuItemKind::Builtin,
            enabled,
            builtin_id: Some(id.to_string()),
            label: String::new(),
            command: None,
        }
    }

    pub fn separator() -> Self {
        Self {
            kind: ContextMenuItemKind::Separator,
            enabled: true,
            builtin_id: None,
            label: String::new(),
            command: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditableContextMenuEntryKind {
    Separator,
    Custom,
}

#[derive(Debug, Clone)]
pub struct EditableContextMenuEntry {
    pub kind: EditableContextMenuEntryKind,
    pub label: String,
    pub command: ContextMenuCommand,
}

impl Default for EditableContextMenuEntry {
    fn default() -> Self {
        Self {
            kind: EditableContextMenuEntryKind::Custom,
            label: String::new(),
            command: ContextMenuCommand::Executable {
                path: String::new(),
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextMenuCommand {
    Executable { path: String },
    CommandLine { template: CommandTemplate },
}

impl Default for ContextMenuCommand {
    fn default() -> Self {
        Self::Executable {
            path: String::new(),
        }
    }
}

impl ContextMenuCommand {
    pub fn command_line_for_image(&self, image_path: &Path) -> Option<String> {
        let image = quote_arg(&path_to_string(image_path));
        match self {
            Self::Executable { path } => {
                let exe = path.trim();
                (!exe.is_empty()).then(|| format!("{} {}", quote_arg(exe), image))
            }
            Self::CommandLine { template } => template.command_line_for_image(image_path),
        }
    }

    pub fn is_valid(&self) -> bool {
        match self {
            Self::Executable { path } => !path.trim().is_empty(),
            Self::CommandLine { template } => template.is_valid(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandTemplate {
    pub template: String,
}

impl CommandTemplate {
    pub fn new(template: String) -> Self {
        Self { template }
    }

    pub fn command_line_for_image(&self, image_path: &Path) -> Option<String> {
        let template = self.template.trim();
        if !self.is_valid() {
            return None;
        }
        let quoted_image = quote_arg(&path_to_string(image_path));
        if template.contains("\"%1\"") {
            Some(template.replace("\"%1\"", &quoted_image))
        } else {
            Some(template.replace("%1", &quoted_image))
        }
    }

    pub fn is_valid(&self) -> bool {
        let template = self.template.trim();
        !template.is_empty() && template.contains("%1")
    }
}

pub fn default_context_menu_config_file() -> ContextMenuConfigFile {
    let mut items = Vec::new();
    for desc in CONTEXT_MENU_BUILTINS {
        if matches!(
            desc.id,
            "view_exif" | "rotate_ccw" | "print_current" | "set_wallpaper" | "toggle_fullscreen"
        ) {
            items.push(ContextMenuEntry::separator());
        }
        items.push(ContextMenuEntry::builtin(desc.id, desc.default_enabled));
    }
    ContextMenuConfigFile {
        version: CONTEXT_MENU_FILE_VERSION,
        items,
    }
}

pub fn default_context_menu_file_version() -> u32 {
    CONTEXT_MENU_FILE_VERSION
}

fn default_true() -> bool {
    true
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

pub fn quote_arg(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2 {
        let inner = &trimmed[1..trimmed.len() - 1];
        format!("\"{}\"", inner.replace('"', "\\\""))
    } else {
        format!("\"{}\"", trimmed.replace('"', "\\\""))
    }
}
