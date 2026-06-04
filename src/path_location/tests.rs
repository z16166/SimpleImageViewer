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

use super::*;
use std::path::PathBuf;

#[test]
fn unc_path_detection_matches_common_prefixes() {
    assert!(is_unc_path(Path::new(r"\\server\share\photo.jpg")));
    assert!(is_unc_path(Path::new("//server/share/photo.jpg")));
    assert!(!is_unc_path(Path::new(r"C:\photos\photo.jpg")));
    assert!(!is_unc_path(Path::new("/home/user/photo.jpg")));
}

#[test]
fn unc_paths_are_remote_on_all_platforms() {
    assert!(is_remote_path(Path::new(r"\\server\share\photo.jpg")));
}

#[test]
fn relative_paths_are_not_remote() {
    assert!(!is_remote_path(Path::new("photo.jpg")));
    assert!(!is_remote_path(PathBuf::from("nested/photo.jpg").as_path()));
}

#[cfg(not(windows))]
#[test]
fn non_windows_paths_without_unc_prefix_are_local() {
    assert!(!is_remote_path(Path::new(r"C:\photos\photo.jpg")));
    assert!(!is_remote_path(Path::new("/mnt/nfs/share/photo.jpg")));
}

#[cfg(windows)]
mod windows_tests {
    use super::*;

    #[test]
    fn local_drive_letter_paths_are_not_remote_without_network_mapping() {
        assert!(!is_remote_path(Path::new(r"C:\photos\photo.jpg")));
        assert!(!is_remote_path(Path::new(r"D:\folder\image.png")));
    }
}
