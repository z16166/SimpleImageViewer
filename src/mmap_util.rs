// Simple Image Viewer - A high-performance, cross-platform image viewer
// Copyright (C) 2024-2026 Simple Image Viewer Contributors
//
// SPDX-License-Identifier: GPL-3.0-only

use std::fs::File;
use std::path::Path;

/// Memory-map an existing file for read-only decoding paths (checklist: avoid `read_to_end` duplication).
pub(crate) fn map_file(path: &Path) -> Result<memmap2::Mmap, String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    unsafe { memmap2::Mmap::map(&file).map_err(|e| e.to_string()) }
}
