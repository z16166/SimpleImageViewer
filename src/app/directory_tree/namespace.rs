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

//! Shell-style namespace paths for the directory tree (`\\?\siv-tree\...`).
//!
//! Tree node keys (`tree_path`) live in this namespace and are distinct from filesystem
//! browse paths. The same on-disk folder may appear under multiple parents with different
//! namespace keys (e.g. Places mount shortcut vs root `/` expansion).

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

pub(crate) const TREE_NAMESPACE_PREFIX: &str = r"\\?\siv-tree\";

pub(crate) fn is_tree_namespace_path(path: &Path) -> bool {
    path.starts_with(TREE_NAMESPACE_PREFIX)
}

pub(crate) fn is_mount_tree_path(path: &Path) -> bool {
    path.starts_with(format!("{TREE_NAMESPACE_PREFIX}Mount\\"))
        || path.starts_with(format!("{TREE_NAMESPACE_PREFIX}Mount/"))
}

pub(crate) fn is_network_share_tree_path(path: &Path) -> bool {
    path.starts_with(format!("{TREE_NAMESPACE_PREFIX}Network\\Share\\"))
        || path.starts_with(format!("{TREE_NAMESPACE_PREFIX}Network/Share/"))
}

fn is_places_sentinel_namespace(path: &Path) -> bool {
    path.as_os_str() == OsStr::new(r"\\?\siv-tree\ThisPC")
        || path.as_os_str() == OsStr::new(r"\\?\siv-tree\Network")
}

fn namespace_segment_count(path: &Path) -> usize {
    path.strip_prefix(TREE_NAMESPACE_PREFIX)
        .map(|rest| rest.components().count())
        .unwrap_or(0)
}

/// True when `path` is a top-level namespace node (Places root, mount, known folder, share).
pub(crate) fn is_namespace_tree_root(path: &Path) -> bool {
    if !is_tree_namespace_path(path) {
        return false;
    }
    if is_places_sentinel_namespace(path) {
        return true;
    }
    let count = namespace_segment_count(path);
    if path.starts_with(format!("{TREE_NAMESPACE_PREFIX}KnownFolder\\"))
        || path.starts_with(format!("{TREE_NAMESPACE_PREFIX}KnownFolder/"))
    {
        return count == 2;
    }
    if is_mount_tree_path(path) {
        return count == 2;
    }
    if is_network_share_tree_path(path) {
        return count == 3;
    }
    false
}

fn encode_mount_id(mount_browse: &Path) -> String {
    mount_browse
        .to_string_lossy()
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b' ' => {
                char::from(byte).to_string()
            }
            b'\\' => "%5C".to_string(),
            b'/' => "%2F".to_string(),
            b':' => "%3A".to_string(),
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

/// Namespace key for a Places drive / mount entry.
pub(crate) fn drive_mount_tree_path(mount_browse: &Path) -> PathBuf {
    PathBuf::from(format!(
        r"{TREE_NAMESPACE_PREFIX}Mount\{}",
        encode_mount_id(mount_browse)
    ))
}

/// Namespace key for a UNC share root under Network.
pub(crate) fn network_share_tree_path(share_browse: &Path) -> PathBuf {
    PathBuf::from(format!(
        r"{TREE_NAMESPACE_PREFIX}Network\Share\{}",
        encode_mount_id(share_browse)
    ))
}

pub(crate) fn network_share_browse_for_tree_path(
    tree: &Path,
    share_roots: impl IntoIterator<Item = PathBuf>,
) -> Option<PathBuf> {
    let mut best: Option<(usize, PathBuf)> = None;
    for share in share_roots {
        let share_tree = network_share_tree_path(&share);
        if tree == share_tree.as_path() {
            let depth = share.components().count();
            if best.as_ref().is_none_or(|(d, _)| depth > *d) {
                best = Some((depth, share));
            }
            continue;
        }
        if tree.starts_with(&share_tree) {
            let mut browse = share.clone();
            if let Ok(rest) = tree.strip_prefix(&share_tree) {
                for component in rest.components() {
                    browse.push(component.as_os_str());
                }
            }
            let depth = share.components().count();
            if best.as_ref().is_none_or(|(d, _)| depth > *d) {
                best = Some((depth, browse));
            }
        }
    }
    best.map(|(_, browse)| browse)
}

/// Extend a namespace parent with the final path segment of `child_browse`.
pub(crate) fn tree_child_path(parent_tree: &Path, child_browse: &Path) -> PathBuf {
    let segment = child_browse
        .file_name()
        .unwrap_or_else(|| child_browse.as_os_str());
    parent_tree.join(segment)
}

/// Build namespace ancestor chain from `root_tree` / `root_browse` to `target_browse`.
pub(crate) fn namespace_ancestor_chain(
    root_tree: &Path,
    root_browse: &Path,
    target_browse: &Path,
    max_depth: usize,
) -> Vec<PathBuf> {
    if target_browse == root_browse {
        return vec![root_tree.to_path_buf()];
    }
    let Ok(relative) = target_browse.strip_prefix(root_browse) else {
        return vec![root_tree.to_path_buf()];
    };

    let mut chain = vec![root_tree.to_path_buf()];
    let mut tree = root_tree.to_path_buf();
    for component in relative.components() {
        if chain.len() >= max_depth {
            break;
        }
        tree = tree.join(component.as_os_str());
        chain.push(tree.clone());
    }
    chain
}

pub(crate) fn mount_browse_for_tree_path(
    tree: &Path,
    mount_roots: impl IntoIterator<Item = PathBuf>,
) -> Option<PathBuf> {
    let mut best: Option<(usize, PathBuf)> = None;
    for mount in mount_roots {
        let mount_tree = drive_mount_tree_path(&mount);
        if tree == mount_tree.as_path() {
            let depth = mount.components().count();
            if best.as_ref().is_none_or(|(d, _)| depth > *d) {
                best = Some((depth, mount));
            }
            continue;
        }
        if tree.starts_with(&mount_tree) {
            let mut browse = mount.clone();
            if let Ok(rest) = tree.strip_prefix(&mount_tree) {
                for component in rest.components() {
                    browse.push(component.as_os_str());
                }
            }
            let depth = mount.components().count();
            if best.as_ref().is_none_or(|(d, _)| depth > *d) {
                best = Some((depth, browse));
            }
        }
    }
    best.map(|(_, browse)| browse)
}

pub(crate) fn known_folder_browse_for_tree_path(
    tree: &Path,
    known_roots: impl IntoIterator<Item = (PathBuf, PathBuf)>,
) -> Option<PathBuf> {
    let mut best: Option<(usize, PathBuf)> = None;
    for (tree_root, browse_root) in known_roots {
        if tree == tree_root.as_path() {
            let depth = browse_root.components().count();
            if best.as_ref().is_none_or(|(d, _)| depth > *d) {
                best = Some((depth, browse_root));
            }
            continue;
        }
        if tree.starts_with(&tree_root) {
            let mut browse = browse_root.clone();
            if let Ok(rest) = tree.strip_prefix(&tree_root) {
                for component in rest.components() {
                    browse.push(component.as_os_str());
                }
            }
            let depth = browse_root.components().count();
            if best.as_ref().is_none_or(|(d, _)| depth > *d) {
                best = Some((depth, browse));
            }
        }
    }
    best.map(|(_, browse)| browse)
}

/// Namespace ancestor chain from a mount/known-folder/share root down to `tree`.
pub(crate) fn namespace_tree_ancestor_chain(tree: &Path) -> Vec<PathBuf> {
    if !is_tree_namespace_path(tree) {
        return Vec::new();
    }
    let mut chain = vec![tree.to_path_buf()];
    let mut current = tree.to_path_buf();
    while !is_namespace_tree_root(&current) {
        let Some(parent) = current.parent() else {
            break;
        };
        if !parent.starts_with(TREE_NAMESPACE_PREFIX) {
            break;
        }
        chain.push(parent.to_path_buf());
        current = parent.to_path_buf();
    }
    chain.reverse();
    chain
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mount_and_root_mount_have_distinct_namespace_keys() {
        let root = drive_mount_tree_path(Path::new("/"));
        let happy = drive_mount_tree_path(Path::new("/run/media/happy"));
        assert_ne!(root, happy);
        assert!(is_mount_tree_path(&root));
        assert!(is_mount_tree_path(&happy));
    }

    #[test]
    fn same_browse_path_differs_by_parent_namespace() {
        let root_mount = drive_mount_tree_path(Path::new("/"));
        let happy_mount = drive_mount_tree_path(Path::new("/run/media/happy"));
        let browse = PathBuf::from("/run/media/happy/CDROM");

        let via_root = tree_child_path(
            &tree_child_path(
                &tree_child_path(
                    &tree_child_path(&root_mount, &PathBuf::from("/run")),
                    &PathBuf::from("/run/media"),
                ),
                &PathBuf::from("/run/media/happy"),
            ),
            &browse,
        );
        let via_mount = tree_child_path(&happy_mount, &browse);
        assert_ne!(via_root, via_mount);
    }

    #[test]
    fn namespace_ancestor_chain_extends_mount_tree() {
        let mount = drive_mount_tree_path(Path::new("/run/media/happy"));
        let target = PathBuf::from("/run/media/happy/CDROM/custom");
        let chain = namespace_ancestor_chain(&mount, Path::new("/run/media/happy"), &target, 16);
        assert_eq!(chain.len(), 3);
        assert_eq!(chain[0], mount);
        assert_eq!(
            chain[2],
            tree_child_path(
                &tree_child_path(&mount, &PathBuf::from("/run/media/happy/CDROM")),
                &target
            )
        );
    }

    #[test]
    fn namespace_tree_ancestor_chain_walks_namespace_parents() {
        let mount = drive_mount_tree_path(Path::new("/run/media/happy"));
        let cdrom = tree_child_path(&mount, &PathBuf::from("/run/media/happy/CDROM"));
        let custom = tree_child_path(&cdrom, &PathBuf::from("/run/media/happy/CDROM/custom"));
        let chain = namespace_tree_ancestor_chain(&custom);
        assert_eq!(chain, vec![mount, cdrom, custom]);
    }
}
