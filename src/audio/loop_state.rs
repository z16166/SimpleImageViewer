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
use super::cue::{load_cue, CueSheet};
use super::player::AudioError;
use super::playlist::{build_base_non_m3u_set, expand_m3u_excluding_base, is_m3u_path};
use super::slots::{
    set_cue_markers, set_cue_track, set_current_path, set_current_track, set_error, set_metadata,
};
use super::sources::symphonia::{get_file_metadata, open_source};

use crate::constants::AUDIO_RECOVERY_COOLDOWN;
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::wasapi::{wasapi_is_device_available, AUDIO_HW_POS_ZERO_GRACE};

// ---------------------------------------------------------------------------
// Shared slot bundle passed into the audio thread
// ---------------------------------------------------------------------------

pub(crate) struct AudioSlots {
    pub(crate) err_slot: AudioError,
    pub(crate) track_slot: Arc<Mutex<Option<String>>>,
    pub(crate) path_slot: Arc<Mutex<Option<PathBuf>>>,
    pub(crate) meta_slot: Arc<Mutex<Option<String>>>,
    pub(crate) tracks_flag: Arc<AtomicBool>,
    pub(crate) cue_track_slot: Arc<Mutex<Option<usize>>>,
    pub(crate) cue_markers_slot: Arc<Mutex<Vec<u64>>>,
    pub(crate) pos_ms: Arc<std::sync::atomic::AtomicU64>,
    pub(crate) dur_ms: Arc<std::sync::atomic::AtomicU64>,
    pub(crate) device_slot: Arc<Mutex<Option<String>>>,
}

// ---------------------------------------------------------------------------
// All mutable state that lives inside the audio event loop
// ---------------------------------------------------------------------------

pub(crate) struct AudioLoopState {
    pub(crate) base_playlist: Vec<PathBuf>,
    pub(crate) injected_playlist: VecDeque<PathBuf>,
    pub(crate) injected_history: Vec<PathBuf>,
    pub(crate) suppress_injected_history_once: bool,
    pub(crate) current_track_idx: usize,
    pub(crate) forced_next_path: Option<(PathBuf, bool)>,
    pub(crate) current_file_path: Option<PathBuf>,
    pub(crate) current_from_injected: bool,
    pub(crate) stopped: bool,
    pub(crate) paused: bool,
    pub(crate) current_volume: f32,
    pub(crate) cue_sheet: Option<CueSheet>,
    pub(crate) current_file_start: Instant,
    pub(crate) last_seek_offset: Duration,
    pub(crate) paused_at: Option<Instant>,
    pub(crate) total_paused: Duration,
    pub(crate) pending_start_track_idx: Option<usize>,
    pub(crate) backend_sink: Option<rodio::MixerDeviceSink>,
    pub(crate) backend_player: Option<rodio::Player>,
    pub(crate) last_backend_attempt: Option<Instant>,
    pub(crate) sink_base_pos: Duration,
    pub(crate) last_hw_pos: Duration,
    pub(crate) hw_pos_zero_since: Option<Instant>,
    pub(crate) hw_pos_zero_fallback_active: bool,
    pub(crate) shutdown_flag: Arc<AtomicBool>,
}

impl AudioLoopState {
    /// Elapsed playback time within the current opened stream segment (since `current_file_start`),
    /// excluding any active or accumulated pause intervals.
    fn playback_elapsed_since_current_stream(&self) -> Duration {
        let now = Instant::now();
        let mut gross = now.saturating_duration_since(self.current_file_start);
        gross = gross.saturating_sub(self.total_paused);
        if let Some(pa) = self.paused_at {
            gross = gross.saturating_sub(now.saturating_duration_since(pa));
        }
        gross
    }

    /// Absolute position in the current audio file (decoder timeline), including seek / CUE offset.
    fn absolute_playback_position(&self) -> Duration {
        self.playback_elapsed_since_current_stream()
            .saturating_add(self.last_seek_offset)
    }

    /// Reset wall-clock tracking for a newly opened stream segment. Keeps `paused_at` / `total_paused`
    /// consistent with `current_file_start` (switching output devices without this can leave
    /// `paused_at` from before the switch and force the reported position to stick at 0).
    fn reanchor_playback_clock_for_new_segment(&mut self) {
        self.current_file_start = Instant::now();
        self.total_paused = Duration::ZERO;
        self.hw_pos_zero_since = None;
        self.hw_pos_zero_fallback_active = false;
        if self.paused {
            self.paused_at = Some(Instant::now());
        } else {
            self.paused_at = None;
        }
    }

    pub(crate) fn new(shutdown_flag: Arc<AtomicBool>) -> Self {
        Self {
            base_playlist: Vec::new(),
            injected_playlist: VecDeque::new(),
            injected_history: Vec::new(),
            suppress_injected_history_once: false,
            current_track_idx: 0,
            forced_next_path: None,
            current_file_path: None,
            current_from_injected: false,
            stopped: true,
            paused: false,
            current_volume: 1.0,
            cue_sheet: None,
            current_file_start: Instant::now(),
            last_seek_offset: Duration::ZERO,
            paused_at: None,
            total_paused: Duration::ZERO,
            pending_start_track_idx: None,
            backend_sink: None,
            backend_player: None,
            last_backend_attempt: None,
            sink_base_pos: Duration::ZERO,
            last_hw_pos: Duration::ZERO,
            hw_pos_zero_since: None,
            hw_pos_zero_fallback_active: false,
            shutdown_flag,
        }
    }

    /// Try to open the audio output device if it isn't already open.
    fn ensure_backend(&mut self, slots: &AudioSlots) -> bool {
        if self.backend_sink.is_some() && self.backend_player.is_some() {
            return true;
        }
        let can_retry = self
            .last_backend_attempt
            .map_or(true, |l| l.elapsed() >= AUDIO_RECOVERY_COOLDOWN);
        if !can_retry {
            return false;
        }
        self.last_backend_attempt = Some(Instant::now());

        if !unsafe { wasapi_is_device_available() } {
            log::debug!("[RECOVERY] ensure_backend: Hardware is busy, skipping attempt");
            return false;
        }

        let selected_device = slots.device_slot.lock().clone();
        let sink_result = if let Some(ref name) = selected_device {
            use rodio::cpal::traits::{DeviceTrait, HostTrait};
            ::rodio::cpal::default_host()
                .output_devices()
                .ok()
                .and_then(|mut ds| {
                    ds.find(|d| {
                        d.description()
                            .ok()
                            .map(|desc| {
                                let n = desc.name();
                                if let Some(drv) = desc.driver() {
                                    format!("{} ({})", n, drv)
                                } else {
                                    n.to_string()
                                }
                            })
                            .as_ref()
                            == Some(name)
                    })
                })
                .map(|d| rodio::DeviceSinkBuilder::from_device(d).and_then(|b| b.open_stream()))
                .unwrap_or_else(|| Ok(rodio::DeviceSinkBuilder::open_default_sink()?))
        } else {
            rodio::DeviceSinkBuilder::open_default_sink()
        };

        match sink_result {
            Ok(sink) => {
                let p = rodio::Player::connect_new(sink.mixer());
                p.set_volume(self.current_volume);
                if self.paused {
                    p.pause();
                } else {
                    p.play();
                }
                self.backend_sink = Some(sink);
                self.backend_player = Some(p);
                log::debug!("[RECOVERY] ensure_backend: device opened successfully");
                true
            }
            Err(e) => {
                let msg = format!("Audio device error: {e}");
                log::warn!("{msg}");
                set_error(&slots.err_slot, msg);
                false
            }
        }
    }

    pub(crate) fn handle_stop(&mut self, slots: &AudioSlots) {
        self.stopped = true;
        self.base_playlist.clear();
        self.injected_playlist.clear();
        self.injected_history.clear();
        self.suppress_injected_history_once = false;
        self.forced_next_path = None;
        self.current_file_path = None;
        self.current_from_injected = false;
        self.backend_player = None;
        self.backend_sink = None;
        self.sink_base_pos = Duration::ZERO;
        self.cue_sheet = None;
        set_current_track(&slots.track_slot, None);
        set_metadata(&slots.meta_slot, None);
        slots.tracks_flag.store(false, Ordering::Relaxed);
        set_cue_markers(&slots.cue_markers_slot, Vec::new());
        set_cue_track(&slots.cue_track_slot, None);
    }

    pub(crate) fn handle_pause(&mut self) {
        self.paused = true;
        self.paused_at = Some(Instant::now());
        if let Some(ref p) = self.backend_player {
            p.pause();
        }
    }

    pub(crate) fn handle_play(&mut self, slots: &AudioSlots) {
        if self.paused {
            self.paused = false;
            if let Some(pa) = self.paused_at.take() {
                self.total_paused += pa.elapsed();
            }
        }
        if self.stopped && !self.base_playlist.is_empty() {
            self.stopped = false;
            let resume_pos = self.last_hw_pos.saturating_add(self.last_seek_offset);
            self.backend_player = None;
            self.backend_sink = None;
            if self.current_track_idx > 0 {
                self.current_track_idx -= 1;
            }
            let Some((path, from_injected)) = self.take_next_playable_path() else {
                self.stopped = true;
                return;
            };
            self.current_file_path = Some(path.clone());
            self.current_from_injected = from_injected;
            if let Some(source) = open_source(&path, resume_pos, &self.shutdown_flag) {
                if self.ensure_backend(slots) {
                    let resumed = if let Some(p) = self.backend_player.as_mut() {
                        slots.dur_ms.store(
                            source
                                .total_duration()
                                .map(|d| d.as_millis() as u64)
                                .unwrap_or(0),
                            Ordering::Relaxed,
                        );
                        p.append(source);
                        self.sink_base_pos = p.get_pos();
                        self.last_seek_offset = resume_pos;
                        p.play();
                        self.current_track_idx += 1;
                        self.last_hw_pos = Duration::ZERO;
                        true
                    } else {
                        false
                    };
                    if resumed {
                        self.reanchor_playback_clock_for_new_segment();
                    }
                }
            }
        } else if let Some(ref p) = self.backend_player {
            p.play();
        }
    }

    pub(crate) fn handle_seek(&mut self, pos: Duration, slots: &AudioSlots) {
        let Some(path) = self.current_file_path.clone() else {
            return;
        };
        if let Some(source) = open_source(&path, pos, &self.shutdown_flag) {
            if self.ensure_backend(slots) {
                let seek_ok = if let Some(p) = self.backend_player.as_mut() {
                    p.clear();
                    self.sink_base_pos = p.get_pos();
                    let total_dur = source
                        .total_duration()
                        .map(|d| d.as_millis() as u64)
                        .unwrap_or(0);
                    p.append(source);
                    slots.dur_ms.store(total_dur, Ordering::Relaxed);
                    slots
                        .pos_ms
                        .store(pos.as_millis() as u64, Ordering::Relaxed);
                    self.last_seek_offset = pos;
                    self.last_hw_pos = Duration::ZERO;
                    true
                } else {
                    false
                };
                if seek_ok {
                    self.reanchor_playback_clock_for_new_segment();
                    if let Some(p) = self.backend_player.as_ref() {
                        if !self.paused {
                            p.play();
                        } else {
                            p.pause();
                        }
                    }
                    log::debug!("[AUDIO] Seek to {} ms successful", pos.as_millis());
                }
            }
        } else {
            log::error!("[AUDIO] Failed to re-open file for seek: {:?}", path);
        }
    }

    pub(crate) fn handle_set_device(&mut self, slots: &AudioSlots) {
        let resume_pos = self.absolute_playback_position();
        self.backend_sink = None;
        self.backend_player = None;
        self.sink_base_pos = Duration::ZERO;
        if self.current_file_path.is_none() || self.stopped {
            return;
        }
        let path = self.current_file_path.clone().unwrap_or_default();
        if let Some(source) = open_source(&path, resume_pos, &self.shutdown_flag) {
            if self.ensure_backend(slots) {
                let switched = if let Some(p) = self.backend_player.as_mut() {
                    p.append(source);
                    self.sink_base_pos = p.get_pos();
                    self.last_seek_offset = resume_pos;
                    self.last_hw_pos = Duration::ZERO;
                    true
                } else {
                    false
                };
                if switched {
                    self.reanchor_playback_clock_for_new_segment();
                    if let Some(p) = self.backend_player.as_ref() {
                        if !self.paused {
                            p.play();
                        } else {
                            p.pause();
                        }
                    }
                    log::info!(
                        "[AUDIO] Device switched, playback resumed at {}ms",
                        resume_pos.as_millis()
                    );
                }
            }
        }
    }

    pub(crate) fn handle_next_file(&mut self, slots: &AudioSlots) {
        if let Some(ref p) = self.backend_player {
            p.clear();
        }
        self.forced_next_path = None;
        // Keep injected queue/history so `NextFile` can advance within expanded m3u tracks.
        self.cue_sheet = None;
        set_cue_track(&slots.cue_track_slot, None);
        slots.tracks_flag.store(false, Ordering::Relaxed);
    }

    pub(crate) fn handle_prev_file(&mut self, slots: &AudioSlots) {
        if self.rewind_injected_one_step() {
            self.suppress_injected_history_once = true;
            if let Some(ref p) = self.backend_player {
                p.clear();
            }
            self.cue_sheet = None;
            set_cue_track(&slots.cue_track_slot, None);
            slots.tracks_flag.store(false, Ordering::Relaxed);
            return;
        }
        if self.current_track_idx > 1 {
            self.current_track_idx -= 2;
        } else {
            self.current_track_idx = self.base_playlist.len().saturating_sub(1);
        }
        // We are switching back to base-list reverse navigation. Clear injected forward/rewind
        // state so the next selected entry is resolved from base order only.
        self.injected_playlist.clear();
        self.injected_history.clear();
        self.suppress_injected_history_once = false;
        self.current_from_injected = false;
        if let Some((path, from_injected)) = self.take_prev_playable_path() {
            self.forced_next_path = Some((path, from_injected));
            if from_injected {
                self.current_from_injected = true;
                self.suppress_injected_history_once = true;
            }
        } else {
            self.stopped = true;
        }
        if let Some(ref p) = self.backend_player {
            p.clear();
        }
        self.cue_sheet = None;
        set_cue_track(&slots.cue_track_slot, None);
        slots.tracks_flag.store(false, Ordering::Relaxed);
    }

    pub(crate) fn handle_next_track(&mut self, slots: &AudioSlots) {
        let cue = match &self.cue_sheet {
            Some(c) => c,
            None => return,
        };
        let elapsed = self.absolute_playback_position();
        let current_idx = cue
            .tracks
            .iter()
            .position(|t| t.start > elapsed)
            .unwrap_or(cue.tracks.len())
            .saturating_sub(1);
        if current_idx + 1 >= cue.tracks.len() {
            if let Some(ref p) = self.backend_player {
                p.clear();
            }
            return;
        }
        let next_t = cue.tracks[current_idx + 1].clone();
        let path = match self.current_file_path.clone() {
            Some(p) => p,
            None => return,
        };
        if let Some(source) = open_source(&path, next_t.start, &self.shutdown_flag) {
            if self.ensure_backend(slots) {
                let next_track_ok = if let Some(ref p) = self.backend_player {
                    p.clear();
                    self.sink_base_pos = p.get_pos();
                    p.append(source);
                    self.last_seek_offset = next_t.start;
                    self.last_hw_pos = Duration::ZERO;
                    true
                } else {
                    false
                };
                if next_track_ok {
                    self.reanchor_playback_clock_for_new_segment();
                    if let Some(p) = self.backend_player.as_ref() {
                        if !self.paused {
                            p.play();
                        } else {
                            p.pause();
                        }
                    }
                    slots
                        .pos_ms
                        .store(next_t.start.as_millis() as u64, Ordering::Relaxed);
                    let meta =
                        format!("{}. {} - {}", next_t.number, next_t.title, next_t.performer);
                    set_metadata(&slots.meta_slot, Some(meta));
                    set_cue_track(&slots.cue_track_slot, Some(current_idx + 1));
                }
            }
        }
    }

    pub(crate) fn handle_prev_track(&mut self, slots: &AudioSlots) {
        let cue = match &self.cue_sheet {
            Some(c) => c,
            None => return,
        };
        let elapsed = self.absolute_playback_position();
        let current_idx = cue
            .tracks
            .iter()
            .position(|t| t.start > elapsed)
            .unwrap_or(cue.tracks.len())
            .saturating_sub(1);
        let time_in_track = elapsed.saturating_sub(cue.tracks[current_idx].start);
        let target_idx = if time_in_track > Duration::from_secs(3) || current_idx == 0 {
            current_idx
        } else {
            current_idx - 1
        };
        let target_t = cue.tracks[target_idx].clone();
        let path = match self.current_file_path.clone() {
            Some(p) => p,
            None => return,
        };
        if let Some(source) = open_source(&path, target_t.start, &self.shutdown_flag) {
            if self.ensure_backend(slots) {
                let prev_track_ok = if let Some(ref p) = self.backend_player {
                    p.clear();
                    self.sink_base_pos = p.get_pos();
                    p.append(source);
                    self.last_seek_offset = target_t.start;
                    self.last_hw_pos = Duration::ZERO;
                    true
                } else {
                    false
                };
                if prev_track_ok {
                    self.reanchor_playback_clock_for_new_segment();
                    if let Some(p) = self.backend_player.as_ref() {
                        if !self.paused {
                            p.play();
                        } else {
                            p.pause();
                        }
                    }
                    let meta = format!(
                        "{}. {} - {}",
                        target_t.number, target_t.title, target_t.performer
                    );
                    set_metadata(&slots.meta_slot, Some(meta));
                    set_cue_track(&slots.cue_track_slot, Some(target_idx));
                }
            }
        }
    }

    pub(crate) fn handle_set_playlist(
        &mut self,
        new_list: Vec<PathBuf>,
        start_idx: Option<usize>,
        start_track_idx: Option<usize>,
        initial_paused: bool,
        slots: &AudioSlots,
    ) {
        self.base_playlist = new_list;
        self.injected_playlist.clear();
        self.injected_history.clear();
        self.suppress_injected_history_once = false;
        self.forced_next_path = None;
        self.current_track_idx = start_idx.unwrap_or(0);
        self.current_file_path = None;
        self.current_from_injected = false;
        self.pending_start_track_idx = start_track_idx;
        self.stopped = false;
        self.paused = initial_paused;
        if initial_paused {
            self.paused_at = Some(Instant::now());
        } else {
            self.paused_at = None;
            self.total_paused = Duration::ZERO;
        }
        if self.ensure_backend(slots) {
            slots.dur_ms.store(0, Ordering::Relaxed);
            if let Some(ref p) = self.backend_player {
                p.clear();
                if self.paused {
                    p.pause();
                } else {
                    p.play();
                }
            }
        }
        set_current_path(&slots.path_slot, None);
        set_metadata(&slots.meta_slot, None);
        set_cue_track(&slots.cue_track_slot, start_track_idx);
    }

    /// Load the next file from the playlist into the sink. Returns false if
    /// the caller should `continue` (backend not yet ready).
    pub(crate) fn feed_next_file(&mut self, slots: &AudioSlots) -> bool {
        let previous = self.current_file_path.clone();
        let next_selection = self
            .forced_next_path
            .take()
            .or_else(|| self.take_next_playable_path());
        let Some((path, from_injected)) = next_selection else {
            self.stopped = true;
            return true;
        };
        if from_injected {
            if self.suppress_injected_history_once {
                self.suppress_injected_history_once = false;
            } else if self.current_from_injected {
                if let Some(prev) = previous {
                    self.injected_history.push(prev);
                }
            } else {
                self.injected_history.clear();
            }
        } else {
            self.injected_history.clear();
            self.suppress_injected_history_once = false;
        }
        let filename = match path.file_name() {
            Some(n) => n.to_string_lossy().to_string(),
            None => return true,
        };
        let source = match open_source(&path, Duration::ZERO, &self.shutdown_flag) {
            Some(s) => s,
            None => return true,
        };

        // Update UI immediately before any device operations.
        set_current_track(&slots.track_slot, Some(filename));
        set_current_path(&slots.path_slot, Some(path.clone()));
        self.current_file_path = Some(path.clone());
        self.current_from_injected = from_injected;
        let total_dur = source
            .total_duration()
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        slots.dur_ms.store(total_dur, Ordering::Relaxed);
        slots.pos_ms.store(0, Ordering::Relaxed);

        self.cue_sheet = load_cue(&path, &self.shutdown_flag);
        slots
            .tracks_flag
            .store(self.cue_sheet.is_some(), Ordering::Relaxed);
        self.update_metadata_for_new_file(slots, &path);

        if !self.ensure_backend(slots) {
            return false;
        }
        let fed = if let Some(p) = self.backend_player.as_mut() {
            p.append(source);
            self.sink_base_pos = p.get_pos();
            self.last_seek_offset = Duration::ZERO;
            self.last_hw_pos = Duration::ZERO;
            true
        } else {
            false
        };
        if fed {
            self.reanchor_playback_clock_for_new_segment();
        }

        // Seek to a saved CUE track if resuming from saved state.
        if let (Some(track_idx), Some(cue)) = (self.pending_start_track_idx.take(), &self.cue_sheet)
        {
            if track_idx < cue.tracks.len() {
                let t = cue.tracks[track_idx].clone();
                if t.start > Duration::ZERO {
                    if let Some(s2) = open_source(&path, t.start, &self.shutdown_flag) {
                        if self.ensure_backend(slots) {
                            let cue_seek_ok = if let Some(p) = self.backend_player.as_mut() {
                                p.clear();
                                p.append(s2);
                                self.last_seek_offset = t.start;
                                true
                            } else {
                                false
                            };
                            if cue_seek_ok {
                                self.reanchor_playback_clock_for_new_segment();
                                let meta = format!("{}. {} - {}", t.number, t.title, t.performer);
                                set_metadata(&slots.meta_slot, Some(meta));
                                set_cue_track(&slots.cue_track_slot, Some(track_idx));
                            }
                        }
                    }
                }
            }
        }

        if let Some(ref p) = self.backend_player {
            p.set_volume(self.current_volume);
            if self.paused {
                p.pause();
            } else {
                p.play();
            }
        }
        true
    }

    fn take_next_playable_path(&mut self) -> Option<(PathBuf, bool)> {
        if !self.injected_playlist.is_empty() {
            return self.injected_playlist.pop_front().map(|path| (path, true));
        }
        if self.base_playlist.is_empty() {
            return None;
        }
        let max_scan = self.base_playlist.len().max(1);
        let base_path_set = build_base_non_m3u_set(&self.base_playlist);
        for _ in 0..max_scan {
            if self.current_track_idx >= self.base_playlist.len() {
                self.current_track_idx = 0;
                log::info!("[AUDIO] Playlist finished, wrapping around to start.");
            }
            let path = self.base_playlist[self.current_track_idx].clone();
            self.current_track_idx += 1;
            if is_m3u_path(&path) {
                let expanded = expand_m3u_excluding_base(&path, &base_path_set);
                if expanded.is_empty() {
                    log::warn!("[AUDIO] m3u has no playable entries: {:?}", path);
                    continue;
                }
                self.injected_playlist = expanded.into();
                self.injected_history.clear();
                self.suppress_injected_history_once = false;
                return self.injected_playlist.pop_front().map(|next| (next, true));
            }
            return Some((path, false));
        }
        self.stopped = true;
        None
    }

    fn take_prev_playable_path(&mut self) -> Option<(PathBuf, bool)> {
        if self.base_playlist.is_empty() {
            return None;
        }
        let max_scan = self.base_playlist.len().max(1);
        let base_path_set = build_base_non_m3u_set(&self.base_playlist);
        for _ in 0..max_scan {
            if self.current_track_idx >= self.base_playlist.len() {
                self.current_track_idx = self.base_playlist.len().saturating_sub(1);
            }
            let path = self.base_playlist[self.current_track_idx].clone();
            self.current_track_idx = if self.current_track_idx == 0 {
                self.base_playlist.len().saturating_sub(1)
            } else {
                self.current_track_idx - 1
            };
            if is_m3u_path(&path) {
                let expanded = expand_m3u_excluding_base(&path, &base_path_set);
                if expanded.is_empty() {
                    continue;
                }
                self.injected_history = expanded[..expanded.len().saturating_sub(1)].to_vec();
                // Reverse navigation should land on the last expanded m3u track first.
                if let Some(last) = expanded.last() {
                    return Some((last.clone(), true));
                }
                continue;
            }
            return Some((path, false));
        }
        None
    }

    /// Rewind one step inside the currently injected chain.
    ///
    /// Strategy:
    /// - `injected_history` stores already-played injected tracks in forward order.
    /// - On Prev, we pull one item from history and push both `previous` then `current`
    ///   to the front of `injected_playlist`, so playback can restart from the previous
    ///   injected entry while still keeping the current one as the next forward item.
    /// - `suppress_injected_history_once` prevents this synthetic rewind transition from
    ///   immediately re-recording duplicate history on the next `feed_next_file`.
    fn rewind_injected_one_step(&mut self) -> bool {
        if !self.current_from_injected {
            return false;
        }
        let Some(previous) = self.injected_history.pop() else {
            return false;
        };
        if let Some(current) = self.current_file_path.clone() {
            self.injected_playlist.push_front(current);
        }
        self.injected_playlist.push_front(previous);
        true
    }

    fn update_metadata_for_new_file(&self, slots: &AudioSlots, path: &Path) {
        if let Some(ref cue) = self.cue_sheet {
            let initial_idx = self.pending_start_track_idx.unwrap_or(0);
            if let Some(t) = cue.tracks.get(initial_idx) {
                let meta = format!("{}. {} - {}", t.number, t.title, t.performer);
                set_metadata(&slots.meta_slot, Some(meta));
                set_cue_track(&slots.cue_track_slot, Some(initial_idx));
            }
            let markers: Vec<u64> = cue
                .tracks
                .iter()
                .map(|t| t.start.as_millis() as u64)
                .collect();
            set_cue_markers(&slots.cue_markers_slot, markers);
        } else if let Some(meta) = get_file_metadata(path) {
            set_metadata(&slots.meta_slot, Some(meta));
            set_cue_markers(&slots.cue_markers_slot, Vec::new());
        } else {
            set_metadata(&slots.meta_slot, None);
            set_cue_markers(&slots.cue_markers_slot, Vec::new());
        }
    }

    pub(crate) fn recover_orphaned_backend(&mut self, slots: &AudioSlots) {
        if !self.ensure_backend(slots) {
            return;
        }
        let path = match self.current_file_path.clone() {
            Some(p) => p,
            None => return,
        };
        let resume_pos = self.last_hw_pos.saturating_add(self.last_seek_offset);
        if let Some(source) = open_source(&path, resume_pos, &self.shutdown_flag) {
            let recovered = if let Some(p) = self.backend_player.as_mut() {
                p.append(source);
                self.sink_base_pos = p.get_pos();
                self.last_seek_offset = resume_pos;
                self.last_hw_pos = Duration::ZERO;
                true
            } else {
                false
            };
            if recovered {
                self.reanchor_playback_clock_for_new_segment();
                if let Some(p) = self.backend_player.as_ref() {
                    if !self.paused {
                        p.play();
                    } else {
                        p.pause();
                    }
                }
                log::info!(
                    "[AUDIO] Auto-recovered playback at {}ms",
                    resume_pos.as_millis()
                );
            }
        }
    }

    pub(crate) fn update_cue_track_highlight(&self, slots: &AudioSlots) {
        if let Some(ref cue) = self.cue_sheet {
            if self.backend_player.is_some() {
                let elapsed = self.absolute_playback_position();
                let idx = cue
                    .tracks
                    .iter()
                    .position(|t| t.start > elapsed)
                    .unwrap_or(cue.tracks.len())
                    .saturating_sub(1);
                let current_t = &cue.tracks[idx];
                let meta = format!(
                    "{}. {} - {}",
                    current_t.number, current_t.title, current_t.performer
                );
                if let Some(mut g) = slots.meta_slot.try_lock() {
                    if g.as_ref() != Some(&meta) {
                        *g = Some(meta);
                        set_cue_track(&slots.cue_track_slot, Some(idx));
                    }
                }
            }
        } else {
            set_cue_track(&slots.cue_track_slot, None);
        }
    }

    pub(crate) fn update_position(&mut self, slots: &AudioSlots) {
        if self.stopped {
            slots.pos_ms.store(0, Ordering::Relaxed);
            slots.dur_ms.store(0, Ordering::Relaxed);
            self.hw_pos_zero_since = None;
            self.hw_pos_zero_fallback_active = false;
        } else if let Some(ref p) = self.backend_player {
            let hw_pos = p.get_pos().saturating_sub(self.sink_base_pos);
            let segment_pos = self.playback_elapsed_since_current_stream();
            let effective_segment_pos = if hw_pos > Duration::ZERO {
                if self.hw_pos_zero_fallback_active {
                    log::info!("[AUDIO] Hardware position recovered; leaving wall-clock fallback.");
                }
                self.hw_pos_zero_since = None;
                self.hw_pos_zero_fallback_active = false;
                hw_pos
            } else {
                let zero_since = self.hw_pos_zero_since.get_or_insert_with(Instant::now);
                if !self.hw_pos_zero_fallback_active
                    && segment_pos >= AUDIO_HW_POS_ZERO_GRACE
                    && zero_since.elapsed() >= AUDIO_HW_POS_ZERO_GRACE
                {
                    self.hw_pos_zero_fallback_active = true;
                    log::warn!(
                        "[AUDIO] Hardware position stuck at 0 for {:?}; using wall-clock fallback.",
                        zero_since.elapsed()
                    );
                }
                if self.hw_pos_zero_fallback_active {
                    segment_pos
                } else {
                    hw_pos
                }
            };
            self.last_hw_pos = effective_segment_pos;
            let mut raw_abs_pos = effective_segment_pos.saturating_add(self.last_seek_offset);
            let cap_ms = slots.dur_ms.load(Ordering::Relaxed);
            if cap_ms > 0 {
                raw_abs_pos = raw_abs_pos.min(Duration::from_millis(cap_ms));
            }
            slots
                .pos_ms
                .store(raw_abs_pos.as_millis() as u64, Ordering::Relaxed);
        }
    }
}
