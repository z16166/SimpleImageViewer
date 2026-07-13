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

//! Folder-tree keyboard navigation helpers.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crossbeam_channel::Sender;
use eframe::egui;

use super::view::{DirectoryTreeUiChrome, DirectoryTreeView};
use super::{
    DirectoryTreeCommand, DirectoryTreeNode, is_places_sentinel_namespace_path,
    network_namespace_path, send_directory_tree_command, this_pc_namespace_path,
};
use crate::app::ImageViewerApp;
use crate::directory_tree_places::KnownFolderEntry;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum FolderTreeNavOutcome {
    /// Select this namespace node (or toggle-expand if Places sentinel).
    MoveTo(PathBuf),
    /// Ensure expanded + children scan; select first child now or after load.
    ExpandTowardFirstChild(PathBuf),
    /// Force-refresh children of this node; keep selection.
    RefreshChildren(PathBuf),
    Noop,
}

/// Immutable tree data needed to resolve a keyboard navigation request during paint.
#[derive(Clone, Copy)]
pub(super) struct FolderTreeNavInput<'a> {
    pub(super) nodes: &'a HashMap<PathBuf, Arc<DirectoryTreeNode>>,
    pub(super) known_folders: &'a [KnownFolderEntry],
    pub(super) network_visible: bool,
}

pub(super) fn resolve_folder_tree_key(
    input: FolderTreeNavInput<'_>,
    current: &Path,
    key: egui::Key,
) -> FolderTreeNavOutcome {
    if !input.nodes.contains_key(current) {
        return FolderTreeNavOutcome::Noop;
    }
    match key {
        egui::Key::ArrowLeft => find_parent_namespace(input, current)
            .map(FolderTreeNavOutcome::MoveTo)
            .unwrap_or(FolderTreeNavOutcome::Noop),
        egui::Key::ArrowRight => {
            let Some(node) = input.nodes.get(current) else {
                return FolderTreeNavOutcome::Noop;
            };
            if node.expanded && node.children_loaded {
                if let Some(first) = node.children.first() {
                    FolderTreeNavOutcome::MoveTo(first.clone())
                } else {
                    FolderTreeNavOutcome::Noop
                }
            } else {
                FolderTreeNavOutcome::ExpandTowardFirstChild(current.to_path_buf())
            }
        }
        egui::Key::ArrowUp => previous_sibling(input, current)
            .map(FolderTreeNavOutcome::MoveTo)
            .unwrap_or_else(|| {
                find_parent_namespace(input, current)
                    .map(FolderTreeNavOutcome::MoveTo)
                    .unwrap_or(FolderTreeNavOutcome::Noop)
            }),
        egui::Key::ArrowDown => {
            if let Some(next) = next_sibling(input, current) {
                FolderTreeNavOutcome::MoveTo(next)
            } else if let Some(parent) = find_parent_namespace(input, current) {
                next_sibling(input, &parent)
                    .map(FolderTreeNavOutcome::MoveTo)
                    .unwrap_or(FolderTreeNavOutcome::Noop)
            } else {
                FolderTreeNavOutcome::Noop
            }
        }
        egui::Key::F5 => FolderTreeNavOutcome::RefreshChildren(current.to_path_buf()),
        _ => FolderTreeNavOutcome::Noop,
    }
}

fn top_level_roots(input: FolderTreeNavInput<'_>) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = input
        .known_folders
        .iter()
        .map(|entry| entry.namespace_path.clone())
        .collect();
    roots.push(this_pc_namespace_path());
    if input.network_visible {
        roots.push(network_namespace_path());
    }
    roots
}

fn find_parent_namespace(input: FolderTreeNavInput<'_>, path: &Path) -> Option<PathBuf> {
    for (parent, node) in input.nodes {
        if node
            .children
            .iter()
            .any(|child| child.as_os_str() == path.as_os_str())
        {
            return Some(parent.clone());
        }
    }
    None
}

fn sibling_list(input: FolderTreeNavInput<'_>, path: &Path) -> Vec<PathBuf> {
    if let Some(parent) = find_parent_namespace(input, path) {
        input
            .nodes
            .get(&parent)
            .map(|node| node.children.clone())
            .unwrap_or_default()
    } else {
        top_level_roots(input)
    }
}

fn previous_sibling(input: FolderTreeNavInput<'_>, path: &Path) -> Option<PathBuf> {
    let siblings = sibling_list(input, path);
    let idx = siblings
        .iter()
        .position(|sibling| sibling.as_os_str() == path.as_os_str())?;
    (idx > 0).then(|| siblings[idx - 1].clone())
}

fn next_sibling(input: FolderTreeNavInput<'_>, path: &Path) -> Option<PathBuf> {
    let siblings = sibling_list(input, path);
    let idx = siblings
        .iter()
        .position(|sibling| sibling.as_os_str() == path.as_os_str())?;
    siblings.get(idx + 1).cloned()
}

pub(super) fn try_handle_folder_tree_keys(
    ui: &mut egui::Ui,
    view: &DirectoryTreeView,
    chrome: &mut DirectoryTreeUiChrome,
    command_tx: &Sender<DirectoryTreeCommand>,
    embedded: bool,
) {
    if !ImageViewerApp::directory_tree_list_accepts_keyboard_input(ui.ctx(), embedded)
        || !chrome.folder_tree_keyboard_active
    {
        return;
    }
    let Some(current) = view.selected_namespace_path() else {
        return;
    };

    let pressed = ui.input(|input| {
        [
            egui::Key::ArrowLeft,
            egui::Key::ArrowRight,
            egui::Key::ArrowUp,
            egui::Key::ArrowDown,
            egui::Key::F5,
        ]
        .into_iter()
        .find(|key| input.key_pressed(*key))
    });
    let Some(key) = pressed else {
        return;
    };

    let outcome = resolve_folder_tree_key(
        FolderTreeNavInput {
            nodes: view.nodes(),
            known_folders: view.known_folders(),
            network_visible: view.network_visible(),
        },
        current,
        key,
    );
    ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, key));

    match outcome {
        FolderTreeNavOutcome::Noop => {}
        FolderTreeNavOutcome::MoveTo(path) => {
            if is_places_sentinel_namespace_path(&path) {
                chrome.folder_tree_keyboard_active = true;
                chrome.image_list_keyboard_active = false;
                send_directory_tree_command(command_tx, DirectoryTreeCommand::ToggleExpanded(path));
            } else if let Some(node) = view.nodes().get(&path) {
                chrome.folder_tree_keyboard_active = true;
                chrome.image_list_keyboard_active = false;
                chrome.scroll_folder_tree_to_selected = true;
                send_directory_tree_command(
                    command_tx,
                    DirectoryTreeCommand::SelectDirectory {
                        namespace_path: path,
                        fs_path: node.fs_path.clone(),
                    },
                );
            } else {
                log::debug!(
                    "[DirectoryTree] MoveTo skipped; node missing from paint snapshot: {}",
                    path.display()
                );
            }
        }
        FolderTreeNavOutcome::ExpandTowardFirstChild(path) => {
            send_directory_tree_command(
                command_tx,
                DirectoryTreeCommand::ExpandTowardFirstChild(path),
            );
        }
        FolderTreeNavOutcome::RefreshChildren(path) => {
            chrome.scroll_folder_tree_to_selected = true;
            send_directory_tree_command(command_tx, DirectoryTreeCommand::RefreshChildren(path));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{DirectoryTreeTreeState, MAX_DIRECTORY_TREE_NODES, directory_tree_node};
    use super::*;

    fn resolve_tree_key(
        tree: &DirectoryTreeTreeState,
        current: &Path,
        key: egui::Key,
    ) -> FolderTreeNavOutcome {
        let nodes = tree
            .nodes
            .iter()
            .map(|(path, node)| (path.clone(), Arc::new(node.clone())))
            .collect::<HashMap<_, _>>();
        resolve_folder_tree_key(
            FolderTreeNavInput {
                nodes: &nodes,
                known_folders: &tree.known_folders,
                network_visible: tree.network_visible,
            },
            current,
            key,
        )
    }

    fn insert(tree: &mut DirectoryTreeTreeState, path: PathBuf, name: &str, fs: PathBuf) {
        tree.nodes
            .or_insert_with(path, MAX_DIRECTORY_TREE_NODES, || {
                directory_tree_node(name, fs)
            })
            .expect("insert");
    }

    fn link(tree: &mut DirectoryTreeTreeState, parent: &Path, child: PathBuf) {
        let node = tree.nodes.get_mut(parent).expect("parent");
        node.children.push(child);
        node.children_loaded = true;
        node.expanded = true;
    }

    #[test]
    fn left_moves_to_parent_or_noop_at_root() {
        let mut tree = DirectoryTreeTreeState::default();
        let root = this_pc_namespace_path();
        let child = PathBuf::from(r"\\?\siv-tree\this-pc\C");
        insert(&mut tree, root.clone(), "This PC", PathBuf::from("ThisPc"));
        insert(&mut tree, child.clone(), "C:", PathBuf::from("C:\\"));
        link(&mut tree, &root, child.clone());

        assert_eq!(
            resolve_tree_key(&tree, &child, egui::Key::ArrowLeft),
            FolderTreeNavOutcome::MoveTo(root.clone())
        );
        assert_eq!(
            resolve_tree_key(&tree, &root, egui::Key::ArrowLeft),
            FolderTreeNavOutcome::Noop
        );
    }

    #[test]
    fn right_selects_first_child_or_requests_expand() {
        let mut tree = DirectoryTreeTreeState::default();
        let root = this_pc_namespace_path();
        let child = PathBuf::from(r"\\?\siv-tree\this-pc\C");
        insert(&mut tree, root.clone(), "This PC", PathBuf::from("ThisPc"));
        insert(&mut tree, child.clone(), "C:", PathBuf::from("C:\\"));
        link(&mut tree, &root, child.clone());

        assert_eq!(
            resolve_tree_key(&tree, &root, egui::Key::ArrowRight),
            FolderTreeNavOutcome::MoveTo(child.clone())
        );

        tree.nodes.get_mut(&root).expect("root node").expanded = false;
        assert_eq!(
            resolve_tree_key(&tree, &root, egui::Key::ArrowRight),
            FolderTreeNavOutcome::ExpandTowardFirstChild(root.clone())
        );

        let unloaded = PathBuf::from(r"\\?\siv-tree\this-pc\D");
        insert(&mut tree, unloaded.clone(), "D:", PathBuf::from("D:\\"));
        // children_loaded false
        assert_eq!(
            resolve_tree_key(&tree, &unloaded, egui::Key::ArrowRight),
            FolderTreeNavOutcome::ExpandTowardFirstChild(unloaded)
        );
    }

    #[test]
    fn up_previous_sibling_else_parent() {
        let mut tree = DirectoryTreeTreeState::default();
        let root = this_pc_namespace_path();
        let a = PathBuf::from(r"\\?\siv-tree\this-pc\A");
        let b = PathBuf::from(r"\\?\siv-tree\this-pc\B");
        insert(&mut tree, root.clone(), "This PC", PathBuf::from("ThisPc"));
        insert(&mut tree, a.clone(), "A", PathBuf::from("A:\\"));
        insert(&mut tree, b.clone(), "B", PathBuf::from("B:\\"));
        link(&mut tree, &root, a.clone());
        link(&mut tree, &root, b.clone());

        assert_eq!(
            resolve_tree_key(&tree, &b, egui::Key::ArrowUp),
            FolderTreeNavOutcome::MoveTo(a.clone())
        );
        assert_eq!(
            resolve_tree_key(&tree, &a, egui::Key::ArrowUp),
            FolderTreeNavOutcome::MoveTo(root)
        );
    }

    #[test]
    fn down_next_sibling_else_uncle() {
        let mut tree = DirectoryTreeTreeState::default();
        let root = this_pc_namespace_path();
        let parent = PathBuf::from(r"\\?\siv-tree\this-pc\C");
        let uncle = PathBuf::from(r"\\?\siv-tree\this-pc\D");
        let leaf = PathBuf::from(r"\\?\siv-tree\this-pc\C\leaf");
        insert(&mut tree, root.clone(), "This PC", PathBuf::from("ThisPc"));
        insert(&mut tree, parent.clone(), "C:", PathBuf::from("C:\\"));
        insert(&mut tree, uncle.clone(), "D:", PathBuf::from("D:\\"));
        insert(&mut tree, leaf.clone(), "leaf", PathBuf::from("C:\\leaf"));
        link(&mut tree, &root, parent.clone());
        link(&mut tree, &root, uncle.clone());
        link(&mut tree, &parent, leaf.clone());

        assert_eq!(
            resolve_tree_key(&tree, &leaf, egui::Key::ArrowDown),
            FolderTreeNavOutcome::MoveTo(uncle.clone())
        );
        assert_eq!(
            resolve_tree_key(&tree, &uncle, egui::Key::ArrowDown),
            FolderTreeNavOutcome::Noop
        );
    }

    #[test]
    fn f5_requests_refresh_children() {
        let mut tree = DirectoryTreeTreeState::default();
        let current = PathBuf::from(r"\\?\siv-tree\this-pc\C");
        insert(&mut tree, current.clone(), "C:", PathBuf::from("C:\\"));

        assert_eq!(
            resolve_tree_key(&tree, &current, egui::Key::F5),
            FolderTreeNavOutcome::RefreshChildren(current)
        );
    }

    #[test]
    fn right_on_loaded_empty_children_is_noop() {
        let mut tree = DirectoryTreeTreeState::default();
        let empty = PathBuf::from(r"\\?\siv-tree\this-pc\Empty");
        insert(
            &mut tree,
            empty.clone(),
            "Empty",
            PathBuf::from("C:\\Empty"),
        );
        let node = tree.nodes.get_mut(&empty).expect("empty node");
        node.children_loaded = true;
        node.expanded = true;
        node.children.clear();

        assert_eq!(
            resolve_tree_key(&tree, &empty, egui::Key::ArrowRight),
            FolderTreeNavOutcome::Noop
        );
    }

    #[test]
    fn top_level_up_down_moves_among_roots() {
        let mut tree = DirectoryTreeTreeState::default();
        let this_pc = this_pc_namespace_path();
        let network = network_namespace_path();
        insert(
            &mut tree,
            this_pc.clone(),
            "This PC",
            PathBuf::from("ThisPc"),
        );
        insert(
            &mut tree,
            network.clone(),
            "Network",
            PathBuf::from("Network"),
        );
        tree.network_visible = true;

        assert_eq!(
            resolve_tree_key(&tree, &this_pc, egui::Key::ArrowDown),
            FolderTreeNavOutcome::MoveTo(network.clone())
        );
        assert_eq!(
            resolve_tree_key(&tree, &network, egui::Key::ArrowUp),
            FolderTreeNavOutcome::MoveTo(this_pc.clone())
        );
        assert_eq!(
            resolve_tree_key(&tree, &this_pc, egui::Key::ArrowUp),
            FolderTreeNavOutcome::Noop
        );
        assert_eq!(
            resolve_tree_key(&tree, &network, egui::Key::ArrowDown),
            FolderTreeNavOutcome::Noop
        );
    }

    #[test]
    fn unknown_path_resolves_to_noop() {
        let tree = DirectoryTreeTreeState::default();
        let missing = PathBuf::from(r"\\?\siv-tree\missing");

        assert_eq!(
            resolve_tree_key(&tree, &missing, egui::Key::ArrowLeft),
            FolderTreeNavOutcome::Noop
        );
        assert_eq!(
            resolve_tree_key(&tree, &missing, egui::Key::F5),
            FolderTreeNavOutcome::Noop
        );
    }
}
