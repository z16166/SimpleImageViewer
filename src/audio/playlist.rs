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
use super::cue::read_text_file_with_fallback;

use crate::constants::is_supported_music_extension;
use crate::scanner::is_offline;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

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

pub(crate) fn build_base_non_m3u_set(base_playlist: &[PathBuf]) -> HashSet<PathBuf> {
    base_playlist
        .iter()
        .filter(|p| !is_m3u_path(p))
        .map(|p| canonical_or_clone(p))
        .collect()
}

pub(crate) fn expand_m3u_excluding_base(m3u_path: &Path, base_path_set: &HashSet<PathBuf>) -> Vec<PathBuf> {
    parse_m3u_entries(m3u_path)
        .into_iter()
        .filter(|p| !base_path_set.contains(p))
        .collect()
}

pub(crate) fn is_m3u_path(path: &Path) -> bool {
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
