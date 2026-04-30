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

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::hdr::tiled::{
    HdrTileBuffer, HdrTileCache, HdrTiledSource, HdrTiledSourceKind,
    configured_hdr_tile_cache_max_bytes, validate_tile_bounds,
};
use crate::hdr::types::{
    DEFAULT_MAX_DISPLAY_NITS, DEFAULT_SDR_WHITE_NITS, HdrColorSpace, HdrImageBuffer, HdrPixelFormat,
};

#[cfg(test)]
use std::path::Path;

const JPEG_SOI: [u8; 2] = [0xFF, 0xD8];
const JPEG_SOS: u8 = 0xDA;
const JPEG_EOI: u8 = 0xD9;
const JPEG_APP1: u8 = 0xE1;
const HDR_GAIN_MAP_NAMESPACE: &str = "http://ns.adobe.com/hdr-gain-map/1.0/";
const HDR_GAIN_MAP_VERSION: &str = "hdrgm:Version";
const DEFAULT_TARGET_HDR_CAPACITY: f32 = DEFAULT_MAX_DISPLAY_NITS / DEFAULT_SDR_WHITE_NITS;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UltraHdrJpegInfo {
    pub(crate) is_ultra_hdr: bool,
    pub(crate) primary_xmp_has_gain_map: bool,
    pub(crate) gain_map_item_count: usize,
}

#[cfg(test)]
fn inspect_ultra_hdr_jpeg(path: &Path) -> Result<UltraHdrJpegInfo, String> {
    let bytes = std::fs::read(path).map_err(|err| err.to_string())?;
    inspect_ultra_hdr_jpeg_bytes(&bytes)
}

#[cfg(test)]
fn extract_gain_map_jpeg(path: &Path) -> Result<Vec<u8>, String> {
    let bytes = std::fs::read(path).map_err(|err| err.to_string())?;
    extract_gain_map_jpeg_bytes(&bytes)
}

#[cfg(test)]
fn decode_ultra_hdr_jpeg(path: &Path) -> Result<HdrImageBuffer, String> {
    let bytes = std::fs::read(path).map_err(|err| err.to_string())?;
    decode_ultra_hdr_jpeg_bytes(&bytes)
}

pub(crate) fn decode_ultra_hdr_jpeg_bytes(bytes: &[u8]) -> Result<HdrImageBuffer, String> {
    let info = inspect_ultra_hdr_jpeg_bytes(bytes)?;
    if !info.is_ultra_hdr {
        return Err("JPEG does not advertise Ultra HDR gain map metadata".to_string());
    }

    let (width, height, sdr_rgba) = libjpeg_turbo::decode_to_rgba(bytes)?;
    let gain_map_jpeg = extract_gain_map_jpeg_bytes(bytes)?;
    let metadata = gain_map_metadata(&gain_map_jpeg)?;
    let (gain_width, gain_height, gain_rgba) = libjpeg_turbo::decode_to_rgba(&gain_map_jpeg)?;

    let mut rgba_f32 = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for x in 0..width {
            let sdr_index = (y as usize * width as usize + x as usize) * 4;
            let gain_value =
                sample_gain_map_value(&gain_rgba, gain_width, gain_height, x, y, width, height);
            append_hdr_pixel_from_sdr_and_gain(
                &mut rgba_f32,
                &sdr_rgba[sdr_index..sdr_index + 4],
                gain_value,
                metadata,
                DEFAULT_TARGET_HDR_CAPACITY,
            );
        }
    }

    Ok(HdrImageBuffer {
        width,
        height,
        format: HdrPixelFormat::Rgba32Float,
        color_space: HdrColorSpace::LinearSrgb,
        rgba_f32: Arc::new(rgba_f32),
    })
}

pub(crate) fn apply_orientation_to_hdr_buffer(
    buffer: HdrImageBuffer,
    orientation: u16,
) -> HdrImageBuffer {
    if orientation <= 1 {
        return buffer;
    }

    let expected_len = buffer.width as usize * buffer.height as usize * 4;
    if buffer.rgba_f32.len() != expected_len {
        return buffer;
    }

    let (out_w, out_h) = if (5..=8).contains(&orientation) {
        (buffer.height, buffer.width)
    } else {
        (buffer.width, buffer.height)
    };
    let mut out = vec![0.0_f32; out_w as usize * out_h as usize * 4];

    for y in 0..buffer.height {
        for x in 0..buffer.width {
            let (nx, ny) = match orientation {
                2 => (buffer.width - 1 - x, y),
                3 => (buffer.width - 1 - x, buffer.height - 1 - y),
                4 => (x, buffer.height - 1 - y),
                5 => (y, x),
                6 => (buffer.height - 1 - y, x),
                7 => (buffer.height - 1 - y, buffer.width - 1 - x),
                8 => (y, buffer.width - 1 - x),
                _ => (x, y),
            };
            let src_idx = (y as usize * buffer.width as usize + x as usize) * 4;
            let dst_idx = (ny as usize * out_w as usize + nx as usize) * 4;
            out[dst_idx..dst_idx + 4].copy_from_slice(&buffer.rgba_f32[src_idx..src_idx + 4]);
        }
    }

    HdrImageBuffer {
        width: out_w,
        height: out_h,
        format: buffer.format,
        color_space: buffer.color_space,
        rgba_f32: Arc::new(out),
    }
}

#[derive(Debug)]
pub struct UltraHdrTiledImageSource {
    path: PathBuf,
    width: u32,
    height: u32,
    physical_width: u32,
    physical_height: u32,
    orientation: u16,
    gain_width: u32,
    gain_height: u32,
    gain_rgba: Arc<Vec<u8>>,
    metadata: GainMapMetadata,
    tile_cache: Mutex<HdrTileCache>,
}

impl UltraHdrTiledImageSource {
    pub(crate) fn open(path: PathBuf, orientation: u16) -> Result<Self, String> {
        let bytes = std::fs::read(&path).map_err(|err| err.to_string())?;
        let info = inspect_ultra_hdr_jpeg_bytes(&bytes)?;
        if !info.is_ultra_hdr {
            return Err("JPEG does not advertise Ultra HDR gain map metadata".to_string());
        }

        let decompressor = libjpeg_turbo::Decompressor::new()?;
        let (physical_width, physical_height, _) = decompressor.decompress_header(&bytes)?;
        let physical_width = u32::try_from(physical_width)
            .map_err(|_| "Ultra HDR JPEG width is negative".to_string())?;
        let physical_height = u32::try_from(physical_height)
            .map_err(|_| "Ultra HDR JPEG height is negative".to_string())?;
        let (width, height) = oriented_dimensions(physical_width, physical_height, orientation);

        let gain_map_jpeg = extract_gain_map_jpeg_bytes(&bytes)?;
        let metadata = gain_map_metadata(&gain_map_jpeg)?;
        let (gain_width, gain_height, gain_rgba) = libjpeg_turbo::decode_to_rgba(&gain_map_jpeg)?;

        Ok(Self {
            path,
            width,
            height,
            physical_width,
            physical_height,
            orientation,
            gain_width,
            gain_height,
            gain_rgba: Arc::new(gain_rgba),
            metadata,
            tile_cache: Mutex::new(HdrTileCache::new(configured_hdr_tile_cache_max_bytes())),
        })
    }
}

impl HdrTiledSource for UltraHdrTiledImageSource {
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

    fn generate_sdr_preview(&self, max_w: u32, max_h: u32) -> Result<(u32, u32, Vec<u8>), String> {
        let tile = self.extract_tile_rgba32f_arc(0, 0, self.width, self.height)?;
        let pixels = crate::hdr::decode::hdr_to_sdr_rgba8(
            &HdrImageBuffer {
                width: tile.width,
                height: tile.height,
                format: HdrPixelFormat::Rgba32Float,
                color_space: tile.color_space,
                rgba_f32: Arc::clone(&tile.rgba_f32),
            },
            0.0,
        )?;
        let image = image::RgbaImage::from_raw(tile.width, tile.height, pixels)
            .ok_or_else(|| "Failed to build Ultra HDR SDR preview image".to_string())?;
        let preview = image::imageops::thumbnail(&image, max_w, max_h);
        Ok((preview.width(), preview.height(), preview.into_raw()))
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

        let bytes = std::fs::read(&self.path).map_err(|err| err.to_string())?;
        let (decoded_width, decoded_height, sdr_rgba) = libjpeg_turbo::decode_to_rgba(&bytes)?;
        if decoded_width != self.physical_width || decoded_height != self.physical_height {
            return Err("Ultra HDR JPEG dimensions changed while extracting tile".to_string());
        }

        let mut rgba_f32 = Vec::with_capacity(width as usize * height as usize * 4);
        for dy in 0..height {
            for dx in 0..width {
                let display_x = x + dx;
                let display_y = y + dy;
                let (physical_x, physical_y) = display_to_physical_pixel(
                    display_x,
                    display_y,
                    self.physical_width,
                    self.physical_height,
                    self.orientation,
                );
                let sdr_index =
                    (physical_y as usize * self.physical_width as usize + physical_x as usize) * 4;
                let gain_value = sample_gain_map_value(
                    &self.gain_rgba,
                    self.gain_width,
                    self.gain_height,
                    physical_x,
                    physical_y,
                    self.physical_width,
                    self.physical_height,
                );
                append_hdr_pixel_from_sdr_and_gain(
                    &mut rgba_f32,
                    &sdr_rgba[sdr_index..sdr_index + 4],
                    gain_value,
                    self.metadata,
                    DEFAULT_TARGET_HDR_CAPACITY,
                );
            }
        }

        let tile = Arc::new(HdrTileBuffer {
            width,
            height,
            color_space: HdrColorSpace::LinearSrgb,
            rgba_f32: Arc::new(rgba_f32),
        });

        if let Ok(mut cache) = self.tile_cache.lock() {
            cache.insert(key, Arc::clone(&tile));
        }

        Ok(tile)
    }
}

fn inspect_ultra_hdr_jpeg_bytes(bytes: &[u8]) -> Result<UltraHdrJpegInfo, String> {
    if !bytes.starts_with(&JPEG_SOI) {
        return Err("not a JPEG stream".to_string());
    }

    let mut primary_xmp_has_gain_map = false;
    let mut gain_map_item_count = 0;

    for segment in primary_metadata_segments(bytes)? {
        if segment.marker != JPEG_APP1 {
            continue;
        }

        let text = String::from_utf8_lossy(segment.payload);
        if text.contains(HDR_GAIN_MAP_NAMESPACE) && text.contains(HDR_GAIN_MAP_VERSION) {
            primary_xmp_has_gain_map = true;
        }
        gain_map_item_count += text.matches("Item:Semantic=\"GainMap\"").count();
        gain_map_item_count += text.matches("Item:Semantic='GainMap'").count();
        gain_map_item_count += text.matches("Semantic=\"GainMap\"").count();
        gain_map_item_count += text.matches("Semantic='GainMap'").count();
    }

    Ok(UltraHdrJpegInfo {
        is_ultra_hdr: primary_xmp_has_gain_map && gain_map_item_count > 0,
        primary_xmp_has_gain_map,
        gain_map_item_count,
    })
}

#[derive(Debug, Clone, Copy)]
struct GainMapMetadata {
    gain_map_min: f32,
    gain_map_max: f32,
    gamma: f32,
    offset_sdr: f32,
    offset_hdr: f32,
    hdr_capacity_min: f32,
    hdr_capacity_max: f32,
}

fn gain_map_metadata(gain_map_jpeg: &[u8]) -> Result<GainMapMetadata, String> {
    primary_metadata_segments(gain_map_jpeg)?
        .iter()
        .filter(|segment| segment.marker == JPEG_APP1)
        .find_map(|segment| {
            let text = String::from_utf8_lossy(segment.payload);
            if !text.contains(HDR_GAIN_MAP_NAMESPACE) || !text.contains(HDR_GAIN_MAP_VERSION) {
                return None;
            }
            let gain_map_max = attribute_f32(&text, "hdrgm:GainMapMax")?;
            Some(GainMapMetadata {
                gain_map_min: attribute_f32(&text, "hdrgm:GainMapMin").unwrap_or(0.0),
                gain_map_max,
                gamma: attribute_f32(&text, "hdrgm:Gamma").unwrap_or(1.0),
                offset_sdr: attribute_f32(&text, "hdrgm:OffsetSDR").unwrap_or(1.0 / 64.0),
                offset_hdr: attribute_f32(&text, "hdrgm:OffsetHDR").unwrap_or(1.0 / 64.0),
                hdr_capacity_min: attribute_f32(&text, "hdrgm:HDRCapacityMin").unwrap_or(0.0),
                hdr_capacity_max: attribute_f32(&text, "hdrgm:HDRCapacityMax")
                    .unwrap_or(gain_map_max),
            })
        })
        .ok_or_else(|| "Ultra HDR gain map metadata not found".to_string())
}

fn attribute_f32(text: &str, name: &str) -> Option<f32> {
    parse_quoted_attribute(text, name)?.parse().ok()
}

fn append_hdr_pixel_from_sdr_and_gain(
    rgba_f32: &mut Vec<f32>,
    sdr_rgba: &[u8],
    gain_value: f32,
    metadata: GainMapMetadata,
    target_hdr_capacity: f32,
) {
    for channel in &sdr_rgba[..3] {
        rgba_f32.push(recover_hdr_channel_from_sdr_and_gain(
            *channel,
            gain_value,
            metadata,
            target_hdr_capacity,
        ));
    }
    rgba_f32.push(f32::from(sdr_rgba[3]) / 255.0);
}

fn recover_hdr_channel_from_sdr_and_gain(
    sdr_channel: u8,
    gain_value: f32,
    metadata: GainMapMetadata,
    target_hdr_capacity: f32,
) -> f32 {
    let gain_weight = gain_map_weight(metadata, target_hdr_capacity);
    let log_boost = metadata.gain_map_min
        + (metadata.gain_map_max - metadata.gain_map_min)
            * gain_value.powf(1.0 / metadata.gamma.max(f32::MIN_POSITIVE))
            * gain_weight;
    let boost = 2.0_f32.powf(log_boost);

    let linear_sdr = srgb_u8_to_linear_f32(sdr_channel);
    ((linear_sdr + metadata.offset_sdr) * boost - metadata.offset_hdr).max(0.0)
}

fn gain_map_weight(metadata: GainMapMetadata, target_hdr_capacity: f32) -> f32 {
    let capacity_range = metadata.hdr_capacity_max - metadata.hdr_capacity_min;
    if capacity_range <= f32::EPSILON {
        return if target_hdr_capacity >= metadata.hdr_capacity_max {
            1.0
        } else {
            0.0
        };
    }

    ((target_hdr_capacity - metadata.hdr_capacity_min) / capacity_range).clamp(0.0, 1.0)
}

fn srgb_u8_to_linear_f32(value: u8) -> f32 {
    let encoded = f32::from(value) / 255.0;
    if encoded <= 0.04045 {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    }
}

fn sample_gain_map_value(
    gain_rgba: &[u8],
    gain_width: u32,
    gain_height: u32,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
) -> f32 {
    if gain_width == 0 || gain_height == 0 || width == 0 || height == 0 {
        return 0.0;
    }

    let gx = ((x as f32 + 0.5) * gain_width as f32 / width as f32 - 0.5)
        .clamp(0.0, gain_width.saturating_sub(1) as f32);
    let gy = ((y as f32 + 0.5) * gain_height as f32 / height as f32 - 0.5)
        .clamp(0.0, gain_height.saturating_sub(1) as f32);
    let x0 = gx.floor() as u32;
    let y0 = gy.floor() as u32;
    let x1 = (x0 + 1).min(gain_width - 1);
    let y1 = (y0 + 1).min(gain_height - 1);
    let tx = gx - x0 as f32;
    let ty = gy - y0 as f32;

    let top = lerp(
        gain_map_luma(gain_rgba, gain_width, x0, y0),
        gain_map_luma(gain_rgba, gain_width, x1, y0),
        tx,
    );
    let bottom = lerp(
        gain_map_luma(gain_rgba, gain_width, x0, y1),
        gain_map_luma(gain_rgba, gain_width, x1, y1),
        tx,
    );
    lerp(top, bottom, ty)
}

fn gain_map_luma(gain_rgba: &[u8], gain_width: u32, x: u32, y: u32) -> f32 {
    let index = (y as usize * gain_width as usize + x as usize) * 4;
    f32::from(gain_rgba[index]) / 255.0
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn oriented_dimensions(width: u32, height: u32, orientation: u16) -> (u32, u32) {
    if (5..=8).contains(&orientation) {
        (height, width)
    } else {
        (width, height)
    }
}

fn display_to_physical_pixel(
    display_x: u32,
    display_y: u32,
    physical_width: u32,
    physical_height: u32,
    orientation: u16,
) -> (u32, u32) {
    match orientation {
        2 => (physical_width - 1 - display_x, display_y),
        3 => (
            physical_width - 1 - display_x,
            physical_height - 1 - display_y,
        ),
        4 => (display_x, physical_height - 1 - display_y),
        5 => (display_y, display_x),
        6 => (display_y, physical_height - 1 - display_x),
        7 => (
            physical_width - 1 - display_y,
            physical_height - 1 - display_x,
        ),
        8 => (physical_width - 1 - display_y, display_x),
        _ => (display_x, display_y),
    }
}

fn extract_gain_map_jpeg_bytes(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let length = primary_metadata_segments(bytes)?
        .iter()
        .filter(|segment| segment.marker == JPEG_APP1)
        .find_map(|segment| {
            let text = String::from_utf8_lossy(segment.payload);
            gain_map_item_length(&text)
        })
        .ok_or_else(|| "Ultra HDR gain map item length not found".to_string())?;

    if length > bytes.len() {
        return Err("Ultra HDR gain map length exceeds JPEG file size".to_string());
    }

    let start = bytes.len() - length;
    let gain_map = &bytes[start..];
    if !gain_map.starts_with(&JPEG_SOI) || !gain_map.ends_with(&[0xFF, JPEG_EOI]) {
        return Err("Ultra HDR gain map payload is not a trailing JPEG stream".to_string());
    }

    Ok(gain_map.to_vec())
}

fn gain_map_item_length(xmp: &str) -> Option<usize> {
    let semantic_index = xmp
        .find("Item:Semantic=\"GainMap\"")
        .or_else(|| xmp.find("Item:Semantic='GainMap'"))
        .or_else(|| xmp.find("Semantic=\"GainMap\""))
        .or_else(|| xmp.find("Semantic='GainMap'"))?;
    let item_start = xmp[..semantic_index].rfind("<Container:Item")?;
    let item_end = xmp[semantic_index..].find('>')? + semantic_index;
    let item = &xmp[item_start..item_end];
    attribute_usize(item, "Item:Length").or_else(|| attribute_usize(item, "Length"))
}

fn attribute_usize(text: &str, name: &str) -> Option<usize> {
    parse_quoted_attribute(text, name)?.parse().ok()
}

fn parse_quoted_attribute<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let start = text.find(name)? + name.len();
    let tail = text[start..].trim_start();
    let tail = tail.strip_prefix('=')?.trim_start();
    let quote = tail.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let value_start = quote.len_utf8();
    let value_end = tail[value_start..].find(quote)?;
    Some(&tail[value_start..value_start + value_end])
}

#[derive(Debug, Clone, Copy)]
struct JpegSegment<'a> {
    marker: u8,
    payload: &'a [u8],
}

fn primary_metadata_segments(bytes: &[u8]) -> Result<Vec<JpegSegment<'_>>, String> {
    let mut segments = Vec::new();
    let mut offset = JPEG_SOI.len();

    while offset < bytes.len() {
        if bytes[offset] != 0xFF {
            return Err(format!("invalid JPEG marker at byte offset {offset}"));
        }

        while offset < bytes.len() && bytes[offset] == 0xFF {
            offset += 1;
        }
        if offset >= bytes.len() {
            break;
        }

        let marker = bytes[offset];
        offset += 1;

        if marker == JPEG_SOS || marker == JPEG_EOI {
            break;
        }
        if marker_has_no_payload(marker) {
            continue;
        }
        if offset + 2 > bytes.len() {
            return Err("truncated JPEG segment length".to_string());
        }

        let segment_len = u16::from_be_bytes([bytes[offset], bytes[offset + 1]]) as usize;
        if segment_len < 2 {
            return Err(format!("invalid JPEG segment length {segment_len}"));
        }
        let payload_start = offset + 2;
        let payload_end = offset
            .checked_add(segment_len)
            .ok_or_else(|| "JPEG segment length overflow".to_string())?;
        if payload_end > bytes.len() {
            return Err("truncated JPEG segment payload".to_string());
        }

        segments.push(JpegSegment {
            marker,
            payload: &bytes[payload_start..payload_end],
        });
        offset = payload_end;
    }

    Ok(segments)
}

fn marker_has_no_payload(marker: u8) -> bool {
    marker == 0x01 || (0xD0..=0xD7).contains(&marker)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn ultra_hdr_samples_root() -> Option<PathBuf> {
        std::env::var_os("SIV_ULTRA_HDR_SAMPLES_DIR")
            .map(PathBuf::from)
            .or_else(|| Some(PathBuf::from(r"F:\HDR\Ultra_HDR_Samples")))
            .filter(|path| path.is_dir())
    }

    fn sample_path(root: &Path, relative: &str) -> PathBuf {
        relative
            .split('/')
            .fold(root.to_path_buf(), |path, segment| path.join(segment))
    }

    #[test]
    fn ultra_hdr_original_samples_are_detected_as_jpeg_r() {
        let Some(root) = ultra_hdr_samples_root() else {
            eprintln!(
                "skipping Ultra HDR corpus test; set SIV_ULTRA_HDR_SAMPLES_DIR to Ultra_HDR_Samples"
            );
            return;
        };

        for index in 1..=10 {
            let path = sample_path(
                &root,
                &format!("Originals/Ultra_HDR_Samples_Originals_{index:02}.jpg"),
            );
            if !path.is_file() {
                eprintln!("skipping Ultra HDR sample {}; file missing", path.display());
                continue;
            }

            let info = inspect_ultra_hdr_jpeg(&path).expect("inspect Ultra HDR JPEG_R sample");
            assert!(
                info.is_ultra_hdr,
                "{} should be detected as Ultra HDR",
                path.display()
            );
            assert!(
                info.primary_xmp_has_gain_map,
                "{} should advertise hdrgm metadata",
                path.display()
            );
            assert!(
                info.gain_map_item_count >= 1,
                "{} should include a gain map item",
                path.display()
            );
        }
    }

    #[test]
    fn plain_jpeg_xmp_is_not_detected_as_jpeg_r() {
        let bytes = minimal_jpeg_with_app1_xmp(
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF></rdf:RDF></x:xmpmeta>"#,
        );

        let info = inspect_ultra_hdr_jpeg_bytes(&bytes).expect("inspect plain JPEG");

        assert!(!info.is_ultra_hdr);
        assert!(!info.primary_xmp_has_gain_map);
        assert_eq!(info.gain_map_item_count, 0);
    }

    #[test]
    fn ultra_hdr_original_gain_map_jpeg_is_extractable() {
        let Some(root) = ultra_hdr_samples_root() else {
            eprintln!(
                "skipping Ultra HDR corpus test; set SIV_ULTRA_HDR_SAMPLES_DIR to Ultra_HDR_Samples"
            );
            return;
        };
        let path = sample_path(&root, "Originals/Ultra_HDR_Samples_Originals_01.jpg");
        if !path.is_file() {
            eprintln!("skipping Ultra HDR gain map extraction test; sample missing");
            return;
        }

        let gain_map_jpeg = extract_gain_map_jpeg(&path).expect("extract embedded gain map JPEG");
        let (width, height, pixels) =
            libjpeg_turbo::decode_to_rgba(gain_map_jpeg.as_slice()).expect("decode gain map JPEG");

        assert_eq!((width, height), (1020, 768));
        assert_eq!(pixels.len(), width as usize * height as usize * 4);
    }

    #[test]
    fn ultra_hdr_original_decodes_to_hdr_float_buffer() {
        let Some(root) = ultra_hdr_samples_root() else {
            eprintln!(
                "skipping Ultra HDR corpus test; set SIV_ULTRA_HDR_SAMPLES_DIR to Ultra_HDR_Samples"
            );
            return;
        };
        let path = sample_path(&root, "Originals/Ultra_HDR_Samples_Originals_01.jpg");
        if !path.is_file() {
            eprintln!("skipping Ultra HDR decode test; sample missing");
            return;
        }

        let hdr = decode_ultra_hdr_jpeg(&path).expect("decode Ultra HDR JPEG_R");

        assert_eq!((hdr.width, hdr.height), (4080, 3072));
        assert_eq!(hdr.format, crate::hdr::types::HdrPixelFormat::Rgba32Float);
        assert_eq!(
            hdr.color_space,
            crate::hdr::types::HdrColorSpace::LinearSrgb
        );
        assert_eq!(
            hdr.rgba_f32.len(),
            hdr.width as usize * hdr.height as usize * 4
        );
        assert!(
            hdr.rgba_f32
                .chunks_exact(4)
                .any(|pixel| pixel[0] > 1.0 || pixel[1] > 1.0 || pixel[2] > 1.0),
            "Ultra HDR decode should recover highlights above SDR white"
        );
    }

    #[test]
    fn gain_map_sampling_interpolates_between_source_pixels() {
        let gain_rgba = vec![
            0, 0, 0, 255, //
            255, 255, 255, 255,
        ];

        let sampled = sample_gain_map_value(&gain_rgba, 2, 1, 1, 0, 3, 1);

        assert!((sampled - 0.5).abs() < 0.01);
    }

    #[test]
    fn gain_map_item_length_accepts_length_after_semantic() {
        let xmp = r#"
            <Container:Item
              Item:Mime="image/jpeg"
              Item:Semantic="GainMap"
              Item:Length="12345"/>
        "#;

        assert_eq!(gain_map_item_length(xmp), Some(12345));
    }

    #[test]
    fn gain_map_metadata_parses_hdr_capacity_bounds() {
        let gain_map_jpeg = minimal_jpeg_with_app1_xmp(
            r#"
            <rdf:Description
              xmlns:hdrgm="http://ns.adobe.com/hdr-gain-map/1.0/"
              hdrgm:Version="1.0"
              hdrgm:GainMapMax="3.0"
              hdrgm:HDRCapacityMin="1.25"
              hdrgm:HDRCapacityMax="4.5"/>
        "#,
        );

        let metadata = gain_map_metadata(&gain_map_jpeg).expect("parse gain map metadata");

        assert_eq!(metadata.hdr_capacity_min, 1.25);
        assert_eq!(metadata.hdr_capacity_max, 4.5);
    }

    #[test]
    fn hdr_orientation_rotates_float_buffer_like_exif_orientation() {
        let hdr = HdrImageBuffer {
            width: 2,
            height: 1,
            format: HdrPixelFormat::Rgba32Float,
            color_space: HdrColorSpace::LinearSrgb,
            rgba_f32: Arc::new(vec![
                1.0, 0.0, 0.0, 1.0, //
                0.0, 1.0, 0.0, 1.0,
            ]),
        };

        let oriented = apply_orientation_to_hdr_buffer(hdr, 6);

        assert_eq!((oriented.width, oriented.height), (1, 2));
        assert_eq!(
            oriented.rgba_f32.as_slice(),
            &[
                1.0, 0.0, 0.0, 1.0, //
                0.0, 1.0, 0.0, 1.0,
            ]
        );
    }

    #[test]
    fn display_to_physical_maps_orientation_six() {
        assert_eq!(display_to_physical_pixel(0, 0, 2, 1, 6), (0, 0));
        assert_eq!(display_to_physical_pixel(0, 1, 2, 1, 6), (1, 0));
    }

    #[test]
    fn hdr_capacity_scales_gain_map_application() {
        let metadata = GainMapMetadata {
            gain_map_min: 0.0,
            gain_map_max: 2.0,
            gamma: 1.0,
            offset_sdr: 0.0,
            offset_hdr: 0.0,
            hdr_capacity_min: 1.0,
            hdr_capacity_max: 3.0,
        };

        assert_eq!(gain_map_weight(metadata, 0.5), 0.0);
        assert!((gain_map_weight(metadata, 2.0) - 0.5).abs() < 0.001);
        assert_eq!(gain_map_weight(metadata, 4.0), 1.0);
    }

    #[test]
    fn hdr_capacity_weight_changes_recovered_hdr_pixel() {
        let metadata = GainMapMetadata {
            gain_map_min: 0.0,
            gain_map_max: 2.0,
            gamma: 1.0,
            offset_sdr: 0.0,
            offset_hdr: 0.0,
            hdr_capacity_min: 0.0,
            hdr_capacity_max: 2.0,
        };
        let sdr = [255, 255, 255, 255];

        let low = recover_hdr_channel_from_sdr_and_gain(255, 1.0, metadata, 0.0);
        let mid = recover_hdr_channel_from_sdr_and_gain(255, 1.0, metadata, 1.0);
        let high = recover_hdr_channel_from_sdr_and_gain(255, 1.0, metadata, 2.0);

        assert!((low - 1.0).abs() < 0.001);
        assert!(mid > low && mid < high);
        assert!((high - 4.0).abs() < 0.001);

        let mut rgba = Vec::new();
        append_hdr_pixel_from_sdr_and_gain(&mut rgba, &sdr, 1.0, metadata, 2.0);
        assert!((rgba[0] - high).abs() < 0.001);
    }

    fn minimal_jpeg_with_app1_xmp(xmp: &str) -> Vec<u8> {
        let payload = format!("http://ns.adobe.com/xap/1.0/\0{xmp}");
        let len = u16::try_from(payload.len() + 2).expect("test XMP fits in JPEG segment");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&[0xFF, 0xD8, 0xFF, 0xE1]);
        bytes.extend_from_slice(&len.to_be_bytes());
        bytes.extend_from_slice(payload.as_bytes());
        bytes.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x02, 0xFF, 0xD9]);
        bytes
    }
}
