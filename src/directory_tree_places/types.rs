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

//! Shared types for directory-tree "Places" roots (known folders and drives).

use std::path::PathBuf;

/// Identifies a well-known user folder shown under Places.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KnownFolderKind {
    Desktop,
    Documents,
    Pictures,
    Downloads,
    Music,
    Videos,
    Profile,
}

impl KnownFolderKind {
    pub fn slug(self) -> &'static str {
        match self {
            Self::Desktop => "Desktop",
            Self::Documents => "Documents",
            Self::Pictures => "Pictures",
            Self::Downloads => "Downloads",
            Self::Music => "Music",
            Self::Videos => "Videos",
            Self::Profile => "Profile",
        }
    }
}

/// Stable tree-node key for a known shell folder (distinct from its filesystem path).
pub fn known_folder_namespace_path(kind: KnownFolderKind) -> PathBuf {
    PathBuf::from(format!(r"\\?\siv-tree\KnownFolder\{}", kind.slug()))
}

/// A known folder entry (Desktop, Documents, etc.).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownFolderEntry {
    pub kind: KnownFolderKind,
    pub display_name: String,
    pub namespace_path: PathBuf,
    pub fs_path: PathBuf,
}

/// A filesystem drive or mount point shown under "This PC" / Places.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriveEntry {
    pub display_name: String,
    pub fs_path: PathBuf,
}

/// Places sidebar data: known folders, drives, and section labels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryTreePlaces {
    pub known_folders: Vec<KnownFolderEntry>,
    pub drives: Vec<DriveEntry>,
    /// Shell-enumerated network locations with filesystem paths (Windows).
    pub network_locations: Vec<DriveEntry>,
    pub this_pc_label: String,
    pub network_label: String,
}
