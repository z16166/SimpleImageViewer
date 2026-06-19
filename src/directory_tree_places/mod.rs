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

//! Platform-specific discovery of directory-tree Places (known folders and drives).

pub mod types;

mod fs;

pub use types::{DirectoryTreePlaces, KnownFolderEntry};

#[cfg(windows)]
mod windows;

#[cfg(unix)]
mod unix;

/// Load known folders, drives, and the drives-section label for the directory tree.
pub fn load() -> DirectoryTreePlaces {
    #[cfg(windows)]
    {
        return windows::load();
    }

    #[cfg(unix)]
    {
        return unix::load();
    }

    #[cfg(not(any(windows, unix)))]
    {
        return stub_load();
    }
}

#[cfg(not(any(windows, unix)))]
fn stub_load() -> DirectoryTreePlaces {
    use types::{KnownFolderEntry, KnownFolderKind, known_folder_tree_path};

    let known_folders = dirs::home_dir()
        .into_iter()
        .filter(|path| fs::path_is_accessible_directory(path))
        .map(|path| KnownFolderEntry {
            kind: KnownFolderKind::Profile,
            display_name: rust_i18n::t!("directory_tree.place_profile").to_string(),
            tree_path: known_folder_tree_path(KnownFolderKind::Profile),
            filesystem_path: path,
        })
        .collect();

    DirectoryTreePlaces {
        known_folders,
        drives: Vec::new(),
        this_pc_label: rust_i18n::t!("directory_tree.places").to_string(),
        network_label: rust_i18n::t!("directory_tree.network").to_string(),
    }
}
