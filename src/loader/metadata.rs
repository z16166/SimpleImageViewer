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

use std::io::{BufRead, Read, Seek, SeekFrom};
use std::path::Path;

use super::types::DecodedImage;

/// Cap for EXIF `JPEGInterchangeFormatLength` allocations (untrusted metadata).
const MAX_EXIF_THUMB_BYTES: usize = 64 * 1024 * 1024;

/// Outcome of an EXIF embedded-JPEG thumbnail probe ([`preload-debug`] logs use [`label`](Self::label)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExifThumbProbe {
    ContainerUnreadable,
    NoThumbOffset,
    NoThumbLength,
    ThumbLengthInvalid,
    SeekFailed,
    ReadFailed,
    JpegDecodeFailed,
    /// EXIF JPEG extracted but failed strip aspect/downsample checks.
    AspectRejected,
    Found,
}

impl ExifThumbProbe {
    #[cfg_attr(not(feature = "preload-debug"), allow(dead_code))]
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::ContainerUnreadable => "container_unreadable",
            Self::NoThumbOffset => "no_thumb_offset",
            Self::NoThumbLength => "no_thumb_length",
            Self::ThumbLengthInvalid => "thumb_length_invalid",
            Self::SeekFailed => "seek_failed",
            Self::ReadFailed => "read_failed",
            Self::JpegDecodeFailed => "jpeg_decode_failed",
            Self::AspectRejected => "aspect_rejected",
            Self::Found => "found",
        }
    }
}

/// Extra fields for [`preload-debug`] strip diagnostics.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ExifThumbProbeDetail {
    #[cfg_attr(not(feature = "preload-debug"), allow(dead_code))]
    pub offset: Option<u32>,
    #[cfg_attr(not(feature = "preload-debug"), allow(dead_code))]
    pub len: Option<u32>,
    #[cfg_attr(not(feature = "preload-debug"), allow(dead_code))]
    pub thumb_w: Option<u32>,
    #[cfg_attr(not(feature = "preload-debug"), allow(dead_code))]
    pub thumb_h: Option<u32>,
}

pub(crate) fn extract_exif_thumbnail(path: &Path) -> Option<DecodedImage> {
    exif_probe_image(extract_exif_thumbnail_probed(path))
}

pub(crate) fn extract_exif_thumbnail_from_bytes(bytes: &[u8], path: &Path) -> Option<DecodedImage> {
    use std::io::Cursor;
    let mut reader = Cursor::new(bytes);
    exif_probe_image(extract_exif_thumbnail_from_reader(&mut reader, path))
}

fn exif_probe_image(
    probed: (Option<DecodedImage>, ExifThumbProbe, ExifThumbProbeDetail),
) -> Option<DecodedImage> {
    let (image, _, _) = probed;
    image
}

pub(crate) fn extract_exif_thumbnail_from_mmap_probed(
    mmap: &memmap2::Mmap,
    path: &Path,
) -> (Option<DecodedImage>, ExifThumbProbe, ExifThumbProbeDetail) {
    use std::io::Cursor;
    let mut reader = Cursor::new(mmap.as_ref());
    extract_exif_thumbnail_from_reader(&mut reader, path)
}

pub(crate) fn extract_exif_thumbnail_probed(
    path: &Path,
) -> (Option<DecodedImage>, ExifThumbProbe, ExifThumbProbeDetail) {
    match crate::mmap_util::map_file(path) {
        Ok(mmap) => return extract_exif_thumbnail_from_mmap_probed(&mmap, path),
        Err(err) => {
            log::debug!(
                "EXIF thumbnail mmap failed for {:?}, falling back to File::open: {err}",
                path.file_name().unwrap_or_default()
            );
        }
    }
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(_) => {
            return (
                None,
                ExifThumbProbe::ContainerUnreadable,
                ExifThumbProbeDetail::default(),
            );
        }
    };
    let mut reader = std::io::BufReader::new(file);
    extract_exif_thumbnail_from_reader(&mut reader, path)
}

fn extract_exif_thumbnail_from_reader<R: BufRead + Read + Seek>(
    reader: &mut R,
    path: &Path,
) -> (Option<DecodedImage>, ExifThumbProbe, ExifThumbProbeDetail) {
    use exif::Reader;

    let exifreader = Reader::new();
    let exif_data = match exifreader.read_from_container(reader) {
        Ok(data) => data,
        Err(_) => {
            return (
                None,
                ExifThumbProbe::ContainerUnreadable,
                ExifThumbProbeDetail::default(),
            );
        }
    };

    let offset = exif_data
        .get_field(exif::Tag::JPEGInterchangeFormat, exif::In::THUMBNAIL)
        .and_then(|f| f.value.get_uint(0));
    let length = exif_data
        .get_field(exif::Tag::JPEGInterchangeFormatLength, exif::In::THUMBNAIL)
        .and_then(|f| f.value.get_uint(0));

    let Some(off) = offset else {
        return (
            None,
            ExifThumbProbe::NoThumbOffset,
            ExifThumbProbeDetail::default(),
        );
    };
    let Some(len) = length else {
        return (
            None,
            ExifThumbProbe::NoThumbLength,
            ExifThumbProbeDetail {
                offset: Some(off),
                ..ExifThumbProbeDetail::default()
            },
        );
    };

    let len_usize = len as usize;
    if len_usize == 0 || len_usize > MAX_EXIF_THUMB_BYTES {
        return (
            None,
            ExifThumbProbe::ThumbLengthInvalid,
            ExifThumbProbeDetail {
                offset: Some(off),
                len: Some(len),
                ..ExifThumbProbeDetail::default()
            },
        );
    }

    if reader.seek(SeekFrom::Start(off as u64)).is_err() {
        return (
            None,
            ExifThumbProbe::SeekFailed,
            ExifThumbProbeDetail {
                offset: Some(off),
                len: Some(len),
                ..ExifThumbProbeDetail::default()
            },
        );
    }

    let mut blob = vec![0_u8; len_usize];
    if reader.read_exact(&mut blob).is_err() {
        return (
            None,
            ExifThumbProbe::ReadFailed,
            ExifThumbProbeDetail {
                offset: Some(off),
                len: Some(len),
                ..ExifThumbProbeDetail::default()
            },
        );
    }

    match libjpeg_turbo::decode_to_rgba(&blob) {
        Ok((w, h, rgba)) => {
            log::debug!(
                "[{}] Extracted EXIF thumbnail ({}x{}) from offset {}",
                path.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("unknown"),
                w,
                h,
                off
            );
            (
                Some(DecodedImage::new(w, h, rgba)),
                ExifThumbProbe::Found,
                ExifThumbProbeDetail {
                    offset: Some(off),
                    len: Some(len),
                    thumb_w: Some(w),
                    thumb_h: Some(h),
                },
            )
        }
        Err(_) => (
            None,
            ExifThumbProbe::JpegDecodeFailed,
            ExifThumbProbeDetail {
                offset: Some(off),
                len: Some(len),
                ..ExifThumbProbeDetail::default()
            },
        ),
    }
}
