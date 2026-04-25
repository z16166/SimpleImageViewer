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

use crate::scanner::is_offline;
use crate::constants::{AUDIO_BUFFER_CAPACITY, AUDIO_BUFFER_QUEUE_DEPTH, AUDIO_CHUNK_SIZE, AUDIO_RECOVERY_COOLDOWN, DEFAULT_CHANNELS, DEFAULT_SAMPLE_RATE};
use crossbeam_channel::Sender;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lofty::prelude::*;
use lofty::read_from_path;
use std::num::NonZero;
use rodio::Source;
use std::ffi::c_void;
#[cfg(target_os = "windows")]
use std::os::windows::ffi::OsStrExt;

#[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
use monkey_sdk_sys::*;

use symphonia::core::audio::{AudioBuffer, AudioBufferRef, Signal};
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::Time;
use symphonia::core::conv::FromSample;

// --- Audio Normalization Constants ---
const NORM_I8: f32  = 128.0;
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
unsafe fn wasapi_is_device_available() -> bool { true }
#[cfg(not(windows))]
unsafe fn wasapi_poll_device_lost() -> bool { false }

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
    Stop,     // Clears playlist and stops playback, but keeps thread alive
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



    pub fn start_at(&mut self, files: Vec<PathBuf>, start_index: Option<usize>, start_track_index: Option<usize>, paused: bool) {
        self.ensure_thread_started();
        if let Some(tx) = &self.cmd_tx {
            let _ = tx.send(AudioCommand::SetPlaylist(files, start_index, start_track_index, paused));
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
                    run_audio_loop(rx, shutdown_flag, err_slot, track_slot, path_slot, meta_slot, tracks_flag, cue_track_slot, needs_restart, cue_markers_slot, pos_ms, dur_ms, dev_slot)
                });
            match res {
                Ok(handle) => { self.thread_handle = Some(handle); }
                Err(e) => { log::error!("[Audio] Failed to spawn audio thread: {}", e); }
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
            .map(|e| matches!(e.to_lowercase().as_str(), "mp3" | "flac" | "ogg" | "wav" | "aac" | "m4a" | "ape"))
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
            album_performer = line.trim_start_matches("PERFORMER").trim().trim_matches('"').to_string();
        } else if line.starts_with("TRACK") {
            if let Some(t) = current_track.take() {
                tracks.push(t);
            }
            let Some(num_str) = line.split_whitespace().nth(1) else { continue };
            let Ok(num) = num_str.parse::<u32>() else { continue };
            current_track = Some(CueTrack {
                number: num,
                title: format!("Track {num}"),
                performer: album_performer.clone(),
                start: Duration::ZERO,
            });
        } else if let Some(ref mut t) = current_track {
            if line.starts_with("TITLE") {
                t.title = line.trim_start_matches("TITLE").trim().trim_matches('"').to_string();
            } else if line.starts_with("PERFORMER") {
                t.performer = line.trim_start_matches("PERFORMER").trim().trim_matches('"').to_string();
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

    if shutdown_flag.load(Ordering::Relaxed) { return None; }

    // 2. Pattern replacement (e.g. (APE).ape -> (CUE).cue)
    if let Some(filename) = audio_path.file_name().and_then(|n| n.to_str()) {
        if filename.contains("(APE)") {
            let new_filename = filename.replace("(APE)", "(CUE)");
            let alt_cue_path = audio_path.with_file_name(new_filename).with_extension("cue");
            if alt_cue_path.exists() {
                log::debug!("Found CUE by pattern replacement: {:?}", alt_cue_path);
                return parse_cue_file(&alt_cue_path);
            }
        }
    }

    if shutdown_flag.load(Ordering::Relaxed) { return None; }

    // 3. Directory scan and fuzzy matching
    if let Some(parent) = audio_path.parent() {
        if let Ok(entries) = fs::read_dir(parent) {
            let mut cue_files = Vec::new();
            for entry in entries.flatten() {
                if shutdown_flag.load(Ordering::Relaxed) { return None; }
                let p = entry.path();
                if p.is_file() && p.extension().map(|e| e.to_string_lossy().to_lowercase() == "cue").unwrap_or(false) {
                    cue_files.push(p);
                }
            }

            if cue_files.len() == 1 {
                log::debug!("Using the only CUE file in directory: {:?}", cue_files[0]);
                return parse_cue_file(&cue_files[0]);
            }
            
            if !cue_files.is_empty() {
                let audio_stem = audio_path.file_stem().and_then(|s| s.to_str())?.to_lowercase();
                // Remove common suffixes to increase matching success rate
                let clean_audio = audio_stem.replace("(ape)", "").replace("(cue)", "").replace(" ", "").replace(".", "").replace("-", "");
                
                for cue_p in cue_files {
                    if shutdown_flag.load(Ordering::Relaxed) { return None; }
                    if let Some(cue_stem) = cue_p.file_stem().and_then(|s| s.to_str()) {
                        let cue_stem_lower = cue_stem.to_lowercase();
                        let clean_cue = cue_stem_lower.replace("(ape)", "").replace("(cue)", "").replace(" ", "").replace(".", "").replace("-", "");
                        if clean_audio == clean_cue || clean_audio.contains(&clean_cue) || clean_cue.contains(&clean_audio) {
                            log::debug!("Found CUE by fuzzy match: {:?} -> {:?}", audio_path.file_name(), cue_p.file_name());
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
    pub fn new_with_offset(path: &Path, shutdown_flag: Arc<AtomicBool>, offset: Duration) -> Option<Self> {
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
            log::error!("[AUDIO] Native Monkey's Audio SDK failed to open: {:?}", path.file_name());
            return None;
        }

        let mut sample_rate: i32 = 0;
        let mut bits_per_sample: i32 = 0;
        let mut channels: i32 = 0;
        let mut total_blocks: i64 = 0;

        if unsafe { monkey_decoder_get_info(decoder_ptr, &mut sample_rate, &mut bits_per_sample, &mut channels, &mut total_blocks) } != 0 {
            log::error!("[AUDIO] Native Monkey's Audio SDK failed to get info: {:?}", path.file_name());
            unsafe { monkey_decoder_close(decoder_ptr) };
            return None;
        }

        log::info!("[AUDIO] Native APE Info: Rate={}, Bits={}, Chan={}, Blocks={}", 
            sample_rate, bits_per_sample, channels, total_blocks);

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
                log::warn!("[AUDIO] Native APE seek failed to block {} for {:?}", target_block, path.file_name());
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
            monkey_decoder_decode_blocks(self.decoder, raw_buffer.as_mut_ptr(), BLOCKS_TO_DECODE, &mut blocks_retrieved)
        };

        #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
        let ret = { let _ = raw_buffer; -1 };

        if ret != 0 || blocks_retrieved == 0 {
            return false;
        }

        self.buffer.clear();
        self.buffer_pos = 0;
        self.current_block += blocks_retrieved as i64;

        let bits = self.bits_per_sample;
        let bytes_per_sample = (bits / 8) as usize;
        
        for chunk in raw_buffer[..blocks_retrieved as usize * bytes_per_block].chunks_exact(bytes_per_sample) {
            let sample = match bits {
                8 => (chunk[0] as i8 as f32) / NORM_I8,
                16 => (i16::from_le_bytes([chunk[0], chunk[1]]) as f32) / NORM_I16,
                24 => {
                    let val = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], if chunk[2] & 0x80 != 0 { 0xFF } else { 0x00 }]);
                    val as f32 / NORM_I24
                },
                32 => (i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]) as f32) / NORM_I32,
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
            Some(Duration::from_secs_f64(self.total_blocks as f64 / self.sample_rate as f64))
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
        let sample_rate = track.codec_params.sample_rate.unwrap_or(DEFAULT_SAMPLE_RATE);
        let channels = track.codec_params.channels.map(|c| c.count() as u16).unwrap_or(DEFAULT_CHANNELS);
        
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
                        AudioBufferRef::F32(ref buf) => Self::push_interleaved_to_vec(&mut self.buffer, buf),
                        AudioBufferRef::U8(ref buf) => Self::push_interleaved_to_vec(&mut self.buffer, buf),
                        AudioBufferRef::U16(ref buf) => Self::push_interleaved_to_vec(&mut self.buffer, buf),
                        AudioBufferRef::U24(ref buf) => Self::push_interleaved_to_vec(&mut self.buffer, buf),
                        AudioBufferRef::U32(ref buf) => Self::push_interleaved_to_vec(&mut self.buffer, buf),
                        AudioBufferRef::S8(ref buf) => Self::push_interleaved_to_vec(&mut self.buffer, buf),
                        AudioBufferRef::S16(ref buf) => Self::push_interleaved_to_vec(&mut self.buffer, buf),
                        AudioBufferRef::S24(ref buf) => Self::push_interleaved_to_vec(&mut self.buffer, buf),
                        AudioBufferRef::S32(ref buf) => Self::push_interleaved_to_vec(&mut self.buffer, buf),
                        AudioBufferRef::F64(ref buf) => Self::push_interleaved_to_vec(&mut self.buffer, buf),
                    }
                    return true;
                }
                Err(SymphoniaError::IoError(_)) => return false,
                Err(SymphoniaError::DecodeError(_)) => continue,
                Err(_) => return false,
            }
        }
    }

    fn push_interleaved_to_vec<S: symphonia::core::sample::Sample>(target: &mut Vec<f32>, buf: &AudioBuffer<S>) 
    where f32: symphonia::core::conv::FromSample<S> {
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
    where S: rodio::Source<Item = f32> + Send + 'static 
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
                    if thread_local_shutdown.load(Ordering::Relaxed) || thread_global_shutdown.load(Ordering::Relaxed) {
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
        std::num::NonZero::new(self.channels).unwrap_or(std::num::NonZero::new(DEFAULT_CHANNELS).unwrap())
    }

    fn sample_rate(&self) -> std::num::NonZero<u32> {
        std::num::NonZero::new(self.sample_rate).unwrap_or(std::num::NonZero::new(DEFAULT_SAMPLE_RATE).unwrap())
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
    let tag = tagged_file.primary_tag().or_else(|| tagged_file.first_tag())?;

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
            log::error!("[AUDIO] Failed to create native ApeSource for {:?}", path.file_name());
        }
        source.map(|s| Box::new(BufferedSource::new(s, shutdown_flag)) as Box<dyn rodio::Source<Item = f32> + Send>)
    } else {
        // Use our high-performance SymphoniaSource for all other formats
        SymphoniaSource::new_with_offset(path, Arc::clone(&shutdown_flag), offset)
            .map(|s| Box::new(BufferedSource::new(s, shutdown_flag)) as Box<dyn rodio::Source<Item = f32> + Send>)
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
    let mut backend_sink = None;
    let mut backend_player: Option<rodio::Player> = None;
    let mut last_backend_attempt: Option<Instant> = None;

    // Initialize the WASAPI session listener (Windows only)
    unsafe { wasapi_monitor_init(); }

    macro_rules! ensure_backend {
        ($v:expr, $p:expr) => {{
            let mut res = true;
            if backend_sink.is_none() || backend_player.is_none() {
                let can_retry = last_backend_attempt.map_or(true, |l| l.elapsed() >= AUDIO_RECOVERY_COOLDOWN);
                if can_retry {
                    last_backend_attempt = Some(Instant::now());

                    if !unsafe { wasapi_is_device_available() } {
                        log::debug!("[RECOVERY] ensure_backend: Hardware is busy, skipping attempt");
                        res = false;
                    } else {
                        // Support custom device selection
                        let selected_device = device_slot.lock().unwrap().clone();
                        let sink_result = if let Some(ref name) = selected_device {
                             use ::rodio::cpal::traits::{HostTrait, DeviceTrait};
                             ::rodio::cpal::default_host().output_devices()
                                .ok()
                                .and_then(|mut ds| ds.find(|d| d.description().ok().map(|desc| {
                                     let n = desc.name();
                                     if let Some(drv) = desc.driver() {
                                         format!("{} ({})", n, drv)
                                     } else {
                                         n.to_string()
                                     }
                                 }).as_ref() == Some(name)))
                                .map(|d| rodio::DeviceSinkBuilder::from_device(d).and_then(|b| b.open_stream()))
                                .unwrap_or_else(|| Ok(rodio::DeviceSinkBuilder::open_default_sink()?))
                        } else {
                            rodio::DeviceSinkBuilder::open_default_sink()
                        };

                        match sink_result {
                            Ok(sink) => {
                                let p = rodio::Player::connect_new(sink.mixer());
                                p.set_volume($v);
                                if $p { p.pause(); } else { p.play(); }
                                backend_sink = Some(sink);
                                backend_player = Some(p);
                                log::debug!("[RECOVERY] ensure_backend: device opened successfully");
                                res = true;
                            }
                            Err(e) => {
                                let msg = format!("Audio device error: {e}");
                                log::warn!("{msg}");
                                set_error(&err_slot, msg);
                                res = false;
                            }
                        }
                    }
                } else {
                    res = false;
                }
            }
            res
        }};
    }

    let mut playlist: Vec<PathBuf> = Vec::new();
    let mut current_track_idx: usize = 0;
    let mut stopped = true;
    let mut last_hw_pos: Duration = Duration::ZERO;
    let mut paused = false;
    let mut current_volume: f32 = 1.0;

    let mut cue_sheet: Option<CueSheet> = None;
    let mut current_file_start: Instant = Instant::now();
    let mut last_seek_offset: Duration = Duration::ZERO;
    let mut paused_at: Option<Instant> = None;
    let mut total_paused: Duration = Duration::ZERO;
    let mut pending_start_track_idx: Option<usize> = None;

    loop {
        let timeout = if !stopped && !paused { Duration::from_millis(100) } else { Duration::from_secs(1) };
        let cmd = rx.recv_timeout(timeout);

        match cmd {
            Ok(AudioCommand::Shutdown) => {
                backend_player.take().map(|p| p.stop());
                backend_sink.take();
                set_current_track(&track_slot, None);
                set_metadata(&meta_slot, None);
                unsafe { wasapi_monitor_uninit(); }
                return;
            }
            Ok(AudioCommand::Stop) => {
                stopped = true;
                playlist.clear();
                backend_player = None;
                backend_sink = None;
                set_current_track(&track_slot, None);
                set_metadata(&meta_slot, None);
                tracks_flag.store(false, Ordering::Relaxed);
                set_cue_markers(&cue_markers_slot, Vec::new());
                cue_sheet = None;
                set_cue_track(&cue_track_slot, None);
            }
            Ok(AudioCommand::Pause) => {
                paused = true;
                paused_at = Some(Instant::now());
                if let Some(ref p) = backend_player {
                    p.pause();
                }
            }
            Ok(AudioCommand::Play) => {
                if paused {
                    paused = false;
                    if let Some(pa) = paused_at.take() {
                        total_paused += pa.elapsed();
                    }
                }
                if stopped && !playlist.is_empty() {
                    stopped = false;
                    let resume_pos = last_hw_pos.saturating_add(last_seek_offset);
                    backend_player = None;
                    backend_sink = None;
                    if current_track_idx > 0 { current_track_idx -= 1; }
                    let path = playlist[current_track_idx % playlist.len()].clone();
                    if let Ok(file) = fs::File::open(&path) {
                        let reader = std::io::BufReader::with_capacity(AUDIO_BUFFER_CAPACITY, file);
                        if let Some(source) = create_source(&path, reader, Arc::clone(&shutdown_flag), resume_pos) {
                            if ensure_backend!(current_volume, paused) {
                                if cue_sheet.is_none() || dur_ms.load(Ordering::Relaxed) == 0 {
                                    dur_ms.store(source.total_duration().map(|d| d.as_millis() as u64).unwrap_or(0), Ordering::Relaxed);
                                }
                                if let Some(ref p) = backend_player {
                                    p.append(source);
                                    last_seek_offset = resume_pos;
                                    p.play();
                                    current_track_idx += 1;
                                    last_hw_pos = Duration::ZERO;
                                    current_file_start = Instant::now();
                                    total_paused = Duration::ZERO;
                                }
                            }
                        }
                    }
                } else if let Some(ref p) = backend_player {
                    p.play();
                }
            }
            Ok(AudioCommand::Seek(pos)) => {
                if !playlist.is_empty() {
                    let path_idx = current_track_idx.saturating_sub(1) % playlist.len();
                    let path = playlist[path_idx].clone();
                    if let Ok(file) = fs::File::open(&path) {
                        let reader = std::io::BufReader::with_capacity(AUDIO_BUFFER_CAPACITY, file);
                        if let Some(source) = create_source(&path, reader, Arc::clone(&shutdown_flag), pos) {
                            // Ensure backend exists but DON'T let it restart the sink if unnecessary
                            if ensure_backend!(current_volume, paused) {
                                if let Some(ref p) = backend_player {
                                    // Clear ONLY after we have a valid source ready to append.
                                    // This prevents the player from being momentarily empty,
                                    // which would trigger false "end of playlist" detection.
                                    p.clear();
                                    let total_dur = source.total_duration().map(|d| d.as_millis() as u64).unwrap_or(0);
                                    p.append(source);
                                    if cue_sheet.is_none() || dur_ms.load(Ordering::Relaxed) == 0 {
                                        dur_ms.store(total_dur, Ordering::Relaxed);
                                    }
                                    last_seek_offset = pos;
                                    last_hw_pos = Duration::ZERO;
                                    current_file_start = Instant::now();
                                    total_paused = Duration::ZERO;
                                    if !paused { p.play(); }
                                    log::debug!("[AUDIO] Seek to {} ms successful", pos.as_millis());
                                }
                            }
                        }
                    } else {
                        log::error!("[AUDIO] Failed to re-open file for seek: {:?}", path);
                    }
                }
            }
            Ok(AudioCommand::SetDevice(_)) => {
                // Seamless switching: Capture current position before tearing down
                let resume_pos = if let Some(ref p) = backend_player {
                    p.get_pos().saturating_add(last_seek_offset)
                } else {
                    last_seek_offset
                };

                backend_sink = None;
                backend_player = None;

                // If we were playing, immediately trigger a restart on the new device
                if !playlist.is_empty() && !stopped {
                    let path_idx = current_track_idx.saturating_sub(1) % playlist.len();
                    let path = playlist[path_idx].clone();
                    if let Ok(file) = fs::File::open(&path) {
                        let reader = std::io::BufReader::with_capacity(AUDIO_BUFFER_CAPACITY, file);
                        if let Some(source) = create_source(&path, reader, Arc::clone(&shutdown_flag), resume_pos) {
                            if ensure_backend!(current_volume, paused) {
                                if let Some(ref p) = backend_player {
                                    p.append(source);
                                    last_seek_offset = resume_pos;
                                    last_hw_pos = Duration::ZERO;
                                    current_file_start = Instant::now();
                                    total_paused = Duration::ZERO;
                                    if !paused { p.play(); }
                                    log::info!("[AUDIO] Device switched, playback resumed at {}ms", resume_pos.as_millis());
                                }
                            }
                        }
                    }
                }
            }
            Ok(AudioCommand::NextFile) => {
                if let Some(ref p) = backend_player {
                    p.clear();
                }
                cue_sheet = None;
                set_cue_track(&cue_track_slot, None);
                tracks_flag.store(false, Ordering::Relaxed);
            }
            Ok(AudioCommand::PrevFile) => {
                if current_track_idx > 1 {
                    current_track_idx -= 2;
                } else {
                    current_track_idx = playlist.len().saturating_sub(1);
                }
                if let Some(ref p) = backend_player {
                    p.clear();
                }
                cue_sheet = None;
                set_cue_track(&cue_track_slot, None);
                tracks_flag.store(false, Ordering::Relaxed);
            }
            Ok(AudioCommand::NextTrack) => {
                if let Some(ref cue) = cue_sheet {
                    let elapsed = current_file_start.elapsed()
                        .saturating_sub(total_paused)
                        .saturating_add(last_seek_offset);
                    let current_idx = cue.tracks.iter().position(|t| t.start > elapsed).unwrap_or(cue.tracks.len()).saturating_sub(1);
                    if current_idx + 1 < cue.tracks.len() {
                        let next_t = &cue.tracks[current_idx + 1];
                                if let Some(path) = playlist.get(current_track_idx.saturating_sub(1)) {
                                    if let Ok(file) = std::fs::File::open(path) {
                                        let reader = std::io::BufReader::with_capacity(AUDIO_BUFFER_CAPACITY, file);
                                        if let Some(source) = create_source(path, reader, Arc::clone(&shutdown_flag), next_t.start) {
                                            if ensure_backend!(current_volume, paused) {
                                                if let Some(ref p) = backend_player {
                                                    p.clear();
                                                    let total_dur = source.total_duration();
                                                    p.append(source);
                                                    p.play();
                                                    if cue_sheet.is_none() {
                                                        let dur = cue.tracks.get(current_idx + 2).map(|t| t.start).unwrap_or_else(|| total_dur.unwrap_or(Duration::ZERO)).saturating_sub(next_t.start);
                                                        dur_ms.store(dur.as_millis() as u64, Ordering::Relaxed);
                                                    }
                                                }
                                            }
                                    last_seek_offset = next_t.start;
                                    current_file_start = Instant::now();
                                    total_paused = Duration::ZERO;
                                    paused_at = None;
                                    let meta = format!("{}. {} - {}", next_t.number, next_t.title, next_t.performer);
                                    set_metadata(&meta_slot, Some(meta));
                                    set_cue_track(&cue_track_slot, Some(current_idx + 1));
                                }
                            }
                        }
                    } else if let Some(ref p) = backend_player {
                        p.clear();
                    }
                }
            }
            Ok(AudioCommand::PrevTrack) => {
                if let Some(ref cue) = cue_sheet {
                    let elapsed = current_file_start.elapsed()
                        .saturating_sub(total_paused)
                        .saturating_add(last_seek_offset);
                    // Determine which CUE track is currently playing
                    let current_idx = cue.tracks.iter().position(|t| t.start > elapsed).unwrap_or(cue.tracks.len()).saturating_sub(1);
                    let current_t = &cue.tracks[current_idx];

                    // Car-audio style: compute time elapsed within the current CUE track.
                    // If >3s into the track, restart from _this_ track's beginning;
                    // if ≤3s (or at track 0), jump to the previous track.
                    let time_in_track = elapsed.saturating_sub(current_t.start);
                    let target_idx = if time_in_track > Duration::from_secs(3) || current_idx == 0 {
                        current_idx // Restart current track
                    } else {
                        current_idx - 1 // Jump to previous track
                    };
                    let target_t = &cue.tracks[target_idx];

                    if let Some(path) = playlist.get(current_track_idx.saturating_sub(1)) {
                        if let Ok(file) = std::fs::File::open(path) {
                            let reader = std::io::BufReader::with_capacity(AUDIO_BUFFER_CAPACITY, file);
                            if let Some(source) = create_source(path, reader, Arc::clone(&shutdown_flag), target_t.start) {
                                if ensure_backend!(current_volume, paused) {
                                    if let Some(ref p) = backend_player {
                                        p.clear(); // Must clear before appending to jump immediately
                                        let total_dur = source.total_duration();
                                        p.append(source);
                                        p.play();
                                        if cue_sheet.is_none() {
                                            let dur = cue.tracks.get(target_idx + 1).map(|t| t.start).unwrap_or_else(|| total_dur.unwrap_or(Duration::ZERO)).saturating_sub(target_t.start);
                                            dur_ms.store(dur.as_millis() as u64, Ordering::Relaxed);
                                        }
                                    }
                                }
                                last_seek_offset = target_t.start;
                                current_file_start = Instant::now();
                                total_paused = Duration::ZERO;
                                paused_at = None;
                                let meta = format!("{}. {} - {}", target_t.number, target_t.title, target_t.performer);
                                set_metadata(&meta_slot, Some(meta));
                                set_cue_track(&cue_track_slot, Some(target_idx));
                            }
                        }
                    }
                }
            }
            Ok(AudioCommand::SetPlaylist(new_list, start_idx, start_track_idx, initial_paused)) => {
                playlist = new_list;
                current_track_idx = start_idx.unwrap_or(0);
                pending_start_track_idx = start_track_idx;
                stopped = false;
                paused = initial_paused;
                if initial_paused {
                    paused_at = Some(Instant::now());
                } else {
                    paused_at = None;
                    total_paused = Duration::ZERO;
                }
                
                if ensure_backend!(current_volume, paused) {
                    dur_ms.store(0, Ordering::Relaxed); // Will be updated by FEED when loading first file
                    if let Some(ref p) = backend_player {
                        p.clear();
                        if paused { p.pause(); } else { p.play(); }
                    }
                }
                set_current_path(&path_slot, None);
                set_metadata(&meta_slot, None);
                set_cue_track(&cue_track_slot, start_track_idx);
            }
            Ok(AudioCommand::SetVolume(v)) => {
                current_volume = v;
                if let Some(ref p) = backend_player {
                    p.set_volume(v);
                }
            }
            Err(_) => {}
        }

        #[cfg(windows)]
        {
            if unsafe { wasapi_poll_device_lost() } {
                log::warn!("[WATCHDOG] Audio device lost (native event). Dropping backend for orphan recovery.");
                // Do NOT set stopped=true — that would clear pos/dur and trigger a full restart.
                // Instead, just drop the backend. The "ORPHANED STATE" branch (below) will
                // automatically rebuild the backend at the last known position once the device
                // becomes available again.
                backend_player = None;
                backend_sink = None;
            }
        }

        if !stopped && !playlist.is_empty() {
            let player_exists = backend_player.is_some();
            let player_empty = backend_player.as_ref().map_or(false, |p| p.empty());

            if player_empty {
                // Natural end of track - proceed to next
                if current_track_idx < playlist.len() {
                    let path = playlist[current_track_idx].clone();
                    let filename = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                    if let Ok(file) = fs::File::open(&path) {
                        let reader = std::io::BufReader::with_capacity(AUDIO_BUFFER_CAPACITY, file);
                        if let Some(source) = create_source(&path, reader, Arc::clone(&shutdown_flag), Duration::ZERO) {
                            set_current_track(&track_slot, Some(filename));
                            set_current_path(&path_slot, Some(path.clone()));
                            cue_sheet = load_cue(&path, &shutdown_flag);
                            tracks_flag.store(cue_sheet.is_some(), Ordering::Relaxed);

                            if let Some(ref cue) = cue_sheet {
                                // If we have a pending start track, use it for metadata immediately.
                                // Otherwise default to the first track.
                                let initial_track_idx = pending_start_track_idx.unwrap_or(0);
                                if let Some(t) = cue.tracks.get(initial_track_idx) {
                                    let meta = format!("{}. {} - {}", t.number, t.title, t.performer);
                                    set_metadata(&meta_slot, Some(meta));
                                    set_cue_track(&cue_track_slot, Some(initial_track_idx));
                                }
                                let markers: Vec<u64> = cue.tracks.iter().map(|t| t.start.as_millis() as u64).collect();
                                set_cue_markers(&cue_markers_slot, markers);
                            } else if let Some(meta) = get_file_metadata(&path) {
                                set_metadata(&meta_slot, Some(meta));
                                set_cue_markers(&cue_markers_slot, Vec::new());
                            } else {
                                set_metadata(&meta_slot, None);
                                set_cue_markers(&cue_markers_slot, Vec::new());
                            }

                            if ensure_backend!(current_volume, paused) {
                                if let Some(ref p) = backend_player {
                                    let total_dur = source.total_duration().map(|d| d.as_millis() as u64).unwrap_or(0);
                                    p.append(source);
                                    if cue_sheet.is_none() || dur_ms.load(Ordering::Relaxed) == 0 {
                                        dur_ms.store(total_dur, Ordering::Relaxed);
                                    }
                                    current_track_idx += 1;
                                    last_seek_offset = Duration::ZERO;
                                    last_hw_pos = Duration::ZERO;
                                    current_file_start = Instant::now();
                                    total_paused = Duration::ZERO;
                                    if paused {
                                        paused_at = Some(Instant::now());
                                    }
                                }
                            } else {
                                continue;
                            }

                            if let (Some(track_idx), Some(cue)) = (pending_start_track_idx.take(), &cue_sheet) {
                                if track_idx < cue.tracks.len() {
                                    let t = &cue.tracks[track_idx];
                                    // Even if t.start is 0, we still want to go through the seek logic
                                    // if it's not the first load (though here it IS the first load).
                                    // The important part is that we already set the metadata above.
                                    if t.start > Duration::ZERO {
                                    if let Ok(f2) = std::fs::File::open(&path) {
                                        let r2 = std::io::BufReader::with_capacity(AUDIO_BUFFER_CAPACITY, f2);
                                        if let Some(s2) = create_source(&path, r2, Arc::clone(&shutdown_flag), t.start) {
                                            if ensure_backend!(current_volume, paused) {
                                                if let Some(ref p) = backend_player {
                                                    p.clear();
                                                    let _total_dur = s2.total_duration();
                                                    p.append(s2);
                                                    last_seek_offset = t.start;
                                                    current_file_start = Instant::now();
                                                    let meta = format!("{}. {} - {}", t.number, t.title, t.performer);
                                                    set_metadata(&meta_slot, Some(meta));
                                                    set_cue_track(&cue_track_slot, Some(track_idx));
                                                }
                                            }
                                        }
                                    }
                                    }
                                }
                            }

                            if let Some(ref p) = backend_player {
                                p.set_volume(current_volume);
                                if paused { p.pause(); } else { p.play(); }
                            }
                        }
                    }
                } else {
                    stopped = true;
                    set_current_track(&track_slot, None);
                    set_current_path(&path_slot, None);
                    set_metadata(&meta_slot, None);
                    tracks_flag.store(false, Ordering::Relaxed);
                    set_cue_markers(&cue_markers_slot, Vec::new());
                }
            } else if !player_exists {
                // ORPHANED STATE: !stopped but no backend_player.
                // This happens during device switch recovery or hardware lost.
                if ensure_backend!(current_volume, paused) {
                    // Try to restore current track at last known position
                    let path_idx = current_track_idx.saturating_sub(1) % playlist.len();
                    let path = playlist[path_idx].clone();
                    if let Ok(file) = fs::File::open(&path) {
                        let reader = std::io::BufReader::with_capacity(AUDIO_BUFFER_CAPACITY, file);
                        let resume_pos = last_hw_pos.saturating_add(last_seek_offset);
                        if let Some(source) = create_source(&path, reader, Arc::clone(&shutdown_flag), resume_pos) {
                            if let Some(ref p) = backend_player {
                                p.append(source);
                                last_seek_offset = resume_pos;
                                last_hw_pos = Duration::ZERO;
                                current_file_start = Instant::now();
                                total_paused = Duration::ZERO;
                                if !paused { p.play(); }
                                log::info!("[AUDIO] Auto-recovered playback at {}ms", resume_pos.as_millis());
                            }
                        }
                    }
                }
            }
        }

        if !stopped && !paused && backend_player.as_ref().map_or(false, |p| !p.empty()) {
            if shutdown_flag.load(Ordering::Relaxed) { return; }
            if let Some(ref cue) = cue_sheet {
                if let Some(ref p) = backend_player {
                    let elapsed = p.get_pos().saturating_add(last_seek_offset);
                    let idx = cue.tracks.iter().position(|t| t.start > elapsed).unwrap_or(cue.tracks.len()).saturating_sub(1);
                    let current_t = &cue.tracks[idx];
                    let meta = format!("{}. {} - {}", current_t.number, current_t.title, current_t.performer);
                    if let Ok(mut g) = meta_slot.try_lock() {
                        if g.as_ref() != Some(&meta) {
                            *g = Some(meta);
                            set_cue_track(&cue_track_slot, Some(idx));
                        }
                    }
                }
            } else {
                set_cue_track(&cue_track_slot, None);
            }
        }

        if stopped {
            pos_ms.store(0, Ordering::Relaxed);
            dur_ms.store(0, Ordering::Relaxed);
        } else if let Some(ref p) = backend_player {
            let hw_pos = p.get_pos();
            last_hw_pos = hw_pos;
            
            // 1. Calculate the raw mixer position in the file
            let raw_abs_pos = hw_pos.saturating_add(last_seek_offset);
            
            // 2. COMPENSATE for the decoding buffer latency (approx 0.7s - 1.5s).
            // Since we can't easily poll the Boxed source, and the mixer is ahead,
            // we use the stored buffer info if available or a consistent approach.
            // For now, we report the mixer position, but ensure it's absolute so it matches ticks.
            
            // 3. Update the global position for the UI
            // In CD mode (as seen in screenshot), the slider needs to be absolute
            // to align with the blue CUE track tick.
            pos_ms.store(raw_abs_pos.as_millis() as u64, Ordering::Relaxed);
        }
    }
}
