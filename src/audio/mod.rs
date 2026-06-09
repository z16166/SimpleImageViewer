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
mod cue;
mod loop_state;
mod norm;
mod player;
mod playlist;
mod run_loop;
mod slots;
mod sources;
mod wasapi;

#[cfg(test)]
mod tests;

pub use player::AudioPlayer;
pub use playlist::collect_music_files;

#[cfg(test)]
pub(crate) use loop_state::AudioLoopState;
#[cfg(test)]
pub(crate) use playlist::{canonical_or_clone, parse_m3u_entries};
