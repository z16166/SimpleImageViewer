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
use std::sync::Arc;

use parking_lot::Mutex;

#[cfg(test)]
use std::cell::Cell;

use crate::hdr::gain_map::{
    GainMapMetadata, append_hdr_pixel_from_sdr_and_gain, gain_map_metadata_diagnostic,
    iso_gain_map_metadata, iso_gain_map_skips_forward_compose, luminance_hints_from_gain_map,
    sample_gain_map_rgb, validate_gain_map_metadata,
};
#[cfg(test)]
use crate::hdr::gain_map::{gain_map_weight, recover_hdr_channel_from_sdr_and_gain};
#[cfg(test)]
use crate::hdr::jpeg_gain_map_gpu::attach_iso_gain_map_hdr_base_from_primary_rgba8;
use crate::hdr::jpeg_gain_map_gpu::{
    attach_iso_deferred_tile_metadata, iso_deferred_from_metadata,
};
use crate::hdr::mpf::{extract_mpf_gain_map_jpeg_from_bytes, mpf_app2_payload_has_gain_map_image};
use crate::hdr::tiled::{
    HdrTileBuffer, HdrTileCache, HdrTiledSource, HdrTiledSourceKind,
    configured_hdr_tile_cache_max_bytes, validate_tile_bounds,
};
use crate::hdr::types::{
    HdrColorSpace, HdrImageBuffer, HdrImageMetadata, HdrPixelFormat, IsoDeferredTileContext,
};
#[cfg(test)]
use crate::hdr::ultra_hdr_compose::compose_ultra_hdr_cpu;
pub(crate) use crate::hdr::ultra_hdr_deferred::{
    decode_ultra_hdr_jpeg_deferred_bytes, load_ultra_hdr_embedded_sdr_master_bytes,
};

#[cfg(test)]
use crate::hdr::types::{HdrToneMapSettings, HdrTransferFunction};

#[cfg(test)]
use std::path::Path;

const JPEG_SOI: [u8; 2] = [0xFF, 0xD8];
const JPEG_SOS: u8 = 0xDA;
const JPEG_EOI: u8 = 0xD9;
const JPEG_APP1: u8 = 0xE1;
const JPEG_APP2: u8 = 0xE2;
const HDR_GAIN_MAP_NAMESPACE: &str = "http://ns.adobe.com/hdr-gain-map/1.0/";
const HDR_GAIN_MAP_VERSION: &str = "hdrgm:Version";
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
    pub(crate) mpf_has_gain_map: bool,
}

#[cfg(test)]
fn inspect_ultra_hdr_jpeg(path: &Path) -> Result<UltraHdrJpegInfo, String> {
    let bytes = crate::mmap_util::map_file(path)?;
    inspect_ultra_hdr_jpeg_bytes(&bytes)
}

#[cfg(test)]
fn extract_gain_map_jpeg(path: &Path) -> Result<Vec<u8>, String> {
    let bytes = crate::mmap_util::map_file(path)?;
    extract_gain_map_jpeg_bytes(&bytes)
}

#[cfg(test)]
fn decode_ultra_hdr_jpeg(path: &Path) -> Result<HdrImageBuffer, String> {
    let bytes = crate::mmap_util::map_file(path)?;
    decode_ultra_hdr_jpeg_bytes_with_cpu_compose(
        &bytes,
        HdrToneMapSettings::default().target_hdr_capacity(),
    )
}

fn hdr_metadata_for_ultra_hdr_gain_map(gain: GainMapMetadata) -> HdrImageMetadata {
    let mut metadata = HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb);
    metadata.luminance = luminance_hints_from_gain_map(gain);
    metadata
}

pub(crate) fn decode_ultra_hdr_jpeg_bytes_with_target_capacity(
    bytes: &[u8],
    target_hdr_capacity: f32,
) -> Result<HdrImageBuffer, String> {
    decode_ultra_hdr_jpeg_deferred_bytes(bytes, target_hdr_capacity)
}

#[cfg(test)]
pub(crate) fn decode_ultra_hdr_jpeg_bytes_with_cpu_compose(
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

    if iso_gain_map_skips_forward_compose(metadata) {
        return attach_iso_gain_map_hdr_base_from_primary_rgba8(
            "JPEG_R", width, height, sdr_rgba, metadata,
        );
    }

    let (gain_width, gain_height, gain_rgba) = libjpeg_turbo::decode_to_rgba(&gain_map_jpeg)?;

    Ok(compose_ultra_hdr_cpu(
        crate::hdr::ultra_hdr_compose::UltraHdrComposeInput {
            width,
            height,
            sdr_rgba: &sdr_rgba,
            gain_rgba: &gain_rgba,
            gain_width,
            gain_height,
            metadata,
            image_metadata: hdr_metadata_for_ultra_hdr_gain_map(metadata),
            target_hdr_capacity,
        },
    ))
}

pub(crate) fn apply_orientation_to_hdr_buffer(
    buffer: HdrImageBuffer,
    orientation: u16,
) -> HdrImageBuffer {
    if orientation <= 1 {
        return buffer;
    }

    if iso_deferred_from_metadata(&buffer.metadata).is_some() {
        return crate::hdr::jpeg_gain_map_gpu::apply_orientation_to_iso_deferred_hdr_buffer(
            buffer,
            orientation,
        );
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
        metadata: buffer.metadata,
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
        let bytes = Arc::new(crate::mmap_util::map_file(&path)?);
        let info = inspect_ultra_hdr_jpeg_bytes(&bytes)?;
        if !info.is_ultra_hdr {
            return Err("JPEG does not advertise Ultra HDR gain map metadata".to_string());
        }

        let (physical_width, physical_height, sdr_rgba) = decode_base_jpeg_rgba(&bytes)?;
        let (width, height) = oriented_dimensions(physical_width, physical_height, orientation);

        let gain_map_jpeg = extract_gain_map_jpeg_bytes(&bytes)?;
        let metadata = gain_map_metadata(&gain_map_jpeg)?;
        if iso_gain_map_skips_forward_compose(metadata) {
            return Err(
                "Ultra HDR tiled deferred path requires forward gain-map direction".to_string(),
            );
        }
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

    fn source_name(&self) -> String {
        self.path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string())
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

    fn metadata(&self) -> HdrImageMetadata {
        hdr_metadata_for_ultra_hdr_gain_map(self.metadata)
    }

    fn generate_hdr_preview(&self, max_w: u32, max_h: u32) -> Result<HdrImageBuffer, String> {
        let (preview_width, preview_height) =
            crate::hdr::tiled::preview_dimensions(self.width, self.height, max_w, max_h);
        if preview_width == 0 || preview_height == 0 {
            return Err("HDR tiled preview dimensions must be non-zero".to_string());
        }

        let mut rgba_f32 = Vec::with_capacity(preview_width as usize * preview_height as usize * 4);
        for preview_y in 0..preview_height {
            let display_y =
                crate::hdr::tiled::preview_sample_coord(preview_y, preview_height, self.height);
            for preview_x in 0..preview_width {
                let display_x =
                    crate::hdr::tiled::preview_sample_coord(preview_x, preview_width, self.width);
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

        Ok(HdrImageBuffer {
            width: preview_width,
            height: preview_height,
            format: HdrPixelFormat::Rgba32Float,
            color_space: HdrColorSpace::LinearSrgb,
            metadata: self.metadata(),
            rgba_f32: Arc::new(rgba_f32),
        })
    }

    fn generate_sdr_preview(&self, max_w: u32, max_h: u32) -> Result<(u32, u32, Vec<u8>), String> {
        let preview = self.generate_hdr_preview(max_w, max_h)?;
        crate::hdr::tiled::sdr_preview_from_hdr_preview(&preview)
    }

    fn cached_tile_rgba32f_arc(
        &self,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    ) -> Option<Arc<HdrTileBuffer>> {
        self.tile_cache.lock().get((x, y, width, height))
    }

    fn protect_cached_tiles(&self, tiles: &[(u32, u32, u32, u32)]) {
        self.tile_cache
            .lock()
            .set_protected_keys(tiles.iter().copied());
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
        {
            let mut cache = self.tile_cache.lock();
            if let Some(tile) = cache.get(key) {
                return Ok(tile);
            }
        }

        let metadata = attach_iso_deferred_tile_metadata(
            crate::hdr::jpeg_gain_map_gpu::IsoDeferredTileMetadataInput {
                source: "JPEG_R",
                sdr_rgba: Arc::clone(&self.sdr_rgba),
                gain_rgba: Arc::clone(&self.gain_rgba),
                gain_width: self.gain_width,
                gain_height: self.gain_height,
                metadata: self.metadata,
                hdr_target_capacity: self.target_hdr_capacity,
                physical_width: self.physical_width,
                physical_height: self.physical_height,
            },
        );
        let tile = Arc::new(HdrTileBuffer::new_iso_deferred_tile(
            width,
            height,
            HdrColorSpace::LinearSrgb,
            metadata,
            IsoDeferredTileContext {
                origin_x: x,
                origin_y: y,
                physical_width: self.physical_width,
                physical_height: self.physical_height,
                orientation: self.orientation,
            },
        ));

        self.tile_cache.lock().insert(key, Arc::clone(&tile));

        Ok(tile)
    }
}

pub(crate) fn inspect_ultra_hdr_jpeg_bytes(bytes: &[u8]) -> Result<UltraHdrJpegInfo, String> {
    if !bytes.starts_with(&JPEG_SOI) {
        return Err("not a JPEG stream".to_string());
    }

    let mut primary_xmp_has_gain_map = false;
    let mut gain_map_item_count = 0;
    let mut mpf_has_gain_map = false;

    let segments = primary_metadata_segments(bytes)?;
    for segment in segments.iter() {
        if segment.marker == JPEG_APP1 {
            let text = String::from_utf8_lossy(segment.payload);
            if text.contains(HDR_GAIN_MAP_NAMESPACE) && text.contains(HDR_GAIN_MAP_VERSION) {
                primary_xmp_has_gain_map = true;
            }
            gain_map_item_count += text.matches("Item:Semantic=\"GainMap\"").count();
            gain_map_item_count += text.matches("Item:Semantic='GainMap'").count();
            gain_map_item_count += text.matches("Semantic=\"GainMap\"").count();
            gain_map_item_count += text.matches("Semantic='GainMap'").count();
        }
        if segment.marker == JPEG_APP2 && mpf_app2_payload_has_gain_map_image(segment.payload) {
            mpf_has_gain_map = true;
        }
    }

    Ok(UltraHdrJpegInfo {
        is_ultra_hdr: primary_xmp_has_gain_map && (gain_map_item_count > 0 || mpf_has_gain_map),
        primary_xmp_has_gain_map,
        gain_map_item_count,
        mpf_has_gain_map,
    })
}

/// Adobe XMP `hdrgm:HDRCapacity*` values are **log₂ headroom** (same as ISO
/// `base_hdr_headroom` / `alternate_hdr_headroom`), not linear luminance ratios.
/// [`GainMapMetadata::hdr_capacity_*`] stores linear peak/SDR ratios (`2^headroom`).
fn hdr_capacity_ratio_from_xmp_headroom(headroom_log2: f32) -> f32 {
    2.0_f32.powf(headroom_log2.max(0.0))
}

pub(crate) fn gain_map_metadata(gain_map_jpeg: &[u8]) -> Result<GainMapMetadata, String> {
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
            let gain_map_max = attribute_rgb_f32(&text, "hdrgm:GainMapMax")
                .ok_or_else(|| "Ultra HDR gain map metadata missing GainMapMax".to_string())?;
            let max_gain_map_max = gain_map_max
                .iter()
                .copied()
                .fold(f32::NEG_INFINITY, f32::max);
            let hdr_capacity_min = hdr_capacity_ratio_from_xmp_headroom(
                attribute_f32(&text, "hdrgm:HDRCapacityMin").unwrap_or(0.0),
            );
            let hdr_capacity_max = attribute_f32(&text, "hdrgm:HDRCapacityMax")
                .map(hdr_capacity_ratio_from_xmp_headroom)
                .unwrap_or_else(|| hdr_capacity_ratio_from_xmp_headroom(max_gain_map_max));
            return validate_gain_map_metadata(GainMapMetadata {
                gain_map_min: attribute_rgb_f32(&text, "hdrgm:GainMapMin").unwrap_or([0.0; 3]),
                gain_map_max,
                gamma: attribute_rgb_f32(&text, "hdrgm:Gamma").unwrap_or([1.0; 3]),
                offset_sdr: attribute_rgb_f32(&text, "hdrgm:OffsetSDR").unwrap_or([1.0 / 64.0; 3]),
                offset_hdr: attribute_rgb_f32(&text, "hdrgm:OffsetHDR").unwrap_or([1.0 / 64.0; 3]),
                hdr_capacity_min,
                hdr_capacity_max,
                backward_direction: true,
            });
        }
        let gain_map_max = attribute_rgb_f32(&text, "hdrgm:GainMapMax")
            .ok_or_else(|| "Ultra HDR gain map metadata missing GainMapMax".to_string())?;
        let max_gain_map_max = gain_map_max
            .iter()
            .copied()
            .fold(f32::NEG_INFINITY, f32::max);
        let hdr_capacity_min = hdr_capacity_ratio_from_xmp_headroom(
            attribute_f32(&text, "hdrgm:HDRCapacityMin").unwrap_or(0.0),
        );
        let hdr_capacity_max = attribute_f32(&text, "hdrgm:HDRCapacityMax")
            .map(hdr_capacity_ratio_from_xmp_headroom)
            .unwrap_or_else(|| hdr_capacity_ratio_from_xmp_headroom(max_gain_map_max));
        return validate_gain_map_metadata(GainMapMetadata {
            gain_map_min: attribute_rgb_f32(&text, "hdrgm:GainMapMin").unwrap_or([0.0; 3]),
            gain_map_max,
            gamma: attribute_rgb_f32(&text, "hdrgm:Gamma").unwrap_or([1.0; 3]),
            offset_sdr: attribute_rgb_f32(&text, "hdrgm:OffsetSDR").unwrap_or([1.0 / 64.0; 3]),
            offset_hdr: attribute_rgb_f32(&text, "hdrgm:OffsetHDR").unwrap_or([1.0 / 64.0; 3]),
            hdr_capacity_min,
            hdr_capacity_max,
            backward_direction: false,
        });
    }

    Err("Ultra HDR gain map metadata not found".to_string())
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

fn oriented_dimensions(width: u32, height: u32, orientation: u16) -> (u32, u32) {
    if (5..=8).contains(&orientation) {
        (height, width)
    } else {
        (width, height)
    }
}

pub(crate) fn display_to_physical_pixel(
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

pub(crate) fn extract_gain_map_jpeg_bytes(bytes: &[u8]) -> Result<Vec<u8>, String> {
    if let Ok(gain_map) = extract_container_gain_map_jpeg_bytes(bytes) {
        return Ok(gain_map);
    }

    let mpf_payload = primary_metadata_segments(bytes)?
        .into_iter()
        .find(|segment| {
            segment.marker == JPEG_APP2 && mpf_app2_payload_has_gain_map_image(segment.payload)
        })
        .ok_or_else(|| "Ultra HDR gain map location metadata not found".to_string())?
        .payload;

    extract_mpf_gain_map_jpeg_from_bytes(bytes, mpf_payload)
}

fn extract_container_gain_map_jpeg_bytes(bytes: &[u8]) -> Result<Vec<u8>, String> {
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
mod tests;
