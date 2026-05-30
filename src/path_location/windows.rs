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

//! Windows-only remote path detection via `GetDriveTypeW`.

use std::path::Path;

/// Returns true when `path` lives on a mapped network drive letter.
pub fn is_mapped_remote_drive(path: &Path) -> bool {
    let Some(root) = drive_root_wide(path) else {
        return false;
    };

    unsafe {
        use winapi::um::fileapi::GetDriveTypeW;
        use winapi::um::winbase::DRIVE_REMOTE;

        GetDriveTypeW(root.as_ptr()) == DRIVE_REMOTE
    }
}

fn drive_root_wide(path: &Path) -> Option<Vec<u16>> {
    let text = path.to_string_lossy();
    let mut chars = text.chars();
    let drive = chars.next()?;
    if !drive.is_ascii_alphabetic() {
        return None;
    }
    if chars.next()? != ':' {
        return None;
    }

    let root = format!("{drive}:\\");
    Some(root.encode_utf16().chain(std::iter::once(0)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_root_wide_extracts_letter_colon_backslash() {
        assert_eq!(
            drive_root_wide(Path::new(r"Z:\nested\file.jpg"))
                .map(|wide| String::from_utf16_lossy(&wide[..wide.len() - 1]))
                .as_deref(),
            Some("Z:\\")
        );
        assert!(drive_root_wide(Path::new(r"\\server\share\file.jpg")).is_none());
        assert!(drive_root_wide(Path::new("relative.jpg")).is_none());
    }
}
