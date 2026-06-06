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

use crate::context_menu::model::{
    CONTEXT_MENU_FILE_VERSION, ContextMenuConfigFile, ContextMenuEntry, ContextMenuItemKind,
    builtin_descriptor, builtin_descriptors,
};
use std::collections::HashSet;

#[derive(Debug, Clone)]
pub struct ContextMenuValidationOutput {
    pub config: ContextMenuConfigFile,
}

pub fn validate_context_menu_config(config: &ContextMenuConfigFile) -> ContextMenuValidationOutput {
    let mut items = Vec::new();
    let mut seen_builtins = HashSet::new();

    for item in &config.items {
        match item.kind {
            ContextMenuItemKind::Builtin => {
                let Some(id) = item.builtin_id.as_deref() else {
                    continue;
                };
                let Some(desc) = builtin_descriptor(id) else {
                    continue;
                };
                if !seen_builtins.insert(desc.id) {
                    continue;
                }
                items.push(ContextMenuEntry::builtin(desc.id, item.enabled));
            }
            ContextMenuItemKind::Separator => items.push(ContextMenuEntry::separator()),
            ContextMenuItemKind::Custom => {
                let label = item.label.trim();
                let Some(command) = item.command.as_ref() else {
                    continue;
                };
                if label.is_empty() || !command.is_valid() {
                    continue;
                }
                items.push(ContextMenuEntry {
                    kind: ContextMenuItemKind::Custom,
                    enabled: item.enabled,
                    builtin_id: None,
                    label: label.to_string(),
                    command: Some(command.clone()),
                });
            }
        }
    }

    for desc in builtin_descriptors() {
        if seen_builtins.insert(desc.id) {
            items.push(ContextMenuEntry::builtin(desc.id, desc.default_enabled));
        }
    }

    ContextMenuValidationOutput {
        config: ContextMenuConfigFile {
            version: CONTEXT_MENU_FILE_VERSION,
            items,
        },
    }
}
