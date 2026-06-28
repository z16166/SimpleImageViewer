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
use super::loop_state::{AudioLoopState, AudioSlots};
use super::player::{AudioCommand, AudioError};
use super::slots::{set_current_track, set_metadata};
use super::wasapi::WasapiMonitorGuard;
#[cfg(windows)]
use super::wasapi::wasapi_poll_device_lost;

use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub(crate) fn run_audio_loop(
    rx: crossbeam_channel::Receiver<AudioCommand>,
    shutdown_flag: Arc<AtomicBool>,
    err_slot: AudioError,
    track_slot: Arc<Mutex<Option<String>>>,
    path_slot: Arc<Mutex<Option<PathBuf>>>,
    meta_slot: Arc<Mutex<Option<String>>>,
    tracks_flag: Arc<AtomicBool>,
    cue_track_slot: Arc<Mutex<Option<usize>>>,
    _needs_restart: Arc<AtomicBool>,
    cue_markers_slot: Arc<Mutex<Vec<u64>>>,
    pos_ms: Arc<std::sync::atomic::AtomicU64>,
    dur_ms: Arc<std::sync::atomic::AtomicU64>,
    device_slot: Arc<Mutex<Option<String>>>,
) {
    let _wasapi_guard = WasapiMonitorGuard::new();

    let slots = AudioSlots {
        err_slot,
        track_slot,
        path_slot,
        meta_slot,
        tracks_flag,
        cue_track_slot,
        cue_markers_slot,
        pos_ms,
        dur_ms,
        device_slot,
    };
    let mut st = AudioLoopState::new(Arc::clone(&shutdown_flag));

    loop {
        let timeout = if !st.stopped && !st.paused {
            Duration::from_millis(100)
        } else {
            Duration::from_secs(1)
        };

        match rx.recv_timeout(timeout) {
            Ok(AudioCommand::Shutdown) => {
                if let Some(p) = st.backend_player.take() {
                    p.stop()
                }
                st.backend_sink.take();
                set_current_track(&slots.track_slot, None);
                set_metadata(&slots.meta_slot, None);
                return;
            }
            Ok(AudioCommand::Stop) => st.handle_stop(&slots),
            Ok(AudioCommand::Pause) => st.handle_pause(),
            Ok(AudioCommand::Play) => st.handle_play(&slots),
            Ok(AudioCommand::Seek(pos)) => st.handle_seek(pos, &slots),
            Ok(AudioCommand::SetDevice) => st.handle_set_device(&slots),
            Ok(AudioCommand::NextFile) => st.handle_next_file(&slots),
            Ok(AudioCommand::PrevFile) => st.handle_prev_file(&slots),
            Ok(AudioCommand::NextTrack) => st.handle_next_track(&slots),
            Ok(AudioCommand::PrevTrack) => st.handle_prev_track(&slots),
            Ok(AudioCommand::SetVolume(v)) => {
                st.current_volume = v;
                if let Some(ref p) = st.backend_player {
                    p.set_volume(v);
                }
            }
            Ok(AudioCommand::SetPlaylist(list, si, ti, paused)) => {
                st.handle_set_playlist(list, si, ti, paused, &slots);
            }
            Err(_) => {}
        }

        // Windows: poll for hardware device-lost events
        #[cfg(windows)]
        if unsafe { wasapi_poll_device_lost() } {
            log::warn!("[WATCHDOG] Audio device lost. Dropping backend for orphan recovery.");
            st.backend_player = None;
            st.backend_sink = None;
        }

        // FEED: load the next file when the sink runs empty
        if !st.stopped && (!st.base_playlist.is_empty() || !st.injected_playlist.is_empty()) {
            let player_empty = st.backend_player.as_ref().is_some_and(|p| p.empty());
            let player_exists = st.backend_player.is_some();

            if player_empty {
                if !st.feed_next_file(&slots) {
                    continue;
                }
            } else if !player_exists {
                // Orphaned: backend lost while still "playing" — try to recover
                st.recover_orphaned_backend(&slots);
            }
        }

        // CUE track highlight update
        if !st.stopped && !st.paused {
            if shutdown_flag.load(Ordering::Relaxed) {
                return;
            }
            if st.backend_player.as_ref().is_some_and(|p| !p.empty()) {
                st.update_cue_track_highlight(&slots);
            }
        }

        // Position reporting for UI slider
        st.update_position(&slots);
    }
}
