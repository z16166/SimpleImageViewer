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

//! Unix Places: dirs crate known folders + platform volume mount points.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::fs::path_is_accessible_directory;
use super::types::{
    DirectoryTreePlaces, DriveEntry, KnownFolderEntry, KnownFolderKind, known_folder_tree_path,
};

pub(super) fn load() -> DirectoryTreePlaces {
    DirectoryTreePlaces {
        known_folders: load_known_folders(),
        drives: enumerate_volumes(),
        network_locations: Vec::new(),
        this_pc_label: rust_i18n::t!("directory_tree.places").to_string(),
        network_label: rust_i18n::t!("directory_tree.network").to_string(),
    }
}

fn load_known_folders() -> Vec<KnownFolderEntry> {
    const SPECS: [(KnownFolderKind, fn() -> Option<PathBuf>, &str); 7] = [
        (
            KnownFolderKind::Desktop,
            dirs::desktop_dir,
            "directory_tree.place_desktop",
        ),
        (
            KnownFolderKind::Documents,
            dirs::document_dir,
            "directory_tree.place_documents",
        ),
        (
            KnownFolderKind::Pictures,
            dirs::picture_dir,
            "directory_tree.place_pictures",
        ),
        (
            KnownFolderKind::Downloads,
            dirs::download_dir,
            "directory_tree.place_downloads",
        ),
        (
            KnownFolderKind::Music,
            dirs::audio_dir,
            "directory_tree.place_music",
        ),
        (
            KnownFolderKind::Videos,
            dirs::video_dir,
            "directory_tree.place_videos",
        ),
        (
            KnownFolderKind::Profile,
            dirs::home_dir,
            "directory_tree.place_profile",
        ),
    ];

    SPECS
        .into_iter()
        .filter_map(|(kind, resolve, i18n_key)| {
            let filesystem_path = resolve()?;
            if !path_is_accessible_directory(&filesystem_path) {
                return None;
            }
            Some(KnownFolderEntry {
                kind,
                display_name: rust_i18n::t!(i18n_key).to_string(),
                tree_path: known_folder_tree_path(kind),
                filesystem_path,
            })
        })
        .collect()
}

fn enumerate_volumes() -> Vec<DriveEntry> {
    let mut paths = HashSet::new();

    #[cfg(target_os = "macos")]
    collect_mount_dirs(Path::new("/Volumes"), &mut paths);

    #[cfg(target_os = "linux")]
    {
        collect_mount_dirs(Path::new("/media"), &mut paths);
        collect_mount_dirs(Path::new("/run/media"), &mut paths);
        if let Some(user) = std::env::var_os("USER") {
            collect_mount_dirs(&PathBuf::from("/media").join(&user), &mut paths);
            collect_mount_dirs(&PathBuf::from("/run/media").join(&user), &mut paths);
        }
        collect_mount_dirs(Path::new("/mnt"), &mut paths);
    }

    paths.insert(PathBuf::from("/"));

    let paths = filter_nested_mount_paths(paths);

    let mut drives: Vec<DriveEntry> = paths
        .into_iter()
        .map(|path| DriveEntry {
            display_name: volume_display_name(&path),
            path,
        })
        .filter(|drive| path_is_accessible_directory(&drive.path))
        .collect();

    drives.sort_by(|left, right| left.path.cmp(&right.path));
    drives
}

/// Drop mount paths that live inside another listed mount (e.g. `/run/media/happy/CDROM`
/// when `/run/media/happy` is already a Places drive). Root `/` does not suppress siblings.
fn filter_nested_mount_paths(paths: HashSet<PathBuf>) -> HashSet<PathBuf> {
    let root = PathBuf::from("/");
    let mut sorted: Vec<PathBuf> = paths.into_iter().collect();
    sorted.sort_by_key(|path| path.components().count());

    let mut kept: Vec<PathBuf> = Vec::new();
    for path in sorted {
        let nested = kept.iter().any(|ancestor| {
            *ancestor != root
                && path.starts_with(ancestor)
                && path.components().count() > ancestor.components().count()
        });
        if !nested {
            kept.push(path);
        }
    }
    kept.into_iter().collect()
}

fn collect_mount_dirs(root: &Path, out: &mut HashSet<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir()
            || crate::scanner::skip_directory_traversal_entry(
                &path,
                &file_type,
                entry.metadata().ok().as_ref(),
            )
        {
            continue;
        }
        out.insert(path);
    }
}

fn volume_display_name(path: &Path) -> String {
    if path == Path::new("/") {
        return path.display().to_string();
    }

    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .unwrap_or_else(|| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::filter_nested_mount_paths;
    use std::collections::HashSet;
    use std::path::PathBuf;

    fn paths(values: &[&str]) -> HashSet<PathBuf> {
        values.iter().map(|value| PathBuf::from(value)).collect()
    }

    fn sorted(values: HashSet<PathBuf>) -> Vec<PathBuf> {
        let mut list: Vec<PathBuf> = values.into_iter().collect();
        list.sort();
        list
    }

    #[test]
    fn filter_nested_mount_paths_drops_child_when_parent_listed() {
        let filtered =
            filter_nested_mount_paths(paths(&["/", "/run/media/happy", "/run/media/happy/CDROM"]));
        assert_eq!(
            sorted(filtered),
            vec![PathBuf::from("/"), PathBuf::from("/run/media/happy"),]
        );
    }

    #[test]
    fn filter_nested_mount_paths_keeps_sibling_mounts() {
        let filtered = filter_nested_mount_paths(paths(&["/", "/mnt/hgfs", "/run/media/happy"]));
        assert_eq!(
            sorted(filtered),
            vec![
                PathBuf::from("/"),
                PathBuf::from("/mnt/hgfs"),
                PathBuf::from("/run/media/happy"),
            ]
        );
    }

    #[test]
    fn filter_nested_mount_paths_root_does_not_remove_other_mounts() {
        let filtered = filter_nested_mount_paths(paths(&["/", "/mnt/usb"]));
        assert_eq!(
            sorted(filtered),
            vec![PathBuf::from("/"), PathBuf::from("/mnt/usb")]
        );
    }
}
