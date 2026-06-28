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
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, TryRecvError};
use eframe::egui;
use rust_i18n::t;

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
    Folder,
    MusicFile,
    ExecutableFile,
}

#[derive(Debug, Clone)]
struct FolderPickerCompletion {
    generation: u64,
    purpose: FolderPickerPurpose,
    path: Option<PathBuf>,
}

pub(crate) struct FolderPickerRuntime {
    result_tx: Sender<FolderPickerCompletion>,
    result_rx: Receiver<FolderPickerCompletion>,
    in_flight: bool,
    started_at: Option<std::time::Instant>,
    /// Monotonic id for the in-flight dialog; stale worker results are ignored.
    active_generation: u64,
    next_generation: u64,
}

pub(crate) const FOLDER_PICKER_TIMEOUT: Duration = Duration::from_secs(600);

fn next_folder_picker_generation(current: u64) -> u64 {
    let mut next = current.wrapping_add(1);
    if next == 0 {
        next = 1;
    }
    next
}

impl FolderPickerRuntime {
    pub(crate) fn new() -> Self {
        let (result_tx, result_rx) = crossbeam_channel::bounded(1);
        Self {
            result_tx,
            result_rx,
            in_flight: false,
            started_at: None,
            active_generation: 0,
            next_generation: 0,
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
        self.begin_async_rfd_dialog(frame, purpose, AsyncRfdMode::Folder, starting_directory);
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
            AsyncRfdMode::MusicFile,
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
            AsyncRfdMode::ExecutableFile,
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

        if self.folder_picker.result_rx.try_recv().is_ok() {
            log::debug!("[FolderPicker] Drained stale completion before opening dialog");
        }

        let mut dialog = crate::app::rfd_parent::async_folder_dialog_for_main_window(frame);
        if let Some(dir) = starting_directory {
            dialog = dialog.set_directory(dir);
        }
        if matches!(mode, AsyncRfdMode::MusicFile) {
            dialog = dialog.add_filter(
                t!("folder_picker.filter_music").to_string(),
                crate::constants::MUSIC_SUPPORTED_EXTENSIONS,
            );
        }
        if matches!(mode, AsyncRfdMode::ExecutableFile) {
            dialog = apply_executable_file_filter(dialog);
        }

        let tx = self.folder_picker.result_tx.clone();
        self.folder_picker.next_generation =
            next_folder_picker_generation(self.folder_picker.next_generation);
        let generation = self.folder_picker.next_generation;
        self.folder_picker.active_generation = generation;
        self.folder_picker.in_flight = true;
        self.folder_picker.started_at = Some(Instant::now());

        if !self
            .background_threads
            .spawn("siv-folder-picker".to_string(), move || {
                let path = pollster::block_on(async move {
                    match mode {
                        AsyncRfdMode::Folder => dialog
                            .pick_folder()
                            .await
                            .map(|handle| handle.path().to_path_buf()),
                        AsyncRfdMode::MusicFile => dialog
                            .pick_file()
                            .await
                            .map(|handle| handle.path().to_path_buf()),
                        AsyncRfdMode::ExecutableFile => dialog
                            .pick_file()
                            .await
                            .map(|handle| handle.path().to_path_buf()),
                    }
                });
                let _ = tx.send(FolderPickerCompletion {
                    generation,
                    purpose,
                    path,
                });
            })
        {
            log::error!("[FolderPicker] Failed to spawn picker worker thread");
            self.folder_picker.in_flight = false;
            self.folder_picker.started_at = None;
            self.status_message = t!("folder_picker.failed_to_open").to_string();
        }
    }

    pub(crate) fn poll_folder_picker_results(&mut self, ctx: &egui::Context) {
        // When not in_flight (including after timeout), late worker completions sit in the
        // bounded(1) channel until begin_async_rfd_dialog drains them before the next open.
        if !self.folder_picker.in_flight {
            return;
        }

        if self
            .folder_picker
            .started_at
            .is_some_and(|started| started.elapsed() > FOLDER_PICKER_TIMEOUT)
        {
            log::warn!(
                "[FolderPicker] Dialog exceeded {}s; resetting in-flight state",
                FOLDER_PICKER_TIMEOUT.as_secs()
            );
            // 0 is the sentinel (inactive) generation; stale worker results with gen > 0 are rejected.
            self.folder_picker.active_generation = 0;
            self.folder_picker.in_flight = false;
            self.folder_picker.started_at = None;
            // rfd has no cancel API; the worker stays blocked until the user dismisses the dialog.
            self.status_message = t!("folder_picker.timed_out").to_string();
            ctx.request_repaint();
            return;
        }

        match self.folder_picker.result_rx.try_recv() {
            Ok(completion) => {
                if completion.generation != self.folder_picker.active_generation {
                    log::debug!(
                        "[FolderPicker] Ignoring stale result (gen {} != active {})",
                        completion.generation,
                        self.folder_picker.active_generation
                    );
                    ctx.request_repaint();
                    return;
                }
                self.folder_picker.in_flight = false;
                self.folder_picker.started_at = None;
                self.folder_picker.active_generation = 0;
                self.apply_folder_picker_completion(completion);
                ctx.request_repaint();
            }
            Err(TryRecvError::Empty) => {
                ctx.request_repaint();
            }
            Err(TryRecvError::Disconnected) => {
                log::warn!("[FolderPicker] Worker disconnected without sending a result");
                self.folder_picker.in_flight = false;
                self.folder_picker.started_at = None;
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
fn apply_executable_file_filter(dialog: rfd::AsyncFileDialog) -> rfd::AsyncFileDialog {
    #[cfg(target_os = "windows")]
    {
        dialog.add_filter(t!("folder_picker.filter_executable").to_string(), &["exe"])
    }
    #[cfg(target_os = "macos")]
    {
        dialog.add_filter(t!("folder_picker.filter_application").to_string(), &["app"])
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        dialog.add_filter(
            t!("folder_picker.filter_executable").to_string(),
            &["", "bin", "AppImage"],
        )
    }
}

#[cfg(target_arch = "wasm32")]
fn apply_executable_file_filter(dialog: rfd::AsyncFileDialog) -> rfd::AsyncFileDialog {
    dialog
}
