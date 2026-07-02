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

//! Helpers for classifying file paths by storage location (local vs remote).

use std::path::Path;

#[cfg(windows)]
mod windows;

/// Returns true when `path` uses a UNC prefix (`\\server\share\...`).
pub fn is_unc_path(path: &Path) -> bool {
    let text = path.to_string_lossy();
    text.starts_with(r"\\") || text.starts_with("//")
}

/// Returns true when the file is on a remote/network location.
///
/// All platforms treat UNC paths as remote. On Windows, mapped network drive
/// letters are detected separately in the platform-specific module.
pub fn is_remote_path(path: &Path) -> bool {
    if is_unc_path(path) {
        return true;
    }

    #[cfg(windows)]
    {
        windows::is_mapped_remote_drive(path)
    }

    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(test)]
mod tests;
