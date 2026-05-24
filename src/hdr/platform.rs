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

/// True when WAYLAND_DISPLAY is set and is not an X11-style `:N` display.
pub fn wayland_session_from_display_var(display: Option<&str>) -> bool {
    display.is_some_and(|d| !d.is_empty() && !d.starts_with(':'))
}

pub fn is_wayland_session() -> bool {
    wayland_session_from_display_var(std::env::var("WAYLAND_DISPLAY").ok().as_deref())
}

/// Linux native HDR is Wayland-only in v1; X11 stays SDR.
pub fn linux_native_hdr_platform_eligible() -> bool {
    cfg!(target_os = "linux") && is_wayland_session()
}

#[cfg(test)]
mod tests {
    #[test]
    fn wayland_session_detects_env_var() {
        assert!(super::wayland_session_from_display_var(Some("wayland-1")));
        assert!(!super::wayland_session_from_display_var(None));
    }

    #[test]
    fn x11_session_is_not_wayland() {
        assert!(!super::wayland_session_from_display_var(Some(":0")));
    }
}
