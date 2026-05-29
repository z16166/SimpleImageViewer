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

// ── Constants ────────────────────────────────────────────────────────────────

/// egui temp data key for tracking the current modal generation.
pub const ID_MODAL_GENERATION: &str = "__modal_generation";

/// egui temp data key suffix for storing measured window height.
pub const ID_MEASURED_HEIGHT: &str = "__measured_h";

/// egui temp data key for tracking modal transition (None -> Some).
pub const ID_PREV_HAD_MODAL: &str = "__prev_had_modal";

/// The result returned by a modal dialog's `show()` method each frame.
///
/// `egui::Modal` is an immediate-mode widget: it does not block execution.
/// Each dialog returns this value to tell the dispatch layer what happened:
/// keep open, dismiss (drop state), or hand off a confirmed action.
///
/// This type is the **only** shared contract between dialog modules and the
/// dispatch layer in `input.rs`. Each dialog's internal `State` and the
/// specific `Action` it can produce are private to that dialog's module.
#[derive(Debug, PartialEq)]
pub enum ModalResult {
    /// The dialog is still open and waiting for user input.
    Pending,
    /// The user dismissed the dialog (Cancel / close / Escape).
    Dismissed,
    /// The user confirmed an action. The payload is dialog-specific.
    Confirmed(ModalAction),
}

/// Cross-dialog confirmed action payloads. Variants are named after the
/// operation, not the originating dialog, to keep the dispatch arm clean.
///
/// Internal details that only the dialog module needs (e.g. the raw text the
/// user typed before parsing) never appear here — only the resolved,
/// ready-to-execute result.
#[derive(Debug, PartialEq)]
pub enum ModalAction {
    /// Navigate to a specific 0-based image index.
    GotoIndex(usize),
    /// Set the desktop wallpaper using the current image and the given mode.
    SetWallpaper(String),
    /// The user confirmed in the generic confirm dialog; the tag identifies
    /// which operation was confirmed.
    ConfirmTagged(crate::ui::dialogs::confirm::ConfirmTag),
    /// Apply the selected file associations (Windows only).
    #[cfg(target_os = "windows")]
    ApplyFileAssoc(Vec<String>),
}

/// The single active modal dialog. Only one can be open at a time.
///
/// Each variant owns the dialog's complete runtime state. Setting
/// `active_modal` to `None` immediately drops and cleans up that state —
/// no stale fields linger in `ImageViewerApp` between open/close cycles.
///
/// Each dialog's `State` type is defined in its own module; this enum is
/// the only place that assembles them into a single sum type.
pub enum ActiveModal {
    /// Generic confirm/cancel dialog.  State is private to [`confirm`].
    Confirm(crate::ui::dialogs::confirm::State),
    /// "Go to image #N" dialog.  State is private to [`goto`].
    Goto(crate::ui::dialogs::goto::State),
    /// Wallpaper mode selector.  State is private to [`wallpaper`].
    Wallpaper(crate::ui::dialogs::wallpaper::State),
    /// EXIF data viewer.  State is private to [`exif`].
    Exif(crate::ui::dialogs::exif::State),
    /// XMP metadata viewer.  State is private to [`xmp`].
    Xmp(crate::ui::dialogs::xmp::State),
    /// File association manager (Windows only).  State is private to [`file_assoc`].
    #[cfg(target_os = "windows")]
    FileAssoc(crate::ui::dialogs::file_assoc::State),
}
