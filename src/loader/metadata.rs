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

use super::types::DecodedImage;

pub(crate) fn extract_exif_thumbnail(path: &Path) -> Option<DecodedImage> {
    use exif::Reader;
    let file = std::fs::File::open(path).ok()?;
    let mut reader = std::io::BufReader::new(file);
    let exifreader = Reader::new();

    if let Ok(exif_data) = exifreader.read_from_container(&mut reader) {
        // Find thumbnail offset and length in IFD1 (THUMBNAIL)
        let offset = exif_data
            .get_field(exif::Tag::JPEGInterchangeFormat, exif::In::THUMBNAIL)
            .and_then(|f| f.value.get_uint(0));
        let length = exif_data
            .get_field(exif::Tag::JPEGInterchangeFormatLength, exif::In::THUMBNAIL)
            .and_then(|f| f.value.get_uint(0));

        if let (Some(off), Some(len)) = (offset, length) {
            use std::io::{Read, Seek, SeekFrom};
            let mut f = std::fs::File::open(path).ok()?;
            f.seek(SeekFrom::Start(off as u64)).ok()?;
            let mut blob = vec![0u8; len as usize];
            if f.read_exact(&mut blob).is_ok() {
                if let Ok(img) = image::load_from_memory(&blob) {
                    let rgba = img.into_rgba8();
                    log::info!(
                        "[{}] Extracted EXIF thumbnail ({}x{}) from offset {}",
                        path.file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("unknown"),
                        rgba.width(),
                        rgba.height(),
                        off
                    );
                    return Some(DecodedImage::new(
                        rgba.width(),
                        rgba.height(),
                        rgba.into_raw(),
                    ));
                }
            }
        }
    }
    None
}
