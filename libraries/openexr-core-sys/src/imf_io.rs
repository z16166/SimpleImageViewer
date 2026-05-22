// Simple Image Viewer - A high-performance, cross-platform image viewer
// Copyright (C) 2024-2026 Simple Image Viewer Contributors
//
// SPDX-License-Identifier: GPL-3.0-only

//! UTF-8 labels for OpenEXR Imf diagnostics (file paths opened in Rust stay Unicode-safe).

use std::ffi::CString;
use std::path::Path;

/// Encode `path` as a UTF-8 NUL-terminated C string for Imf `fileName()` / logging.
///
/// Opening the file should happen in Rust (`File::open(path)` / mmap) so Unicode paths
/// work on all platforms; this helper is only for optional debug labels passed to Imf.
pub fn path_utf8_cstr(path: &Path) -> Result<CString, String> {
    let utf8 = path_to_utf8(path)?;
    CString::new(utf8.as_bytes())
        .map_err(|_| format!("EXR path contains an interior NUL: {}", path.display()))
}

fn path_to_utf8(path: &Path) -> Result<String, String> {
    if let Some(s) = path.to_str() {
        return Ok(s.to_owned());
    }

    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let bytes = path.as_os_str().as_bytes();
        std::str::from_utf8(bytes)
            .map(|s| s.to_owned())
            .map_err(|_| format!("EXR path is not valid UTF-8: {}", path.display()))
    }

    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        if wide.iter().any(|&u| u == 0) {
            return Err(format!(
                "EXR path contains an interior NUL (wide): {}",
                path.display()
            ));
        }
        let utf8: String = char::decode_utf16(wide)
            .map(|unit| unit.unwrap_or('\u{FFFD}'))
            .collect();
        Ok(utf8)
    }

    #[cfg(not(any(unix, windows)))]
    {
        Err(format!(
            "EXR path is not valid UTF-8: {}",
            path.display()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn utf8_path_round_trips_through_cstring() {
        let path = Path::new("样例/测试.exr");
        let cstr = path_utf8_cstr(path).expect("utf8 path");
        assert_eq!(cstr.to_str().expect("cstr utf8"), "样例/测试.exr");
    }
}
