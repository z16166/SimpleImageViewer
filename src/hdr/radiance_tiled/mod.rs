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

mod header;
mod layout;
mod rle;
mod source;
mod tile_decode;

#[cfg(test)]
mod tests;

pub(crate) use header::decode_radiance_rgba32f_from_mmap;
pub use source::RadianceHdrTiledImageSource;

#[cfg(test)]
pub(crate) use header::parse_radiance_dimensions_line;
#[cfg(test)]
pub(crate) use layout::RadianceRasterLayout;
