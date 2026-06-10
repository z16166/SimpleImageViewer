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
use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::Arc;

use super::player::AudioError;

pub(crate) fn set_error(slot: &AudioError, msg: impl Into<String>) {
    *slot.lock() = Some(msg.into());
}

pub(crate) fn set_current_track(slot: &Arc<Mutex<Option<String>>>, name: Option<String>) {
    *slot.lock() = name;
}

pub(crate) fn set_current_path(slot: &Arc<Mutex<Option<PathBuf>>>, path: Option<PathBuf>) {
    *slot.lock() = path;
}

pub(crate) fn set_metadata(slot: &Arc<Mutex<Option<String>>>, meta: Option<String>) {
    *slot.lock() = meta;
}

pub(crate) fn set_cue_track(slot: &Arc<Mutex<Option<usize>>>, idx: Option<usize>) {
    *slot.lock() = idx;
}

pub(crate) fn set_cue_markers(slot: &Arc<Mutex<Vec<u64>>>, markers: Vec<u64>) {
    *slot.lock() = markers;
}
