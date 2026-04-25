use crate::app::ImageViewerApp;
use crate::audio::collect_music_files;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

impl ImageViewerApp {
    pub(crate) fn process_music_scan_results(&mut self) {
        if let Some(ref rx) = self.music_scan_rx {
            if let Ok(files) = rx.try_recv() {
                self.scanning_music = false;
                self.music_scan_rx = None;
                self.music_scan_cancel = None; // Thread finished or aborted

                // If it was aborted (returned empty), don't update count unless it's genuinely empty
                if !files.is_empty() {
                    self.cached_music_count = Some(files.len());

                    // Try to resume from last played track
                    let mut start_idx = None;
                    if let Some(last_path) = &self.settings.last_music_file {
                        if let Some(idx) = files.iter().position(|p| p == last_path) {
                            start_idx = Some(idx);
                        }
                    }

                    let start_track_idx = if start_idx.is_some() {
                        self.settings.last_music_cue_track
                    } else {
                        None
                    };
                    self.audio.start_at(
                        files,
                        start_idx,
                        start_track_idx,
                        self.settings.music_paused,
                    );
                    self.audio.set_volume(self.settings.volume);
                    // Reset the HUD idle timer so music controls appear immediately —
                    // scanning a large library can take longer than MUSIC_HUD_IDLE_SECONDS,
                    // so without this the HUD would remain hidden after a startup resume.
                    self.music_hud_last_activity = std::time::Instant::now();
                } else if self.music_scan_path.is_some() {
                    // Check if truly empty or just aborted
                    // Actually, if it's aborted, files will be empty.
                    // We don't want to set cached_music_count to Some(0) if it was an abort.
                }
            }
        }
    }

    pub(crate) fn open_music_file_dialog(&mut self) {
        let dialog = rfd::FileDialog::new().add_filter(
            "Music files",
            &["mp3", "flac", "ogg", "wav", "aac", "m4a", "ape"],
        );
        if let Some(path) = dialog.pick_file() {
            self.settings.music_path = Some(path.clone());
            self.restart_audio_if_enabled();
        }
    }

    pub(crate) fn open_music_dir_dialog(&mut self) {
        if let Some(dir) = rfd::FileDialog::new().pick_folder() {
            self.settings.music_path = Some(dir.clone());
            self.restart_audio_if_enabled();
        }
    }

    pub(crate) fn restart_audio_if_enabled(&mut self) {
        // If not playing music, cancel any running scan and stop audio
        if !self.settings.play_music {
            if let Some(cancel) = self.music_scan_cancel.take() {
                cancel.store(false, Ordering::Relaxed);
            }
            self.audio.stop();
            self.scanning_music = false;
            self.music_scan_rx = None;
            self.music_scan_path = None;
            return;
        }

        // We ARE playing music.
        if let Some(path) = self.settings.music_path.clone() {
            // If already scanning or loaded THIS path, don't restart scan
            if self.music_scan_path.as_ref() == Some(&path)
                && (self.scanning_music || self.cached_music_count.is_some())
            {
                return;
            }

            // Path changed or first scan: Cancel old scan if any
            if let Some(cancel) = self.music_scan_cancel.take() {
                cancel.store(false, Ordering::Relaxed);
            }
            self.audio.stop();

            self.scanning_music = true;
            self.music_scan_path = Some(path.clone());
            let cancel_signal = Arc::new(AtomicBool::new(true));
            self.music_scan_cancel = Some(Arc::clone(&cancel_signal));

            let (tx, rx) = crossbeam_channel::unbounded();
            self.music_scan_rx = Some(rx);

            // Background scan — do NOT block the UI
            std::thread::spawn(move || {
                let files = collect_music_files(&path, Some(cancel_signal));
                let _ = tx.send(files);
            });
        } else {
            // No path selected
            self.audio.stop();
            self.cached_music_count = None;
            self.music_scan_path = None;
        }
    }

    // ------------------------------------------------------------------
    // Audio: Force restart after hardware stall
    // ------------------------------------------------------------------

    /// Force a full audio restart, bypassing the "already scanned" guard.
    /// Used when the audio watchdog detects a hardware stall.
    pub(crate) fn force_restart_audio(&mut self) {
        // Stop audio and clear ALL scan state so restart_audio_if_enabled
        // doesn't short-circuit with "already scanning this path".
        if let Some(cancel) = self.music_scan_cancel.take() {
            cancel.store(false, Ordering::Relaxed);
        }
        self.audio.stop();
        self.scanning_music = false;
        self.music_scan_rx = None;
        self.music_scan_path = None;
        self.cached_music_count = None;

        // Now trigger a full restart (will re-scan and SetPlaylist)
        self.restart_audio_if_enabled();
    }

    // ------------------------------------------------------------------
    // UI: Settings panel
}
