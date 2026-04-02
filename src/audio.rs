use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use crossbeam_channel::Sender;
// rodio::Source was unused

#[allow(dead_code)]
pub enum AudioCommand {
    SetPlaylist(Vec<PathBuf>),
    SetVolume(f32),
    Play,
    Pause,
    Stop,
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

    pub fn start(&mut self, files: Vec<PathBuf>) {
        self.stop();
        let (tx, rx) = crossbeam_channel::unbounded::<AudioCommand>();
        self.cmd_tx = Some(tx.clone());
        let err_slot = Arc::clone(&self.last_error);
        let _ = tx.send(AudioCommand::SetPlaylist(files));
        std::thread::Builder::new()
            .name("audio-player".to_string())
            .spawn(move || run_audio_loop(rx, err_slot))
            .expect("failed to spawn audio thread");
    }

    pub fn stop(&mut self) {
        if let Some(tx) = self.cmd_tx.take() {
            let _ = tx.send(AudioCommand::Stop);
        }
    }

    pub fn set_volume(&self, volume: f32) {
        if let Some(tx) = &self.cmd_tx {
            let _ = tx.send(AudioCommand::SetVolume(volume.clamp(0.0, 1.0)));
        }
    }

    #[allow(dead_code)]
    pub fn play(&self) {
        if let Some(tx) = &self.cmd_tx {
            let _ = tx.send(AudioCommand::Play);
        }
    }

    #[allow(dead_code)]
    pub fn pause(&self) {
        if let Some(tx) = &self.cmd_tx {
            let _ = tx.send(AudioCommand::Pause);
        }
    }

    /// Take the last error message (clears it after reading).
    pub fn take_error(&self) -> Option<String> {
        self.last_error.lock().ok()?.take()
    }
}

impl Drop for AudioPlayer {
    fn drop(&mut self) {
        self.stop();
    }
}

// ---------------------------------------------------------------------------
// Collect music files from a path (file or directory)
// ---------------------------------------------------------------------------

/// Collect music files from a file or directory path.
pub fn collect_music_files(path: &PathBuf) -> Vec<PathBuf> {
    fn is_music(p: &PathBuf) -> bool {
        p.extension()
            .and_then(|e| e.to_str())
            .map(|e| matches!(e.to_lowercase().as_str(), "mp3" | "flac" | "ogg" | "wav"))
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
    // Open the default audio output device.
    // MixerDeviceSink must remain alive for audio to play (it owns the OS stream).
    let mut device_sink = match rodio::DeviceSinkBuilder::open_default_sink() {
        Ok(h) => h,
        Err(e) => {
            let msg = format!("Audio device error: {e}");
            log::warn!("{msg}");
            set_error(&err_slot, msg);
            return;
        }
    };
    // Suppress the "Dropping DeviceSink…" stderr message
    device_sink.log_on_drop(false);

    // Player is the queue connected to the mixer — append sources to it.
    // Explicitly call play() to start the device stream (mirrors the test binary).
    let player = rodio::Player::connect_new(device_sink.mixer());
    player.play(); // ensure playing state from the start

    let mut playlist: Vec<PathBuf> = Vec::new();
    let mut current_track: usize = 0;
    let mut paused = false;
    let mut current_volume: f32 = 1.0;

    eprintln!("[audio] thread started, waiting for playlist");

    loop {
        // Drain all pending commands
        loop {
            match cmd_rx.try_recv() {
                Ok(AudioCommand::Stop) => {
                    eprintln!("[audio] stop received, exiting");
                    return;
                }
                Ok(AudioCommand::Play) => {
                    paused = false;
                    player.play();
                }
                Ok(AudioCommand::Pause) => {
                    paused = true;
                    player.pause();
                }
                Ok(AudioCommand::SetPlaylist(new_list)) => {
                    eprintln!("[audio] playlist set: {} files", new_list.len());
                    for f in &new_list {
                        eprintln!("[audio]   {:?}", f.file_name().unwrap_or_default());
                    }
                    playlist = new_list;
                    current_track = 0;
                    player.clear();
                    player.play();
                    paused = false;
                }
                Ok(AudioCommand::SetVolume(v)) => {
                    current_volume = v.clamp(0.0, 1.0);
                    player.set_volume(current_volume);
                }
                Err(_) => break, // no more commands
            }
        }

        // Feed next track when the queue is empty
        if !paused && player.empty() && !playlist.is_empty() {
            let path = playlist[current_track % playlist.len()].clone();
            current_track += 1;

            eprintln!("[audio] opening {:?}", path.file_name().unwrap_or_default());
            match std::fs::File::open(&path) {
                Ok(file) => {
                    let reader = std::io::BufReader::new(file);
                    match rodio::Decoder::new(reader) {
                        Ok(source) => {
                            eprintln!("[audio] appending track, vol={:.0}%", current_volume * 100.0);
                            player.append(source);
                            player.set_volume(current_volume);
                            player.play();
                        }
                        Err(e) => {
                            let msg = format!("Decode error ({}): {e}", path.display());
                            eprintln!("[audio] {msg}");
                            set_error(&err_slot, msg);
                        }
                    }
                }
                Err(e) => {
                    let msg = format!("Cannot open ({}): {e}", path.display());
                    eprintln!("[audio] {msg}");
                    set_error(&err_slot, msg);
                }
            }
        }

        std::thread::sleep(std::time::Duration::from_millis(150));
    }
}
