use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use crossbeam_channel::Sender;
use std::time::Duration;

#[allow(dead_code)]
pub enum AudioCommand {
    SetPlaylist(Vec<PathBuf>),
    SetVolume(f32),
    Play,
    Pause,
    Stop,   // Clears playlist and stops playback, but keeps thread alive
    Shutdown, // Terminates the thread
}

/// Shared error slot: audio thread writes here, UI thread reads and clears it.
pub type AudioError = Arc<Mutex<Option<String>>>;

pub struct AudioPlayer {
    cmd_tx: Option<Sender<AudioCommand>>,
    pub last_error: AudioError,
}

impl AudioPlayer {
    pub fn new() -> Self {
        Self {
            cmd_tx: None,
            last_error: Arc::new(Mutex::new(None)),
        }
    }

    /// Ensure the audio thread is running and playing the selected files.
    pub fn start(&mut self, files: Vec<PathBuf>) {
        self.ensure_thread_started();
        if let Some(tx) = &self.cmd_tx {
            let _ = tx.send(AudioCommand::SetPlaylist(files));
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

    pub fn take_error(&self) -> Option<String> {
        self.last_error.lock().ok()?.take()
    }

    fn ensure_thread_started(&mut self) {
        if self.cmd_tx.is_none() {
            let (tx, rx) = crossbeam_channel::unbounded::<AudioCommand>();
            self.cmd_tx = Some(tx);
            let err_slot = Arc::clone(&self.last_error);
            
            std::thread::Builder::new()
                .name("audio-player".to_string())
                .spawn(move || run_audio_loop(rx, err_slot))
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
// Collect music files
// ---------------------------------------------------------------------------

pub fn collect_music_files(path: &PathBuf) -> Vec<PathBuf> {
    fn is_music(p: &PathBuf) -> bool {
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
        let mut collected: Vec<PathBuf> = walkdir::WalkDir::new(path)
            .into_iter()
            .flatten()
            .map(|e| e.path().to_path_buf())
            .filter(|p| p.is_file() && is_music(p))
            .collect();
        collected.sort();
        files = collected;
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

fn run_audio_loop(
    cmd_rx: crossbeam_channel::Receiver<AudioCommand>,
    err_slot: AudioError,
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
    let mut current_track: usize = 0;
    let mut stopped = true;
    let mut paused = false;
    let mut current_volume: f32 = 1.0;

    loop {
        // Wait for a command, or timeout to check if we need to feed the next track.
        // On command: process immediately. On timeout: check player state.
        let cmd = cmd_rx.recv_timeout(Duration::from_millis(200));

        match cmd {
            Ok(AudioCommand::Shutdown) => {
                player.stop();
                return;
            }
            Ok(AudioCommand::Stop) => {
                stopped = true;
                playlist.clear();
                player.clear();
            }
            Ok(AudioCommand::Play) => {
                paused = false;
                stopped = false;
                player.play();
            }
            Ok(AudioCommand::Pause) => {
                paused = true;
                player.pause();
            }
            Ok(AudioCommand::SetPlaylist(new_list)) => {
                playlist = new_list;
                current_track = 0;
                stopped = false;
                paused = false;
                player.clear();
                player.play();
            }
            Ok(AudioCommand::SetVolume(v)) => {
                current_volume = v;
                player.set_volume(v);
            }
            Err(_) => {
                // Timeout happened (no command). 
                // Just fall through to the track-feeding logic below.
            }
        }

        // Feed next track if queue is empty
        if !stopped && !paused && player.empty() && !playlist.is_empty() {
            let path = playlist[current_track % playlist.len()].clone();
            current_track += 1;

            if let Ok(file) = std::fs::File::open(&path) {
                let reader = std::io::BufReader::new(file);
                if let Ok(source) = rodio::Decoder::new(reader) {
                    player.append(source);
                    player.set_volume(current_volume);
                    player.play();
                }
            }
        }
    }
}
