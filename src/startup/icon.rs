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

use eframe::egui;

/// Title-bar icon. On Windows this is decoded from the same PE `.ico` resource (id 1) used for
/// the taskbar; elsewhere `build.rs` embeds `OUT_DIR/siv_window_icon_rgba256.bin` from `icon.png`.
pub fn load_icon() -> egui::IconData {
    #[cfg(windows)]
    {
        if let Some((rgba, width, height)) = crate::windows_utils::load_icon_rgba_from_pe() {
            return egui::IconData {
                rgba,
                width,
                height,
            };
        }
        log::warn!(
            "Failed to load application icon from PE resources; title bar may show a generic icon"
        );
        return egui::IconData {
            rgba: Vec::new(),
            width: 0,
            height: 0,
        };
    }
    #[cfg(not(windows))]
    {
        return load_icon_from_build_rgba();
    }
}

/// 256×256 RGBA from `build.rs` (`emit_viewport_icon_rgba`); Linux/macOS only.
#[cfg(not(windows))]
fn load_icon_from_build_rgba() -> egui::IconData {
    const W: u32 = 256;
    const H: u32 = 256;
    let rgba = include_bytes!(concat!(env!("OUT_DIR"), "/siv_window_icon_rgba256.bin"));
    debug_assert_eq!(rgba.len(), (W * H * 4) as usize);
    egui::IconData {
        rgba: rgba.to_vec(),
        width: W,
        height: H,
    }
}
