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
use super::run_loop::run_audio_loop;

use crossbeam_channel::Sender;
use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub enum AudioCommand {
    SetPlaylist(Vec<PathBuf>, Option<usize>, Option<usize>, bool), // files, start_file_idx, start_track_idx, paused
    SetVolume(f32),
    Play,
    Pause,
    NextFile,
    PrevFile,
    NextTrack,
    PrevTrack,
    Stop, // Clears playlist and stops playback, but keeps thread alive
    Seek(Duration),
    SetDevice,
    Shutdown, // Terminates the thread
}

/// Shared error slot: audio thread writes here, UI thread reads and clears it.
pub type AudioError = Arc<Mutex<Option<String>>>;

pub struct AudioPlayer {
    cmd_tx: Option<Sender<AudioCommand>>,
    pub last_error: AudioError,
    pub current_track: Arc<Mutex<Option<String>>>,
    pub current_track_path: Arc<Mutex<Option<PathBuf>>>,
    pub current_metadata: Arc<Mutex<Option<String>>>,
    pub has_tracks: Arc<AtomicBool>,
    pub current_cue_track: Arc<Mutex<Option<usize>>>,
    pub pos_ms: Arc<std::sync::atomic::AtomicU64>,
    pub dur_ms: Arc<std::sync::atomic::AtomicU64>,
    pub current_device: Arc<Mutex<Option<String>>>,
    pub shutdown_flag: Arc<AtomicBool>,
    /// Set by the audio thread when a hardware stall is detected.
    /// The UI thread should poll this and trigger a full restart.
    pub needs_restart: Arc<AtomicBool>,
    pub cue_markers: Arc<Mutex<Vec<u64>>>,
    thread_handle: Option<std::thread::JoinHandle<()>>,
}

impl AudioPlayer {
    pub fn new() -> Self {
        Self {
            cmd_tx: None,
            last_error: Arc::new(Mutex::new(None)),
            current_track: Arc::new(Mutex::new(None)),
            current_track_path: Arc::new(Mutex::new(None)),
            current_metadata: Arc::new(Mutex::new(None)),
            has_tracks: Arc::new(AtomicBool::new(false)),
            current_cue_track: Arc::new(Mutex::new(None)),
            pos_ms: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            dur_ms: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            current_device: Arc::new(Mutex::new(None)),
            shutdown_flag: Arc::new(AtomicBool::new(false)),
            needs_restart: Arc::new(AtomicBool::new(false)),
            cue_markers: Arc::new(Mutex::new(Vec::new())),
            thread_handle: None,
        }
    }

    pub fn start_at(
        &mut self,
        files: Vec<PathBuf>,
        start_index: Option<usize>,
        start_track_index: Option<usize>,
        paused: bool,
    ) {
        self.ensure_thread_started();
        if let Some(tx) = &self.cmd_tx {
            let _ = tx.send(AudioCommand::SetPlaylist(
                files,
                start_index,
                start_track_index,
                paused,
            ));
        }
    }

    /// Stop playback and clear the queue. Lightweight; does not close hardware.
    pub fn stop(&mut self) {
        if let Some(tx) = &self.cmd_tx {
            let _ = tx.send(AudioCommand::Stop);
        }
    }

    pub fn set_volume(&self, volume: f32) {
        if let Some(tx) = &self.cmd_tx {
            let _ = tx.send(AudioCommand::SetVolume(volume.clamp(0.0, 1.0)));
        }
    }

    pub fn play(&self) {
        if let Some(tx) = &self.cmd_tx {
            let _ = tx.send(AudioCommand::Play);
        }
    }

    pub fn pause(&self) {
        if let Some(tx) = &self.cmd_tx {
            let _ = tx.send(AudioCommand::Pause);
        }
    }

    pub fn next_file(&self) {
        if let Some(tx) = &self.cmd_tx {
            let _ = tx.send(AudioCommand::NextFile);
        }
    }

    pub fn prev_file(&self) {
        if let Some(tx) = &self.cmd_tx {
            let _ = tx.send(AudioCommand::PrevFile);
        }
    }

    pub fn next_track(&self) {
        if let Some(tx) = &self.cmd_tx {
            let _ = tx.send(AudioCommand::NextTrack);
        }
    }

    pub fn prev_track(&self) {
        if let Some(tx) = &self.cmd_tx {
            let _ = tx.send(AudioCommand::PrevTrack);
        }
    }

    pub fn take_error(&self) -> Option<String> {
        self.last_error.try_lock()?.take()
    }

    pub fn get_current_track(&self) -> Option<String> {
        self.current_track.try_lock()?.clone()
    }

    pub fn get_current_track_path(&self) -> Option<PathBuf> {
        self.current_track_path.try_lock()?.clone()
    }

    pub fn get_metadata(&self) -> Option<String> {
        self.current_metadata.try_lock()?.clone()
    }

    pub fn has_tracks(&self) -> bool {
        self.has_tracks.load(Ordering::Relaxed)
    }

    fn ensure_thread_started(&mut self) {
        if self.cmd_tx.is_none() {
            let (tx, rx) = crossbeam_channel::unbounded::<AudioCommand>();
            self.cmd_tx = Some(tx);
            let err_slot = Arc::clone(&self.last_error);
            let track_slot = Arc::clone(&self.current_track);
            let path_slot = Arc::clone(&self.current_track_path);
            let meta_slot = Arc::clone(&self.current_metadata);
            let tracks_flag = Arc::clone(&self.has_tracks);
            let cue_track_slot = Arc::clone(&self.current_cue_track);
            let pos_ms = Arc::clone(&self.pos_ms);
            let dur_ms = Arc::clone(&self.dur_ms);
            let dev_slot = Arc::clone(&self.current_device);
            let shutdown_flag = Arc::clone(&self.shutdown_flag);
            let needs_restart = Arc::clone(&self.needs_restart);
            let cue_markers_slot = Arc::clone(&self.cue_markers);
            shutdown_flag.store(false, Ordering::Relaxed);
            needs_restart.store(false, Ordering::Relaxed);

            let res = std::thread::Builder::new()
                .name("audio-player".to_string())
                .spawn(move || {
                    run_audio_loop(
                        rx,
                        shutdown_flag,
                        err_slot,
                        track_slot,
                        path_slot,
                        meta_slot,
                        tracks_flag,
                        cue_track_slot,
                        needs_restart,
                        cue_markers_slot,
                        pos_ms,
                        dur_ms,
                        dev_slot,
                    )
                });
            match res {
                Ok(handle) => {
                    self.thread_handle = Some(handle);
                }
                Err(e) => {
                    log::error!("[Audio] Failed to spawn audio thread: {}", e);
                }
            }
        }
    }

    pub fn take_needs_restart(&self) -> bool {
        self.needs_restart.swap(false, Ordering::Relaxed)
    }

    pub fn get_pos_ms(&self) -> u64 {
        self.pos_ms.load(Ordering::Relaxed)
    }

    pub fn get_duration_ms(&self) -> u64 {
        self.dur_ms.load(Ordering::Relaxed)
    }

    pub fn get_current_cue_track(&self) -> Option<usize> {
        self.current_cue_track.try_lock()?.clone()
    }

    pub fn get_cue_markers(&self) -> Vec<u64> {
        self.cue_markers.lock().clone()
    }

    pub fn seek(&self, pos: Duration) {
        if let Some(tx) = &self.cmd_tx {
            let _ = tx.send(AudioCommand::Seek(pos));
        }
    }

    pub fn list_devices(&self) -> Vec<String> {
        use rodio::cpal::traits::{DeviceTrait, HostTrait};
        match ::rodio::cpal::default_host().output_devices() {
            Ok(devices) => devices
                .filter_map(|d| {
                    d.description().ok().map(|desc| {
                        let name = desc.name();
                        if let Some(driver) = desc.driver() {
                            format!("{} ({})", name, driver)
                        } else {
                            name.to_string()
                        }
                    })
                })
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    pub fn set_device(&self, device_name: Option<String>) {
        if let Some(mut guard) = self.current_device.try_lock() {
            *guard = device_name.clone();
        }
        if let Some(tx) = &self.cmd_tx {
            let _ = tx.send(AudioCommand::SetDevice);
        }
    }
}

impl Drop for AudioPlayer {
    fn drop(&mut self) {
        self.shutdown_flag.store(true, Ordering::Relaxed);
        if let Some(tx) = self.cmd_tx.take() {
            let _ = tx.send(AudioCommand::Shutdown);
        }
    }
}
