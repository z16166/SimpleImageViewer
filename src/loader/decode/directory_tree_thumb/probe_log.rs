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

use std::path::Path;

use crate::loader::metadata::{ExifThumbProbe, ExifThumbProbeDetail};

#[cfg(feature = "heif-native")]
use crate::hdr::heif::{HeifThumbProbe, HeifThumbProbeDetail};

#[cfg(feature = "preload-debug")]
pub(super) fn log_strip_exif_probe(
    path: &Path,
    probe: ExifThumbProbe,
    detail: ExifThumbProbeDetail,
    logical: Option<(u32, u32)>,
    max_side: u32,
    extra: &str,
) {
    crate::preload_debug!(
        "[PreloadDebug][Strip] exif_probe path={} outcome={} offset={:?} len={:?} thumb={:?} logical={:?} max_side={} {}",
        path.display(),
        probe.label(),
        detail.offset,
        detail.len,
        detail
            .thumb_w
            .zip(detail.thumb_h)
            .map(|(w, h)| format!("{w}x{h}"))
            .unwrap_or_else(|| "-".to_string()),
        logical,
        max_side,
        extra
    );
}

#[cfg(not(feature = "preload-debug"))]
pub(super) fn log_strip_exif_probe(
    _path: &Path,
    _probe: ExifThumbProbe,
    _detail: ExifThumbProbeDetail,
    _logical: Option<(u32, u32)>,
    _max_side: u32,
    _extra: &str,
) {
}

#[cfg(all(feature = "preload-debug", feature = "heif-native"))]
pub(super) fn log_strip_heif_probe(
    path: &Path,
    probe: HeifThumbProbe,
    detail: HeifThumbProbeDetail,
    max_side: u32,
    extra: &str,
) {
    crate::preload_debug!(
        "[PreloadDebug][Strip] heif_thumb_probe path={} outcome={} count={:?} id={:?} thumb={:?} primary={:?} decode_ms={:?} max_side={} {}",
        path.display(),
        probe.label(),
        detail.thumb_count,
        detail.thumb_id,
        detail
            .thumb_w
            .zip(detail.thumb_h)
            .map(|(w, h)| format!("{w}x{h}"))
            .unwrap_or_else(|| "-".to_string()),
        detail
            .primary_w
            .zip(detail.primary_h)
            .map(|(w, h)| format!("{w}x{h}"))
            .unwrap_or_else(|| "-".to_string()),
        detail.decode_ms,
        max_side,
        extra
    );
}

#[cfg(all(not(feature = "preload-debug"), feature = "heif-native"))]
pub(super) fn log_strip_heif_probe(
    _path: &Path,
    _probe: HeifThumbProbe,
    _detail: HeifThumbProbeDetail,
    _max_side: u32,
    _extra: &str,
) {
}

#[cfg(feature = "preload-debug")]
pub(super) fn log_strip_decode_path(path: &Path, kind: &str, logical: (u32, u32), out_w: u32, out_h: u32) {
    crate::preload_debug!(
        "[PreloadDebug][Strip] decode_path path={} kind={} logical={}x{} out={}x{}",
        path.display(),
        kind,
        logical.0,
        logical.1,
        out_w,
        out_h
    );
}

#[cfg(not(feature = "preload-debug"))]
pub(super) fn log_strip_decode_path(
    _path: &Path,
    _kind: &str,
    _logical: (u32, u32),
    _out_w: u32,
    _out_h: u32,
) {
}
