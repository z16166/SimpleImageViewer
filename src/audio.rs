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

use crate::constants::{
    AUDIO_BUFFER_CAPACITY, AUDIO_BUFFER_QUEUE_DEPTH, AUDIO_CHUNK_SIZE, AUDIO_RECOVERY_COOLDOWN,
    DEFAULT_CHANNELS, DEFAULT_SAMPLE_RATE, is_supported_music_extension,
};
use crate::scanner::is_offline;
use crossbeam_channel::Sender;
use std::collections::{HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lofty::prelude::*;
use lofty::read_from_path;
use rodio::Source;
use std::ffi::c_void;
use std::num::NonZero;
#[cfg(target_os = "windows")]
use std::os::windows::ffi::OsStrExt;

#[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
use monkey_sdk_sys::*;

use symphonia::core::audio::{AudioBuffer, AudioBufferRef, Signal};
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::conv::FromSample;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::Time;

// --- Audio Normalization Constants ---
const NORM_I8: f32 = 128.0;
const NORM_I16: f32 = 32768.0;
const NORM_I24: f32 = 8388608.0;
const NORM_I32: f32 = 2147483648.0;
const AUDIO_HW_POS_ZERO_GRACE: Duration = Duration::from_millis(500);

#[cfg(windows)]
unsafe extern "C" {
    fn wasapi_monitor_init();
    fn wasapi_monitor_uninit();
    fn wasapi_is_device_available() -> bool;
    fn wasapi_poll_device_lost() -> bool;
}

#[cfg(not(windows))]
#[allow(dead_code)]
unsafe fn wasapi_monitor_init() {}
#[cfg(not(windows))]
#[allow(dead_code)]
unsafe fn wasapi_monitor_uninit() {}
#[cfg(not(windows))]
#[allow(dead_code)]
unsafe fn wasapi_is_device_available() -> bool {
    true
}
#[cfg(not(windows))]
#[allow(dead_code)]
unsafe fn wasapi_poll_device_lost() -> bool {
    false
}

#[allow(dead_code)]
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
    SetDevice(Option<String>),
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
        self.last_error.try_lock().ok()?.take()
    }

    pub fn get_current_track(&self) -> Option<String> {
        self.current_track.try_lock().ok()?.clone()
    }

    pub fn get_current_track_path(&self) -> Option<PathBuf> {
        self.current_track_path.try_lock().ok()?.clone()
    }

    pub fn get_metadata(&self) -> Option<String> {
        self.current_metadata.try_lock().ok()?.clone()
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
        self.current_cue_track.try_lock().ok()?.clone()
    }

    pub fn get_cue_markers(&self) -> Vec<u64> {
        self.cue_markers.lock().unwrap().clone()
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
        if let Ok(mut guard) = self.current_device.try_lock() {
            *guard = device_name.clone();
        }
        if let Some(tx) = &self.cmd_tx {
            let _ = tx.send(AudioCommand::SetDevice(device_name));
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

// ---------------------------------------------------------------------------
// Collect music files (with cancellation support)
// ---------------------------------------------------------------------------

pub fn collect_music_files(path: &PathBuf, cancel: Option<Arc<AtomicBool>>) -> Vec<PathBuf> {
    fn is_music(p: &Path) -> bool {
        p.extension()
            .and_then(|e| e.to_str())
            .map(is_supported_music_extension)
            .unwrap_or(false)
    }

    let mut files = Vec::new();
    if path.is_file() {
        if is_music(path) {
            files.push(path.clone());
        }
    } else if path.is_dir() {
        // Walk directory and check cancel signal periodically
        for entry in walkdir::WalkDir::new(path)
            .follow_links(false)
            .into_iter()
            .flatten()
        {
            // Check cancellation
            if let Some(ref c) = cancel {
                if !c.load(Ordering::Relaxed) {
                    return Vec::new(); // Abort
                }
            }

            let p = entry.path();
            if p.is_file() && is_music(p) && !is_offline(p) {
                files.push(p.to_path_buf());
            }
        }
        files.sort();
    }
    files
}

fn is_supported_audio_or_playlist(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(is_supported_music_extension)
        .unwrap_or(false)
}

fn canonical_or_clone(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn build_base_non_m3u_set(base_playlist: &[PathBuf]) -> HashSet<PathBuf> {
    base_playlist
        .iter()
        .filter(|p| !is_m3u_path(p))
        .map(|p| canonical_or_clone(p))
        .collect()
}

fn expand_m3u_excluding_base(m3u_path: &Path, base_path_set: &HashSet<PathBuf>) -> Vec<PathBuf> {
    parse_m3u_entries(m3u_path)
        .into_iter()
        .filter(|p| !base_path_set.contains(p))
        .collect()
}

fn is_m3u_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("m3u"))
        .unwrap_or(false)
}

fn normalize_playlist_candidate(m3u_parent: &Path, raw_entry: &str) -> Option<PathBuf> {
    let line = raw_entry.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let candidate = PathBuf::from(line);
    let resolved = if candidate.is_absolute() {
        candidate
    } else {
        m3u_parent.join(candidate)
    };
    let canonical = resolved.canonicalize().unwrap_or(resolved);
    if canonical.is_file() && is_supported_audio_or_playlist(&canonical) {
        Some(canonical)
    } else {
        None
    }
}

fn parse_m3u_entries(m3u_path: &Path) -> Vec<PathBuf> {
    let content = match read_text_file_with_fallback(m3u_path) {
        Some(c) => c,
        None => return Vec::new(),
    };
    let parent = m3u_path.parent().unwrap_or_else(|| Path::new("."));
    let mut items = Vec::new();
    for line in content.lines() {
        if let Some(path) = normalize_playlist_candidate(parent, line) {
            items.push(path);
        }
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn make_temp_dir(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("siv-{name}-{nonce}"));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn collect_music_files_accepts_m3u_extension() {
        let dir = make_temp_dir("m3u-collect");
        let m3u = dir.join("list.m3u");
        let mp3 = dir.join("song.mp3");
        let txt = dir.join("note.txt");
        fs::write(&m3u, b"song.mp3\n").expect("write m3u");
        fs::write(&mp3, b"fake").expect("write mp3");
        fs::write(&txt, b"ignore").expect("write txt");

        let files = collect_music_files(&dir, None);
        assert!(files.iter().any(|p| p == &m3u));
        assert!(files.iter().any(|p| p == &mp3));
        assert!(!files.iter().any(|p| p == &txt));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn collect_music_files_uses_shared_extension_list() {
        let dir = make_temp_dir("music-ext-shared");
        let m4a = dir.join("track.m4a");
        let txt = dir.join("note.txt");
        fs::write(&m4a, b"fake").expect("write m4a");
        fs::write(&txt, b"ignore").expect("write txt");

        let files = collect_music_files(&dir, None);
        assert!(files.iter().any(|p| p == &m4a));
        assert!(!files.iter().any(|p| p == &txt));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_m3u_expands_relative_and_absolute_paths() {
        let dir = make_temp_dir("m3u-parse");
        let rel = dir.join("rel.mp3");
        let abs = dir.join("abs.flac");
        let missing = dir.join("missing.mp3");
        fs::write(&rel, b"fake").expect("write rel");
        fs::write(&abs, b"fake").expect("write abs");
        let m3u = dir.join("playlist.m3u");
        let content = format!(
            "#EXTM3U\n#EXTINF:1,track\n{}\n{}\n{}\n",
            rel.file_name().unwrap().to_string_lossy(),
            abs.to_string_lossy(),
            missing.file_name().unwrap().to_string_lossy()
        );
        fs::write(&m3u, content).expect("write playlist");

        let entries = parse_m3u_entries(&m3u);
        let rel_norm = rel.canonicalize().expect("canonical rel");
        let abs_norm = abs.canonicalize().expect("canonical abs");
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().any(|p| p == &rel_norm));
        assert!(entries.iter().any(|p| p == &abs_norm));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn take_next_playable_path_filters_entries_already_in_base_playlist() {
        let dir = make_temp_dir("m3u-dedup-base");
        let in_base = dir.join("in-base.mp3");
        let only_in_m3u = dir.join("only-in-m3u.mp3");
        fs::write(&in_base, b"fake").expect("write in_base");
        fs::write(&only_in_m3u, b"fake").expect("write only_in_m3u");
        let m3u = dir.join("playlist.m3u");
        let content = format!(
            "{}\n{}\n",
            in_base.to_string_lossy(),
            only_in_m3u.to_string_lossy()
        );
        fs::write(&m3u, content).expect("write playlist");

        let mut st = AudioLoopState::new(Arc::new(AtomicBool::new(false)));
        st.base_playlist = vec![in_base.clone(), m3u];
        st.current_track_idx = 1;

        let next = st.take_next_playable_path();
        assert_eq!(
            next,
            Some((only_in_m3u.canonicalize().expect("canonical only_in_m3u"), true))
        );
        assert!(st.injected_playlist.is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn m3u_dedup_to_empty_still_advances_on_base_playlist() {
        let dir = make_temp_dir("m3u-dedup-empty");
        let base1 = dir.join("base1.mp3");
        let base2 = dir.join("base2.mp3");
        fs::write(&base1, b"fake").expect("write base1");
        fs::write(&base2, b"fake").expect("write base2");
        let m3u = dir.join("playlist.m3u");
        let content = format!(
            "{}\n{}\n",
            base1.to_string_lossy(),
            base2.to_string_lossy()
        );
        fs::write(&m3u, content).expect("write playlist");

        let mut st = AudioLoopState::new(Arc::new(AtomicBool::new(false)));
        st.base_playlist = vec![base1.clone(), m3u, base2.clone()];
        st.current_track_idx = 1;

        // m3u entries are fully deduped against base_playlist, so this should skip m3u
        // and continue to the next base track.
        assert_eq!(st.take_next_playable_path(), Some((base2.clone(), false)));
        // Then wrap around and continue playing base tracks normally.
        assert_eq!(st.take_next_playable_path(), Some((base1, false)));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn prev_file_on_m3u_returns_last_expanded_track_not_first() {
        let dir = make_temp_dir("m3u-prev-last-track");
        let a = dir.join("a.mp3");
        let b = dir.join("b.mp3");
        let t1 = dir.join("t1.mp3");
        let t2 = dir.join("t2.mp3");
        fs::write(&a, b"fake").expect("write a");
        fs::write(&b, b"fake").expect("write b");
        fs::write(&t1, b"fake").expect("write t1");
        fs::write(&t2, b"fake").expect("write t2");
        let m3u = dir.join("list.m3u");
        fs::write(
            &m3u,
            format!("{}\n{}\n", t1.to_string_lossy(), t2.to_string_lossy()),
        )
        .expect("write m3u");

        let mut st = AudioLoopState::new(Arc::new(AtomicBool::new(false)));
        st.base_playlist = vec![a, m3u, b];
        st.current_track_idx = 3; // Just finished B, next forward index wrapped state

        // Emulate Prev behavior: seek previous base slot then resolve playable path in reverse.
        if st.current_track_idx > 1 {
            st.current_track_idx -= 2;
        } else {
            st.current_track_idx = st.base_playlist.len().saturating_sub(1);
        }
        let prev = st.take_prev_playable_path();
        assert_eq!(
            prev,
            Some((canonical_or_clone(&t2), true)),
            "Prev on m3u should land on last expanded track"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn prev_file_skips_empty_dedup_m3u_and_reaches_previous_base_track() {
        let dir = make_temp_dir("m3u-prev-skip-empty");
        let a = dir.join("a.mp3");
        let b = dir.join("b.mp3");
        fs::write(&a, b"fake").expect("write a");
        fs::write(&b, b"fake").expect("write b");
        let m3u = dir.join("dup.m3u");
        fs::write(&m3u, format!("{}\n{}\n", a.to_string_lossy(), b.to_string_lossy()))
            .expect("write m3u");

        let mut st = AudioLoopState::new(Arc::new(AtomicBool::new(false)));
        st.base_playlist = vec![a.clone(), m3u, b];
        st.current_track_idx = 3; // After B

        // Emulate Prev behavior: step back and resolve in reverse.
        if st.current_track_idx > 1 {
            st.current_track_idx -= 2;
        } else {
            st.current_track_idx = st.base_playlist.len().saturating_sub(1);
        }
        assert_eq!(st.take_prev_playable_path(), Some((a, false)));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn next_playable_none_marks_stopped_for_all_empty_m3u_without_base_tracks() {
        let dir = make_temp_dir("m3u-all-empty-loop");
        let missing = dir.join("missing.mp3");
        let m3u = dir.join("empty.m3u");
        fs::write(&m3u, format!("{}\n", missing.to_string_lossy())).expect("write m3u");

        let mut st = AudioLoopState::new(Arc::new(AtomicBool::new(false)));
        st.base_playlist = vec![m3u];
        st.stopped = false;

        assert_eq!(st.take_next_playable_path(), None);
        assert!(st.stopped, "state should stop when no playable entries remain");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn prev_file_then_next_file_walks_back_through_m3u_chain() {
        let dir = make_temp_dir("m3u-prev-then-next");
        let a = dir.join("a.mp3");
        let b = dir.join("b.mp3");
        let t1 = dir.join("t1.mp3");
        let t2 = dir.join("t2.mp3");
        fs::write(&a, b"fake").expect("write a");
        fs::write(&b, b"fake").expect("write b");
        fs::write(&t1, b"fake").expect("write t1");
        fs::write(&t2, b"fake").expect("write t2");
        let m3u = dir.join("list.m3u");
        fs::write(
            &m3u,
            format!("{}\n{}\n", t1.to_string_lossy(), t2.to_string_lossy()),
        )
        .expect("write m3u");

        let mut st = AudioLoopState::new(Arc::new(AtomicBool::new(false)));
        st.base_playlist = vec![a, m3u, b];
        st.current_track_idx = 3; // Simulate that B was just played.

        if st.current_track_idx > 1 {
            st.current_track_idx -= 2;
        } else {
            st.current_track_idx = st.base_playlist.len().saturating_sub(1);
        }
        let prev = st.take_prev_playable_path().expect("prev path");
        assert_eq!(prev, (canonical_or_clone(&t2), true));

        st.forced_next_path = Some(prev.clone());
        let picked_prev = st.forced_next_path.take().expect("forced next");
        assert_eq!(picked_prev, (canonical_or_clone(&t2), true));
        assert_eq!(st.injected_history, vec![canonical_or_clone(&t1)]);

        // Next prev inside injected chain should rewind to T1.
        st.current_file_path = Some(canonical_or_clone(&t2));
        st.current_from_injected = true;
        assert!(st.rewind_injected_one_step());
        assert_eq!(st.injected_playlist.pop_front(), Some(canonical_or_clone(&t1)));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn next_file_consumes_remaining_injected_entries() {
        let dir = make_temp_dir("m3u-next-file");
        let a = dir.join("a.ape");
        let b = dir.join("b.ape");
        fs::write(&a, b"fake").expect("write a");
        fs::write(&b, b"fake").expect("write b");

        let mut st = AudioLoopState::new(Arc::new(AtomicBool::new(false)));
        st.base_playlist = vec![dir.join("list.m3u")];
        st.injected_playlist.push_back(a.clone());
        st.injected_playlist.push_back(b.clone());

        assert_eq!(st.take_next_playable_path(), Some((a.clone(), true)));
        st.current_file_path = Some(a.clone());
        st.current_from_injected = true;
        assert_eq!(st.take_next_playable_path(), Some((b.clone(), true)));
        assert!(st.injected_playlist.is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn prev_file_rewinds_injected_history() {
        let dir = make_temp_dir("m3u-prev-file");
        let a = dir.join("a.ape");
        let b = dir.join("b.ape");
        let c = dir.join("c.ape");
        fs::write(&a, b"fake").expect("write a");
        fs::write(&b, b"fake").expect("write b");
        fs::write(&c, b"fake").expect("write c");

        let mut st = AudioLoopState::new(Arc::new(AtomicBool::new(false)));
        st.injected_playlist.push_back(c.clone());
        st.injected_history = vec![a.clone()];
        st.current_file_path = Some(b.clone());
        st.current_from_injected = true;

        assert!(st.rewind_injected_one_step());
        assert_eq!(st.injected_playlist.pop_front(), Some(a));
        assert_eq!(st.injected_playlist.pop_front(), Some(b));
        assert_eq!(st.injected_playlist.pop_front(), Some(c));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn prev_file_can_exit_injected_chain_to_base_playlist() {
        let mut st = AudioLoopState::new(Arc::new(AtomicBool::new(false)));
        let f1 = PathBuf::from("f1.flac");
        let f2 = PathBuf::from("f2.flac");
        let f3 = PathBuf::from("f3.flac");
        let a1 = PathBuf::from("a1.ape");
        let a2 = PathBuf::from("a2.ape");

        st.base_playlist = vec![f1.clone(), f2.clone(), f3.clone()];
        st.current_track_idx = 0;
        st.current_file_path = Some(a2.clone());
        st.current_from_injected = true;
        st.injected_history = vec![a1.clone()];

        // First prev inside injected chain should rewind to a1 and queue current a2.
        assert!(st.rewind_injected_one_step());
        assert_eq!(st.injected_history.len(), 0);
        assert_eq!(st.injected_playlist.pop_front(), Some(a1.clone()));
        assert_eq!(st.injected_playlist.pop_front(), Some(a2.clone()));
        // Simulate playback of a1 after rewind: do not record forward history.
        st.suppress_injected_history_once = true;
        st.current_file_path = Some(a1);
        st.current_from_injected = true;

        // No more injected history => fallback to base prev behavior should not get trapped in injected.
        assert!(!st.rewind_injected_one_step());
        if st.current_track_idx > 1 {
            st.current_track_idx -= 2;
        } else {
            st.current_track_idx = st.base_playlist.len().saturating_sub(1);
        }
        st.injected_playlist.clear();
        st.injected_history.clear();
        st.suppress_injected_history_once = false;
        st.current_from_injected = false;

        assert_eq!(st.current_track_idx, 2);
        assert!(!st.current_from_injected);
        assert!(st.injected_playlist.is_empty());
        assert!(st.injected_history.is_empty());
    }
}

// ---------------------------------------------------------------------------
// Background audio loop — rodio 0.22
// ---------------------------------------------------------------------------

fn set_error(slot: &AudioError, msg: impl Into<String>) {
    if let Ok(mut g) = slot.lock() {
        *g = Some(msg.into());
    }
}

fn set_current_track(slot: &Arc<Mutex<Option<String>>>, name: Option<String>) {
    if let Ok(mut g) = slot.lock() {
        *g = name;
    }
}

fn set_current_path(slot: &Arc<Mutex<Option<PathBuf>>>, path: Option<PathBuf>) {
    if let Ok(mut g) = slot.lock() {
        *g = path;
    }
}

fn set_metadata(slot: &Arc<Mutex<Option<String>>>, meta: Option<String>) {
    if let Ok(mut g) = slot.lock() {
        *g = meta;
    }
}

fn set_cue_track(slot: &Arc<Mutex<Option<usize>>>, idx: Option<usize>) {
    if let Ok(mut g) = slot.lock() {
        *g = idx;
    }
}

fn set_cue_markers(slot: &Arc<Mutex<Vec<u64>>>, markers: Vec<u64>) {
    if let Ok(mut g) = slot.lock() {
        *g = markers;
    }
}

// ---------------------------------------------------------------------------
// CUE Support
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct CueTrack {
    number: u32,
    title: String,
    performer: String,
    start: Duration,
}

struct CueSheet {
    tracks: Vec<CueTrack>,
}

fn parse_cue_time(time_str: &str) -> Option<Duration> {
    // MM:SS:FF where FF is 1/75th of a second
    let parts: Vec<&str> = time_str.split(':').collect();
    if parts.len() != 3 {
        return None;
    }
    let m = parts[0].parse::<u64>().ok()?;
    let s = parts[1].parse::<u64>().ok()?;
    let f = parts[2].parse::<u64>().ok()?;

    Some(Duration::from_secs(m * 60 + s) + Duration::from_micros(f * 1000000 / 75))
}

fn read_text_file_with_fallback(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    // Try UTF-8 first (including BOM)
    let bytes_no_bom = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        &bytes[3..]
    } else {
        &bytes
    };
    match std::str::from_utf8(bytes_no_bom) {
        Ok(s) => Some(s.to_string()),
        Err(_) => {
            // Fallback to GBK/GB18030 for Chinese CUE files
            let (decoded, _, had_errors) = encoding_rs::GBK.decode(&bytes);
            if had_errors {
                log::warn!("CUE file {:?} has encoding issues", path);
            }
            Some(decoded.into_owned())
        }
    }
}

fn parse_cue_file(cue_path: &Path) -> Option<CueSheet> {
    let content = read_text_file_with_fallback(cue_path)?;
    let mut tracks = Vec::new();
    let mut current_track: Option<CueTrack> = None;
    let mut album_performer = String::new();

    for line in content.lines() {
        let line = line.trim();
        if line.starts_with("PERFORMER") && current_track.is_none() {
            album_performer = line
                .trim_start_matches("PERFORMER")
                .trim()
                .trim_matches('"')
                .to_string();
        } else if line.starts_with("TRACK") {
            if let Some(t) = current_track.take() {
                tracks.push(t);
            }
            let Some(num_str) = line.split_whitespace().nth(1) else {
                continue;
            };
            let Ok(num) = num_str.parse::<u32>() else {
                continue;
            };
            current_track = Some(CueTrack {
                number: num,
                title: format!("Track {num}"),
                performer: album_performer.clone(),
                start: Duration::ZERO,
            });
        } else if let Some(ref mut t) = current_track {
            if line.starts_with("TITLE") {
                t.title = line
                    .trim_start_matches("TITLE")
                    .trim()
                    .trim_matches('"')
                    .to_string();
            } else if line.starts_with("PERFORMER") {
                t.performer = line
                    .trim_start_matches("PERFORMER")
                    .trim()
                    .trim_matches('"')
                    .to_string();
            } else if line.starts_with("INDEX 01") {
                let time_str = line.trim_start_matches("INDEX 01").trim();
                if let Some(d) = parse_cue_time(time_str) {
                    t.start = d;
                }
            }
        }
    }
    if let Some(t) = current_track {
        tracks.push(t);
    }

    if tracks.is_empty() {
        None
    } else {
        Some(CueSheet { tracks })
    }
}

fn load_cue(audio_path: &Path, shutdown_flag: &AtomicBool) -> Option<CueSheet> {
    // 1. Direct match
    let cue_path = audio_path.with_extension("cue");
    if cue_path.exists() {
        return parse_cue_file(&cue_path);
    }

    if shutdown_flag.load(Ordering::Relaxed) {
        return None;
    }

    // 2. Pattern replacement (e.g. (APE).ape -> (CUE).cue)
    if let Some(filename) = audio_path.file_name().and_then(|n| n.to_str()) {
        if filename.contains("(APE)") {
            let new_filename = filename.replace("(APE)", "(CUE)");
            let alt_cue_path = audio_path
                .with_file_name(new_filename)
                .with_extension("cue");
            if alt_cue_path.exists() {
                log::debug!("Found CUE by pattern replacement: {:?}", alt_cue_path);
                return parse_cue_file(&alt_cue_path);
            }
        }
    }

    if shutdown_flag.load(Ordering::Relaxed) {
        return None;
    }

    // 3. Directory scan and fuzzy matching
    if let Some(parent) = audio_path.parent() {
        if let Ok(entries) = fs::read_dir(parent) {
            let mut cue_files = Vec::new();
            for entry in entries.flatten() {
                if shutdown_flag.load(Ordering::Relaxed) {
                    return None;
                }
                let p = entry.path();
                if p.is_file()
                    && p.extension()
                        .map(|e| e.to_string_lossy().to_lowercase() == "cue")
                        .unwrap_or(false)
                {
                    cue_files.push(p);
                }
            }

            if cue_files.len() == 1 {
                log::debug!("Using the only CUE file in directory: {:?}", cue_files[0]);
                return parse_cue_file(&cue_files[0]);
            }

            if !cue_files.is_empty() {
                let audio_stem = audio_path
                    .file_stem()
                    .and_then(|s| s.to_str())?
                    .to_lowercase();
                // Remove common suffixes to increase matching success rate
                let clean_audio = audio_stem
                    .replace("(ape)", "")
                    .replace("(cue)", "")
                    .replace(" ", "")
                    .replace(".", "")
                    .replace("-", "");

                for cue_p in cue_files {
                    if shutdown_flag.load(Ordering::Relaxed) {
                        return None;
                    }
                    if let Some(cue_stem) = cue_p.file_stem().and_then(|s| s.to_str()) {
                        let cue_stem_lower = cue_stem.to_lowercase();
                        let clean_cue = cue_stem_lower
                            .replace("(ape)", "")
                            .replace("(cue)", "")
                            .replace(" ", "")
                            .replace(".", "")
                            .replace("-", "");
                        if clean_audio == clean_cue
                            || clean_audio.contains(&clean_cue)
                            || clean_cue.contains(&clean_audio)
                        {
                            log::debug!(
                                "Found CUE by fuzzy match: {:?} -> {:?}",
                                audio_path.file_name(),
                                cue_p.file_name()
                            );
                            return parse_cue_file(&cue_p);
                        }
                    }
                }
            }
        }
    }

    None
}

// ---------------------------------------------------------------------------
// Custom APE Source using official Monkey's Audio SDK (Native)
// ---------------------------------------------------------------------------

struct ApeSource {
    decoder: *mut c_void,
    sample_rate: u32,
    channels: u16,
    bits_per_sample: u16,
    total_blocks: i64,
    current_block: i64,
    buffer: Vec<f32>,
    buffer_pos: usize,
    shutdown_flag: Arc<AtomicBool>,
}

// Explicitly mark as Send because the raw pointer is managed carefully
unsafe impl Send for ApeSource {}

impl ApeSource {
    pub fn new_with_offset(
        path: &Path,
        shutdown_flag: Arc<AtomicBool>,
        offset: Duration,
    ) -> Option<Self> {
        let decoder_ptr = {
            #[cfg(target_os = "windows")]
            {
                let mut wide_path: Vec<u16> = path.as_os_str().encode_wide().collect();
                wide_path.push(0);
                unsafe { monkey_decoder_open(wide_path.as_ptr() as *const _) }
            }

            #[cfg(any(target_os = "linux", target_os = "macos"))]
            {
                // On Linux/macOS, str_utfn is wchar_t which is 32-bit (UTF-32)
                let s = path.to_string_lossy();
                let mut wide_path: Vec<u32> = s.chars().map(|c| c as u32).collect();
                wide_path.push(0);
                unsafe { monkey_decoder_open(wide_path.as_ptr() as *const _) }
            }

            #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
            {
                log::error!("[AUDIO] Native APE SDK is not supported on this platform.");
                return None;
            }
        };

        if decoder_ptr.is_null() {
            log::error!(
                "[AUDIO] Native Monkey's Audio SDK failed to open: {:?}",
                path.file_name()
            );
            return None;
        }

        let mut sample_rate: i32 = 0;
        let mut bits_per_sample: i32 = 0;
        let mut channels: i32 = 0;
        let mut total_blocks: i64 = 0;

        if unsafe {
            monkey_decoder_get_info(
                decoder_ptr,
                &mut sample_rate,
                &mut bits_per_sample,
                &mut channels,
                &mut total_blocks,
            )
        } != 0
        {
            log::error!(
                "[AUDIO] Native Monkey's Audio SDK failed to get info: {:?}",
                path.file_name()
            );
            unsafe { monkey_decoder_close(decoder_ptr) };
            return None;
        }

        log::info!(
            "[AUDIO] Native APE Info: Rate={}, Bits={}, Chan={}, Blocks={}",
            sample_rate,
            bits_per_sample,
            channels,
            total_blocks
        );

        let mut source = Self {
            decoder: decoder_ptr,
            sample_rate: sample_rate as u32,
            channels: channels as u16,
            bits_per_sample: bits_per_sample as u16,
            total_blocks,
            current_block: 0,
            buffer: Vec::new(),
            buffer_pos: 0,
            shutdown_flag,
        };

        if offset > Duration::ZERO {
            let offset_blocks = (offset.as_secs_f64() * sample_rate as f64) as i64;
            let target_block = offset_blocks.min(total_blocks.saturating_sub(1));
            if unsafe { monkey_decoder_seek(decoder_ptr, target_block) } == 0 {
                source.current_block = target_block;
            } else {
                log::warn!(
                    "[AUDIO] Native APE seek failed to block {} for {:?}",
                    target_block,
                    path.file_name()
                );
            }
        }

        Some(source)
    }

    fn decode_next_blocks(&mut self) -> bool {
        if self.shutdown_flag.load(Ordering::Relaxed) {
            return false;
        }

        const BLOCKS_TO_DECODE: i32 = 4096;
        let bytes_per_block = (self.channels as i32 * (self.bits_per_sample as i32 / 8)) as usize;
        let mut raw_buffer = vec![0u8; BLOCKS_TO_DECODE as usize * bytes_per_block];
        let mut blocks_retrieved: i32 = 0;

        #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
        let ret = unsafe {
            monkey_decoder_decode_blocks(
                self.decoder,
                raw_buffer.as_mut_ptr(),
                BLOCKS_TO_DECODE,
                &mut blocks_retrieved,
            )
        };

        #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
        let ret = {
            let _ = raw_buffer;
            -1
        };

        if ret != 0 || blocks_retrieved == 0 {
            return false;
        }

        self.buffer.clear();
        self.buffer_pos = 0;
        self.current_block += blocks_retrieved as i64;

        let bits = self.bits_per_sample;
        let bytes_per_sample = (bits / 8) as usize;

        for chunk in
            raw_buffer[..blocks_retrieved as usize * bytes_per_block].chunks_exact(bytes_per_sample)
        {
            let sample = match bits {
                8 => (chunk[0] as i8 as f32) / NORM_I8,
                16 => (i16::from_le_bytes([chunk[0], chunk[1]]) as f32) / NORM_I16,
                24 => {
                    let val = i32::from_le_bytes([
                        chunk[0],
                        chunk[1],
                        chunk[2],
                        if chunk[2] & 0x80 != 0 { 0xFF } else { 0x00 },
                    ]);
                    val as f32 / NORM_I24
                }
                32 => {
                    (i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]) as f32) / NORM_I32
                }
                _ => 0.0,
            };
            self.buffer.push(sample);
        }

        true
    }
}

impl Drop for ApeSource {
    fn drop(&mut self) {
        #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
        unsafe {
            monkey_decoder_close(self.decoder);
        }
    }
}

impl Iterator for ApeSource {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        if self.buffer_pos >= self.buffer.len() {
            if !self.decode_next_blocks() {
                return None;
            }
        }

        let sample = self.buffer[self.buffer_pos];
        self.buffer_pos += 1;
        Some(sample)
    }
}

impl rodio::Source for ApeSource {
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> NonZero<u16> {
        NonZero::new(self.channels).unwrap_or(NonZero::new(DEFAULT_CHANNELS).unwrap())
    }

    fn sample_rate(&self) -> NonZero<u32> {
        NonZero::new(self.sample_rate).unwrap_or(NonZero::new(DEFAULT_SAMPLE_RATE).unwrap())
    }

    fn total_duration(&self) -> Option<Duration> {
        if self.sample_rate > 0 {
            Some(Duration::from_secs_f64(
                self.total_blocks as f64 / self.sample_rate as f64,
            ))
        } else {
            None
        }
    }
}

struct SymphoniaSource {
    reader: Box<dyn FormatReader>,
    decoder: Box<dyn symphonia::core::codecs::Decoder>,
    track_id: u32,
    buffer: Vec<f32>,
    buffer_pos: usize,
    sample_rate: u32,
    channels: u16,
    total_duration: Option<Duration>,
    shutdown_flag: Arc<AtomicBool>,
}

impl SymphoniaSource {
    pub fn new_with_offset(
        path: &Path,
        shutdown_flag: Arc<AtomicBool>,
        offset: Duration,
    ) -> Option<Self> {
        let file = std::fs::File::open(path).ok()?;
        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }

        let mss = MediaSourceStream::new(Box::new(file), Default::default());
        let format_opts = FormatOptions::default();
        let metadata_opts = MetadataOptions::default();
        let decoder_opts = DecoderOptions { verify: false };

        let probed = symphonia::default::get_probe()
            .format(&hint, mss, &format_opts, &metadata_opts)
            .ok()?;

        let mut reader = probed.format;

        let track = reader
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL)?;

        let track_id = track.id;
        let sample_rate = track
            .codec_params
            .sample_rate
            .unwrap_or(DEFAULT_SAMPLE_RATE);
        let channels = track
            .codec_params
            .channels
            .map(|c| c.count() as u16)
            .unwrap_or(DEFAULT_CHANNELS);

        let decoder = symphonia::default::get_codecs()
            .make(&track.codec_params, &decoder_opts)
            .ok()?;

        let total_duration = match (track.codec_params.n_frames, track.codec_params.sample_rate) {
            (Some(n), Some(s)) => Some(Duration::from_secs_f64(n as f64 / s as f64)),
            _ => None,
        };

        // Perform native seek if offset > 0
        if offset > Duration::ZERO {
            let seek_to = SeekTo::Time {
                time: Time::from(offset.as_secs_f64()),
                track_id: Some(track_id),
            };
            let _ = reader.seek(SeekMode::Accurate, seek_to);
        }

        Some(Self {
            reader,
            decoder,
            track_id,
            buffer: Vec::new(),
            buffer_pos: 0,
            sample_rate,
            channels,
            total_duration,
            shutdown_flag,
        })
    }

    fn refill_buffer(&mut self) -> bool {
        loop {
            if self.shutdown_flag.load(Ordering::Relaxed) {
                return false;
            }

            let packet = match self.reader.next_packet() {
                Ok(packet) => packet,
                Err(SymphoniaError::IoError(_)) => return false,
                Err(SymphoniaError::DecodeError(_)) => continue,
                Err(_) => return false,
            };

            if packet.track_id() != self.track_id {
                continue;
            }

            match self.decoder.decode(&packet) {
                Ok(decoded) => {
                    self.buffer.clear();
                    self.buffer_pos = 0;

                    match decoded {
                        AudioBufferRef::F32(ref buf) => {
                            Self::push_interleaved_to_vec(&mut self.buffer, buf)
                        }
                        AudioBufferRef::U8(ref buf) => {
                            Self::push_interleaved_to_vec(&mut self.buffer, buf)
                        }
                        AudioBufferRef::U16(ref buf) => {
                            Self::push_interleaved_to_vec(&mut self.buffer, buf)
                        }
                        AudioBufferRef::U24(ref buf) => {
                            Self::push_interleaved_to_vec(&mut self.buffer, buf)
                        }
                        AudioBufferRef::U32(ref buf) => {
                            Self::push_interleaved_to_vec(&mut self.buffer, buf)
                        }
                        AudioBufferRef::S8(ref buf) => {
                            Self::push_interleaved_to_vec(&mut self.buffer, buf)
                        }
                        AudioBufferRef::S16(ref buf) => {
                            Self::push_interleaved_to_vec(&mut self.buffer, buf)
                        }
                        AudioBufferRef::S24(ref buf) => {
                            Self::push_interleaved_to_vec(&mut self.buffer, buf)
                        }
                        AudioBufferRef::S32(ref buf) => {
                            Self::push_interleaved_to_vec(&mut self.buffer, buf)
                        }
                        AudioBufferRef::F64(ref buf) => {
                            Self::push_interleaved_to_vec(&mut self.buffer, buf)
                        }
                    }
                    return true;
                }
                Err(SymphoniaError::IoError(_)) => return false,
                Err(SymphoniaError::DecodeError(_)) => continue,
                Err(_) => return false,
            }
        }
    }

    fn push_interleaved_to_vec<S: symphonia::core::sample::Sample>(
        target: &mut Vec<f32>,
        buf: &AudioBuffer<S>,
    ) where
        f32: symphonia::core::conv::FromSample<S>,
    {
        let channels = buf.spec().channels.count();
        let frames = buf.frames();
        target.reserve(frames * channels);

        for i in 0..frames {
            for c in 0..channels {
                let sample = buf.chan(c)[i];
                target.push(f32::from_sample(sample));
            }
        }
    }
}

/// A wrapper for audio sources that performs decoding in a background thread to prevent stuttering.
struct BufferedSource {
    rx: crossbeam_channel::Receiver<Vec<f32>>,
    current_chunk: Vec<f32>,
    current_pos: usize,
    sample_rate: u32,
    channels: u16,
    total_duration: Option<Duration>,
    local_shutdown: Arc<AtomicBool>,
}

impl BufferedSource {
    pub fn new<S>(source: S, global_shutdown: Arc<AtomicBool>) -> Self
    where
        S: rodio::Source<Item = f32> + Send + 'static,
    {
        let sample_rate = source.sample_rate().get();
        let channels = source.channels().get();
        let total_duration = source.total_duration();

        let (tx, rx) = crossbeam_channel::bounded::<Vec<f32>>(AUDIO_BUFFER_QUEUE_DEPTH);
        let local_shutdown = Arc::new(AtomicBool::new(false));
        let thread_local_shutdown = Arc::clone(&local_shutdown);
        let thread_global_shutdown = Arc::clone(&global_shutdown);

        let res = std::thread::Builder::new()
            .name("audio-decoder".to_string())
            .spawn(move || {
                let mut source = source;
                let mut chunk = Vec::with_capacity(AUDIO_CHUNK_SIZE);

                loop {
                    // Stop if either local source is dropped OR global app is shutting down
                    if thread_local_shutdown.load(Ordering::Relaxed)
                        || thread_global_shutdown.load(Ordering::Relaxed)
                    {
                        break;
                    }

                    if let Some(sample) = source.next() {
                        chunk.push(sample);
                        if chunk.len() >= AUDIO_CHUNK_SIZE {
                            if tx.send(std::mem::take(&mut chunk)).is_err() {
                                break;
                            }
                            chunk.reserve(AUDIO_CHUNK_SIZE);
                        }
                    } else {
                        if !chunk.is_empty() {
                            let _ = tx.send(chunk);
                        }
                        break;
                    }
                }
            });
        if let Err(e) = res {
            log::error!("[Audio] Failed to spawn audio decoder thread: {}", e);
        }

        // WARM-UP: Wait briefly for the first chunk to ensure we don't start with silence.
        let mut current_chunk = Vec::new();
        if let Ok(first_chunk) = rx.recv_timeout(Duration::from_millis(100)) {
            current_chunk = first_chunk;
        }

        Self {
            rx,
            current_chunk,
            current_pos: 0,
            sample_rate,
            channels,
            total_duration,
            local_shutdown,
        }
    }
}

impl Iterator for BufferedSource {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        if self.current_pos >= self.current_chunk.len() {
            // Use try_recv to NEVER block the mixer thread.
            match self.rx.try_recv() {
                Ok(chunk) => {
                    self.current_chunk = chunk;
                    self.current_pos = 0;
                }
                Err(crossbeam_channel::TryRecvError::Empty) => {
                    // Buffer underrun: return silence instead of blocking.
                    return Some(0.0);
                }
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    return None;
                }
            }
        }

        let sample = *self.current_chunk.get(self.current_pos).unwrap_or(&0.0);
        self.current_pos += 1;
        Some(sample)
    }
}

impl rodio::Source for BufferedSource {
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> std::num::NonZero<u16> {
        std::num::NonZero::new(self.channels)
            .unwrap_or(std::num::NonZero::new(DEFAULT_CHANNELS).unwrap())
    }

    fn sample_rate(&self) -> std::num::NonZero<u32> {
        std::num::NonZero::new(self.sample_rate)
            .unwrap_or(std::num::NonZero::new(DEFAULT_SAMPLE_RATE).unwrap())
    }

    fn total_duration(&self) -> Option<Duration> {
        self.total_duration
    }
}

impl Drop for BufferedSource {
    fn drop(&mut self) {
        // Trigger local shutdown only!
        self.local_shutdown.store(true, Ordering::Relaxed);
    }
}

impl Iterator for SymphoniaSource {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        if self.buffer_pos >= self.buffer.len() {
            if !self.refill_buffer() {
                return None;
            }
        }
        let sample = self.buffer[self.buffer_pos];
        self.buffer_pos += 1;
        Some(sample)
    }
}

impl rodio::Source for SymphoniaSource {
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> NonZero<u16> {
        NonZero::new(self.channels).unwrap_or(NonZero::new(DEFAULT_CHANNELS).unwrap())
    }

    fn sample_rate(&self) -> NonZero<u32> {
        NonZero::new(self.sample_rate).unwrap_or(NonZero::new(DEFAULT_SAMPLE_RATE).unwrap())
    }

    fn total_duration(&self) -> Option<Duration> {
        self.total_duration
    }
}

fn get_file_metadata(path: &Path) -> Option<String> {
    let tagged_file = read_from_path(path).ok()?;
    let tag = tagged_file
        .primary_tag()
        .or_else(|| tagged_file.first_tag())?;

    let title = tag.title()?;
    let artist = tag.artist()?;
    let track = tag.track();

    if let Some(t) = track {
        Some(format!("{t}. {title} - {artist}"))
    } else {
        Some(format!("{title} - {artist}"))
    }
}

fn create_source(
    path: &Path,
    _reader: std::io::BufReader<std::fs::File>,
    shutdown_flag: Arc<AtomicBool>,
    offset: Duration,
) -> Option<Box<dyn rodio::Source<Item = f32> + Send>> {
    let is_ape = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase() == "ape")
        .unwrap_or(false);
    if is_ape {
        let source = ApeSource::new_with_offset(path, Arc::clone(&shutdown_flag), offset);
        if source.is_none() {
            log::error!(
                "[AUDIO] Failed to create native ApeSource for {:?}",
                path.file_name()
            );
        }
        source.map(|s| {
            Box::new(BufferedSource::new(s, shutdown_flag))
                as Box<dyn rodio::Source<Item = f32> + Send>
        })
    } else {
        // Use our high-performance SymphoniaSource for all other formats
        SymphoniaSource::new_with_offset(path, Arc::clone(&shutdown_flag), offset).map(|s| {
            Box::new(BufferedSource::new(s, shutdown_flag))
                as Box<dyn rodio::Source<Item = f32> + Send>
        })
    }
}

/// Open a file at `path` and create an audio source starting at `offset`.
fn open_source(
    path: &Path,
    offset: Duration,
    shutdown_flag: &Arc<AtomicBool>,
) -> Option<Box<dyn rodio::Source<Item = f32> + Send>> {
    let file = fs::File::open(path).ok()?;
    let reader = std::io::BufReader::with_capacity(AUDIO_BUFFER_CAPACITY, file);
    create_source(path, reader, Arc::clone(shutdown_flag), offset)
}

// ---------------------------------------------------------------------------
// Shared slot bundle passed into the audio thread
// ---------------------------------------------------------------------------

struct AudioSlots {
    err_slot: AudioError,
    track_slot: Arc<Mutex<Option<String>>>,
    path_slot: Arc<Mutex<Option<PathBuf>>>,
    meta_slot: Arc<Mutex<Option<String>>>,
    tracks_flag: Arc<AtomicBool>,
    cue_track_slot: Arc<Mutex<Option<usize>>>,
    cue_markers_slot: Arc<Mutex<Vec<u64>>>,
    pos_ms: Arc<std::sync::atomic::AtomicU64>,
    dur_ms: Arc<std::sync::atomic::AtomicU64>,
    device_slot: Arc<Mutex<Option<String>>>,
}

// ---------------------------------------------------------------------------
// All mutable state that lives inside the audio event loop
// ---------------------------------------------------------------------------

struct AudioLoopState {
    base_playlist: Vec<PathBuf>,
    injected_playlist: VecDeque<PathBuf>,
    injected_history: Vec<PathBuf>,
    suppress_injected_history_once: bool,
    current_track_idx: usize,
    forced_next_path: Option<(PathBuf, bool)>,
    current_file_path: Option<PathBuf>,
    current_from_injected: bool,
    stopped: bool,
    paused: bool,
    current_volume: f32,
    cue_sheet: Option<CueSheet>,
    current_file_start: Instant,
    last_seek_offset: Duration,
    paused_at: Option<Instant>,
    total_paused: Duration,
    pending_start_track_idx: Option<usize>,
    backend_sink: Option<rodio::MixerDeviceSink>,
    backend_player: Option<rodio::Player>,
    last_backend_attempt: Option<Instant>,
    sink_base_pos: Duration,
    last_hw_pos: Duration,
    hw_pos_zero_since: Option<Instant>,
    hw_pos_zero_fallback_active: bool,
    shutdown_flag: Arc<AtomicBool>,
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

    fn new(shutdown_flag: Arc<AtomicBool>) -> Self {
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

        let selected_device = slots.device_slot.lock().unwrap().clone();
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

    fn handle_stop(&mut self, slots: &AudioSlots) {
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

    fn handle_pause(&mut self) {
        self.paused = true;
        self.paused_at = Some(Instant::now());
        if let Some(ref p) = self.backend_player {
            p.pause();
        }
    }

    fn handle_play(&mut self, slots: &AudioSlots) {
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

    fn handle_seek(&mut self, pos: Duration, slots: &AudioSlots) {
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

    fn handle_set_device(&mut self, slots: &AudioSlots) {
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

    fn handle_next_file(&mut self, slots: &AudioSlots) {
        if let Some(ref p) = self.backend_player {
            p.clear();
        }
        self.forced_next_path = None;
        // Keep injected queue/history so `NextFile` can advance within expanded m3u tracks.
        self.cue_sheet = None;
        set_cue_track(&slots.cue_track_slot, None);
        slots.tracks_flag.store(false, Ordering::Relaxed);
    }

    fn handle_prev_file(&mut self, slots: &AudioSlots) {
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

    fn handle_next_track(&mut self, slots: &AudioSlots) {
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

    fn handle_prev_track(&mut self, slots: &AudioSlots) {
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

    fn handle_set_playlist(
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
    fn feed_next_file(&mut self, slots: &AudioSlots) -> bool {
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

    fn recover_orphaned_backend(&mut self, slots: &AudioSlots) {
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

    fn update_cue_track_highlight(&self, slots: &AudioSlots) {
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
                if let Ok(mut g) = slots.meta_slot.try_lock() {
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

    fn update_position(&mut self, slots: &AudioSlots) {
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

fn run_audio_loop(
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
    unsafe {
        wasapi_monitor_init();
    }

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
                st.backend_player.take().map(|p| p.stop());
                st.backend_sink.take();
                set_current_track(&slots.track_slot, None);
                set_metadata(&slots.meta_slot, None);
                unsafe {
                    wasapi_monitor_uninit();
                }
                return;
            }
            Ok(AudioCommand::Stop) => st.handle_stop(&slots),
            Ok(AudioCommand::Pause) => st.handle_pause(),
            Ok(AudioCommand::Play) => st.handle_play(&slots),
            Ok(AudioCommand::Seek(pos)) => st.handle_seek(pos, &slots),
            Ok(AudioCommand::SetDevice(_)) => st.handle_set_device(&slots),
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
            let player_empty = st.backend_player.as_ref().map_or(false, |p| p.empty());
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
            if st.backend_player.as_ref().map_or(false, |p| !p.empty()) {
                st.update_cue_track_highlight(&slots);
            }
        }

        // Position reporting for UI slider
        st.update_position(&slots);
    }
}
