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
use std::io::{BufRead, Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::hdr::tiled::{
    HdrTileBuffer, HdrTileCache, HdrTiledSource, HdrTiledSourceKind,
    configured_hdr_tile_cache_max_bytes, validate_tile_bounds,
};
use crate::hdr::types::{HdrColorSpace, HdrImageBuffer, HdrPixelFormat};

#[derive(Clone, Copy, Debug, Default)]
struct Rgbe8Pixel {
    rgb: [u8; 3],
    exponent: u8,
}

#[derive(Debug)]
pub struct RadianceHdrTiledImageSource {
    #[allow(dead_code)]
    path: PathBuf,
    mmap: Arc<memmap2::Mmap>,
    width: u32,
    height: u32,
    params: crate::hdr::decode::RadianceHeaderParams,
    scanline_offsets: Vec<usize>,
    tile_cache: Mutex<HdrTileCache>,
}

impl RadianceHdrTiledImageSource {
    pub(crate) fn open(path: &Path) -> Result<Self, String> {
        let file = File::open(path).map_err(|err| err.to_string())?;
        let mmap = Arc::new(unsafe { memmap2::Mmap::map(&file).map_err(|err| err.to_string())? });
        let mut params = crate::hdr::decode::RadianceHeaderParams::default();
        let mut reader = Cursor::new(&mmap[..]);
        let (width, height) = read_radiance_header(&mut reader, &mut params)?;
        let data_offset = reader.position() as usize;
        let scanline_offsets = build_radiance_scanline_offsets(&mmap, data_offset, width, height)?;
        log::debug!("[HDR] {}: {}", path.display(), params.diagnostic_label());

        Ok(Self {
            path: path.to_path_buf(),
            mmap,
            width,
            height,
            params,
            scanline_offsets,
            tile_cache: Mutex::new(HdrTileCache::new(configured_hdr_tile_cache_max_bytes())),
        })
    }
}

impl HdrTiledSource for RadianceHdrTiledImageSource {
    fn source_kind(&self) -> HdrTiledSourceKind {
        HdrTiledSourceKind::DiskBacked
    }

    fn width(&self) -> u32 {
        self.width
    }

    fn height(&self) -> u32 {
        self.height
    }

    fn color_space(&self) -> HdrColorSpace {
        HdrColorSpace::LinearSrgb
    }

    fn generate_hdr_preview(&self, max_w: u32, max_h: u32) -> Result<HdrImageBuffer, String> {
        decode_radiance_hdr_preview(
            &self.mmap,
            self.width,
            self.height,
            self.params,
            &self.scanline_offsets,
            max_w,
            max_h,
        )
    }

    fn generate_sdr_preview(&self, max_w: u32, max_h: u32) -> Result<(u32, u32, Vec<u8>), String> {
        decode_radiance_sdr_preview(
            &self.mmap,
            self.width,
            self.height,
            self.params,
            &self.scanline_offsets,
            max_w,
            max_h,
        )
    }

    fn cached_tile_rgba32f_arc(
        &self,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    ) -> Option<Arc<HdrTileBuffer>> {
        self.tile_cache
            .lock()
            .ok()
            .and_then(|mut cache| cache.get((x, y, width, height)))
    }

    fn protect_cached_tiles(&self, tiles: &[(u32, u32, u32, u32)]) {
        if let Ok(mut cache) = self.tile_cache.lock() {
            cache.set_protected_keys(tiles.iter().copied());
        }
    }

    fn extract_tile_rgba32f_arc(
        &self,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    ) -> Result<Arc<HdrTileBuffer>, String> {
        validate_tile_bounds(self.width, self.height, x, y, width, height)?;
        let key = (x, y, width, height);
        if let Ok(mut cache) = self.tile_cache.lock() {
            if let Some(tile) = cache.get(key) {
                return Ok(tile);
            }
        }

        let rgba = decode_radiance_tile_window(
            &self.mmap,
            self.width,
            self.height,
            self.params,
            &self.scanline_offsets,
            x,
            y,
            width,
            height,
        )?;

        let tile = Arc::new(HdrTileBuffer {
            width,
            height,
            color_space: HdrColorSpace::LinearSrgb,
            rgba_f32: Arc::new(rgba),
        });

        if let Ok(mut cache) = self.tile_cache.lock() {
            cache.insert(key, Arc::clone(&tile));
        }

        Ok(tile)
    }
}

fn decode_radiance_tile_window(
    mmap: &[u8],
    expected_width: u32,
    expected_height: u32,
    params: crate::hdr::decode::RadianceHeaderParams,
    scanline_offsets: &[usize],
    x: u32,
    y: u32,
    width: u32,
    height: u32,
) -> Result<Vec<f32>, String> {
    validate_scanline_offsets(expected_height, scanline_offsets)?;
    let mut reader = Cursor::new(mmap);

    let mut scanline = vec![Rgbe8Pixel::default(); expected_width as usize];
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    let first_row = y;
    let last_row_exclusive = y + height;
    for row in first_row..last_row_exclusive {
        reader.set_position(scanline_offsets[row as usize] as u64);
        read_scanline(&mut reader, &mut scanline)?;

        let start = x as usize;
        let end = start + width as usize;
        for pixel in &scanline[start..end] {
            let rgb = pixel.to_rgb_f32();
            rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 1.0]);
        }
    }
    params.apply_to_pixels(&mut rgba);

    Ok(rgba)
}

fn decode_radiance_sdr_preview(
    mmap: &[u8],
    expected_width: u32,
    expected_height: u32,
    params: crate::hdr::decode::RadianceHeaderParams,
    scanline_offsets: &[usize],
    max_w: u32,
    max_h: u32,
) -> Result<(u32, u32, Vec<u8>), String> {
    let preview = decode_radiance_hdr_preview(
        mmap,
        expected_width,
        expected_height,
        params,
        scanline_offsets,
        max_w,
        max_h,
    )?;
    let pixels = crate::hdr::decode::hdr_to_sdr_rgba8(&preview, 0.0)?;
    Ok((preview.width, preview.height, pixels))
}

fn decode_radiance_hdr_preview(
    mmap: &[u8],
    expected_width: u32,
    expected_height: u32,
    params: crate::hdr::decode::RadianceHeaderParams,
    scanline_offsets: &[usize],
    max_w: u32,
    max_h: u32,
) -> Result<HdrImageBuffer, String> {
    let (preview_width, preview_height) =
        preview_dimensions(expected_width, expected_height, max_w, max_h);
    if preview_width == 0 || preview_height == 0 {
        return Err("Radiance HDR preview dimensions must be non-zero".to_string());
    }

    validate_scanline_offsets(expected_height, scanline_offsets)?;
    let mut reader = Cursor::new(mmap);

    let mut scanline = vec![Rgbe8Pixel::default(); expected_width as usize];
    let mut rgba = Vec::with_capacity(preview_width as usize * preview_height as usize * 4);

    for preview_y in 0..preview_height {
        let source_y = preview_sample_coord(preview_y, preview_height, expected_height);
        reader.set_position(scanline_offsets[source_y as usize] as u64);
        read_scanline(&mut reader, &mut scanline)?;

        for preview_x in 0..preview_width {
            let source_x = preview_sample_coord(preview_x, preview_width, expected_width) as usize;
            let rgb = scanline[source_x].to_rgb_f32();
            rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 1.0]);
        }
    }
    params.apply_to_pixels(&mut rgba);

    Ok(HdrImageBuffer {
        width: preview_width,
        height: preview_height,
        format: HdrPixelFormat::Rgba32Float,
        color_space: HdrColorSpace::LinearSrgb,
        rgba_f32: Arc::new(rgba),
    })
}

fn preview_dimensions(width: u32, height: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    if width == 0 || height == 0 || max_w == 0 || max_h == 0 {
        return (0, 0);
    }

    let scale = (max_w as f32 / width as f32)
        .min(max_h as f32 / height as f32)
        .min(1.0);
    let preview_width = ((width as f32 * scale).round() as u32).clamp(1, max_w);
    let preview_height = ((height as f32 * scale).round() as u32).clamp(1, max_h);
    (preview_width, preview_height)
}

fn preview_sample_coord(preview_coord: u32, preview_extent: u32, source_extent: u32) -> u32 {
    if preview_extent <= 1 {
        return 0;
    }

    ((u64::from(preview_coord) * u64::from(source_extent - 1)) / u64::from(preview_extent - 1))
        as u32
}

fn build_radiance_scanline_offsets(
    mmap: &[u8],
    data_offset: usize,
    width: u32,
    height: u32,
) -> Result<Vec<usize>, String> {
    let mut reader = Cursor::new(mmap);
    reader.set_position(data_offset as u64);
    let mut offsets = Vec::with_capacity(height as usize);
    for _ in 0..height {
        offsets.push(reader.position() as usize);
        skip_scanline(&mut reader, width as usize)?;
    }
    Ok(offsets)
}

fn validate_scanline_offsets(
    expected_height: u32,
    scanline_offsets: &[usize],
) -> Result<(), String> {
    if scanline_offsets.len() != expected_height as usize {
        return Err(format!(
            "Radiance HDR scanline index has {} rows, expected {expected_height}",
            scanline_offsets.len()
        ));
    }
    Ok(())
}

fn read_radiance_header<R: BufRead>(
    reader: &mut R,
    params: &mut crate::hdr::decode::RadianceHeaderParams,
) -> Result<(u32, u32), String> {
    let mut line = String::new();
    reader.read_line(&mut line).map_err(|err| err.to_string())?;
    if line.trim_end() != "#?RADIANCE" {
        return Err("Radiance HDR signature not found".to_string());
    }

    loop {
        line.clear();
        let bytes_read = reader.read_line(&mut line).map_err(|err| err.to_string())?;
        if bytes_read == 0 {
            return Err("EOF in Radiance HDR header".to_string());
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            break;
        }
        if !trimmed.starts_with('#') {
            params.apply_header_line(trimmed);
        }
    }

    line.clear();
    reader.read_line(&mut line).map_err(|err| err.to_string())?;
    parse_standard_dimensions(line.trim())
}

fn parse_standard_dimensions(line: &str) -> Result<(u32, u32), String> {
    let mut parts = line.split_whitespace();
    let Some(y_tag) = parts.next() else {
        return Err("Radiance HDR dimensions line is empty".to_string());
    };
    let Some(height) = parts.next() else {
        return Err("Radiance HDR dimensions line is missing height".to_string());
    };
    let Some(x_tag) = parts.next() else {
        return Err("Radiance HDR dimensions line is missing X tag".to_string());
    };
    let Some(width) = parts.next() else {
        return Err("Radiance HDR dimensions line is missing width".to_string());
    };
    if parts.next().is_some() || y_tag != "-Y" || x_tag != "+X" {
        return Err(format!(
            "Unsupported Radiance HDR orientation in dimensions line: {line}"
        ));
    }

    let width = width
        .parse::<u32>()
        .map_err(|err| format!("Invalid Radiance HDR width: {err}"))?;
    let height = height
        .parse::<u32>()
        .map_err(|err| format!("Invalid Radiance HDR height: {err}"))?;
    Ok((width, height))
}

fn read_scanline<R: Read>(reader: &mut R, scanline: &mut [Rgbe8Pixel]) -> Result<(), String> {
    if scanline.is_empty() {
        return Ok(());
    }

    let first = read_rgbe(reader)?;
    if first.rgb[0] == 2 && first.rgb[1] == 2 && first.rgb[2] < 128 {
        decode_component(reader, scanline.len(), |offset, value| {
            scanline[offset].rgb[0] = value;
        })?;
        decode_component(reader, scanline.len(), |offset, value| {
            scanline[offset].rgb[1] = value;
        })?;
        decode_component(reader, scanline.len(), |offset, value| {
            scanline[offset].rgb[2] = value;
        })?;
        decode_component(reader, scanline.len(), |offset, value| {
            scanline[offset].exponent = value;
        })?;
    } else {
        decode_old_rle(reader, first, scanline)?;
    }
    Ok(())
}

fn skip_scanline(reader: &mut Cursor<&[u8]>, width: usize) -> Result<(), String> {
    if width == 0 {
        return Ok(());
    }

    let first = read_rgbe(reader)?;
    if first.rgb[0] == 2 && first.rgb[1] == 2 && first.rgb[2] < 128 {
        for _ in 0..4 {
            skip_component(reader, width)?;
        }
    } else {
        skip_old_rle(reader, first, width)?;
    }
    Ok(())
}

fn skip_component(reader: &mut Cursor<&[u8]>, width: usize) -> Result<(), String> {
    let mut pos = 0usize;
    while pos < width {
        let run = read_byte(reader)?;
        if run <= 128 {
            let count = run as usize;
            if pos + count > width {
                return Err(format!(
                    "Wrong Radiance HDR scanline length: got {}, expected {width}",
                    pos + count
                ));
            }
            let next = reader
                .position()
                .checked_add(count as u64)
                .ok_or_else(|| "Radiance HDR scanline offset overflow".to_string())?;
            if next > reader.get_ref().len() as u64 {
                return Err("EOF in Radiance HDR scanline".to_string());
            }
            reader.set_position(next);
            pos += count;
        } else {
            let count = (run - 128) as usize;
            if pos + count > width {
                return Err(format!(
                    "Wrong Radiance HDR scanline length: got {}, expected {width}",
                    pos + count
                ));
            }
            read_byte(reader)?;
            pos += count;
        }
    }
    Ok(())
}

fn skip_old_rle(reader: &mut Cursor<&[u8]>, first: Rgbe8Pixel, width: usize) -> Result<(), String> {
    let mut x = 1usize;
    let mut run_multiplier = 1usize;
    let mut previous = first;

    while x < width {
        let pixel = read_rgbe(reader)?;
        if pixel.rgb == [1, 1, 1] {
            let count = pixel.exponent as usize * run_multiplier;
            run_multiplier *= 256;
            if x + count > width {
                return Err(format!(
                    "Wrong Radiance HDR scanline length: got {}, expected {width}",
                    x + count
                ));
            }
            x += count;
        } else {
            run_multiplier = 1;
            previous = pixel;
            x += 1;
        }
    }
    let _ = previous;
    Ok(())
}

fn decode_component<R: Read, F: FnMut(usize, u8)>(
    reader: &mut R,
    width: usize,
    mut set_component: F,
) -> Result<(), String> {
    let mut pos = 0usize;
    let mut buf = [0u8; 128];
    while pos < width {
        let run = read_byte(reader)?;
        if run <= 128 {
            let count = run as usize;
            if pos + count > width {
                return Err(format!(
                    "Wrong Radiance HDR scanline length: got {}, expected {width}",
                    pos + count
                ));
            }
            reader
                .read_exact(&mut buf[..count])
                .map_err(|err| err.to_string())?;
            for (offset, value) in buf[..count].iter().copied().enumerate() {
                set_component(pos + offset, value);
            }
            pos += count;
        } else {
            let count = (run - 128) as usize;
            if pos + count > width {
                return Err(format!(
                    "Wrong Radiance HDR scanline length: got {}, expected {width}",
                    pos + count
                ));
            }
            let value = read_byte(reader)?;
            for offset in 0..count {
                set_component(pos + offset, value);
            }
            pos += count;
        }
    }
    Ok(())
}

fn decode_old_rle<R: Read>(
    reader: &mut R,
    first: Rgbe8Pixel,
    scanline: &mut [Rgbe8Pixel],
) -> Result<(), String> {
    scanline[0] = first;
    let mut x = 1usize;
    let mut run_multiplier = 1usize;
    let mut previous = first;

    while x < scanline.len() {
        let pixel = read_rgbe(reader)?;
        if pixel.rgb == [1, 1, 1] {
            let count = pixel.exponent as usize * run_multiplier;
            run_multiplier *= 256;
            if x + count > scanline.len() {
                return Err(format!(
                    "Wrong Radiance HDR scanline length: got {}, expected {}",
                    x + count,
                    scanline.len()
                ));
            }
            for dst in &mut scanline[x..x + count] {
                *dst = previous;
            }
            x += count;
        } else {
            run_multiplier = 1;
            previous = pixel;
            scanline[x] = pixel;
            x += 1;
        }
    }
    Ok(())
}

fn read_rgbe<R: Read>(reader: &mut R) -> Result<Rgbe8Pixel, String> {
    let mut bytes = [0u8; 4];
    reader
        .read_exact(&mut bytes)
        .map_err(|err| err.to_string())?;
    Ok(Rgbe8Pixel {
        rgb: [bytes[0], bytes[1], bytes[2]],
        exponent: bytes[3],
    })
}

fn read_byte<R: Read>(reader: &mut R) -> Result<u8, String> {
    let mut byte = [0u8; 1];
    reader
        .read_exact(&mut byte)
        .map_err(|err| err.to_string())?;
    Ok(byte[0])
}

impl Rgbe8Pixel {
    fn to_rgb_f32(self) -> [f32; 3] {
        if self.exponent == 0 {
            return [0.0; 3];
        }
        let scale = 2.0_f32.powi(i32::from(self.exponent) - 128 - 8);
        [
            f32::from(self.rgb[0]) * scale,
            f32::from(self.rgb[1]) * scale,
            f32::from(self.rgb[2]) * scale,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_tile_applies_radiance_exposure_and_colorcorr() {
        let path = std::env::temp_dir().join(format!(
            "simple_image_viewer_radiance_tile_params_{}.hdr",
            std::process::id()
        ));
        let bytes = b"#?RADIANCE\nFORMAT=32-bit_rle_rgbe\nEXPOSURE=2\nCOLORCORR=2 4 8\n\n-Y 1 +X 1\n\x80\x80\x80\x81";
        std::fs::write(&path, bytes).expect("write test HDR");

        let source = RadianceHdrTiledImageSource::open(&path).expect("open Radiance HDR source");
        let tile = source
            .extract_tile_rgba32f_arc(0, 0, 1, 1)
            .expect("extract Radiance HDR tile");
        let _ = std::fs::remove_file(&path);

        assert!((tile.rgba_f32[0] - 0.25).abs() < 0.01);
        assert!((tile.rgba_f32[1] - 0.125).abs() < 0.01);
        assert!((tile.rgba_f32[2] - 0.0625).abs() < 0.01);
        assert_eq!(tile.rgba_f32[3], 1.0);
    }

    #[test]
    fn static_and_tiled_radiance_decode_apply_same_header_params() {
        let path = std::env::temp_dir().join(format!(
            "simple_image_viewer_radiance_static_tile_consistency_{}.hdr",
            std::process::id()
        ));
        let bytes = b"#?RADIANCE\nFORMAT=32-bit_rle_rgbe\nEXPOSURE=2\nCOLORCORR=2 4 8\n\n-Y 1 +X 1\n\x80\x80\x80\x81";
        std::fs::write(&path, bytes).expect("write test HDR");

        let static_hdr = crate::hdr::decode::decode_hdr_image(&path).expect("decode static HDR");
        let source = RadianceHdrTiledImageSource::open(&path).expect("open tiled HDR");
        let tile = source
            .extract_tile_rgba32f_arc(0, 0, 1, 1)
            .expect("extract tiled HDR");
        let _ = std::fs::remove_file(&path);

        assert_eq!(static_hdr.color_space, tile.color_space);
        assert_eq!(static_hdr.rgba_f32.len(), tile.rgba_f32.len());
        for (static_value, tile_value) in static_hdr.rgba_f32.iter().zip(tile.rgba_f32.iter()) {
            assert!(
                (static_value - tile_value).abs() < 0.01,
                "static={static_value}, tile={tile_value}"
            );
        }
    }

    #[test]
    fn extract_tile_does_not_require_full_image_decode_budget() {
        let path = std::env::temp_dir().join(format!(
            "simple_image_viewer_radiance_tile_window_{}.hdr",
            std::process::id()
        ));
        let width = 8193_u32;
        let height = 8193_u32;
        let mut bytes =
            format!("#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n-Y {height} +X {width}\n").into_bytes();
        for _ in 0..height {
            append_constant_new_rle_scanline(&mut bytes, width, [128, 128, 128], 129);
        }
        std::fs::write(&path, bytes).expect("write oversized tiled HDR");

        let source = RadianceHdrTiledImageSource::open(&path).expect("open Radiance HDR source");
        let tile = source
            .extract_tile_rgba32f_arc(0, 0, 1, 1)
            .expect("extract first pixel without full-image decode");
        let _ = std::fs::remove_file(&path);

        assert_eq!((tile.width, tile.height), (1, 1));
        assert!((tile.rgba_f32[0] - 1.0).abs() < 0.01);
        assert!((tile.rgba_f32[1] - 1.0).abs() < 0.01);
        assert!((tile.rgba_f32[2] - 1.0).abs() < 0.01);
        assert_eq!(tile.rgba_f32[3], 1.0);
    }

    #[test]
    fn generate_preview_does_not_require_full_image_decode_budget() {
        let path = std::env::temp_dir().join(format!(
            "simple_image_viewer_radiance_preview_window_{}.hdr",
            std::process::id()
        ));
        let width = 8193_u32;
        let height = 8193_u32;
        let mut bytes =
            format!("#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n-Y {height} +X {width}\n").into_bytes();
        for _ in 0..height {
            append_constant_new_rle_scanline(&mut bytes, width, [128, 128, 128], 129);
        }
        std::fs::write(&path, bytes).expect("write oversized preview HDR");

        let source = RadianceHdrTiledImageSource::open(&path).expect("open Radiance HDR source");
        let (preview_width, preview_height, pixels) = source
            .generate_sdr_preview(1, 1)
            .expect("generate sampled preview without full-image decode");
        let _ = std::fs::remove_file(&path);

        assert_eq!((preview_width, preview_height), (1, 1));
        assert_eq!(pixels.len(), 4);
        assert!(pixels[0] > 0);
        assert!(pixels[1] > 0);
        assert!(pixels[2] > 0);
        assert_eq!(pixels[3], 255);
    }

    #[test]
    fn open_indexes_radiance_scanline_offsets_for_direct_tile_decode() {
        let path = std::env::temp_dir().join(format!(
            "simple_image_viewer_radiance_scanline_index_{}.hdr",
            std::process::id()
        ));
        let width = 4_u32;
        let height = 4_u32;
        let mut bytes =
            format!("#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n-Y {height} +X {width}\n").into_bytes();
        for row in 0..height {
            append_constant_new_rle_scanline(
                &mut bytes,
                width,
                [32 + row as u8, 64 + row as u8, 96 + row as u8],
                129,
            );
        }
        std::fs::write(&path, bytes).expect("write indexed HDR");

        let source = RadianceHdrTiledImageSource::open(&path).expect("open Radiance HDR source");
        assert_eq!(source.scanline_offsets.len(), height as usize);

        let tile = source
            .extract_tile_rgba32f_arc(1, 3, 2, 1)
            .expect("extract deep tile via scanline offset index");
        let _ = std::fs::remove_file(&path);

        let expected = f32::from(32_u8 + 3) * 2.0_f32.powi(129 - 128 - 8);
        assert!((tile.rgba_f32[0] - expected).abs() < 0.001);
        assert!((tile.rgba_f32[4] - expected).abs() < 0.001);
    }

    fn append_constant_new_rle_scanline(
        bytes: &mut Vec<u8>,
        width: u32,
        rgb: [u8; 3],
        exponent: u8,
    ) {
        bytes.extend_from_slice(&[2, 2, (width >> 8) as u8, (width & 0xff) as u8]);
        for value in [rgb[0], rgb[1], rgb[2], exponent] {
            let mut remaining = width;
            while remaining > 0 {
                let run = remaining.min(127);
                bytes.push(128 + run as u8);
                bytes.push(value);
                remaining -= run;
            }
        }
    }
}
