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

mod constants;
mod decode;
mod handle;
mod load;
mod mmap;
mod orientation;
mod scanline;
mod thumbnail;
mod tiled;

#[cfg(test)]
mod tests;

pub use load::load_via_libtiff;
#[cfg(test)]
pub use load::peek_tiff_tags;
pub(crate) use orientation::{apply_orientation_buffer, apply_orientation_buffer_from_slice};

#[cfg(test)]
pub(crate) use constants::*;
#[cfg(test)]
pub(crate) use decode::{tiff_ieee_scene_linear_eligible, tiff_uint16_rgb_scene_linear_eligible};
#[cfg(test)]
pub(crate) use libtiff_viewer as lib;
