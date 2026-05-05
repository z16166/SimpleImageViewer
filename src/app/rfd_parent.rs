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
