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
//! Tree node keys (`namespace_path`) live in this namespace and are distinct from filesystem
//! browse paths. The same on-disk folder may appear under multiple parents with different
//! namespace keys (e.g. Places mount shortcut vs root `/` expansion).

use std::ffi::OsStr;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

pub(crate) const THIS_PC_NAMESPACE_PATH: &str = r"\\?\siv-tree/ThisPC";
pub(crate) const NETWORK_NAMESPACE_PATH: &str = r"\\?\siv-tree/Network";

const VERBATIM_NAMESPACE_PREFIX: &str = r"\\?\";
const CANONICAL_TREE_PREFIX: &str = r"\\?\siv-tree/";
const MOUNT_PREFIX_FORWARD: &str = r"\\?\siv-tree/Mount/";
const NETWORK_SHARE_PREFIX_FORWARD: &str = r"\\?\siv-tree/Network/Share/";

/// Canonicalize shell namespace keys so `/` separates components on every OS.
///
/// Windows treats `\` as a separator; on Unix it is a literal byte inside one
/// component, which breaks ancestor walks and mount/known-folder root detection.
pub(crate) fn normalize_tree_namespace_path(path: PathBuf) -> PathBuf {
    let Some(text) = path.to_str() else {
        return path;
    };
    if !text.starts_with(VERBATIM_NAMESPACE_PREFIX) {
        return path;
    }
    let mut rest = text[VERBATIM_NAMESPACE_PREFIX.len()..].replace('\\', "/");
    while rest.contains("//") {
        rest = rest.replace("//", "/");
    }
    PathBuf::from(format!("{VERBATIM_NAMESPACE_PREFIX}{rest}"))
}

fn namespace_join(parent: &Path, segment: &OsStr) -> PathBuf {
    let parent = normalize_tree_namespace_path(parent.to_path_buf());
    let mut text = parent.to_string_lossy().into_owned();
    if !text.ends_with('/') {
        text.push('/');
    }
    text.push_str(&segment.to_string_lossy());
    normalize_tree_namespace_path(PathBuf::from(text))
}

pub(crate) fn is_tree_namespace_path(path: &Path) -> bool {
    normalize_tree_namespace_path(path.to_path_buf())
        .to_str()
        .is_some_and(|text| text.starts_with(VERBATIM_NAMESPACE_PREFIX))
}

pub(crate) fn is_mount_namespace_path(path: &Path) -> bool {
    normalize_tree_namespace_path(path.to_path_buf())
        .to_str()
        .is_some_and(|text| text.starts_with(MOUNT_PREFIX_FORWARD))
}

pub(crate) fn is_network_share_namespace_path(path: &Path) -> bool {
    normalize_tree_namespace_path(path.to_path_buf())
        .to_str()
        .is_some_and(|text| text.starts_with(NETWORK_SHARE_PREFIX_FORWARD))
}

pub(crate) fn is_places_sentinel_namespace_path(path: &Path) -> bool {
    let path = normalize_tree_namespace_path(path.to_path_buf());
    path.as_os_str() == OsStr::new(THIS_PC_NAMESPACE_PATH)
        || path.as_os_str() == OsStr::new(NETWORK_NAMESPACE_PATH)
}

fn namespace_path_segments(path: &Path) -> Option<Vec<String>> {
    let path = normalize_tree_namespace_path(path.to_path_buf());
    let text = path.to_str()?;
    let rest = text.strip_prefix(CANONICAL_TREE_PREFIX)?;
    Some(
        rest.split('/')
            .filter(|segment| !segment.is_empty())
            .map(str::to_owned)
            .collect(),
    )
}

/// Percent-encode a mount filesystem path into a single namespace segment.
///
/// Uses the OS byte representation of the path (not lossy UTF-8) so non-ASCII paths stay unique.
fn encode_mount_id(mount_fs_path: &Path) -> String {
    let mut result = String::with_capacity(mount_fs_path.as_os_str().len().saturating_mul(3));
    for &byte in mount_fs_path.as_os_str().as_encoded_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => {
                result.push(byte as char);
            }
            b' ' => result.push_str("%20"),
            b'%' => result.push_str("%25"),
            b'\\' => result.push_str("%5C"),
            b'/' => result.push_str("%2F"),
            b':' => result.push_str("%3A"),
            _ => {
                let _ = write!(result, "%{byte:02X}");
            }
        }
    }
    result
}

/// Namespace key for a Places drive / mount entry.
pub(crate) fn drive_mount_namespace_path(mount_fs_path: &Path) -> PathBuf {
    normalize_tree_namespace_path(PathBuf::from(format!(
        "{CANONICAL_TREE_PREFIX}Mount/{}",
        encode_mount_id(mount_fs_path)
    )))
}

/// Namespace key for a UNC share root under Network.
pub(crate) fn network_share_namespace_path(share_fs_path: &Path) -> PathBuf {
    normalize_tree_namespace_path(PathBuf::from(format!(
        "{CANONICAL_TREE_PREFIX}Network/Share/{}",
        encode_mount_id(share_fs_path)
    )))
}

fn fs_path_for_namespace_tree(
    tree: &Path,
    roots: impl IntoIterator<Item = (PathBuf, PathBuf)>,
) -> Option<PathBuf> {
    let tree_segments = namespace_path_segments(tree)?;
    let mut best: Option<(usize, PathBuf)> = None;
    for (namespace_root, fs_root) in roots {
        let namespace_root = normalize_tree_namespace_path(namespace_root);
        let Some(root_segments) = namespace_path_segments(&namespace_root) else {
            continue;
        };
        if tree_segments.len() < root_segments.len() {
            continue;
        }
        if tree_segments[..root_segments.len()] != root_segments[..] {
            continue;
        }
        let mut browse = fs_root.clone();
        for segment in &tree_segments[root_segments.len()..] {
            browse.push(segment);
        }
        let depth = fs_root.components().count();
        if best.as_ref().is_none_or(|(d, _)| depth > *d) {
            best = Some((depth, browse));
        }
    }
    best.map(|(_, browse)| browse)
}

pub(crate) fn fs_path_for_network_share_namespace(
    tree: &Path,
    share_roots: impl IntoIterator<Item = PathBuf>,
) -> Option<PathBuf> {
    fs_path_for_namespace_tree(
        tree,
        share_roots
            .into_iter()
            .map(|share| (network_share_namespace_path(&share), share)),
    )
}

/// Extend a namespace parent using browse-path segments from `parent_browse` to `child_browse`.
pub(crate) fn namespace_child_path(
    parent_namespace: &Path,
    parent_fs_path: &Path,
    child_fs_path: &Path,
) -> PathBuf {
    let parent_namespace = normalize_tree_namespace_path(parent_namespace.to_path_buf());
    if let Ok(relative) = child_fs_path.strip_prefix(parent_fs_path) {
        let mut namespace = parent_namespace;
        for component in relative.components() {
            namespace = namespace_join(&namespace, component.as_os_str());
        }
        return namespace;
    }
    // Fallback: `read_dir` children are normally direct subdirectories of `parent_fs_path`.
    // Junctions or unusual layouts may share a file_name; callers should prefer the prefix path.
    if child_fs_path.as_os_str().is_empty() {
        return parent_namespace;
    }
    if let Some(name) = child_fs_path.file_name() {
        if !name.is_empty() {
            return namespace_join(&parent_namespace, name);
        }
    }
    let mut namespace = parent_namespace;
    for component in child_fs_path.components() {
        namespace = namespace_join(&namespace, component.as_os_str());
    }
    namespace
}

/// Build namespace ancestor chain from mount/known-folder/share root to `target_fs_path`.
pub(crate) fn namespace_ancestor_chain(
    root_namespace: &Path,
    root_fs_path: &Path,
    target_fs_path: &Path,
    max_depth: usize,
) -> Vec<PathBuf> {
    let root_namespace = normalize_tree_namespace_path(root_namespace.to_path_buf());
    if target_fs_path == root_fs_path {
        return vec![root_namespace];
    }
    let Ok(relative) = target_fs_path.strip_prefix(root_fs_path) else {
        return vec![root_namespace];
    };

    let mut chain = vec![root_namespace.clone()];
    let mut namespace = root_namespace;
    for component in relative.components() {
        if chain.len() >= max_depth {
            break;
        }
        namespace = namespace_join(&namespace, component.as_os_str());
        chain.push(namespace.clone());
    }
    chain
}

pub(crate) fn fs_path_for_mount_namespace(
    tree: &Path,
    mount_roots: impl IntoIterator<Item = PathBuf>,
) -> Option<PathBuf> {
    fs_path_for_namespace_tree(
        tree,
        mount_roots
            .into_iter()
            .map(|mount| (drive_mount_namespace_path(&mount), mount)),
    )
}

pub(crate) fn fs_path_for_known_folder_namespace(
    tree: &Path,
    known_roots: impl IntoIterator<Item = (PathBuf, PathBuf)>,
) -> Option<PathBuf> {
    fs_path_for_namespace_tree(tree, known_roots)
}

fn is_partial_namespace_prefix(segments: &[String]) -> bool {
    match segments.first().map(String::as_str) {
        Some("Mount") | Some("KnownFolder") => segments.len() == 1,
        Some("Network") => segments.len() <= 2,
        _ => false,
    }
}

/// Namespace ancestor chain from a mount/known-folder/share root down to `tree`.
pub(crate) fn namespace_path_ancestor_chain(tree: &Path) -> Vec<PathBuf> {
    let Some(segments) = namespace_path_segments(tree) else {
        return Vec::new();
    };
    if segments.is_empty() {
        return Vec::new();
    }
    let mut chain = Vec::with_capacity(segments.len());
    for len in 1..=segments.len() {
        if is_partial_namespace_prefix(&segments[..len]) {
            continue;
        }
        chain.push(namespace_path_from_segment_strings(&segments[..len]));
    }
    chain
}

fn namespace_path_from_segment_strings(segments: &[String]) -> PathBuf {
    PathBuf::from(format!("{CANONICAL_TREE_PREFIX}{}", segments.join("/")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mount_and_root_mount_have_distinct_namespace_keys() {
        let root = drive_mount_namespace_path(Path::new("/"));
        let happy = drive_mount_namespace_path(Path::new("/run/media/happy"));
        assert_ne!(root, happy);
        assert!(is_mount_namespace_path(&root));
        assert!(is_mount_namespace_path(&happy));
    }

    #[test]
    fn encode_mount_id_escapes_percent() {
        let encoded = encode_mount_id(Path::new("/mnt/a%2Fb"));
        assert!(encoded.contains("%25"));
        assert_ne!(
            encode_mount_id(Path::new("/mnt/a%2Fb")),
            encode_mount_id(Path::new("/mnt/a/b"))
        );
    }

    #[test]
    fn encode_mount_id_escapes_space() {
        let encoded = encode_mount_id(Path::new("/mnt/my photos"));
        assert!(encoded.contains("%20"));
        assert!(!encoded.contains(' '));
    }

    #[test]
    fn namespace_child_path_fallback_skips_empty_segment() {
        let parent = drive_mount_namespace_path(Path::new("/run/media/happy"));
        assert_eq!(
            namespace_child_path(&parent, Path::new("/run/media/happy"), Path::new("")),
            parent,
        );
    }

    #[test]
    fn same_fs_path_differs_by_parent_namespace() {
        let root_mount = drive_mount_namespace_path(Path::new("/"));
        let happy_mount = drive_mount_namespace_path(Path::new("/run/media/happy"));
        let browse = PathBuf::from("/run/media/happy/CDROM");

        let via_root = namespace_child_path(
            &namespace_child_path(
                &namespace_child_path(
                    &namespace_child_path(&root_mount, Path::new("/"), &PathBuf::from("/run")),
                    &PathBuf::from("/run"),
                    &PathBuf::from("/run/media"),
                ),
                &PathBuf::from("/run/media"),
                &PathBuf::from("/run/media/happy"),
            ),
            &PathBuf::from("/run/media/happy"),
            &browse,
        );
        let via_mount = namespace_child_path(&happy_mount, Path::new("/run/media/happy"), &browse);
        assert_ne!(via_root, via_mount);
    }

    #[test]
    fn namespace_ancestor_chain_extends_mount_tree() {
        let mount = drive_mount_namespace_path(Path::new("/run/media/happy"));
        let target = PathBuf::from("/run/media/happy/CDROM/custom");
        let chain = namespace_ancestor_chain(&mount, Path::new("/run/media/happy"), &target, 16);
        assert_eq!(chain.len(), 3);
        assert_eq!(chain[0], mount);
        assert_eq!(
            chain[2],
            namespace_child_path(
                &namespace_child_path(
                    &mount,
                    Path::new("/run/media/happy"),
                    &PathBuf::from("/run/media/happy/CDROM"),
                ),
                &PathBuf::from("/run/media/happy/CDROM"),
                &target,
            )
        );
    }

    #[test]
    fn namespace_path_ancestor_chain_walks_namespace_parents() {
        let mount = drive_mount_namespace_path(Path::new("/run/media/happy"));
        let cdrom = namespace_child_path(
            &mount,
            Path::new("/run/media/happy"),
            &PathBuf::from("/run/media/happy/CDROM"),
        );
        let custom = namespace_child_path(
            &cdrom,
            &PathBuf::from("/run/media/happy/CDROM"),
            &PathBuf::from("/run/media/happy/CDROM/custom"),
        );
        let chain = namespace_path_ancestor_chain(&custom);
        assert_eq!(chain, vec![mount, cdrom, custom]);
    }

    #[test]
    fn normalize_tree_namespace_path_splits_legacy_backslashes() {
        let legacy = PathBuf::from(r"\\?\siv-tree\Mount\%2Frun%2Fmedia%2Fhappy\CDROM\custom");
        let custom = namespace_child_path(
            &namespace_child_path(
                &drive_mount_namespace_path(Path::new("/run/media/happy")),
                Path::new("/run/media/happy"),
                &PathBuf::from("/run/media/happy/CDROM"),
            ),
            &PathBuf::from("/run/media/happy/CDROM"),
            &PathBuf::from("/run/media/happy/CDROM/custom"),
        );
        let normalized = normalize_tree_namespace_path(legacy);
        assert_eq!(normalized, custom);
        let chain = namespace_path_ancestor_chain(&normalized);
        assert_eq!(chain.len(), 3, "{chain:?}");
    }
}
