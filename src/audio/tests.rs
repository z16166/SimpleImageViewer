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
        Some((
            only_in_m3u.canonicalize().expect("canonical only_in_m3u"),
            true
        ))
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
    let content = format!("{}\n{}\n", base1.to_string_lossy(), base2.to_string_lossy());
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
    fs::write(
        &m3u,
        format!("{}\n{}\n", a.to_string_lossy(), b.to_string_lossy()),
    )
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
    assert!(
        st.stopped,
        "state should stop when no playable entries remain"
    );

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
    assert_eq!(
        st.injected_playlist.pop_front(),
        Some(canonical_or_clone(&t1))
    );

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
