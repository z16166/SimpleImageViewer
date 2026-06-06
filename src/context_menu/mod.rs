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

pub mod io;
pub mod model;
pub mod validate;

#[derive(Debug, Clone)]
pub struct RuntimeContextMenuState {
    pub config: model::ContextMenuConfigFile,
}

pub fn load_runtime_context_menu_state() -> Result<RuntimeContextMenuState, String> {
    let config = io::load_or_init_context_menu_file()?;
    Ok(rebuild_runtime_state(&config))
}

pub fn rebuild_runtime_state(config: &model::ContextMenuConfigFile) -> RuntimeContextMenuState {
    RuntimeContextMenuState {
        config: validate::validate_context_menu_config(config).config,
    }
}

#[cfg(test)]
mod tests {
    use crate::context_menu::model::{
        BuiltinActionSource, CommandTemplate, ContextMenuCommand, ContextMenuConfigFile,
        ContextMenuEntry, ContextMenuItemKind, builtin_descriptors,
        default_context_menu_config_file, hotkey_actions_allowed_in_context_menu,
    };
    use crate::context_menu::validate::validate_context_menu_config;
    use crate::hotkeys::model::HotkeyActionId;

    #[test]
    fn default_config_contains_allowed_hotkey_current_image_actions() {
        let cfg = default_context_menu_config_file();
        let builtin_ids: std::collections::HashSet<_> = cfg
            .items
            .iter()
            .filter_map(|item| item.builtin_id.as_deref())
            .collect();

        for action in hotkey_actions_allowed_in_context_menu() {
            let desc = builtin_descriptors()
                .iter()
                .find(|desc| desc.hotkey_action == Some(action))
                .expect("allowed hotkey action has context menu descriptor");
            assert_eq!(desc.source, BuiltinActionSource::HotkeyAction(action));
            assert!(
                builtin_ids.contains(desc.id),
                "missing context menu item for {:?}",
                action
            );
        }

        assert!(
            builtin_descriptors()
                .iter()
                .all(|desc| desc.hotkey_action != Some(HotkeyActionId::NextImage)),
            "navigation hotkeys must not be context-menu candidates"
        );
    }

    #[test]
    fn default_config_enables_only_existing_context_menu_items() {
        let cfg = default_context_menu_config_file();
        for item in cfg
            .items
            .iter()
            .filter(|item| item.kind == ContextMenuItemKind::Builtin)
        {
            let desc = builtin_descriptors()
                .iter()
                .find(|desc| Some(desc.id) == item.builtin_id.as_deref())
                .expect("default builtin has descriptor");
            assert_eq!(item.enabled, desc.default_enabled, "{}", desc.id);
        }
    }

    #[test]
    fn validation_restores_missing_builtin_and_deduplicates_builtin() {
        let mut cfg = default_context_menu_config_file();
        let copy_path = cfg
            .items
            .iter()
            .find(|item| item.builtin_id.as_deref() == Some("copy_path"))
            .cloned()
            .expect("copy_path exists");
        cfg.items
            .retain(|item| item.builtin_id.as_deref() != Some("copy_path"));
        cfg.items.push(copy_path.clone());
        cfg.items.push(copy_path);

        let validated = validate_context_menu_config(&cfg);
        let copy_path_count = validated
            .config
            .items
            .iter()
            .filter(|item| item.builtin_id.as_deref() == Some("copy_path"))
            .count();
        assert_eq!(copy_path_count, 1);
    }

    #[test]
    fn validation_drops_invalid_custom_actions_but_keeps_separators() {
        let cfg = ContextMenuConfigFile {
            version: 1,
            items: vec![
                ContextMenuEntry::separator(),
                ContextMenuEntry {
                    kind: ContextMenuItemKind::Custom,
                    enabled: true,
                    builtin_id: None,
                    label: "   ".to_string(),
                    command: Some(ContextMenuCommand::Executable {
                        path: "C:/Tools/Edit.exe".to_string(),
                    }),
                },
                ContextMenuEntry {
                    kind: ContextMenuItemKind::Custom,
                    enabled: true,
                    builtin_id: None,
                    label: "Open in Editor".to_string(),
                    command: Some(ContextMenuCommand::CommandLine {
                        template: CommandTemplate::new("\"C:/Tools/Edit.exe\" \"%1\"".to_string()),
                    }),
                },
            ],
        };

        let validated = validate_context_menu_config(&cfg);
        assert!(
            validated
                .config
                .items
                .iter()
                .any(|item| item.kind == ContextMenuItemKind::Separator)
        );
        assert!(
            validated
                .config
                .items
                .iter()
                .any(|item| item.label == "Open in Editor")
        );
        assert!(
            validated
                .config
                .items
                .iter()
                .filter(|item| item.kind == ContextMenuItemKind::Custom)
                .all(|item| !item.label.trim().is_empty())
        );
    }

    #[test]
    fn command_building_quotes_paths_and_replaces_placeholder() {
        let image = std::path::Path::new("D:/Work Images/photo 1.jpg");
        let exe = ContextMenuCommand::Executable {
            path: "C:/Program Files/App/App.exe".to_string(),
        };
        let cmd = exe.command_line_for_image(image).expect("exe command");
        assert_eq!(
            cmd,
            "\"C:/Program Files/App/App.exe\" \"D:/Work Images/photo 1.jpg\""
        );

        let template = ContextMenuCommand::CommandLine {
            template: CommandTemplate::new("\"C:/Program Files/App/App.exe\" \"%1\"".to_string()),
        };
        let cmd = template
            .command_line_for_image(image)
            .expect("template command");
        assert_eq!(
            cmd,
            "\"C:/Program Files/App/App.exe\" \"D:/Work Images/photo 1.jpg\""
        );
    }
}
