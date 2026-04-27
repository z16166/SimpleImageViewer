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
    DEFAULT_CHANNELS, DEFAULT_SAMPLE_RATE,
};
use crate::scanner::is_offline;
use crossbeam_channel::Sender;
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

#[cfg(windows)]
unsafe extern "C" {
    fn wasapi_monitor_init();
    fn wasapi_monitor_uninit();
    fn wasapi_is_device_available() -> bool;
    fn wasapi_poll_device_lost() -> bool;
}

#[cfg(not(windows))]
unsafe fn wasapi_monitor_init() {}
#[cfg(not(windows))]
unsafe fn wasapi_monitor_uninit() {}
#[cfg(not(windows))]
unsafe fn wasapi_is_device_available() -> bool {
    true
}
#[cfg(not(windows))]
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
        use ::rodio::cpal::traits::{DeviceTrait, HostTrait};
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
            .map(|e| {
                matches!(
                    e.to_lowercase().as_str(),
                    "mp3" | "flac" | "ogg" | "wav" | "aac" | "m4a" | "ape"
                )
            })
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
    playlist: Vec<PathBuf>,
    current_track_idx: usize,
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
    shutdown_flag: Arc<AtomicBool>,
}

impl AudioLoopState {
    fn new(shutdown_flag: Arc<AtomicBool>) -> Self {
        Self {
            playlist: Vec::new(),
            current_track_idx: 0,
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
            use ::rodio::cpal::traits::{DeviceTrait, HostTrait};
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
        self.playlist.clear();
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
        if self.stopped && !self.playlist.is_empty() {
            self.stopped = false;
            let resume_pos = self.last_hw_pos.saturating_add(self.last_seek_offset);
            self.backend_player = None;
            self.backend_sink = None;
            if self.current_track_idx > 0 {
                self.current_track_idx -= 1;
            }
            let path = self.playlist[self.current_track_idx % self.playlist.len()].clone();
            if let Some(source) = open_source(&path, resume_pos, &self.shutdown_flag) {
                if self.ensure_backend(slots) {
                    if let Some(ref p) = self.backend_player {
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
                        self.current_file_start = Instant::now();
                        self.total_paused = Duration::ZERO;
                    }
                }
            }
        } else if let Some(ref p) = self.backend_player {
            p.play();
        }
    }

    fn handle_seek(&mut self, pos: Duration, slots: &AudioSlots) {
        if self.playlist.is_empty() {
            return;
        }
        let path_idx = self.current_track_idx.saturating_sub(1) % self.playlist.len();
        let path = self.playlist[path_idx].clone();
        if let Some(source) = open_source(&path, pos, &self.shutdown_flag) {
            if self.ensure_backend(slots) {
                if let Some(ref p) = self.backend_player {
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
                    self.current_file_start = Instant::now();
                    self.total_paused = Duration::ZERO;
                    if !self.paused {
                        p.play();
                    }
                    log::debug!("[AUDIO] Seek to {} ms successful", pos.as_millis());
                }
            }
        } else {
            log::error!("[AUDIO] Failed to re-open file for seek: {:?}", path);
        }
    }

    fn handle_set_device(&mut self, slots: &AudioSlots) {
        let resume_pos = if let Some(ref p) = self.backend_player {
            p.get_pos().saturating_add(self.last_seek_offset)
        } else {
            self.last_seek_offset
        };
        self.backend_sink = None;
        self.backend_player = None;
        self.sink_base_pos = Duration::ZERO;
        if self.playlist.is_empty() || self.stopped {
            return;
        }
        let path_idx = self.current_track_idx.saturating_sub(1) % self.playlist.len();
        let path = self.playlist[path_idx].clone();
        if let Some(source) = open_source(&path, resume_pos, &self.shutdown_flag) {
            if self.ensure_backend(slots) {
                if let Some(ref p) = self.backend_player {
                    p.append(source);
                    self.sink_base_pos = p.get_pos();
                    self.last_seek_offset = resume_pos;
                    self.last_hw_pos = Duration::ZERO;
                    self.current_file_start = Instant::now();
                    self.total_paused = Duration::ZERO;
                    if !self.paused {
                        p.play();
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
        self.cue_sheet = None;
        set_cue_track(&slots.cue_track_slot, None);
        slots.tracks_flag.store(false, Ordering::Relaxed);
    }

    fn handle_prev_file(&mut self, slots: &AudioSlots) {
        if self.current_track_idx > 1 {
            self.current_track_idx -= 2;
        } else {
            self.current_track_idx = self.playlist.len().saturating_sub(1);
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
        let elapsed = self
            .current_file_start
            .elapsed()
            .saturating_sub(self.total_paused)
            .saturating_add(self.last_seek_offset);
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
        let path = match self.playlist.get(self.current_track_idx.saturating_sub(1)) {
            Some(p) => p.clone(),
            None => return,
        };
        if let Some(source) = open_source(&path, next_t.start, &self.shutdown_flag) {
            if self.ensure_backend(slots) {
                if let Some(ref p) = self.backend_player {
                    p.clear();
                    self.sink_base_pos = p.get_pos();
                    p.append(source);
                    p.play();
                }
            }
            self.last_seek_offset = next_t.start;
            self.current_file_start = Instant::now();
            self.total_paused = Duration::ZERO;
            self.paused_at = None;
            slots
                .pos_ms
                .store(next_t.start.as_millis() as u64, Ordering::Relaxed);
            let meta = format!("{}. {} - {}", next_t.number, next_t.title, next_t.performer);
            set_metadata(&slots.meta_slot, Some(meta));
            set_cue_track(&slots.cue_track_slot, Some(current_idx + 1));
        }
    }

    fn handle_prev_track(&mut self, slots: &AudioSlots) {
        let cue = match &self.cue_sheet {
            Some(c) => c,
            None => return,
        };
        let elapsed = self
            .current_file_start
            .elapsed()
            .saturating_sub(self.total_paused)
            .saturating_add(self.last_seek_offset);
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
        let path = match self.playlist.get(self.current_track_idx.saturating_sub(1)) {
            Some(p) => p.clone(),
            None => return,
        };
        if let Some(source) = open_source(&path, target_t.start, &self.shutdown_flag) {
            if self.ensure_backend(slots) {
                if let Some(ref p) = self.backend_player {
                    p.clear();
                    p.append(source);
                    p.play();
                }
            }
            self.last_seek_offset = target_t.start;
            self.current_file_start = Instant::now();
            self.total_paused = Duration::ZERO;
            self.paused_at = None;
            let meta = format!(
                "{}. {} - {}",
                target_t.number, target_t.title, target_t.performer
            );
            set_metadata(&slots.meta_slot, Some(meta));
            set_cue_track(&slots.cue_track_slot, Some(target_idx));
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
        self.playlist = new_list;
        self.current_track_idx = start_idx.unwrap_or(0);
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
        if self.current_track_idx >= self.playlist.len() {
            self.current_track_idx = 0;
            log::info!("[AUDIO] Playlist finished, wrapping around to start.");
        }
        let path = self.playlist[self.current_track_idx].clone();
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
        if let Some(ref p) = self.backend_player {
            p.append(source);
            self.sink_base_pos = p.get_pos();
            self.current_track_idx += 1;
            self.last_seek_offset = Duration::ZERO;
            self.last_hw_pos = Duration::ZERO;
            self.current_file_start = Instant::now();
            self.total_paused = Duration::ZERO;
            if self.paused {
                self.paused_at = Some(Instant::now());
            }
        }

        // Seek to a saved CUE track if resuming from saved state.
        if let (Some(track_idx), Some(cue)) = (self.pending_start_track_idx.take(), &self.cue_sheet)
        {
            if track_idx < cue.tracks.len() {
                let t = cue.tracks[track_idx].clone();
                if t.start > Duration::ZERO {
                    if let Some(s2) = open_source(&path, t.start, &self.shutdown_flag) {
                        if self.ensure_backend(slots) {
                            if let Some(ref p) = self.backend_player {
                                p.clear();
                                p.append(s2);
                                self.last_seek_offset = t.start;
                                self.current_file_start = Instant::now();
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
        let path_idx = self.current_track_idx.saturating_sub(1) % self.playlist.len();
        let path = self.playlist[path_idx].clone();
        let resume_pos = self.last_hw_pos.saturating_add(self.last_seek_offset);
        if let Some(source) = open_source(&path, resume_pos, &self.shutdown_flag) {
            if let Some(ref p) = self.backend_player {
                p.append(source);
                self.sink_base_pos = p.get_pos();
                self.last_seek_offset = resume_pos;
                self.last_hw_pos = Duration::ZERO;
                self.current_file_start = Instant::now();
                self.total_paused = Duration::ZERO;
                if !self.paused {
                    p.play();
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
            if let Some(ref p) = self.backend_player {
                let elapsed = p
                    .get_pos()
                    .saturating_sub(self.sink_base_pos)
                    .saturating_add(self.last_seek_offset);
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
        } else if let Some(ref p) = self.backend_player {
            let hw_pos = p.get_pos().saturating_sub(self.sink_base_pos);
            self.last_hw_pos = hw_pos;
            let raw_abs_pos = hw_pos.saturating_add(self.last_seek_offset);
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
        if !st.stopped && !st.playlist.is_empty() {
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
