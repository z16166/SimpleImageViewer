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

//! Tie native [`rfd::FileDialog`] instances to our main window so pickers open on the
//! same monitor (Windows/macOS/Linux portal), instead of the default (often primary) display.

/// Build a file/folder dialog owned by the egui main [`eframe::Frame`].
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn file_dialog_for_main_window(frame: &eframe::Frame) -> rfd::FileDialog {
    rfd::FileDialog::new().set_parent(frame)
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn file_dialog_for_main_window(_frame: &eframe::Frame) -> rfd::FileDialog {
    rfd::FileDialog::new()
}
