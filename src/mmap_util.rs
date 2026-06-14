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

use std::fs::File;
use std::path::Path;

/// Memory-map an existing file for read-only decoding paths (checklist: avoid `read_to_end` duplication).
pub(crate) fn map_file(path: &Path) -> Result<memmap2::Mmap, String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    unsafe { memmap2::Mmap::map(&file).map_err(|e| e.to_string()) }
}
