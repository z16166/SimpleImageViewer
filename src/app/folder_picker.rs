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

//! Non-blocking native file/folder pickers via [`rfd::AsyncFileDialog`].
//!
//! Windows/Linux: `pollster::block_on` runs on a worker thread so UNC / network
//! dialogs cannot freeze the egui main loop.
//!
//! macOS: `AsyncFileDialog` schedules the native panel on the AppKit main thread;
//! the worker thread waits on the future while winit keeps pumping events.

use std::path::PathBuf;

use crossbeam_channel::{Receiver, Sender, TryRecvError};
use eframe::egui;

use super::ImageViewerApp;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FolderPickerPurpose {
    ImageDirectory,
    MusicDirectory,
    MusicFile,
    FileCopyCutModal,
    ContextMenuExecutable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AsyncRfdMode {
    PickFolder,
    PickMusicFile,
    PickExecutableFile,
}

#[derive(Debug, Clone)]
struct FolderPickerCompletion {
    purpose: FolderPickerPurpose,
    path: Option<PathBuf>,
}

pub(crate) struct FolderPickerRuntime {
    result_tx: Sender<FolderPickerCompletion>,
    result_rx: Receiver<FolderPickerCompletion>,
    in_flight: bool,
}

impl FolderPickerRuntime {
    pub(crate) fn new() -> Self {
        let (result_tx, result_rx) = crossbeam_channel::bounded(1);
        Self {
            result_tx,
            result_rx,
            in_flight: false,
        }
    }

    pub(crate) fn in_flight(&self) -> bool {
        self.in_flight
    }
}

impl ImageViewerApp {
    pub(crate) fn request_folder_picker(
        &mut self,
        frame: &eframe::Frame,
        purpose: FolderPickerPurpose,
        starting_directory: Option<PathBuf>,
    ) {
        self.begin_async_rfd_dialog(
            frame,
            purpose,
            AsyncRfdMode::PickFolder,
            starting_directory,
        );
    }

    pub(crate) fn request_music_file_picker(&mut self, frame: &eframe::Frame) {
        let starting_directory = self.settings.music_path.as_ref().and_then(|path| {
            if path.is_dir() {
                Some(path.clone())
            } else {
                path.parent().map(PathBuf::from)
            }
        });
        self.begin_async_rfd_dialog(
            frame,
            FolderPickerPurpose::MusicFile,
            AsyncRfdMode::PickMusicFile,
            starting_directory,
        );
    }

    pub(crate) fn request_context_menu_executable_picker(&mut self, frame: &eframe::Frame) {
        let starting_directory = match &self.context_menu_edit_draft.command {
            crate::context_menu::model::ContextMenuCommand::Executable { path }
                if !path.trim().is_empty() =>
            {
                std::path::Path::new(path.trim())
                    .parent()
                    .map(PathBuf::from)
            }
            _ => None,
        };
        self.begin_async_rfd_dialog(
            frame,
            FolderPickerPurpose::ContextMenuExecutable,
            AsyncRfdMode::PickExecutableFile,
            starting_directory,
        );
    }

    fn begin_async_rfd_dialog(
        &mut self,
        frame: &eframe::Frame,
        purpose: FolderPickerPurpose,
        mode: AsyncRfdMode,
        starting_directory: Option<PathBuf>,
    ) {
        if self.folder_picker.in_flight {
            log::debug!("[FolderPicker] Ignored duplicate request while dialog is open");
            return;
        }

        let mut dialog = crate::app::rfd_parent::async_folder_dialog_for_main_window(frame);
        if let Some(dir) = starting_directory {
            dialog = dialog.set_directory(dir);
        }
        if matches!(mode, AsyncRfdMode::PickMusicFile) {
            dialog = dialog.add_filter(
                "Music files",
                crate::constants::MUSIC_SUPPORTED_EXTENSIONS,
            );
        }
        if matches!(mode, AsyncRfdMode::PickExecutableFile) {
            dialog = apply_executable_file_filter(dialog);
        }

        let tx = self.folder_picker.result_tx.clone();
        self.folder_picker.in_flight = true;

        if std::thread::Builder::new()
            .name("siv-folder-picker".to_string())
            .spawn(move || {
                let path = pollster::block_on(async move {
                    match mode {
                        AsyncRfdMode::PickFolder => dialog
                            .pick_folder()
                            .await
                            .map(|handle| handle.path().to_path_buf()),
                        AsyncRfdMode::PickMusicFile => dialog
                            .pick_file()
                            .await
                            .map(|handle| handle.path().to_path_buf()),
                        AsyncRfdMode::PickExecutableFile => dialog
                            .pick_file()
                            .await
                            .map(|handle| handle.path().to_path_buf()),
                    }
                });
                let _ = tx.send(FolderPickerCompletion { purpose, path });
            })
            .is_err()
        {
            log::error!("[FolderPicker] Failed to spawn picker worker thread");
            self.folder_picker.in_flight = false;
        }
    }

    pub(crate) fn poll_folder_picker_results(&mut self, ctx: &egui::Context) {
        if !self.folder_picker.in_flight {
            return;
        }

        match self.folder_picker.result_rx.try_recv() {
            Ok(completion) => {
                self.folder_picker.in_flight = false;
                self.apply_folder_picker_completion(completion);
                ctx.request_repaint();
            }
            Err(TryRecvError::Empty) => {
                ctx.request_repaint();
            }
            Err(TryRecvError::Disconnected) => {
                log::warn!("[FolderPicker] Worker disconnected without sending a result");
                self.folder_picker.in_flight = false;
            }
        }
    }

    fn apply_folder_picker_completion(&mut self, completion: FolderPickerCompletion) {
        let Some(picked) = completion.path else {
            return;
        };

        match completion.purpose {
            FolderPickerPurpose::ImageDirectory => self.apply_picked_image_directory(picked),
            FolderPickerPurpose::MusicDirectory => {
                self.settings.music_path = Some(picked);
                self.restart_audio_if_enabled();
            }
            FolderPickerPurpose::MusicFile => {
                self.settings.music_path = Some(picked);
                self.restart_audio_if_enabled();
            }
            FolderPickerPurpose::FileCopyCutModal => {
                if let Some(crate::ui::dialogs::modal_state::ActiveModal::FileCopyCut(state)) =
                    self.active_modal.as_mut()
                {
                    state.input = picked.to_string_lossy().into_owned();
                    state.error = None;
                }
            }
            FolderPickerPurpose::ContextMenuExecutable => {
                if self.context_menu_edit_dialog_open
                    && let crate::context_menu::model::ContextMenuCommand::Executable {
                        ref mut path,
                    } = self.context_menu_edit_draft.command
                {
                    *path = picked.to_string_lossy().into_owned();
                }
            }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn apply_executable_file_filter(
    dialog: rfd::AsyncFileDialog,
) -> rfd::AsyncFileDialog {
    #[cfg(target_os = "windows")]
    {
        dialog.add_filter("Executable", &["exe"])
    }
    #[cfg(target_os = "macos")]
    {
        dialog.add_filter("Application", &["app"])
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = dialog;
        dialog
    }
}

#[cfg(target_arch = "wasm32")]
fn apply_executable_file_filter(dialog: rfd::AsyncFileDialog) -> rfd::AsyncFileDialog {
    dialog
}
