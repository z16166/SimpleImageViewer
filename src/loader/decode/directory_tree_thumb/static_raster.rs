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

use crate::loader::{DecodedImage, downsample_decoded_for_strip};

use super::DirectoryTreeThumbDecode;
use super::path_extension_ascii_lower;

pub(super) fn try_static_raster_strip_fast_path(
    path: &Path,
    mmap: Option<&memmap2::Mmap>,
    max_side: u32,
) -> Option<Result<DirectoryTreeThumbDecode, String>> {
    let ext = path_extension_ascii_lower(path)?;
    if !matches!(
        ext.as_str(),
        "png" | "apng" | "webp" | "gif" | "bmp" | "tga" | "ico" | "pnm" | "qoi"
    ) {
        return None;
    }
    let data = mmap?;
    Some(decode_static_raster_strip_from_bytes(
        data.as_ref(),
        max_side,
        Some(ext.as_str()),
    ))
}

pub(super) fn decode_static_raster_strip_from_bytes(
    bytes: &[u8],
    max_side: u32,
    format_hint: Option<&str>,
) -> Result<DirectoryTreeThumbDecode, String> {
    use image::ImageReader;
    use std::io::Cursor;

    if max_side == 0 {
        return Err("static raster strip max_side must be non-zero".to_string());
    }

    let mut dimensions_reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    dimensions_reader.no_limits();
    let mut logical = dimensions_reader
        .into_dimensions()
        .map_err(|e| e.to_string())?;

    let mut decode_reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    decode_reader.no_limits();
    let rgba = decode_reader
        .decode()
        .map_err(|e| e.to_string())?
        .into_rgba8();
    let (width, height) = rgba.dimensions();
    let mut full = DecodedImage::new(width, height, rgba.into_raw());

    let orientation = crate::metadata_utils::get_exif_orientation_from_bytes(bytes);
    if orientation > 4 {
        logical = (logical.1, logical.0);
    }

    let mut decoded = downsample_decoded_for_strip(&full, max_side)?;
    let reusable_full_allowed = reusable_static_raster_full_decode(bytes, format_hint);

    decoded = apply_orientation_to_owned_decoded(decoded, orientation);
    if reusable_full_allowed {
        full = apply_orientation_to_owned_decoded(full, orientation);
    }

    Ok(DirectoryTreeThumbDecode::new(
        decoded,
        logical,
        reusable_full_allowed.then_some(full),
        false,
    ))
}

pub(super) fn apply_orientation_to_owned_decoded(
    mut decoded: DecodedImage,
    orientation: u16,
) -> DecodedImage {
    if orientation <= 1 {
        return decoded;
    }
    let pixels = decoded.take_rgba_owned();
    let (width, height, pixels) = crate::libtiff_loader::apply_orientation_buffer(
        pixels,
        decoded.width,
        decoded.height,
        orientation,
    );
    DecodedImage::new(width, height, pixels)
}

fn reusable_static_raster_full_decode(bytes: &[u8], format_hint: Option<&str>) -> bool {
    match format_hint {
        Some("png") => png_bytes_are_static(bytes),
        Some("webp") => webp_bytes_are_static(bytes),
        Some("bmp" | "tga" | "ico" | "pnm" | "qoi") => true,
        _ => false,
    }
}

pub(super) fn png_bytes_are_static(bytes: &[u8]) -> bool {
    use image::codecs::png::PngDecoder;
    use std::io::Cursor;

    PngDecoder::new(Cursor::new(bytes))
        .and_then(|decoder| decoder.is_apng())
        .map(|is_apng| !is_apng)
        .unwrap_or(false)
}

pub(super) fn webp_bytes_are_static(bytes: &[u8]) -> bool {
    use image::codecs::webp::WebPDecoder;
    use std::io::Cursor;

    WebPDecoder::new(Cursor::new(bytes))
        .map(|decoder| !decoder.has_animation())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::{
        apply_orientation_to_owned_decoded, decode_static_raster_strip_from_bytes,
        png_bytes_are_static, webp_bytes_are_static,
    };
    use crate::loader::preview_aspect_matches_logical;
    use image::ImageEncoder;

    fn encode_test_png(width: u32, height: u32) -> Vec<u8> {
        let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
        for y in 0..height {
            for x in 0..width {
                pixels.extend_from_slice(&[
                    (x % 251) as u8,
                    (y % 241) as u8,
                    ((x + y) % 239) as u8,
                    255,
                ]);
            }
        }
        let mut encoded = Vec::new();
        image::codecs::png::PngEncoder::new(&mut encoded)
            .write_image(&pixels, width, height, image::ColorType::Rgba8.into())
            .expect("encode test PNG");
        encoded
    }

    fn encode_test_webp(width: u32, height: u32) -> Vec<u8> {
        let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
        for y in 0..height {
            for x in 0..width {
                pixels.extend_from_slice(&[
                    (x % 251) as u8,
                    (y % 241) as u8,
                    ((x + y) % 239) as u8,
                    255,
                ]);
            }
        }
        let mut encoded = Vec::new();
        image::codecs::webp::WebPEncoder::new_lossless(&mut encoded)
            .write_image(&pixels, width, height, image::ColorType::Rgba8.into())
            .expect("encode test WebP");
        encoded
    }

    fn test_crc32(bytes: &[u8]) -> u32 {
        let mut crc = 0xffff_ffffu32;
        for &byte in bytes {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                let mask = 0u32.wrapping_sub(crc & 1);
                crc = (crc >> 1) ^ (0xedb8_8320 & mask);
            }
        }
        !crc
    }

    fn append_test_png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        let mut crc_input = Vec::with_capacity(kind.len() + data.len());
        crc_input.extend_from_slice(kind);
        crc_input.extend_from_slice(data);
        out.extend_from_slice(&test_crc32(&crc_input).to_be_bytes());
    }

    fn inject_test_apng_actl_chunk(png: &[u8]) -> Vec<u8> {
        const PNG_SIGNATURE_LEN: usize = 8;
        const IHDR_CHUNK_TOTAL_LEN: usize = 4 + 4 + 13 + 4;
        let insert_at = PNG_SIGNATURE_LEN + IHDR_CHUNK_TOTAL_LEN;
        let mut out = Vec::with_capacity(png.len() + 20);
        out.extend_from_slice(&png[..insert_at]);
        let mut actl = Vec::with_capacity(8);
        actl.extend_from_slice(&1u32.to_be_bytes());
        actl.extend_from_slice(&0u32.to_be_bytes());
        append_test_png_chunk(&mut out, b"acTL", &actl);
        out.extend_from_slice(&png[insert_at..]);
        out
    }

    fn chunk_payload<'a>(container: &'a [u8], kind: &[u8; 4]) -> &'a [u8] {
        let mut pos = 12usize;
        while pos + 8 <= container.len() {
            let size = u32::from_le_bytes(
                container[pos + 4..pos + 8]
                    .try_into()
                    .expect("chunk size bytes"),
            ) as usize;
            let payload_start = pos + 8;
            let payload_end = payload_start + size;
            if &container[pos..pos + 4] == kind {
                return &container[payload_start..payload_end];
            }
            pos = payload_end + (size % 2);
        }
        panic!("missing WebP chunk {:?}", kind);
    }

    fn append_test_webp_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(kind);
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(data);
        if data.len() % 2 != 0 {
            out.push(0);
        }
    }

    fn animated_webp_from_static_webp(static_webp: &[u8], width: u32, height: u32) -> Vec<u8> {
        let vp8l = chunk_payload(static_webp, b"VP8L");

        let mut chunks = Vec::new();
        let mut vp8x = Vec::with_capacity(10);
        vp8x.push(0b0000_0010);
        vp8x.extend_from_slice(&[0, 0, 0]);
        vp8x.extend_from_slice(&(width - 1).to_le_bytes()[..3]);
        vp8x.extend_from_slice(&(height - 1).to_le_bytes()[..3]);
        append_test_webp_chunk(&mut chunks, b"VP8X", &vp8x);

        let mut anim = Vec::with_capacity(6);
        anim.extend_from_slice(&[0, 0, 0, 0]);
        anim.extend_from_slice(&0u16.to_le_bytes());
        append_test_webp_chunk(&mut chunks, b"ANIM", &anim);

        let mut anmf = Vec::new();
        anmf.extend_from_slice(&[0, 0, 0]);
        anmf.extend_from_slice(&[0, 0, 0]);
        anmf.extend_from_slice(&(width - 1).to_le_bytes()[..3]);
        anmf.extend_from_slice(&(height - 1).to_le_bytes()[..3]);
        anmf.extend_from_slice(&100u32.to_le_bytes()[..3]);
        anmf.push(0);
        append_test_webp_chunk(&mut anmf, b"VP8L", vp8l);
        append_test_webp_chunk(&mut chunks, b"ANMF", &anmf);

        let mut out = Vec::with_capacity(12 + chunks.len());
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&((4 + chunks.len()) as u32).to_le_bytes());
        out.extend_from_slice(b"WEBP");
        out.extend_from_slice(&chunks);
        out
    }

    #[test]
    fn static_raster_strip_from_mmap_downsamples_png_to_max_side() {
        let encoded = encode_test_png(120, 60);
        let strip = decode_static_raster_strip_from_bytes(&encoded, 30, Some("png"))
            .expect("decode PNG strip");
        let decoded = strip.preview;
        let logical = strip.logical_size;

        assert_eq!(logical, (120, 60));
        assert_eq!(decoded.width, 30);
        assert_eq!(decoded.height, 15);
        assert!(preview_aspect_matches_logical(
            decoded.width,
            decoded.height,
            logical.0,
            logical.1
        ));
        assert_eq!(decoded.rgba().len(), 30 * 15 * 4);
    }

    #[test]
    fn static_raster_strip_from_static_png_keeps_reusable_full_decode() {
        let encoded = encode_test_png(120, 60);
        let strip = decode_static_raster_strip_from_bytes(&encoded, 30, Some("png"))
            .expect("decode PNG strip");
        let full = strip
            .reusable_full
            .expect("static PNG strip decode should retain full image for preload reuse");

        assert_eq!(full.width, 120);
        assert_eq!(full.height, 60);
        assert_eq!(full.rgba().len(), 120 * 60 * 4);
    }

    #[test]
    fn static_raster_strip_from_static_webp_keeps_reusable_full_decode() {
        let encoded = encode_test_webp(80, 40);
        let strip = decode_static_raster_strip_from_bytes(&encoded, 20, Some("webp"))
            .expect("decode WebP strip");
        let full = strip
            .reusable_full
            .expect("static WebP strip decode should retain full image for preload reuse");

        assert_eq!(full.width, 80);
        assert_eq!(full.height, 40);
        assert_eq!(full.rgba().len(), 80 * 40 * 4);
    }

    #[test]
    fn static_raster_reuse_rejects_apng() {
        let encoded = encode_test_png(8, 4);
        let apng = inject_test_apng_actl_chunk(&encoded);

        assert!(!png_bytes_are_static(&apng));
        let strip = decode_static_raster_strip_from_bytes(&apng, 8, Some("png"))
            .expect("decode APNG default image");
        assert!(
            strip.reusable_full.is_none(),
            "APNG must not reuse the default image as the main static decode"
        );
    }

    #[test]
    fn static_raster_reuse_rejects_animated_webp() {
        let static_webp = encode_test_webp(8, 4);
        let animated_webp = animated_webp_from_static_webp(&static_webp, 8, 4);

        assert!(!webp_bytes_are_static(&animated_webp));
    }

    #[test]
    fn owned_decoded_orientation_rotates_reusable_full_image() {
        let pixels = vec![
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 0, 255, 255, 0, 255, 255, 0,
            255, 255, 255,
        ];
        let decoded = apply_orientation_to_owned_decoded(
            crate::loader::DecodedImage::new(2, 3, pixels),
            6,
        );

        assert_eq!(decoded.width, 3);
        assert_eq!(decoded.height, 2);
        assert_eq!(decoded.rgba().len(), 2 * 3 * 4);
    }
}
