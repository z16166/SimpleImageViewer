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
use std::sync::atomic::{AtomicBool, Ordering};

use std::fs;
use std::path::Path;
use std::time::Duration;

// ---------------------------------------------------------------------------
// CUE Support
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub(crate) struct CueTrack {
    pub(crate) number: u32,
    pub(crate) title: String,
    pub(crate) performer: String,
    pub(crate) start: Duration,
}

pub(crate) struct CueSheet {
    pub(crate) tracks: Vec<CueTrack>,
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

pub(crate) fn read_text_file_with_fallback(path: &Path) -> Option<String> {
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

pub(crate) fn load_cue(audio_path: &Path, shutdown_flag: &AtomicBool) -> Option<CueSheet> {
    // 1. Direct match
    let cue_path = audio_path.with_extension("cue");
    if cue_path.exists() {
        return parse_cue_file(&cue_path);
    }

    if shutdown_flag.load(Ordering::Relaxed) {
        return None;
    }

    // 2. Pattern replacement (e.g. (APE).ape -> (CUE).cue)
    if let Some(filename) = audio_path.file_name().and_then(|n| n.to_str())
        && filename.contains("(APE)")
    {
        let new_filename = filename.replace("(APE)", "(CUE)");
        let alt_cue_path = audio_path
            .with_file_name(new_filename)
            .with_extension("cue");
        if alt_cue_path.exists() {
            log::debug!("Found CUE by pattern replacement: {:?}", alt_cue_path);
            return parse_cue_file(&alt_cue_path);
        }
    }

    if shutdown_flag.load(Ordering::Relaxed) {
        return None;
    }

    // 3. Directory scan and fuzzy matching
    if let Some(parent) = audio_path.parent()
        && let Ok(entries) = fs::read_dir(parent)
    {
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

    None
}
