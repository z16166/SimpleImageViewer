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

#[cfg(test)]
use std::cell::Cell;

use crate::hdr::tiled::{
    HdrTileBuffer, HdrTileCache, HdrTiledSource, HdrTiledSourceKind,
    configured_hdr_tile_cache_max_bytes, validate_tile_bounds,
};
use crate::hdr::types::{HdrColorSpace, HdrImageBuffer, HdrPixelFormat};

#[cfg(test)]
use crate::hdr::types::HdrToneMapSettings;

#[cfg(test)]
use std::path::Path;

const JPEG_SOI: [u8; 2] = [0xFF, 0xD8];
const JPEG_SOS: u8 = 0xDA;
const JPEG_EOI: u8 = 0xD9;
const JPEG_APP1: u8 = 0xE1;
const JPEG_APP2: u8 = 0xE2;
const HDR_GAIN_MAP_NAMESPACE: &str = "http://ns.adobe.com/hdr-gain-map/1.0/";
const HDR_GAIN_MAP_VERSION: &str = "hdrgm:Version";
const ISO_GAIN_MAP_NAMESPACE: &[u8] = b"urn:iso:std:iso:ts:21496:-1\0";
const ISO_MULTI_CHANNEL_FLAG: u8 = 1 << 7;
const ISO_BACKWARD_DIRECTION_FLAG: u8 = 1 << 2;
const ISO_COMMON_DENOMINATOR_FLAG: u8 = 1 << 3;
#[cfg(test)]
thread_local! {
    static BASE_JPEG_DECODE_COUNT: Cell<usize> = const { Cell::new(0) };
}

fn decode_base_jpeg_rgba(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    #[cfg(test)]
    BASE_JPEG_DECODE_COUNT.with(|count| count.set(count.get() + 1));
    libjpeg_turbo::decode_to_rgba(bytes)
}

#[cfg(test)]
fn reset_base_jpeg_decode_count() {
    BASE_JPEG_DECODE_COUNT.with(|count| count.set(0));
}

#[cfg(test)]
fn base_jpeg_decode_count() -> usize {
    BASE_JPEG_DECODE_COUNT.with(Cell::get)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UltraHdrJpegInfo {
    pub(crate) is_ultra_hdr: bool,
    pub(crate) primary_xmp_has_gain_map: bool,
    pub(crate) gain_map_item_count: usize,
}

#[cfg(test)]
fn inspect_ultra_hdr_jpeg(path: &Path) -> Result<UltraHdrJpegInfo, String> {
    let file = std::fs::File::open(path).map_err(|err| err.to_string())?;
    let bytes = unsafe { memmap2::Mmap::map(&file).map_err(|err| err.to_string())? };
    inspect_ultra_hdr_jpeg_bytes(&bytes)
}

#[cfg(test)]
fn extract_gain_map_jpeg(path: &Path) -> Result<Vec<u8>, String> {
    let file = std::fs::File::open(path).map_err(|err| err.to_string())?;
    let bytes = unsafe { memmap2::Mmap::map(&file).map_err(|err| err.to_string())? };
    extract_gain_map_jpeg_bytes(&bytes)
}

#[cfg(test)]
fn decode_ultra_hdr_jpeg(path: &Path) -> Result<HdrImageBuffer, String> {
    let file = std::fs::File::open(path).map_err(|err| err.to_string())?;
    let bytes = unsafe { memmap2::Mmap::map(&file).map_err(|err| err.to_string())? };
    decode_ultra_hdr_jpeg_bytes_with_target_capacity(
        &bytes,
        HdrToneMapSettings::default().target_hdr_capacity(),
    )
}

pub(crate) fn decode_ultra_hdr_jpeg_bytes_with_target_capacity(
    bytes: &[u8],
    target_hdr_capacity: f32,
) -> Result<HdrImageBuffer, String> {
    let info = inspect_ultra_hdr_jpeg_bytes(bytes)?;
    if !info.is_ultra_hdr {
        return Err("JPEG does not advertise Ultra HDR gain map metadata".to_string());
    }

    let (width, height, sdr_rgba) = libjpeg_turbo::decode_to_rgba(bytes)?;
    let gain_map_jpeg = extract_gain_map_jpeg_bytes(bytes)?;
    let metadata = gain_map_metadata(&gain_map_jpeg)?;
    log::debug!(
        "[HDR] Ultra HDR JPEG_R metadata: {}",
        gain_map_metadata_diagnostic(metadata, target_hdr_capacity)
    );
    let (gain_width, gain_height, gain_rgba) = libjpeg_turbo::decode_to_rgba(&gain_map_jpeg)?;

    let mut rgba_f32 = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for x in 0..width {
            let sdr_index = (y as usize * width as usize + x as usize) * 4;
            let gain_value =
                sample_gain_map_rgb(&gain_rgba, gain_width, gain_height, x, y, width, height);
            append_hdr_pixel_from_sdr_and_gain(
                &mut rgba_f32,
                &sdr_rgba[sdr_index..sdr_index + 4],
                gain_value,
                metadata,
                target_hdr_capacity,
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
    #[allow(dead_code)]
    path: PathBuf,
    width: u32,
    height: u32,
    physical_width: u32,
    physical_height: u32,
    orientation: u16,
    sdr_rgba: Arc<Vec<u8>>,
    gain_width: u32,
    gain_height: u32,
    gain_rgba: Arc<Vec<u8>>,
    metadata: GainMapMetadata,
    target_hdr_capacity: f32,
    tile_cache: Mutex<HdrTileCache>,
}

impl UltraHdrTiledImageSource {
    pub(crate) fn open_with_target_capacity(
        path: PathBuf,
        orientation: u16,
        target_hdr_capacity: f32,
    ) -> Result<Self, String> {
        let file = std::fs::File::open(&path).map_err(|err| err.to_string())?;
        let bytes = Arc::new(unsafe { memmap2::Mmap::map(&file).map_err(|err| err.to_string())? });
        let info = inspect_ultra_hdr_jpeg_bytes(&bytes)?;
        if !info.is_ultra_hdr {
            return Err("JPEG does not advertise Ultra HDR gain map metadata".to_string());
        }

        let (physical_width, physical_height, sdr_rgba) = decode_base_jpeg_rgba(&bytes)?;
        let (width, height) = oriented_dimensions(physical_width, physical_height, orientation);

        let gain_map_jpeg = extract_gain_map_jpeg_bytes(&bytes)?;
        let metadata = gain_map_metadata(&gain_map_jpeg)?;
        log::debug!(
            "[HDR] {}: Ultra HDR JPEG_R metadata: {}",
            path.display(),
            gain_map_metadata_diagnostic(metadata, target_hdr_capacity)
        );
        let (gain_width, gain_height, gain_rgba) = libjpeg_turbo::decode_to_rgba(&gain_map_jpeg)?;

        Ok(Self {
            path,
            width,
            height,
            physical_width,
            physical_height,
            orientation,
            sdr_rgba: Arc::new(sdr_rgba),
            gain_width,
            gain_height,
            gain_rgba: Arc::new(gain_rgba),
            metadata,
            target_hdr_capacity,
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
                let gain_value = sample_gain_map_rgb(
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
                    &self.sdr_rgba[sdr_index..sdr_index + 4],
                    gain_value,
                    self.metadata,
                    self.target_hdr_capacity,
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
    gain_map_min: [f32; 3],
    gain_map_max: [f32; 3],
    gamma: [f32; 3],
    offset_sdr: [f32; 3],
    offset_hdr: [f32; 3],
    hdr_capacity_min: f32,
    hdr_capacity_max: f32,
}

fn gain_map_metadata(gain_map_jpeg: &[u8]) -> Result<GainMapMetadata, String> {
    let segments = primary_metadata_segments(gain_map_jpeg)?;
    for segment in segments
        .iter()
        .filter(|segment| segment.marker == JPEG_APP2)
    {
        if let Some(iso_metadata) = iso_gain_map_metadata(segment.payload) {
            return iso_metadata;
        }
    }

    for segment in segments
        .iter()
        .filter(|segment| segment.marker == JPEG_APP1)
    {
        let text = String::from_utf8_lossy(segment.payload);
        if !text.contains(HDR_GAIN_MAP_NAMESPACE) || !text.contains(HDR_GAIN_MAP_VERSION) {
            continue;
        }
        if attribute_bool(&text, "hdrgm:BaseRenditionIsHDR").unwrap_or(false) {
            return Err(
                "Ultra HDR gain map BaseRenditionIsHDR=True is not supported yet".to_string(),
            );
        }
        let gain_map_max = attribute_rgb_f32(&text, "hdrgm:GainMapMax")
            .ok_or_else(|| "Ultra HDR gain map metadata missing GainMapMax".to_string())?;
        let max_gain_map_max = gain_map_max
            .iter()
            .copied()
            .fold(f32::NEG_INFINITY, f32::max);
        return validate_gain_map_metadata(GainMapMetadata {
            gain_map_min: attribute_rgb_f32(&text, "hdrgm:GainMapMin").unwrap_or([0.0; 3]),
            gain_map_max,
            gamma: attribute_rgb_f32(&text, "hdrgm:Gamma").unwrap_or([1.0; 3]),
            offset_sdr: attribute_rgb_f32(&text, "hdrgm:OffsetSDR").unwrap_or([1.0 / 64.0; 3]),
            offset_hdr: attribute_rgb_f32(&text, "hdrgm:OffsetHDR").unwrap_or([1.0 / 64.0; 3]),
            hdr_capacity_min: attribute_f32(&text, "hdrgm:HDRCapacityMin").unwrap_or(0.0),
            hdr_capacity_max: attribute_f32(&text, "hdrgm:HDRCapacityMax")
                .unwrap_or(max_gain_map_max),
        });
    }

    Err("Ultra HDR gain map metadata not found".to_string())
}

fn gain_map_metadata_diagnostic(metadata: GainMapMetadata, target_hdr_capacity: f32) -> String {
    format!(
        "GainMapMin={} GainMapMax={} Gamma={} OffsetSDR={} OffsetHDR={} HDRCapacity=[{:.3},{:.3}] target={:.3} weight={:.3}",
        format_rgb_triplet(metadata.gain_map_min),
        format_rgb_triplet(metadata.gain_map_max),
        format_rgb_triplet(metadata.gamma),
        format_rgb_triplet(metadata.offset_sdr),
        format_rgb_triplet(metadata.offset_hdr),
        metadata.hdr_capacity_min,
        metadata.hdr_capacity_max,
        target_hdr_capacity,
        gain_map_weight(metadata, target_hdr_capacity),
    )
}

fn format_rgb_triplet(values: [f32; 3]) -> String {
    format!("[{:.3},{:.3},{:.3}]", values[0], values[1], values[2])
}

fn validate_gain_map_metadata(metadata: GainMapMetadata) -> Result<GainMapMetadata, String> {
    validate_finite_triplet("GainMapMin", metadata.gain_map_min)?;
    validate_finite_triplet("GainMapMax", metadata.gain_map_max)?;
    validate_finite_triplet("OffsetSDR", metadata.offset_sdr)?;
    validate_finite_triplet("OffsetHDR", metadata.offset_hdr)?;
    for gamma in metadata.gamma {
        if !gamma.is_finite() || gamma <= 0.0 {
            return Err("Ultra HDR gain map metadata has non-positive Gamma".to_string());
        }
    }
    if !metadata.hdr_capacity_min.is_finite() || !metadata.hdr_capacity_max.is_finite() {
        return Err("Ultra HDR gain map metadata has non-finite HDRCapacity".to_string());
    }
    Ok(metadata)
}

fn validate_finite_triplet(name: &str, values: [f32; 3]) -> Result<(), String> {
    if values.iter().all(|value| value.is_finite()) {
        Ok(())
    } else {
        Err(format!("Ultra HDR gain map metadata has non-finite {name}"))
    }
}

fn iso_gain_map_metadata(payload: &[u8]) -> Option<Result<GainMapMetadata, String>> {
    payload
        .strip_prefix(ISO_GAIN_MAP_NAMESPACE)
        .map(parse_iso_gain_map_metadata)
}

fn parse_iso_gain_map_metadata(metadata: &[u8]) -> Result<GainMapMetadata, String> {
    let mut reader = ByteReader::new(metadata);
    let min_version = reader.read_u16()?;
    if min_version != 0 {
        return Err(format!(
            "unsupported ISO 21496-1 gain map metadata minimum version {min_version}"
        ));
    }
    let _writer_version = reader.read_u16()?;
    let flags = reader.read_u8()?;
    if flags & ISO_BACKWARD_DIRECTION_FLAG != 0 {
        return Err("ISO 21496-1 HDR base gain maps are not supported yet".to_string());
    }

    let channel_count = if flags & ISO_MULTI_CHANNEL_FLAG != 0 {
        3
    } else {
        1
    };
    let common_denominator = flags & ISO_COMMON_DENOMINATOR_FLAG != 0;
    let mut fraction = IsoGainMapFraction::default();

    if common_denominator {
        let denominator = reader.read_u32()?;
        fraction.base_hdr_headroom = (reader.read_u32()?, denominator);
        fraction.alternate_hdr_headroom = (reader.read_u32()?, denominator);
        for channel in 0..channel_count {
            fraction.gain_map_min[channel] = (reader.read_i32()?, denominator);
            fraction.gain_map_max[channel] = (reader.read_i32()?, denominator);
            fraction.gamma[channel] = (reader.read_u32()?, denominator);
            fraction.base_offset[channel] = (reader.read_i32()?, denominator);
            fraction.alternate_offset[channel] = (reader.read_i32()?, denominator);
        }
    } else {
        fraction.base_hdr_headroom = (reader.read_u32()?, reader.read_u32()?);
        fraction.alternate_hdr_headroom = (reader.read_u32()?, reader.read_u32()?);
        for channel in 0..channel_count {
            fraction.gain_map_min[channel] = (reader.read_i32()?, reader.read_u32()?);
            fraction.gain_map_max[channel] = (reader.read_i32()?, reader.read_u32()?);
            fraction.gamma[channel] = (reader.read_u32()?, reader.read_u32()?);
            fraction.base_offset[channel] = (reader.read_i32()?, reader.read_u32()?);
            fraction.alternate_offset[channel] = (reader.read_i32()?, reader.read_u32()?);
        }
    }

    if channel_count == 1 {
        for channel in 1..3 {
            fraction.gain_map_min[channel] = fraction.gain_map_min[0];
            fraction.gain_map_max[channel] = fraction.gain_map_max[0];
            fraction.gamma[channel] = fraction.gamma[0];
            fraction.base_offset[channel] = fraction.base_offset[0];
            fraction.alternate_offset[channel] = fraction.alternate_offset[0];
        }
    }

    fraction.into_gain_map_metadata()
}

#[derive(Debug, Clone, Copy)]
struct IsoGainMapFraction {
    gain_map_min: [(i32, u32); 3],
    gain_map_max: [(i32, u32); 3],
    gamma: [(u32, u32); 3],
    base_offset: [(i32, u32); 3],
    alternate_offset: [(i32, u32); 3],
    base_hdr_headroom: (u32, u32),
    alternate_hdr_headroom: (u32, u32),
}

impl Default for IsoGainMapFraction {
    fn default() -> Self {
        Self {
            gain_map_min: [(0, 1); 3],
            gain_map_max: [(0, 1); 3],
            gamma: [(1, 1); 3],
            base_offset: [(0, 1); 3],
            alternate_offset: [(0, 1); 3],
            base_hdr_headroom: (0, 1),
            alternate_hdr_headroom: (0, 1),
        }
    }
}

impl IsoGainMapFraction {
    fn into_gain_map_metadata(self) -> Result<GainMapMetadata, String> {
        let mut gain_map_min = [0.0; 3];
        let mut gain_map_max = [0.0; 3];
        let mut gamma = [1.0; 3];
        let mut offset_sdr = [0.0; 3];
        let mut offset_hdr = [0.0; 3];

        for channel in 0..3 {
            gain_map_min[channel] = signed_fraction(self.gain_map_min[channel])?;
            gain_map_max[channel] = signed_fraction(self.gain_map_max[channel])?;
            gamma[channel] = unsigned_fraction(self.gamma[channel])?;
            offset_sdr[channel] = signed_fraction(self.base_offset[channel])?;
            offset_hdr[channel] = signed_fraction(self.alternate_offset[channel])?;
        }

        validate_gain_map_metadata(GainMapMetadata {
            gain_map_min,
            gain_map_max,
            gamma,
            offset_sdr,
            offset_hdr,
            hdr_capacity_min: 2.0_f32.powf(unsigned_fraction(self.base_hdr_headroom)?),
            hdr_capacity_max: 2.0_f32.powf(unsigned_fraction(self.alternate_hdr_headroom)?),
        })
    }
}

fn signed_fraction((numerator, denominator): (i32, u32)) -> Result<f32, String> {
    if denominator == 0 {
        return Err("ISO 21496-1 gain map metadata has zero denominator".to_string());
    }
    Ok(numerator as f32 / denominator as f32)
}

fn unsigned_fraction((numerator, denominator): (u32, u32)) -> Result<f32, String> {
    if denominator == 0 {
        return Err("ISO 21496-1 gain map metadata has zero denominator".to_string());
    }
    Ok(numerator as f32 / denominator as f32)
}

struct ByteReader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> ByteReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn read_u8(&mut self) -> Result<u8, String> {
        if self.offset >= self.bytes.len() {
            return Err("truncated ISO 21496-1 gain map metadata".to_string());
        }
        let value = self.bytes[self.offset];
        self.offset += 1;
        Ok(value)
    }

    fn read_u16(&mut self) -> Result<u16, String> {
        let bytes = self.read_array::<2>()?;
        Ok(u16::from_be_bytes(bytes))
    }

    fn read_u32(&mut self) -> Result<u32, String> {
        let bytes = self.read_array::<4>()?;
        Ok(u32::from_be_bytes(bytes))
    }

    fn read_i32(&mut self) -> Result<i32, String> {
        let bytes = self.read_array::<4>()?;
        Ok(i32::from_be_bytes(bytes))
    }

    fn read_array<const N: usize>(&mut self) -> Result<[u8; N], String> {
        if self.offset + N > self.bytes.len() {
            return Err("truncated ISO 21496-1 gain map metadata".to_string());
        }
        let mut out = [0; N];
        out.copy_from_slice(&self.bytes[self.offset..self.offset + N]);
        self.offset += N;
        Ok(out)
    }
}

fn attribute_f32(text: &str, name: &str) -> Option<f32> {
    parse_quoted_attribute(text, name)?.parse().ok()
}

fn attribute_bool(text: &str, name: &str) -> Option<bool> {
    match parse_quoted_attribute(text, name)?
        .to_ascii_lowercase()
        .as_str()
    {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn attribute_rgb_f32(text: &str, name: &str) -> Option<[f32; 3]> {
    if let Some(value) = attribute_f32(text, name) {
        return Some([value; 3]);
    }

    let open_tag = format!("<{name}");
    let close_tag = format!("</{name}>");
    let open_start = text.find(&open_tag)?;
    let body_start = text[open_start..].find('>')? + open_start + 1;
    let body_end = text[body_start..].find(&close_tag)? + body_start;
    let body = &text[body_start..body_end];
    let mut values = Vec::new();
    let mut offset = 0;
    while let Some(li_start_rel) = body[offset..].find("<rdf:li") {
        let li_start = offset + li_start_rel;
        let value_start = body[li_start..].find('>')? + li_start + 1;
        let value_end = body[value_start..].find("</rdf:li>")? + value_start;
        values.push(body[value_start..value_end].trim().parse::<f32>().ok()?);
        offset = value_end + "</rdf:li>".len();
    }

    match values.as_slice() {
        [value] => Some([*value; 3]),
        [r, g, b] => Some([*r, *g, *b]),
        _ => None,
    }
}

fn append_hdr_pixel_from_sdr_and_gain(
    rgba_f32: &mut Vec<f32>,
    sdr_rgba: &[u8],
    gain_value: [f32; 3],
    metadata: GainMapMetadata,
    target_hdr_capacity: f32,
) {
    for (channel_index, channel) in sdr_rgba[..3].iter().enumerate() {
        rgba_f32.push(recover_hdr_channel_from_sdr_and_gain(
            *channel,
            gain_value[channel_index],
            metadata,
            channel_index,
            target_hdr_capacity,
        ));
    }
    rgba_f32.push(f32::from(sdr_rgba[3]) / 255.0);
}

fn recover_hdr_channel_from_sdr_and_gain(
    sdr_channel: u8,
    gain_value: f32,
    metadata: GainMapMetadata,
    channel_index: usize,
    target_hdr_capacity: f32,
) -> f32 {
    let channel_index = channel_index.min(2);
    let gain_weight = gain_map_weight(metadata, target_hdr_capacity);
    let log_boost = metadata.gain_map_min[channel_index]
        + (metadata.gain_map_max[channel_index] - metadata.gain_map_min[channel_index])
            * gain_value.powf(1.0 / metadata.gamma[channel_index].max(f32::MIN_POSITIVE))
            * gain_weight;
    let boost = 2.0_f32.powf(log_boost);

    let linear_sdr = srgb_u8_to_linear_f32(sdr_channel);
    ((linear_sdr + metadata.offset_sdr[channel_index]) * boost - metadata.offset_hdr[channel_index])
        .max(0.0)
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

fn sample_gain_map_rgb(
    gain_rgba: &[u8],
    gain_width: u32,
    gain_height: u32,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
) -> [f32; 3] {
    if gain_width == 0 || gain_height == 0 || width == 0 || height == 0 {
        return [0.0; 3];
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

    let mut out = [0.0; 3];
    for (channel_index, channel) in out.iter_mut().enumerate() {
        let top = lerp(
            gain_map_channel(gain_rgba, gain_width, x0, y0, channel_index),
            gain_map_channel(gain_rgba, gain_width, x1, y0, channel_index),
            tx,
        );
        let bottom = lerp(
            gain_map_channel(gain_rgba, gain_width, x0, y1, channel_index),
            gain_map_channel(gain_rgba, gain_width, x1, y1, channel_index),
            tx,
        );
        *channel = lerp(top, bottom, ty);
    }
    out
}

fn gain_map_channel(
    gain_rgba: &[u8],
    gain_width: u32,
    x: u32,
    y: u32,
    channel_index: usize,
) -> f32 {
    let index = (y as usize * gain_width as usize + x as usize) * 4;
    f32::from(gain_rgba[index + channel_index.min(2)]) / 255.0
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
    fn tiled_source_reuses_base_jpeg_decode_for_distinct_tiles() {
        let Some(root) = ultra_hdr_samples_root() else {
            eprintln!(
                "skipping Ultra HDR corpus test; set SIV_ULTRA_HDR_SAMPLES_DIR to Ultra_HDR_Samples"
            );
            return;
        };
        let path = sample_path(&root, "Originals/Ultra_HDR_Samples_Originals_01.jpg");
        if !path.is_file() {
            eprintln!("skipping Ultra HDR tiled decode count test; sample missing");
            return;
        }

        reset_base_jpeg_decode_count();
        let source = UltraHdrTiledImageSource::open_with_target_capacity(
            path,
            1,
            HdrToneMapSettings::default().target_hdr_capacity(),
        )
        .expect("open Ultra HDR tiled source");

        source
            .extract_tile_rgba32f_arc(0, 0, 64, 64)
            .expect("extract first Ultra HDR tile");
        source
            .extract_tile_rgba32f_arc(64, 0, 64, 64)
            .expect("extract second Ultra HDR tile");

        assert_eq!(
            base_jpeg_decode_count(),
            1,
            "Ultra HDR tiled source should decode the base JPEG once and reuse it for distinct tiles"
        );
    }

    #[test]
    fn tiled_source_uses_target_hdr_capacity() {
        let Some(root) = ultra_hdr_samples_root() else {
            eprintln!(
                "skipping Ultra HDR corpus test; set SIV_ULTRA_HDR_SAMPLES_DIR to Ultra_HDR_Samples"
            );
            return;
        };
        let path = sample_path(&root, "Originals/Ultra_HDR_Samples_Originals_01.jpg");
        if !path.is_file() {
            eprintln!("skipping Ultra HDR tiled target capacity test; sample missing");
            return;
        }

        let low = UltraHdrTiledImageSource::open_with_target_capacity(path.clone(), 1, 1.0)
            .expect("open low-capacity Ultra HDR tiled source")
            .extract_tile_rgba32f_arc(0, 0, 64, 64)
            .expect("extract low-capacity tile");
        let high = UltraHdrTiledImageSource::open_with_target_capacity(path, 1, 8.0)
            .expect("open high-capacity Ultra HDR tiled source")
            .extract_tile_rgba32f_arc(0, 0, 64, 64)
            .expect("extract high-capacity tile");

        let low_peak = low
            .rgba_f32
            .chunks_exact(4)
            .map(|pixel| pixel[0].max(pixel[1]).max(pixel[2]))
            .fold(0.0_f32, f32::max);
        let high_peak = high
            .rgba_f32
            .chunks_exact(4)
            .map(|pixel| pixel[0].max(pixel[1]).max(pixel[2]))
            .fold(0.0_f32, f32::max);

        assert!(
            high_peak > low_peak,
            "higher target HDR capacity should recover brighter tiled JPEG_R highlights"
        );
    }

    #[test]
    fn gain_map_sampling_interpolates_between_source_pixels() {
        let gain_rgba = vec![
            0, 0, 0, 255, //
            255, 255, 255, 255,
        ];

        let sampled = sample_gain_map_rgb(&gain_rgba, 2, 1, 1, 0, 3, 1)[0];

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
    fn gain_map_metadata_diagnostic_reports_recovery_parameters() {
        let metadata = GainMapMetadata {
            gain_map_min: [0.1, 0.2, 0.3],
            gain_map_max: [1.0, 2.0, 3.0],
            gamma: [1.0, 1.5, 2.0],
            offset_sdr: [0.01, 0.02, 0.03],
            offset_hdr: [0.04, 0.05, 0.06],
            hdr_capacity_min: 1.25,
            hdr_capacity_max: 4.5,
        };

        let diagnostic = gain_map_metadata_diagnostic(metadata, 3.0);

        assert!(diagnostic.contains("GainMapMin=[0.100,0.200,0.300]"));
        assert!(diagnostic.contains("GainMapMax=[1.000,2.000,3.000]"));
        assert!(diagnostic.contains("Gamma=[1.000,1.500,2.000]"));
        assert!(diagnostic.contains("OffsetSDR=[0.010,0.020,0.030]"));
        assert!(diagnostic.contains("OffsetHDR=[0.040,0.050,0.060]"));
        assert!(diagnostic.contains("HDRCapacity=[1.250,4.500]"));
        assert!(diagnostic.contains("target=3.000"));
    }

    #[test]
    fn gain_map_metadata_rejects_hdr_base_rendition() {
        let gain_map_jpeg = minimal_jpeg_with_app1_xmp(
            r#"
            <rdf:Description
              xmlns:hdrgm="http://ns.adobe.com/hdr-gain-map/1.0/"
              hdrgm:Version="1.0"
              hdrgm:GainMapMax="3.0"
              hdrgm:BaseRenditionIsHDR="True"/>
        "#,
        );

        let err =
            gain_map_metadata(&gain_map_jpeg).expect_err("HDR base gain maps are unsupported");

        assert!(
            err.contains("BaseRenditionIsHDR"),
            "unexpected error message: {err}"
        );
    }

    #[test]
    fn gain_map_metadata_prefers_iso_over_xmp() {
        let mut iso = Vec::new();
        write_iso_common_denominator_metadata(
            &mut iso,
            10,
            0,
            20,
            &[(0, 30, 10, 0, 0), (1, 31, 11, 1, 1), (2, 32, 12, 2, 2)],
        );
        let gain_map_jpeg = minimal_jpeg_with_app1_xmp_and_app2_iso(
            r#"
            <rdf:Description
              xmlns:hdrgm="http://ns.adobe.com/hdr-gain-map/1.0/"
              hdrgm:Version="1.0"
              hdrgm:GainMapMax="1.0"
              hdrgm:HDRCapacityMax="1.0"/>
        "#,
            &iso,
        );

        let metadata = gain_map_metadata(&gain_map_jpeg).expect("parse ISO gain map metadata");

        assert_eq!(metadata.gain_map_min, [0.0, 0.1, 0.2]);
        assert_eq!(metadata.gain_map_max, [3.0, 3.1, 3.2]);
        assert_eq!(metadata.gamma, [1.0, 1.1, 1.2]);
        assert_eq!(metadata.offset_sdr, [0.0, 0.1, 0.2]);
        assert_eq!(metadata.offset_hdr, [0.0, 0.1, 0.2]);
        assert_eq!(metadata.hdr_capacity_min, 1.0);
        assert_eq!(metadata.hdr_capacity_max, 4.0);
    }

    #[test]
    fn gain_map_metadata_parses_ordered_rgb_values() {
        let gain_map_jpeg = minimal_jpeg_with_app1_xmp(
            r#"
            <rdf:Description
              xmlns:hdrgm="http://ns.adobe.com/hdr-gain-map/1.0/"
              hdrgm:Version="1.0"
              hdrgm:HDRCapacityMax="4.0">
              <hdrgm:GainMapMin>
                <rdf:Seq><rdf:li>0.1</rdf:li><rdf:li>0.2</rdf:li><rdf:li>0.3</rdf:li></rdf:Seq>
              </hdrgm:GainMapMin>
              <hdrgm:GainMapMax>
                <rdf:Seq><rdf:li>1.0</rdf:li><rdf:li>2.0</rdf:li><rdf:li>3.0</rdf:li></rdf:Seq>
              </hdrgm:GainMapMax>
              <hdrgm:Gamma>
                <rdf:Seq><rdf:li>1.0</rdf:li><rdf:li>2.0</rdf:li><rdf:li>4.0</rdf:li></rdf:Seq>
              </hdrgm:Gamma>
            </rdf:Description>
        "#,
        );

        let metadata = gain_map_metadata(&gain_map_jpeg).expect("parse RGB gain map metadata");

        assert_eq!(metadata.gain_map_min, [0.1, 0.2, 0.3]);
        assert_eq!(metadata.gain_map_max, [1.0, 2.0, 3.0]);
        assert_eq!(metadata.gamma, [1.0, 2.0, 4.0]);
    }

    #[test]
    fn gain_map_metadata_rejects_non_positive_gamma() {
        let gain_map_jpeg = minimal_jpeg_with_app1_xmp(
            r#"
            <rdf:Description
              xmlns:hdrgm="http://ns.adobe.com/hdr-gain-map/1.0/"
              hdrgm:Version="1.0"
              hdrgm:GainMapMax="3.0"
              hdrgm:Gamma="0.0"/>
        "#,
        );

        let err = gain_map_metadata(&gain_map_jpeg).expect_err("reject non-positive gamma");

        assert!(err.contains("Gamma"));
    }

    #[test]
    fn gain_map_offsets_and_gamma_affect_recovered_hdr_pixel() {
        let metadata = GainMapMetadata {
            gain_map_min: [0.0; 3],
            gain_map_max: [4.0; 3],
            gamma: [2.0; 3],
            offset_sdr: [0.25; 3],
            offset_hdr: [0.10; 3],
            hdr_capacity_min: 0.0,
            hdr_capacity_max: 2.0,
        };

        let recovered = recover_hdr_channel_from_sdr_and_gain(255, 0.25, metadata, 0, 2.0);

        assert!((recovered - 4.9).abs() < 0.001);
    }

    #[test]
    fn gain_map_sampling_preserves_rgb_channels() {
        let gain_rgba = vec![0, 64, 128, 255];

        let sampled = sample_gain_map_rgb(&gain_rgba, 1, 1, 0, 0, 1, 1);

        assert!((sampled[0] - 0.0).abs() < 0.001);
        assert!((sampled[1] - 64.0 / 255.0).abs() < 0.001);
        assert!((sampled[2] - 128.0 / 255.0).abs() < 0.001);
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
            gain_map_min: [0.0; 3],
            gain_map_max: [2.0; 3],
            gamma: [1.0; 3],
            offset_sdr: [0.0; 3],
            offset_hdr: [0.0; 3],
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
            gain_map_min: [0.0; 3],
            gain_map_max: [2.0; 3],
            gamma: [1.0; 3],
            offset_sdr: [0.0; 3],
            offset_hdr: [0.0; 3],
            hdr_capacity_min: 0.0,
            hdr_capacity_max: 2.0,
        };
        let sdr = [255, 255, 255, 255];

        let low = recover_hdr_channel_from_sdr_and_gain(255, 1.0, metadata, 0, 0.0);
        let mid = recover_hdr_channel_from_sdr_and_gain(255, 1.0, metadata, 0, 1.0);
        let high = recover_hdr_channel_from_sdr_and_gain(255, 1.0, metadata, 0, 2.0);

        assert!((low - 1.0).abs() < 0.001);
        assert!(mid > low && mid < high);
        assert!((high - 4.0).abs() < 0.001);

        let mut rgba = Vec::new();
        append_hdr_pixel_from_sdr_and_gain(&mut rgba, &sdr, [1.0; 3], metadata, 2.0);
        assert!((rgba[0] - high).abs() < 0.001);
    }

    #[test]
    fn per_channel_metadata_changes_recovered_hdr_channels() {
        let metadata = GainMapMetadata {
            gain_map_min: [0.0; 3],
            gain_map_max: [1.0, 2.0, 3.0],
            gamma: [1.0; 3],
            offset_sdr: [0.0; 3],
            offset_hdr: [0.0; 3],
            hdr_capacity_min: 0.0,
            hdr_capacity_max: 3.0,
        };
        let mut rgba = Vec::new();

        append_hdr_pixel_from_sdr_and_gain(
            &mut rgba,
            &[255, 255, 255, 255],
            [1.0; 3],
            metadata,
            3.0,
        );

        assert!((rgba[0] - 2.0).abs() < 0.001);
        assert!((rgba[1] - 4.0).abs() < 0.001);
        assert!((rgba[2] - 8.0).abs() < 0.001);
    }

    #[test]
    fn ultra_hdr_decode_uses_target_hdr_capacity() {
        let Some(root) = ultra_hdr_samples_root() else {
            eprintln!(
                "skipping Ultra HDR corpus test; set SIV_ULTRA_HDR_SAMPLES_DIR to Ultra_HDR_Samples"
            );
            return;
        };
        let path = sample_path(&root, "Originals/Ultra_HDR_Samples_Originals_01.jpg");
        if !path.is_file() {
            eprintln!("skipping Ultra HDR target capacity test; sample missing");
            return;
        }
        let file = std::fs::File::open(&path).expect("open Ultra HDR sample");
        let bytes = unsafe { memmap2::Mmap::map(&file).expect("mmap Ultra HDR sample") };

        let low = decode_ultra_hdr_jpeg_bytes_with_target_capacity(&bytes, 1.0)
            .expect("decode low-capacity Ultra HDR");
        let high = decode_ultra_hdr_jpeg_bytes_with_target_capacity(&bytes, 8.0)
            .expect("decode high-capacity Ultra HDR");

        let low_peak = low
            .rgba_f32
            .chunks_exact(4)
            .map(|pixel| pixel[0].max(pixel[1]).max(pixel[2]))
            .fold(0.0_f32, f32::max);
        let high_peak = high
            .rgba_f32
            .chunks_exact(4)
            .map(|pixel| pixel[0].max(pixel[1]).max(pixel[2]))
            .fold(0.0_f32, f32::max);

        assert!(
            high_peak > low_peak,
            "higher target HDR capacity should recover brighter JPEG_R highlights"
        );
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

    fn minimal_jpeg_with_app1_xmp_and_app2_iso(xmp: &str, iso_metadata: &[u8]) -> Vec<u8> {
        let mut bytes = minimal_jpeg_with_app1_xmp(xmp);
        bytes.truncate(bytes.len() - 6);
        let mut payload = b"urn:iso:std:iso:ts:21496:-1\0".to_vec();
        payload.extend_from_slice(iso_metadata);
        let len = u16::try_from(payload.len() + 2).expect("test ISO metadata fits in JPEG segment");
        bytes.extend_from_slice(&[0xFF, 0xE2]);
        bytes.extend_from_slice(&len.to_be_bytes());
        bytes.extend_from_slice(&payload);
        bytes.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x02, 0xFF, 0xD9]);
        bytes
    }

    fn write_iso_common_denominator_metadata(
        out: &mut Vec<u8>,
        denominator: u32,
        base_hdr_headroom_n: u32,
        alternate_hdr_headroom_n: u32,
        channels: &[(i32, i32, u32, i32, i32); 3],
    ) {
        out.extend_from_slice(&0_u16.to_be_bytes());
        out.extend_from_slice(&0_u16.to_be_bytes());
        out.push(0x80 | 0x08);
        out.extend_from_slice(&denominator.to_be_bytes());
        out.extend_from_slice(&base_hdr_headroom_n.to_be_bytes());
        out.extend_from_slice(&alternate_hdr_headroom_n.to_be_bytes());
        for (gain_min, gain_max, gamma, offset_sdr, offset_hdr) in channels {
            out.extend_from_slice(&gain_min.to_be_bytes());
            out.extend_from_slice(&gain_max.to_be_bytes());
            out.extend_from_slice(&gamma.to_be_bytes());
            out.extend_from_slice(&offset_sdr.to_be_bytes());
            out.extend_from_slice(&offset_hdr.to_be_bytes());
        }
    }
}
