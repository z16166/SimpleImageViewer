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

#[allow(dead_code)]
pub enum AudioCommand {
    SetPlaylist(Vec<PathBuf>, Option<usize>),
    SetVolume(f32),
    Play,
    Pause,
    NextFile,
    PrevFile,
    NextTrack,
    PrevTrack,
    Stop,     // Clears playlist and stops playback, but keeps thread alive
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
        }
    }

    /// Ensure the audio thread is running and playing the selected files.
    pub fn start(&mut self, files: Vec<PathBuf>) {
        self.start_at(files, None);
    }

    pub fn start_at(&mut self, files: Vec<PathBuf>, start_index: Option<usize>) {
        self.ensure_thread_started();
        if let Some(tx) = &self.cmd_tx {
            let _ = tx.send(AudioCommand::SetPlaylist(files, start_index));
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
        self.last_error.lock().ok()?.take()
    }

    pub fn get_current_track(&self) -> Option<String> {
        self.current_track.lock().ok()?.clone()
    }

    pub fn get_current_track_path(&self) -> Option<PathBuf> {
        self.current_track_path.lock().ok()?.clone()
    }

    pub fn get_metadata(&self) -> Option<String> {
        self.current_metadata.lock().ok()?.clone()
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

            std::thread::Builder::new()
                .name("audio-player".to_string())
                .spawn(move || {
                    run_audio_loop(rx, err_slot, track_slot, path_slot, meta_slot, tracks_flag)
                })
                .expect("failed to spawn audio thread");
        }
    }
}

impl Drop for AudioPlayer {
    fn drop(&mut self) {
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
            .map(|e| matches!(e.to_lowercase().as_str(), "mp3" | "flac" | "ogg" | "wav" | "aac" | "m4a"))
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

fn load_cue(audio_path: &Path) -> Option<CueSheet> {
    let cue_path = audio_path.with_extension("cue");
    if !cue_path.exists() {
        return None;
    }

    let content = read_text_file_with_fallback(&cue_path)?;
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
            // Use continue instead of ? to avoid losing already-parsed tracks
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

fn run_audio_loop(
    cmd_rx: crossbeam_channel::Receiver<AudioCommand>,
    err_slot: AudioError,
    track_slot: Arc<Mutex<Option<String>>>,
    path_slot: Arc<Mutex<Option<PathBuf>>>,
    meta_slot: Arc<Mutex<Option<String>>>,
    tracks_flag: Arc<AtomicBool>,
) {
    // Open hardware ONLY ONCE per thread life. 
    // On Windows, frequent open/close can hang or crash.
    let device_sink = match rodio::DeviceSinkBuilder::open_default_sink() {
        Ok(h) => h,
        Err(e) => {
            let msg = format!("Audio device error: {e}");
            log::warn!("{msg}");
            set_error(&err_slot, msg);
            return;
        }
    };

    let player = rodio::Player::connect_new(device_sink.mixer());
    player.play();

    let mut playlist: Vec<PathBuf> = Vec::new();
    // current_track_idx = index of NEXT file to play; the currently playing file is at idx-1
    let mut current_track_idx: usize = 0;
    let mut stopped = true;
    let mut paused = false;
    let mut current_volume: f32 = 1.0;

    let mut cue_sheet: Option<CueSheet> = None;
    let mut current_file_start: Instant = Instant::now();
    let mut last_seek_offset: Duration = Duration::ZERO;
    // Pause-aware time tracking for CUE
    let mut paused_at: Option<Instant> = None;
    let mut total_paused: Duration = Duration::ZERO;

    loop {
        // Wait for a command
        let cmd = cmd_rx.recv_timeout(Duration::from_millis(200));

        match cmd {
            Ok(AudioCommand::Shutdown) => {
                player.stop();
                set_current_track(&track_slot, None);
                set_metadata(&meta_slot, None);
                return;
            }
            Ok(AudioCommand::Stop) => {
                stopped = true;
                playlist.clear();
                player.clear();
                set_current_track(&track_slot, None);
                set_metadata(&meta_slot, None);
                tracks_flag.store(false, Ordering::Relaxed);
                cue_sheet = None;
            }
            Ok(AudioCommand::Play) => {
                paused = false;
                stopped = false;
                // Accumulate paused duration for accurate CUE time tracking
                if let Some(pa) = paused_at.take() {
                    total_paused += pa.elapsed();
                }
                player.play();
            }
            Ok(AudioCommand::Pause) => {
                paused = true;
                paused_at = Some(Instant::now());
                player.pause();
            }
            Ok(AudioCommand::NextFile) => {
                player.clear();
                cue_sheet = None;
                tracks_flag.store(false, Ordering::Relaxed);
            }
            Ok(AudioCommand::PrevFile) => {
                // current_track_idx points to the NEXT file to play.
                // Subtract 2 to replay the previous file (Feed loop will +1).
                if current_track_idx > 1 {
                    current_track_idx -= 2;
                } else {
                    // Wrap around to last file
                    current_track_idx = playlist.len().saturating_sub(1);
                }
                player.clear();
                cue_sheet = None;
                tracks_flag.store(false, Ordering::Relaxed);
            }
            Ok(AudioCommand::NextTrack) => {
                if let Some(ref cue) = cue_sheet {
                    let elapsed = current_file_start.elapsed()
                        .saturating_sub(total_paused)
                        .saturating_add(last_seek_offset);
                    // Find current track index
                    let idx = cue
                        .tracks
                        .iter()
                        .position(|t| t.start > elapsed)
                        .unwrap_or(cue.tracks.len());

                    if idx < cue.tracks.len() {
                        let next_t = &cue.tracks[idx];
                        // Seek to next track
                        if let Some(path) = playlist.get(current_track_idx.saturating_sub(1)) {
                            if let Ok(file) = std::fs::File::open(path) {
                                let reader = std::io::BufReader::new(file);
                                if let Ok(source) = rodio::Decoder::new(reader) {
                                    player.clear();
                                    let source = rodio::Source::skip_duration(source, next_t.start);
                                    player.append(source);
                                    player.play();
                                    last_seek_offset = next_t.start;
                                    current_file_start = Instant::now();
                                    total_paused = Duration::ZERO;
                                    paused_at = None;
                                    let meta = format!(
                                        "{}. {} - {}",
                                        next_t.number, next_t.title, next_t.performer
                                    );
                                    set_metadata(&meta_slot, Some(meta));
                                }
                            }
                        }
                    } else {
                        // Beyond last track: go to next file
                        player.clear();
                    }
                }
            }
            Ok(AudioCommand::PrevTrack) => {
                if let Some(ref cue) = cue_sheet {
                    let elapsed = current_file_start.elapsed()
                        .saturating_sub(total_paused)
                        .saturating_add(last_seek_offset);
                    // Find current track index
                    let current_idx = cue
                        .tracks
                        .iter()
                        .position(|t| t.start > elapsed.saturating_sub(Duration::from_secs(3))) // 3s leeway
                        .unwrap_or(cue.tracks.len())
                        .saturating_sub(1);

                    let target_idx = current_idx.saturating_sub(1);
                    let target_t = &cue.tracks[target_idx];

                    if let Some(path) = playlist.get(current_track_idx.saturating_sub(1)) {
                        if let Ok(file) = std::fs::File::open(path) {
                            let reader = std::io::BufReader::new(file);
                            if let Ok(source) = rodio::Decoder::new(reader) {
                                player.clear();
                                if target_t.start > Duration::ZERO {
                                    let source = rodio::Source::skip_duration(source, target_t.start);
                                    player.append(source);
                                } else {
                                    player.append(source);
                                }
                                player.play();
                                last_seek_offset = target_t.start;
                                current_file_start = Instant::now();
                                total_paused = Duration::ZERO;
                                paused_at = None;
                                let meta = format!(
                                    "{}. {} - {}",
                                    target_t.number, target_t.title, target_t.performer
                                );
                                set_metadata(&meta_slot, Some(meta));
                            }
                        }
                    }
                }
            }
            Ok(AudioCommand::SetPlaylist(new_list, start_index)) => {
                playlist = new_list;
                current_track_idx = start_index.unwrap_or(0);
                stopped = false;
                player.clear();
                if paused {
                    player.pause();
                } else {
                    player.play();
                }
                set_current_track(&track_slot, None);
                set_current_path(&path_slot, None);
                set_metadata(&meta_slot, None);
            }
            Ok(AudioCommand::SetVolume(v)) => {
                current_volume = v;
                player.set_volume(v);
            }
            Err(_) => {}
        }

        // Feed next track
        if !stopped && !paused && player.empty() && !playlist.is_empty() {
            let path = playlist[current_track_idx % playlist.len()].clone();
            let filename = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Unknown".to_string());

            current_track_idx += 1;

            if let Ok(file) = std::fs::File::open(&path) {
                let reader = std::io::BufReader::new(file);
                if let Ok(source) = rodio::Decoder::new(reader) {
                    set_current_track(&track_slot, Some(filename));
                    set_current_path(&path_slot, Some(path.clone()));

                    // Check CUE
                    cue_sheet = load_cue(&path);
                    tracks_flag.store(cue_sheet.is_some(), Ordering::Relaxed);

                    // Metadata
                    if let Some(ref cue) = cue_sheet {
                        if let Some(first) = cue.tracks.first() {
                            let meta =
                                format!("{}. {} - {}", first.number, first.title, first.performer);
                            set_metadata(&meta_slot, Some(meta));
                        }
                    } else if let Some(meta) = get_file_metadata(&path) {
                        set_metadata(&meta_slot, Some(meta));
                    } else {
                        set_metadata(&meta_slot, None);
                    }

                    player.append(source);
                    player.set_volume(current_volume);
                    player.play();
                    current_file_start = Instant::now();
                    last_seek_offset = Duration::ZERO;
                    total_paused = Duration::ZERO;
                    paused_at = None;
                }
            }
        }

        // Handle mid-file metadata updates for CUE
        if !stopped && !paused && !player.empty() {
            if let Some(ref cue) = cue_sheet {
                let elapsed = current_file_start.elapsed()
                    .saturating_sub(total_paused)
                    .saturating_add(last_seek_offset);
                // What track are we in?
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
                // Only update if changed to avoid expensive locking
                if let Ok(g) = meta_slot.lock() {
                    if g.as_ref() != Some(&meta) {
                        drop(g);
                        set_metadata(&meta_slot, Some(meta));
                    }
                }
            }
        }
    }
}
