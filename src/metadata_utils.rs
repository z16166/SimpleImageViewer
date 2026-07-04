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

use std::io::{BufRead, BufReader, Cursor, Seek};
use std::path::Path;

/// Extension-only hint for HEIF container orientation sidecars (`Exif` may not be reachable via
/// [`exif::Reader::read_from_container`] on every writer layout).
fn is_heif_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "heic" | "heif" | "hif"))
}

fn is_avif_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "avif" | "avifs"))
}

fn is_jxl_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("jxl"))
}

/// Parse EXIF Orientation from an already-opened container reader.
///
/// Returns `0` when no valid **Orientation** tag (1–8) is found.
fn read_exif_orientation<R: BufRead + Seek>(reader: &mut R) -> u16 {
    let exifreader = exif::Reader::new();
    if let Ok(exif_data) = exifreader.read_from_container(reader)
        && let Some(field) = exif_data.get_field(exif::Tag::Orientation, exif::In::PRIMARY)
    {
        // Some writers store Orientation as BYTE or LONG; Short is most common.
        if let Some(o) = field.value.get_uint(0) {
            let o = o as u16;
            if (1..=8).contains(&o) {
                return o;
            }
        }
    }
    0
}

/// Read EXIF Orientation from an in-memory byte buffer (e.g. mmap’d file data).
///
/// Avoids re-opening the file when the caller already has the bytes in memory.
/// When `path` is supplied, format-specific probes (HEIF **`irot`/`imir`**, AVIF
/// container transform, JXL codestream orientation) use the extension hint.
pub fn get_exif_orientation_from_bytes(data: &[u8], path: Option<&Path>) -> u16 {
    #[cfg(feature = "heif-native")]
    if path.is_some_and(is_heif_extension) {
        return get_heif_display_orientation_from_bytes(data);
    }

    let mut reader = BufReader::new(Cursor::new(data));
    let o = read_exif_orientation(&mut reader);
    if o != 0 {
        return o;
    }

    #[cfg(feature = "avif-native")]
    {
        if path.is_some_and(is_avif_extension)
            && let Some(o) = crate::hdr::avif::libavif_probe_exif_orientation_from_bytes(data)
            && (1..=8).contains(&o)
        {
            return o;
        }
    }
    #[cfg(feature = "jpegxl")]
    {
        if path.is_some_and(is_jxl_extension)
            && let Some(o) = crate::hdr::jpegxl::libjxl_probe_orientation_from_bytes(data)
            && (1..=8).contains(&o)
        {
            return o;
        }
    }
    1
}

/// [`exif::Reader::read_from_container`] first (TIFF / HEIF BMFF scan). When that does not return an
/// **Orientation** tag, **`.avif`/`.avifs`** use libavif’s container transform (`irot` / `imir`) mapped to
/// the same 1–8 EXIF convention. **`.heic`/`.heif`/`.hif`**: primary-item **`irot`/`imir`** when they
/// imply rotation (Apple often leaves the Exif item at **1**), then container / `Exif`-item orientation.
/// Decoding uses matching `ignore_transformations` so pixels are not rotated twice. **`.jxl`**: after
/// container EXIF, **`JxlDecoderSetKeepOrientation`** probe (values 1–8 match EXIF; the main decode path
/// keeps coded orientation too). **Radiance `.hdr`/`.pic`**: scan order is encoded in the **resolution line**
/// (`±X`/`±Y`); the Radiance decoder unfolds that to normal top-left row-major RGBA (not EXIF).
/// **JPEG XR `.jxr`/`.wdp`** do not carry HEIF **`irot`**; orientation for those relies on EXIF /
/// WIC / ImageIO where applicable (`kamadak-exif`, …).
pub fn get_exif_orientation(path: &Path) -> u16 {
    let Ok(mmap) = crate::mmap_util::map_file(path) else {
        return 1;
    };
    get_exif_orientation_from_bytes(&mmap[..], Some(path))
}

/// HEIF display orientation: prefer primary **`irot`/`imir`** over Exif **Orientation=1** (Apple HEIC).
#[cfg(feature = "heif-native")]
fn get_heif_display_orientation_from_bytes(bytes: &[u8]) -> u16 {
    if let Some(o) = crate::hdr::heif::libheif_manual_geometry_exif_orientation_from_bytes(bytes)
        && o > 1
    {
        return o;
    }
    let mut reader = BufReader::new(Cursor::new(bytes));
    let o = read_exif_orientation(&mut reader);
    if o > 1 {
        return o;
    }
    if let Some(o) = crate::hdr::heif::libheif_exif_orientation_tag_from_bytes(bytes)
        && o > 1
    {
        return o;
    }
    crate::hdr::heif::libheif_manual_geometry_exif_orientation_from_bytes(bytes).unwrap_or(1)
}
