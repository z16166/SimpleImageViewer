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
use std::io::{BufReader, Cursor, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use exr::block::chunk::Chunk;
use exr::block::reader::ChunksReader;
use exr::block::{BlockIndex, UncompressedBlock};
use exr::compression::Compression;
use exr::math::Vec2;
use exr::meta::BlockDescription;
use exr::meta::attribute::{ChannelList, Chromaticities, SampleType};
use exr::prelude::MetaData;

use crate::hdr::tiled::{
    HdrTileBuffer, HdrTileCache, HdrTiledSource, HdrTiledSourceKind,
    configured_hdr_tile_cache_max_bytes,
};
use crate::hdr::types::{HdrColorSpace, HdrImageBuffer, HdrPixelFormat};

#[derive(Debug)]
pub struct ExrTiledImageSource {
    path: PathBuf,
    width: u32,
    height: u32,
    color_space: HdrColorSpace,
    requires_disk_backed_decode: bool,
    has_subsampled_channels: bool,
    tile_cache: Mutex<HdrTileCache>,
}

impl ExrTiledImageSource {
    pub fn open(path: &Path) -> Result<Self, String> {
        Self::open_with_cache_budget(path, configured_hdr_tile_cache_max_bytes())
    }

    pub fn open_with_cache_budget(path: &Path, max_cache_bytes: usize) -> Result<Self, String> {
        let header = read_first_header_for_probe(path)?;
        let width = u32::try_from(header.layer_size.width())
            .map_err(|_| "EXR width exceeds u32".to_string())?;
        let height = u32::try_from(header.layer_size.height())
            .map_err(|_| "EXR height exceeds u32".to_string())?;
        if header.deep {
            return Err("deep data not supported yet".to_string());
        }
        let channel_layout = validate_required_rgba_channels(&header.channels)?;
        let chromaticities = header.shared_attributes.chromaticities;
        let color_space = hdr_color_space_from_exr_chromaticities(chromaticities);
        log_unsupported_exr_chromaticities(path, chromaticities);
        let has_subsampled_channels = header
            .channels
            .list
            .iter()
            .any(|channel| channel.sampling != Vec2(1, 1));

        Ok(Self {
            path: path.to_path_buf(),
            width,
            height,
            color_space,
            requires_disk_backed_decode: channel_layout.requires_disk_backed_decode(),
            has_subsampled_channels,
            tile_cache: Mutex::new(HdrTileCache::new(max_cache_bytes)),
        })
    }

    pub(crate) fn requires_disk_backed_decode(&self) -> bool {
        self.requires_disk_backed_decode
    }

    fn extract_tile_rgba32f_arc_unvalidated_scanline(
        &self,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    ) -> Result<Arc<HdrTileBuffer>, String> {
        let (bytes, mut meta_data, offset_tables) =
            read_unvalidated_exr_bytes_and_offsets(&self.path)?;
        normalize_subsampled_metadata_for_decompression(&mut meta_data)?;
        let header = meta_data
            .headers
            .first()
            .ok_or_else(|| "EXR file has no image layers".to_string())?;
        if !matches!(header.blocks, BlockDescription::ScanLines) || header.deep {
            return Err("subsampled EXR fallback supports only flat scanline images".to_string());
        }

        validate_required_rgba_channels(&header.channels)?;
        let channel_roles = channel_roles(&header.channels);
        let tile_bounds = TileBounds::new(x, y, width, height);
        let mut tile = YcaTileAccumulator::new(width, height, yca_luma_weights(self.color_space));

        for offset in offset_tables.first().into_iter().flatten() {
            let mut cursor = Cursor::new(bytes.as_slice());
            cursor
                .seek(SeekFrom::Start(*offset))
                .map_err(|err| err.to_string())?;
            let chunk = Chunk::read(&mut cursor, &meta_data).map_err(|err| err.to_string())?;
            if chunk.layer_index != 0 {
                continue;
            }

            let block = UncompressedBlock::decompress_chunk(chunk, &meta_data, false)
                .map_err(|err| err.to_string())?;
            if !block_intersects_tile(block.index, tile_bounds) {
                continue;
            }
            copy_subsampled_block_to_tile(&block, header, &channel_roles, tile_bounds, &mut tile)?;
        }

        Ok(Arc::new(HdrTileBuffer {
            width,
            height,
            color_space: self.color_space,
            rgba_f32: Arc::new(tile.into_rgba32f()),
        }))
    }

    #[cfg(test)]
    fn cached_tile_count(&self) -> usize {
        self.tile_cache
            .lock()
            .map(|cache| cache.len())
            .unwrap_or_default()
    }

    #[cfg(test)]
    fn cached_tile_bytes(&self) -> usize {
        self.tile_cache
            .lock()
            .map(|cache| cache.current_bytes())
            .unwrap_or_default()
    }
}

impl HdrTiledSource for ExrTiledImageSource {
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
        self.color_space
    }

    fn generate_sdr_preview(&self, max_w: u32, max_h: u32) -> Result<(u32, u32, Vec<u8>), String> {
        if self.has_subsampled_channels {
            return self.generate_sdr_preview_from_hdr_tile(max_w, max_h);
        }

        let preview = exr::prelude::read_first_rgba_layer_from_file(
            &self.path,
            move |resolution, _channels| {
                // Previews should remain visible even for EXR files whose alpha is absent or zero.
                // The HDR tile/rendering path still preserves source alpha.
                PreviewAccumulator::new_with_alpha(
                    resolution.width() as u32,
                    resolution.height() as u32,
                    max_w,
                    max_h,
                    false,
                )
            },
            |preview, position, (r, g, b, a): (f32, f32, f32, f32)| {
                preview.set(position.x() as u32, position.y() as u32, [r, g, b, a]);
            },
        )
        .map_err(|err| err.to_string())?
        .layer_data
        .channel_data
        .pixels;

        preview.into_sdr_rgba8()
    }

    fn extract_tile_rgba32f_arc(
        &self,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    ) -> Result<Arc<HdrTileBuffer>, String> {
        crate::hdr::tiled::validate_tile_bounds(self.width, self.height, x, y, width, height)?;
        let key = (x, y, width, height);
        if let Ok(mut cache) = self.tile_cache.lock() {
            if let Some(tile) = cache.get(key) {
                return Ok(tile);
            }
        }

        let file = File::open(&self.path).map_err(|err| err.to_string())?;
        if self.has_subsampled_channels {
            return self.extract_tile_rgba32f_arc_unvalidated_scanline(x, y, width, height);
        }

        let reader =
            exr::block::read(BufReader::new(file), false).map_err(|err| err.to_string())?;
        let tile_bounds = TileBounds::new(x, y, width, height);
        let chunks = reader
            .filter_chunks(false, |_meta, _tile, block| {
                block.layer == 0
                    && block.level == Vec2(0, 0)
                    && block_intersects_tile(block, tile_bounds)
            })
            .map_err(|err| err.to_string())?;

        let header = chunks
            .headers()
            .first()
            .ok_or_else(|| "EXR file has no image layers".to_string())?
            .clone();
        validate_required_rgba_channels(&header.channels)?;
        let channel_roles = channel_roles(&header.channels);

        let mut rgba = vec![0.0; width as usize * height as usize * 4];
        for alpha in rgba.chunks_exact_mut(4).map(|pixel| &mut pixel[3]) {
            *alpha = 1.0;
        }

        let mut decompressor = chunks.sequential_decompressor(false);
        while let Some(block) = decompressor.next() {
            let block = block.map_err(|err| err.to_string())?;
            for line in block.lines(&header.channels) {
                let Some(channel) = channel_roles
                    .get(line.location.channel)
                    .and_then(|role| *role)
                else {
                    continue;
                };
                copy_line_channel_to_tile(
                    line,
                    header.channels.list[line.location.channel].sample_type,
                    channel,
                    tile_bounds,
                    &mut rgba,
                )?;
            }
        }

        let tile = Arc::new(HdrTileBuffer {
            width,
            height,
            color_space: self.color_space,
            rgba_f32: Arc::new(rgba),
        });

        if let Ok(mut cache) = self.tile_cache.lock() {
            cache.insert(key, Arc::clone(&tile));
        }

        Ok(tile)
    }
}

impl ExrTiledImageSource {
    fn generate_sdr_preview_from_hdr_tile(
        &self,
        max_w: u32,
        max_h: u32,
    ) -> Result<(u32, u32, Vec<u8>), String> {
        let tile = self.extract_tile_rgba32f_arc(0, 0, self.width, self.height)?;
        let pixels = crate::hdr::decode::hdr_to_sdr_rgba8(
            &crate::hdr::types::HdrImageBuffer {
                width: tile.width,
                height: tile.height,
                format: crate::hdr::types::HdrPixelFormat::Rgba32Float,
                color_space: tile.color_space,
                rgba_f32: Arc::clone(&tile.rgba_f32),
            },
            0.0,
        )?;
        let image = image::RgbaImage::from_raw(tile.width, tile.height, pixels)
            .ok_or_else(|| "Failed to build SDR preview image from HDR tile".to_string())?;
        let preview = image::imageops::thumbnail(&image, max_w, max_h);
        Ok((preview.width(), preview.height(), preview.into_raw()))
    }
}

fn read_first_header_for_probe(path: &Path) -> Result<exr::meta::header::Header, String> {
    match File::open(path)
        .map_err(|err| err.to_string())
        .and_then(|file| {
            exr::block::read(BufReader::new(file), false).map_err(|err| err.to_string())
        }) {
        Ok(reader) => reader
            .headers()
            .first()
            .cloned()
            .ok_or_else(|| "EXR file has no image layers".to_string()),
        Err(err) if is_exr_unvalidated_probe_error(&err) => {
            let meta_data = MetaData::read_from_file(path, false).map_err(|err| err.to_string())?;
            meta_data
                .headers
                .first()
                .cloned()
                .ok_or_else(|| "EXR file has no image layers".to_string())
        }
        Err(err) => Err(err),
    }
}

fn is_exr_unvalidated_probe_error(err: &str) -> bool {
    err.contains("channel subsampling not supported yet")
}

fn read_unvalidated_exr_bytes_and_offsets(
    path: &Path,
) -> Result<(Vec<u8>, MetaData, Vec<Vec<u64>>), String> {
    let bytes = std::fs::read(path).map_err(|err| err.to_string())?;
    let mut cursor = Cursor::new(bytes.as_slice());
    let meta_data =
        MetaData::read_from_buffered(&mut cursor, false).map_err(|err| err.to_string())?;
    let mut offset_tables = Vec::with_capacity(meta_data.headers.len());
    for header in &meta_data.headers {
        let mut offsets = Vec::with_capacity(header.chunk_count);
        for _ in 0..header.chunk_count {
            offsets.push(read_u64_le(&mut cursor)?);
        }
        offset_tables.push(offsets);
    }

    Ok((bytes, meta_data, offset_tables))
}

fn read_u64_le(cursor: &mut Cursor<&[u8]>) -> Result<u64, String> {
    let mut bytes = [0_u8; 8];
    cursor
        .read_exact(&mut bytes)
        .map_err(|err| err.to_string())?;
    Ok(u64::from_le_bytes(bytes))
}

fn normalize_subsampled_metadata_for_decompression(meta_data: &mut MetaData) -> Result<(), String> {
    for header in &mut meta_data.headers {
        if header
            .channels
            .list
            .iter()
            .any(|channel| channel.sampling != Vec2(1, 1))
        {
            header.channels.bytes_per_pixel =
                effective_subsampled_bytes_per_pixel(&header.channels)?;
        }
    }
    Ok(())
}

fn effective_subsampled_bytes_per_pixel(channels: &ChannelList) -> Result<usize, String> {
    let denominator = channels
        .list
        .iter()
        .map(|channel| channel.sampling.x() * channel.sampling.y())
        .fold(1_usize, lcm);
    let numerator = channels
        .list
        .iter()
        .map(|channel| {
            channel.sample_type.bytes_per_sample() * denominator
                / (channel.sampling.x() * channel.sampling.y())
        })
        .sum::<usize>();
    if numerator % denominator != 0 {
        return Err("subsampled EXR has non-integral effective bytes per pixel".to_string());
    }
    Ok(numerator / denominator)
}

fn lcm(a: usize, b: usize) -> usize {
    a / gcd(a, b) * b
}

fn gcd(mut a: usize, mut b: usize) -> usize {
    while b != 0 {
        let r = a % b;
        a = b;
        b = r;
    }
    a
}

pub(crate) fn exr_color_space(path: &Path) -> Result<HdrColorSpace, String> {
    let file = File::open(path).map_err(|err| err.to_string())?;
    let reader = exr::block::read(BufReader::new(file), false).map_err(|err| err.to_string())?;
    let header = reader
        .headers()
        .first()
        .ok_or_else(|| "EXR file has no image layers".to_string())?;
    let chromaticities = header.shared_attributes.chromaticities;
    log_unsupported_exr_chromaticities(path, chromaticities);
    Ok(hdr_color_space_from_exr_chromaticities(chromaticities))
}

pub(crate) fn exr_dimensions_unvalidated(path: &Path) -> Result<(u32, u32), String> {
    let meta_data = MetaData::read_from_file(path, false).map_err(|err| err.to_string())?;
    let header = meta_data
        .headers
        .first()
        .ok_or_else(|| "EXR file has no image layers".to_string())?;
    let width = u32::try_from(header.layer_size.width())
        .map_err(|_| "EXR width exceeds u32".to_string())?;
    let height = u32::try_from(header.layer_size.height())
        .map_err(|_| "EXR height exceeds u32".to_string())?;
    Ok((width, height))
}

pub(crate) fn decode_deep_exr_image(path: &Path) -> Result<HdrImageBuffer, String> {
    let (bytes, meta_data, offset_tables) = read_unvalidated_exr_bytes_and_offsets(path)?;
    let header = meta_data
        .headers
        .first()
        .ok_or_else(|| "EXR file has no image layers".to_string())?;
    if !header.deep || !matches!(header.blocks, BlockDescription::ScanLines) {
        return Err("Only deep scanline EXR images are supported".to_string());
    }

    let width = u32::try_from(header.layer_size.width())
        .map_err(|_| "EXR width exceeds u32".to_string())?;
    let height = u32::try_from(header.layer_size.height())
        .map_err(|_| "EXR height exceeds u32".to_string())?;
    let chromaticities = header.shared_attributes.chromaticities;
    let color_space = hdr_color_space_from_exr_chromaticities(chromaticities);
    log_unsupported_exr_chromaticities(path, chromaticities);

    let mut rgba = vec![0.0_f32; width as usize * height as usize * 4];
    let channel_roles = channel_roles(&header.channels);

    for offset in offset_tables.first().into_iter().flatten() {
        let mut cursor = Cursor::new(bytes.as_slice());
        cursor
            .seek(SeekFrom::Start(*offset))
            .map_err(|err| err.to_string())?;
        let (layer_index, block) = read_deep_scanline_chunk_unvalidated(&mut cursor, &meta_data)?;
        if layer_index != 0 {
            continue;
        }

        composite_deep_scanline_block(header, &channel_roles, block, &mut rgba)?;
    }

    Ok(HdrImageBuffer {
        width,
        height,
        format: HdrPixelFormat::Rgba32Float,
        color_space,
        rgba_f32: Arc::new(rgba),
    })
}

pub(crate) fn hdr_color_space_from_exr_chromaticities(
    chromaticities: Option<Chromaticities>,
) -> HdrColorSpace {
    let Some(chromaticities) = chromaticities else {
        // OpenEXR specifies BT.709/sRGB primaries when chromaticities is absent.
        return HdrColorSpace::LinearSrgb;
    };

    if chromaticities_match(chromaticities, SRGB_CHROMATICITIES) {
        HdrColorSpace::LinearSrgb
    } else if chromaticities_match(chromaticities, REC2020_CHROMATICITIES) {
        HdrColorSpace::Rec2020Linear
    } else if chromaticities_match(chromaticities, ACES_AP0_CHROMATICITIES) {
        HdrColorSpace::Aces2065_1
    } else if chromaticities_match(chromaticities, XYZ_CHROMATICITIES) {
        HdrColorSpace::Xyz
    } else {
        HdrColorSpace::Unknown
    }
}

pub(crate) fn unsupported_exr_chromaticities_diagnostic(
    chromaticities: Option<Chromaticities>,
) -> Option<String> {
    let chromaticities = chromaticities?;
    if hdr_color_space_from_exr_chromaticities(Some(chromaticities)) != HdrColorSpace::Unknown {
        return None;
    }

    Some(format!(
        "unsupported EXR chromaticities: red={}, green={}, blue={}, white={}",
        format_xy(chromaticities.red),
        format_xy(chromaticities.green),
        format_xy(chromaticities.blue),
        format_xy(chromaticities.white),
    ))
}

fn log_unsupported_exr_chromaticities(path: &Path, chromaticities: Option<Chromaticities>) {
    if let Some(diagnostic) = unsupported_exr_chromaticities_diagnostic(chromaticities) {
        log::warn!("[HDR] {}: {}", path.display(), diagnostic);
    }
}

fn format_xy(point: Vec2<f32>) -> String {
    format!("({:.4}, {:.4})", point.x(), point.y())
}

const SRGB_CHROMATICITIES: Chromaticities = Chromaticities {
    red: Vec2(0.64, 0.33),
    green: Vec2(0.30, 0.60),
    blue: Vec2(0.15, 0.06),
    white: Vec2(0.3127, 0.3290),
};

const REC2020_CHROMATICITIES: Chromaticities = Chromaticities {
    red: Vec2(0.708, 0.292),
    green: Vec2(0.170, 0.797),
    blue: Vec2(0.131, 0.046),
    white: Vec2(0.3127, 0.3290),
};

const ACES_AP0_CHROMATICITIES: Chromaticities = Chromaticities {
    red: Vec2(0.7347, 0.2653),
    green: Vec2(0.0, 1.0),
    blue: Vec2(0.0001, -0.0770),
    white: Vec2(0.32168, 0.33767),
};

const XYZ_CHROMATICITIES: Chromaticities = Chromaticities {
    red: Vec2(1.0, 0.0),
    green: Vec2(0.0, 1.0),
    blue: Vec2(0.0, 0.0),
    white: Vec2(0.33333, 0.33333),
};

fn chromaticities_match(actual: Chromaticities, expected: Chromaticities) -> bool {
    const EPSILON: f32 = 0.002;
    chromaticity_point_matches(actual.red, expected.red, EPSILON)
        && chromaticity_point_matches(actual.green, expected.green, EPSILON)
        && chromaticity_point_matches(actual.blue, expected.blue, EPSILON)
        && chromaticity_point_matches(actual.white, expected.white, EPSILON)
}

fn chromaticity_point_matches(actual: Vec2<f32>, expected: Vec2<f32>, epsilon: f32) -> bool {
    (actual.x() - expected.x()).abs() <= epsilon && (actual.y() - expected.y()).abs() <= epsilon
}

#[derive(Debug)]
struct PreviewAccumulator {
    source_width: u32,
    source_height: u32,
    width: u32,
    height: u32,
    has_alpha_channel: bool,
    rgba_sum: Vec<f32>,
    sample_counts: Vec<u32>,
}

impl PreviewAccumulator {
    fn new_with_alpha(
        source_width: u32,
        source_height: u32,
        max_w: u32,
        max_h: u32,
        has_alpha_channel: bool,
    ) -> Self {
        let scale = (max_w as f32 / source_width as f32)
            .min(max_h as f32 / source_height as f32)
            .min(1.0);
        let width = ((source_width as f32 * scale).round() as u32).max(1);
        let height = ((source_height as f32 * scale).round() as u32).max(1);
        let rgba_sum = vec![0.0; width as usize * height as usize * 4];
        let sample_counts = vec![0; width as usize * height as usize];
        Self {
            source_width,
            source_height,
            width,
            height,
            has_alpha_channel,
            rgba_sum,
            sample_counts,
        }
    }

    fn set(&mut self, source_x: u32, source_y: u32, rgba: [f32; 4]) {
        let x = ((source_x as u64 * self.width as u64) / self.source_width as u64)
            .min(self.width.saturating_sub(1) as u64) as usize;
        let y = ((source_y as u64 * self.height as u64) / self.source_height as u64)
            .min(self.height.saturating_sub(1) as u64) as usize;
        let pixel = y * self.width as usize + x;
        let offset = pixel * 4;
        let mut rgba = rgba;
        if !self.has_alpha_channel {
            rgba[3] = 1.0;
        }
        for (channel, value) in rgba.iter().enumerate() {
            self.rgba_sum[offset + channel] += *value;
        }
        self.sample_counts[pixel] += 1;
    }

    fn into_sdr_rgba8(self) -> Result<(u32, u32, Vec<u8>), String> {
        let mut rgba_f32 = self.rgba_sum;
        for (pixel, count) in self.sample_counts.iter().enumerate() {
            let offset = pixel * 4;
            if *count == 0 {
                rgba_f32[offset + 3] = 1.0;
                continue;
            }

            let scale = 1.0 / *count as f32;
            for channel in 0..4 {
                rgba_f32[offset + channel] *= scale;
            }
        }

        let pixels = crate::hdr::decode::hdr_to_sdr_rgba8(
            &crate::hdr::types::HdrImageBuffer {
                width: self.width,
                height: self.height,
                format: crate::hdr::types::HdrPixelFormat::Rgba32Float,
                color_space: HdrColorSpace::LinearSrgb,
                rgba_f32: Arc::new(rgba_f32),
            },
            0.0,
        )?;
        Ok((self.width, self.height, pixels))
    }
}

#[derive(Clone, Copy)]
struct TileBounds {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

impl TileBounds {
    fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    fn right(self) -> u32 {
        self.x + self.width
    }

    fn bottom(self) -> u32 {
        self.y + self.height
    }
}

fn block_intersects_tile(block: BlockIndex, tile: TileBounds) -> bool {
    let block_x = block.pixel_position.x() as u32;
    let block_y = block.pixel_position.y() as u32;
    let block_right = block_x + block.pixel_size.width() as u32;
    let block_bottom = block_y + block.pixel_size.height() as u32;

    block_x < tile.right()
        && block_right > tile.x
        && block_y < tile.bottom()
        && block_bottom > tile.y
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChannelRole {
    Red,
    Green,
    Blue,
    Alpha,
    Luminance,
    ChromaRy,
    ChromaBy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExrChannelLayout {
    Rgb,
    Luminance,
}

impl ExrChannelLayout {
    fn requires_disk_backed_decode(self) -> bool {
        matches!(self, Self::Luminance)
    }
}

fn channel_roles(channels: &ChannelList) -> Vec<Option<ChannelRole>> {
    let has_rgb = ["R", "G", "B"].into_iter().all(|name| {
        channels
            .list
            .iter()
            .any(|channel| channel.name.eq_case_insensitive(name))
    });

    channels
        .list
        .iter()
        .map(|channel| {
            if channel.name.eq_case_insensitive("RY") {
                Some(ChannelRole::ChromaRy)
            } else if channel.name.eq_case_insensitive("BY") {
                Some(ChannelRole::ChromaBy)
            } else if !has_rgb
                && (channel.name.eq_case_insensitive("R")
                    || channel.name.eq_case_insensitive("G")
                    || channel.name.eq_case_insensitive("B")
                    || channel.name.eq_case_insensitive("Y")
                    || channel.name.eq_case_insensitive("main"))
            {
                Some(ChannelRole::Luminance)
            } else if channel.name.eq_case_insensitive("R") {
                Some(ChannelRole::Red)
            } else if channel.name.eq_case_insensitive("G") {
                Some(ChannelRole::Green)
            } else if channel.name.eq_case_insensitive("B") {
                Some(ChannelRole::Blue)
            } else if channel.name.eq_case_insensitive("A") {
                Some(ChannelRole::Alpha)
            } else if channel.name.eq_case_insensitive("Y")
                || channel.name.eq_case_insensitive("main")
            {
                Some(ChannelRole::Luminance)
            } else {
                None
            }
        })
        .collect()
}

fn validate_required_rgba_channels(channels: &ChannelList) -> Result<ExrChannelLayout, String> {
    let roles = channel_roles(channels);
    let has_rgb = [ChannelRole::Red, ChannelRole::Green, ChannelRole::Blue]
        .into_iter()
        .all(|role| roles.iter().any(|candidate| *candidate == Some(role)));
    if has_rgb {
        return Ok(ExrChannelLayout::Rgb);
    }

    if roles
        .iter()
        .any(|role| *role == Some(ChannelRole::Luminance))
    {
        return Ok(ExrChannelLayout::Luminance);
    }

    for (name, role) in [
        ("R", ChannelRole::Red),
        ("G", ChannelRole::Green),
        ("B", ChannelRole::Blue),
    ] {
        if !roles.iter().any(|candidate| *candidate == Some(role)) {
            return Err(format!(
                "EXR layer does not contain required {name} channel"
            ));
        }
    }
    unreachable!("RGB channel validation should have returned earlier")
}

fn copy_line_channel_to_tile(
    line: exr::block::lines::LineRef<'_>,
    sample_type: SampleType,
    channel: ChannelRole,
    tile: TileBounds,
    rgba: &mut [f32],
) -> Result<(), String> {
    let line_y = line.location.position.y() as u32;
    if line_y < tile.y || line_y >= tile.bottom() {
        return Ok(());
    }

    let line_x = line.location.position.x() as u32;
    let line_right = line_x + line.location.sample_count as u32;
    let copy_start = line_x.max(tile.x);
    let copy_end = line_right.min(tile.right());
    if copy_start >= copy_end {
        return Ok(());
    }

    let samples = read_line_samples(line, sample_type)?;
    let source_start = (copy_start - line_x) as usize;
    let dest_y = (line_y - tile.y) as usize;
    for source_x in source_start..(source_start + (copy_end - copy_start) as usize) {
        let dest_x = line_x as usize + source_x - tile.x as usize;
        let dest = (dest_y * tile.width as usize + dest_x) * 4;
        match channel {
            ChannelRole::Red => rgba[dest] = samples[source_x],
            ChannelRole::Green => rgba[dest + 1] = samples[source_x],
            ChannelRole::Blue => rgba[dest + 2] = samples[source_x],
            ChannelRole::Alpha => rgba[dest + 3] = samples[source_x],
            ChannelRole::ChromaRy | ChannelRole::ChromaBy => {}
            ChannelRole::Luminance => {
                rgba[dest] = samples[source_x];
                rgba[dest + 1] = samples[source_x];
                rgba[dest + 2] = samples[source_x];
            }
        }
    }

    Ok(())
}

#[derive(Debug)]
struct YcaTileAccumulator {
    width: u32,
    height: u32,
    y: Vec<f32>,
    ry: Vec<f32>,
    by: Vec<f32>,
    alpha: Vec<f32>,
    weights: [f32; 3],
}

impl YcaTileAccumulator {
    fn new(width: u32, height: u32, weights: [f32; 3]) -> Self {
        let len = width as usize * height as usize;
        Self {
            width,
            height,
            y: vec![0.0; len],
            ry: vec![0.0; len],
            by: vec![0.0; len],
            alpha: vec![1.0; len],
            weights,
        }
    }

    fn set_luminance(&mut self, x: usize, y: usize, value: f32) {
        self.y[y * self.width as usize + x] = value;
    }

    fn set_chroma(&mut self, x: usize, y: usize, role: ChannelRole, value: f32) {
        let index = y * self.width as usize + x;
        match role {
            ChannelRole::ChromaRy => self.ry[index] = value,
            ChannelRole::ChromaBy => self.by[index] = value,
            _ => {}
        }
    }

    fn set_alpha(&mut self, x: usize, y: usize, value: f32) {
        self.alpha[y * self.width as usize + x] = value;
    }

    fn into_rgba32f(self) -> Vec<f32> {
        let mut rgba = vec![0.0; self.width as usize * self.height as usize * 4];
        let [wr, wg, wb] = self.weights;
        for index in 0..self.y.len() {
            let y = self.y[index];
            let r = y * (1.0 + self.ry[index]);
            let b = y * (1.0 + self.by[index]);
            let g = if wg.abs() > f32::EPSILON {
                (y - r * wr - b * wb) / wg
            } else {
                y
            };
            let offset = index * 4;
            rgba[offset] = r;
            rgba[offset + 1] = g;
            rgba[offset + 2] = b;
            rgba[offset + 3] = self.alpha[index];
        }
        rgba
    }
}

fn copy_subsampled_block_to_tile(
    block: &UncompressedBlock,
    header: &exr::meta::header::Header,
    channel_roles: &[Option<ChannelRole>],
    tile: TileBounds,
    accumulator: &mut YcaTileAccumulator,
) -> Result<(), String> {
    let block_x = block.index.pixel_position.x() as u32;
    let block_y = block.index.pixel_position.y() as u32;
    let block_width = block.index.pixel_size.width() as u32;
    let block_height = block.index.pixel_size.height() as u32;
    let mut byte_offset = 0;

    for local_y in 0..block_height {
        let image_y = block_y + local_y;
        for (channel_index, channel) in header.channels.list.iter().enumerate() {
            if image_y as usize % channel.sampling.y() != 0 {
                continue;
            }

            let sample_count = block_width as usize / channel.sampling.x();
            let byte_len = sample_count * channel.sample_type.bytes_per_sample();
            let line_end = byte_offset + byte_len;
            if line_end > block.data.len() {
                return Err("subsampled EXR block line exceeds decompressed byte size".to_string());
            }

            let Some(role) = channel_roles.get(channel_index).and_then(|role| *role) else {
                byte_offset = line_end;
                continue;
            };
            let samples = read_samples_from_native_bytes(
                &block.data[byte_offset..line_end],
                channel.sample_type,
            )?;
            copy_subsampled_line_to_tile(
                &samples,
                block_x,
                image_y,
                channel.sampling.x() as u32,
                channel.sampling.y() as u32,
                role,
                tile,
                accumulator,
            );
            byte_offset = line_end;
        }
    }

    Ok(())
}

fn copy_subsampled_line_to_tile(
    samples: &[f32],
    line_x: u32,
    line_y: u32,
    sampling_x: u32,
    sampling_y: u32,
    role: ChannelRole,
    tile: TileBounds,
    accumulator: &mut YcaTileAccumulator,
) {
    for (sample_index, value) in samples.iter().enumerate() {
        let image_x = line_x + sample_index as u32 * sampling_x;
        for dy in 0..sampling_y {
            let y = line_y + dy;
            if y < tile.y || y >= tile.bottom() {
                continue;
            }
            for dx in 0..sampling_x {
                let x = image_x + dx;
                if x < tile.x || x >= tile.right() {
                    continue;
                }
                let tile_x = (x - tile.x) as usize;
                let tile_y = (y - tile.y) as usize;
                match role {
                    ChannelRole::Luminance => accumulator.set_luminance(tile_x, tile_y, *value),
                    ChannelRole::ChromaRy | ChannelRole::ChromaBy => {
                        accumulator.set_chroma(tile_x, tile_y, role, *value);
                    }
                    ChannelRole::Alpha => accumulator.set_alpha(tile_x, tile_y, *value),
                    ChannelRole::Red | ChannelRole::Green | ChannelRole::Blue => {}
                }
            }
        }
    }
}

fn read_samples_from_native_bytes(
    bytes: &[u8],
    sample_type: SampleType,
) -> Result<Vec<f32>, String> {
    match sample_type {
        SampleType::F16 => Ok(bytes
            .chunks_exact(2)
            .map(|bytes| {
                let bits = u16::from_ne_bytes([bytes[0], bytes[1]]);
                exr::prelude::f16::from_bits(bits).to_f32()
            })
            .collect::<Vec<_>>()),
        SampleType::F32 => Ok(bytes
            .chunks_exact(4)
            .map(|bytes| f32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
            .collect::<Vec<_>>()),
        SampleType::U32 => Ok(bytes
            .chunks_exact(4)
            .map(|bytes| u32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as f32)
            .collect::<Vec<_>>()),
    }
}

fn composite_deep_scanline_block(
    header: &exr::meta::header::Header,
    channel_roles: &[Option<ChannelRole>],
    block: exr::block::chunk::CompressedDeepScanLineBlock,
    rgba: &mut [f32],
) -> Result<(), String> {
    let block_y = block
        .y_coordinate
        .checked_sub(header.own_attributes.layer_position.y())
        .ok_or_else(|| "invalid deep EXR scanline block y coordinate".to_string())?;
    if block_y < 0 {
        return Err("invalid deep EXR scanline block before data window".to_string());
    }
    let block_y = block_y as usize;
    let width = header.layer_size.width();
    let block_height = header
        .compression
        .scan_lines_per_block()
        .min(header.layer_size.height().saturating_sub(block_y));
    if block_height == 0 {
        return Ok(());
    }

    let expected_table_bytes = width
        .checked_mul(block_height)
        .and_then(|pixels| pixels.checked_mul(std::mem::size_of::<i32>()))
        .ok_or_else(|| "deep EXR pixel offset table size overflow".to_string())?;
    let table_bytes = decompress_deep_bytes(
        header.compression,
        block
            .compressed_pixel_offset_table
            .into_iter()
            .map(|byte| byte as u8)
            .collect(),
        expected_table_bytes,
    )?;
    let offsets = table_bytes
        .chunks_exact(4)
        .map(|bytes| i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        .collect::<Vec<_>>();
    if offsets.len() != width * block_height {
        return Err("deep EXR pixel offset table has unexpected size".to_string());
    }

    let total_samples = offsets.last().copied().unwrap_or_default().max(0) as usize;
    let sample_bytes = decompress_deep_bytes(
        header.compression,
        block.compressed_sample_data_le,
        block.decompressed_sample_data_size,
    )?;
    let channel_offsets =
        deep_channel_offsets(&header.channels, total_samples, sample_bytes.len())?;

    for local_y in 0..block_height {
        let image_y = block_y + local_y;
        if image_y >= header.layer_size.height() {
            continue;
        }
        for x in 0..width {
            let pixel_index = local_y * width + x;
            let sample_end = offsets[pixel_index].max(0) as usize;
            let sample_start = if pixel_index == 0 {
                0
            } else {
                offsets[pixel_index - 1].max(0) as usize
            };
            if sample_end < sample_start || sample_end > total_samples {
                return Err("deep EXR pixel offset table is not monotonic".to_string());
            }

            let dest = (image_y * width + x) * 4;
            for sample_index in sample_start..sample_end {
                let sample = read_deep_rgba_sample(
                    &sample_bytes,
                    &header.channels,
                    channel_roles,
                    &channel_offsets,
                    sample_index,
                )?;
                composite_sample_over(&mut rgba[dest..dest + 4], sample);
                if rgba[dest + 3] >= 0.999 {
                    break;
                }
            }
        }
    }

    Ok(())
}

fn deep_channel_offsets(
    channels: &ChannelList,
    total_samples: usize,
    sample_data_len: usize,
) -> Result<Vec<usize>, String> {
    let mut offsets = Vec::with_capacity(channels.list.len());
    let mut offset = 0_usize;
    for channel in &channels.list {
        offsets.push(offset);
        offset = offset
            .checked_add(
                total_samples
                    .checked_mul(channel.sample_type.bytes_per_sample())
                    .ok_or_else(|| "deep EXR channel sample byte size overflow".to_string())?,
            )
            .ok_or_else(|| "deep EXR channel offset overflow".to_string())?;
    }
    if offset > sample_data_len {
        return Err("deep EXR sample data is smaller than channel layout".to_string());
    }
    Ok(offsets)
}

fn read_deep_rgba_sample(
    sample_bytes: &[u8],
    channels: &ChannelList,
    channel_roles: &[Option<ChannelRole>],
    channel_offsets: &[usize],
    sample_index: usize,
) -> Result<[f32; 4], String> {
    let mut rgba = [0.0_f32, 0.0, 0.0, 1.0];
    let mut has_alpha = false;

    for (channel_index, channel) in channels.list.iter().enumerate() {
        let Some(role) = channel_roles.get(channel_index).and_then(|role| *role) else {
            continue;
        };
        let value = read_deep_sample(
            sample_bytes,
            channel_offsets[channel_index],
            sample_index,
            channel.sample_type,
        )?;
        match role {
            ChannelRole::Red => rgba[0] = value,
            ChannelRole::Green => rgba[1] = value,
            ChannelRole::Blue => rgba[2] = value,
            ChannelRole::Alpha => {
                rgba[3] = value;
                has_alpha = true;
            }
            ChannelRole::Luminance => {
                rgba[0] = value;
                rgba[1] = value;
                rgba[2] = value;
            }
            ChannelRole::ChromaRy | ChannelRole::ChromaBy => {}
        }
    }

    if !has_alpha {
        rgba[3] = 1.0;
    }
    Ok(rgba)
}

fn read_deep_sample(
    sample_bytes: &[u8],
    channel_offset: usize,
    sample_index: usize,
    sample_type: SampleType,
) -> Result<f32, String> {
    let sample_size = sample_type.bytes_per_sample();
    let offset = channel_offset
        .checked_add(
            sample_index
                .checked_mul(sample_size)
                .ok_or_else(|| "deep EXR sample offset overflow".to_string())?,
        )
        .ok_or_else(|| "deep EXR sample offset overflow".to_string())?;
    let bytes = sample_bytes
        .get(offset..offset + sample_size)
        .ok_or_else(|| "deep EXR sample read exceeds decompressed data".to_string())?;
    match sample_type {
        SampleType::F16 => {
            Ok(exr::prelude::f16::from_bits(u16::from_le_bytes([bytes[0], bytes[1]])).to_f32())
        }
        SampleType::F32 => Ok(f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])),
        SampleType::U32 => Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as f32),
    }
}

fn composite_sample_over(dest: &mut [f32], sample: [f32; 4]) {
    let alpha = sample[3].clamp(0.0, 1.0);
    let remaining = 1.0 - dest[3].clamp(0.0, 1.0);
    let contribution = remaining * alpha;
    dest[0] += sample[0] * contribution;
    dest[1] += sample[1] * contribution;
    dest[2] += sample[2] * contribution;
    dest[3] += contribution;
}

fn decompress_deep_bytes(
    compression: Compression,
    compressed: Vec<u8>,
    expected_byte_size: usize,
) -> Result<Vec<u8>, String> {
    if compressed.len() == expected_byte_size || matches!(compression, Compression::Uncompressed) {
        return Ok(compressed);
    }

    let mut decompressed = match compression {
        Compression::ZIP1 => miniz_oxide::inflate::decompress_to_vec_zlib(&compressed)
            .map_err(|_| "zlib-compressed deep EXR data malformed".to_string())?,
        Compression::RLE => decompress_rle_bytes(&compressed, expected_byte_size)?,
        _ => {
            return Err(format!(
                "deep EXR compression method is not supported yet: {compression:?}"
            ));
        }
    };
    differences_to_samples(&mut decompressed);
    interleave_byte_blocks(&mut decompressed);
    if decompressed.len() != expected_byte_size {
        return Err("deep EXR decompressed data has unexpected size".to_string());
    }
    Ok(decompressed)
}

fn decompress_rle_bytes(compressed: &[u8], expected_byte_size: usize) -> Result<Vec<u8>, String> {
    let mut remaining = compressed;
    let mut decompressed = Vec::with_capacity(expected_byte_size);
    while !remaining.is_empty() && decompressed.len() != expected_byte_size {
        let count = remaining[0] as i8 as i32;
        remaining = &remaining[1..];
        if count < 0 {
            let len = (-count) as usize;
            if remaining.len() < len {
                return Err("malformed RLE-compressed deep EXR data".to_string());
            }
            decompressed.extend_from_slice(&remaining[..len]);
            remaining = &remaining[len..];
        } else {
            let Some((&value, rest)) = remaining.split_first() else {
                return Err("malformed RLE-compressed deep EXR data".to_string());
            };
            decompressed.resize(decompressed.len() + count as usize + 1, value);
            remaining = rest;
        }
    }
    if decompressed.len() != expected_byte_size {
        return Err("RLE-compressed deep EXR data has unexpected size".to_string());
    }
    Ok(decompressed)
}

fn differences_to_samples(buffer: &mut [u8]) {
    if let Some(first) = buffer.first().copied() {
        let mut previous = first as i16;
        for value in &mut buffer[1..] {
            let sample = (previous + *value as i16 - 128) as u8;
            *value = sample;
            previous = sample as i16;
        }
    }
}

fn interleave_byte_blocks(separated: &mut [u8]) {
    let mut interleaved = vec![0_u8; separated.len()];
    let (first_half, second_half) = separated.split_at(separated.len().div_ceil(2));
    for (index, value) in first_half.iter().enumerate() {
        interleaved[index * 2] = *value;
    }
    for (index, value) in second_half.iter().enumerate() {
        let dest = index * 2 + 1;
        if dest < interleaved.len() {
            interleaved[dest] = *value;
        }
    }
    separated.copy_from_slice(&interleaved);
}

fn read_deep_scanline_chunk_unvalidated(
    cursor: &mut Cursor<&[u8]>,
    meta_data: &MetaData,
) -> Result<(usize, exr::block::chunk::CompressedDeepScanLineBlock), String> {
    let layer_index = if meta_data.headers.len() > 1 {
        let part = read_i32_le(cursor)?;
        if part < 0 {
            return Err("invalid deep EXR chunk part number".to_string());
        }
        part as usize
    } else {
        0
    };
    let header = meta_data
        .headers
        .get(layer_index)
        .ok_or_else(|| "invalid deep EXR chunk part number".to_string())?;
    if !header.deep || !matches!(header.blocks, BlockDescription::ScanLines) {
        return Err("Only deep scanline EXR chunks are supported".to_string());
    }

    let y_coordinate = read_i32_le(cursor)?;
    let table_size = usize::try_from(read_u64_le(cursor)?)
        .map_err(|_| "deep EXR table size exceeds usize".to_string())?;
    let sample_data_size = usize::try_from(read_u64_le(cursor)?)
        .map_err(|_| "deep EXR sample data size exceeds usize".to_string())?;
    let decompressed_sample_data_size = usize::try_from(read_u64_le(cursor)?)
        .map_err(|_| "deep EXR raw sample data size exceeds usize".to_string())?;

    let mut table = vec![0_u8; table_size];
    cursor
        .read_exact(&mut table)
        .map_err(|err| err.to_string())?;
    let mut sample_data = vec![0_u8; sample_data_size];
    cursor
        .read_exact(&mut sample_data)
        .map_err(|err| err.to_string())?;

    Ok((
        layer_index,
        exr::block::chunk::CompressedDeepScanLineBlock {
            y_coordinate,
            decompressed_sample_data_size,
            compressed_pixel_offset_table: table.into_iter().map(|byte| byte as i8).collect(),
            compressed_sample_data_le: sample_data,
        },
    ))
}

fn read_i32_le(cursor: &mut Cursor<&[u8]>) -> Result<i32, String> {
    let mut bytes = [0_u8; 4];
    cursor
        .read_exact(&mut bytes)
        .map_err(|err| err.to_string())?;
    Ok(i32::from_le_bytes(bytes))
}

fn yca_luma_weights(color_space: HdrColorSpace) -> [f32; 3] {
    match color_space {
        HdrColorSpace::Xyz => [0.0, 1.0, 0.0],
        HdrColorSpace::Rec2020Linear => [0.2627, 0.6780, 0.0593],
        HdrColorSpace::Aces2065_1 => [0.34396645, 0.7281661, -0.07213255],
        HdrColorSpace::LinearSrgb | HdrColorSpace::LinearScRgb | HdrColorSpace::Unknown => {
            [0.2126, 0.7152, 0.0722]
        }
    }
}

fn read_line_samples(
    line: exr::block::lines::LineRef<'_>,
    sample_type: SampleType,
) -> Result<Vec<f32>, String> {
    match sample_type {
        SampleType::F16 => line
            .read_samples::<exr::prelude::f16>()
            .map(|sample| sample.map(|value| value.to_f32()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|err| err.to_string()),
        SampleType::F32 => line
            .read_samples::<f32>()
            .collect::<Result<Vec<_>, _>>()
            .map_err(|err| err.to_string()),
        SampleType::U32 => line
            .read_samples::<u32>()
            .map(|sample| sample.map(|value| value as f32))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|err| err.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use exr::meta::attribute::Chromaticities;

    use crate::hdr::tiled::{HdrTiledSource, HdrTiledSourceKind};
    use crate::hdr::types::HdrColorSpace;

    fn write_test_exr(width: u32, height: u32, suffix: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "simple_image_viewer_exr_tiled_{}_{}.exr",
            std::process::id(),
            suffix
        ));
        let pixels: Vec<f32> = (0..width * height)
            .flat_map(|index| {
                let value = index as f32;
                [value, value + 0.1, value + 0.2, 1.0]
            })
            .collect();
        let img = image::ImageBuffer::<image::Rgba<f32>, Vec<f32>>::from_raw(width, height, pixels)
            .expect("build test EXR image");
        image::DynamicImage::ImageRgba32F(img)
            .save_with_format(&path, image::ImageFormat::OpenExr)
            .expect("write test EXR");
        path
    }

    fn openexr_images_root() -> Option<PathBuf> {
        std::env::var_os("SIV_OPENEXR_IMAGES_DIR")
            .map(PathBuf::from)
            .or_else(|| Some(PathBuf::from(r"F:\HDR\openexr-images")))
            .filter(|path| path.is_dir())
    }

    fn assert_sample_color_space(root: &Path, relative_path: &str, expected: HdrColorSpace) {
        let path = root.join(relative_path);
        assert!(
            path.is_file(),
            "OpenEXR sample file is missing: {}",
            path.display()
        );
        assert_eq!(
            super::exr_color_space(&path).expect("read OpenEXR sample chromaticities"),
            expected,
            "unexpected color space for {}",
            path.display()
        );
    }

    fn assert_sample_extracts_tile(root: &Path, relative_path: &str) {
        let path = root.join(relative_path);
        assert!(
            path.is_file(),
            "OpenEXR sample file is missing: {}",
            path.display()
        );
        let source = super::ExrTiledImageSource::open_with_cache_budget(&path, 4 * 1024 * 1024)
            .expect("open OpenEXR sample as disk-backed tile source");
        let tile_width = source.width.min(8);
        let tile_height = source.height.min(8);
        let tile = source
            .extract_tile_rgba32f_arc(0, 0, tile_width, tile_height)
            .expect("extract tile from OpenEXR sample");

        assert_eq!(tile.width, tile_width);
        assert_eq!(tile.height, tile_height);
        assert_eq!(
            tile.rgba_f32.len(),
            tile_width as usize * tile_height as usize * 4
        );
    }

    fn assert_sample_generates_preview(root: &Path, relative_path: &str) {
        let path = root.join(relative_path);
        assert!(
            path.is_file(),
            "OpenEXR sample file is missing: {}",
            path.display()
        );
        let source = super::ExrTiledImageSource::open_with_cache_budget(&path, 4 * 1024 * 1024)
            .expect("open OpenEXR sample as disk-backed tile source");
        let (width, height, pixels) = source
            .generate_sdr_preview(64, 64)
            .expect("generate SDR preview from OpenEXR sample");

        assert!(width > 0);
        assert!(height > 0);
        assert!(width <= 64);
        assert!(height <= 64);
        assert_eq!(pixels.len(), width as usize * height as usize * 4);
        assert!(
            pixels.chunks_exact(4).any(|pixel| pixel[3] != 0),
            "preview should contain visible pixels for {}",
            path.display()
        );
    }

    #[test]
    fn exr_tiled_source_extracts_requested_rgba32f_region() {
        let path = write_test_exr(4, 2, "region");

        let source: Arc<dyn HdrTiledSource> =
            Arc::new(super::ExrTiledImageSource::open(&path).expect("open EXR tiled source"));
        assert_eq!(source.source_kind(), HdrTiledSourceKind::DiskBacked);
        assert_eq!(source.width(), 4);
        assert_eq!(source.height(), 2);

        let tile = source
            .extract_tile_rgba32f_arc(1, 0, 2, 2)
            .expect("extract EXR region");
        let _ = std::fs::remove_file(&path);

        assert_eq!(tile.width, 2);
        assert_eq!(tile.height, 2);
        assert_eq!(
            tile.rgba_f32.as_slice(),
            &[
                1.0, 1.1, 1.2, 1.0, 2.0, 2.1, 2.2, 1.0, 5.0, 5.1, 5.2, 1.0, 6.0, 6.1, 6.2, 1.0,
            ]
        );
    }

    #[test]
    fn exr_tiled_source_reuses_cached_tile_arcs() {
        let path = write_test_exr(2, 1, "reuse");
        let source = super::ExrTiledImageSource::open(&path).expect("open EXR tiled source");

        let first = source
            .extract_tile_rgba32f_arc(0, 0, 1, 1)
            .expect("extract first tile");
        let second = source
            .extract_tile_rgba32f_arc(0, 0, 1, 1)
            .expect("extract cached tile");
        let _ = std::fs::remove_file(&path);

        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn exr_tiled_source_evicts_lru_cached_tiles_when_over_budget() {
        let path = write_test_exr(3, 1, "lru");
        let source = super::ExrTiledImageSource::open_with_cache_budget(
            &path,
            2 * 4 * std::mem::size_of::<f32>(),
        )
        .expect("open EXR tiled source");

        let first = source
            .extract_tile_rgba32f_arc(0, 0, 1, 1)
            .expect("extract first tile");
        let second = source
            .extract_tile_rgba32f_arc(1, 0, 1, 1)
            .expect("extract second tile");
        let _third = source
            .extract_tile_rgba32f_arc(2, 0, 1, 1)
            .expect("extract third tile");
        let first_after_eviction = source
            .extract_tile_rgba32f_arc(0, 0, 1, 1)
            .expect("re-extract first tile");
        let _ = std::fs::remove_file(&path);

        assert!(!Arc::ptr_eq(&first, &first_after_eviction));
        assert_eq!(source.cached_tile_count(), 2);
        assert!(source.cached_tile_bytes() <= 2 * 4 * std::mem::size_of::<f32>());
        assert!(Arc::strong_count(&second) >= 1);
    }

    #[test]
    fn preview_accumulator_averages_source_pixels_that_map_to_same_preview_pixel() {
        let mut preview = super::PreviewAccumulator::new_with_alpha(2, 1, 1, 1, true);

        preview.set(0, 0, [4.0, 4.0, 4.0, 1.0]);
        preview.set(1, 0, [0.0, 0.0, 0.0, 1.0]);
        let (_w, _h, pixels) = preview.into_sdr_rgba8().expect("tone map preview");

        assert!(
            pixels[0] > 0,
            "downscaled preview should retain energy from bright source pixels"
        );
    }

    #[test]
    fn exr_chromaticities_classify_known_hdr_color_spaces() {
        let srgb = Chromaticities {
            red: exr::math::Vec2(0.64, 0.33),
            green: exr::math::Vec2(0.30, 0.60),
            blue: exr::math::Vec2(0.15, 0.06),
            white: exr::math::Vec2(0.3127, 0.3290),
        };
        let rec2020 = Chromaticities {
            red: exr::math::Vec2(0.708, 0.292),
            green: exr::math::Vec2(0.170, 0.797),
            blue: exr::math::Vec2(0.131, 0.046),
            white: exr::math::Vec2(0.3127, 0.3290),
        };
        let aces_ap0 = Chromaticities {
            red: exr::math::Vec2(0.7347, 0.2653),
            green: exr::math::Vec2(0.0, 1.0),
            blue: exr::math::Vec2(0.0001, -0.0770),
            white: exr::math::Vec2(0.32168, 0.33767),
        };
        let xyz = Chromaticities {
            red: exr::math::Vec2(1.0, 0.0),
            green: exr::math::Vec2(0.0, 1.0),
            blue: exr::math::Vec2(0.0, 0.0),
            white: exr::math::Vec2(0.33333, 0.33333),
        };
        let unknown = Chromaticities {
            red: exr::math::Vec2(0.8, 0.2),
            green: exr::math::Vec2(0.2, 0.8),
            blue: exr::math::Vec2(0.1, 0.1),
            white: exr::math::Vec2(0.333, 0.333),
        };

        assert_eq!(
            super::hdr_color_space_from_exr_chromaticities(None),
            HdrColorSpace::LinearSrgb
        );
        assert_eq!(
            super::hdr_color_space_from_exr_chromaticities(Some(srgb)),
            HdrColorSpace::LinearSrgb
        );
        assert_eq!(
            super::hdr_color_space_from_exr_chromaticities(Some(rec2020)),
            HdrColorSpace::Rec2020Linear
        );
        assert_eq!(
            super::hdr_color_space_from_exr_chromaticities(Some(aces_ap0)),
            HdrColorSpace::Aces2065_1
        );
        assert_eq!(
            super::hdr_color_space_from_exr_chromaticities(Some(xyz)),
            HdrColorSpace::Xyz
        );
        assert_eq!(
            super::hdr_color_space_from_exr_chromaticities(Some(unknown)),
            HdrColorSpace::Unknown
        );
    }

    #[test]
    fn unsupported_exr_chromaticities_diagnostic_includes_xy_coordinates() {
        let unsupported = Chromaticities {
            red: exr::math::Vec2(0.8, 0.2),
            green: exr::math::Vec2(0.2, 0.8),
            blue: exr::math::Vec2(0.1, 0.1),
            white: exr::math::Vec2(0.333, 0.333),
        };

        let diagnostic = super::unsupported_exr_chromaticities_diagnostic(Some(unsupported))
            .expect("unsupported chromaticities should produce a diagnostic");

        assert!(diagnostic.contains("unsupported EXR chromaticities"));
        assert!(diagnostic.contains("red=(0.8000, 0.2000)"));
        assert!(diagnostic.contains("green=(0.2000, 0.8000)"));
        assert!(diagnostic.contains("blue=(0.1000, 0.1000)"));
        assert!(diagnostic.contains("white=(0.3330, 0.3330)"));
    }

    #[test]
    fn openexr_standard_samples_classify_expected_color_spaces() {
        let Some(root) = openexr_images_root() else {
            eprintln!(
                "skipping OpenEXR sample corpus test; set SIV_OPENEXR_IMAGES_DIR to openexr-images"
            );
            return;
        };

        assert_sample_color_space(
            &root,
            "Chromaticities/Rec709.exr",
            HdrColorSpace::LinearSrgb,
        );
        assert_sample_color_space(&root, "Chromaticities/XYZ.exr", HdrColorSpace::Xyz);
        assert_sample_color_space(
            &root,
            "TestImages/WideColorGamut.exr",
            HdrColorSpace::LinearSrgb,
        );
        assert_sample_color_space(&root, "ScanLines/Carrots.exr", HdrColorSpace::Aces2065_1);
    }

    #[test]
    fn openexr_standard_samples_extract_hdr_tiles() {
        let Some(root) = openexr_images_root() else {
            eprintln!(
                "skipping OpenEXR sample corpus test; set SIV_OPENEXR_IMAGES_DIR to openexr-images"
            );
            return;
        };

        assert_sample_extracts_tile(&root, "ScanLines/Carrots.exr");
        assert_sample_extracts_tile(&root, "TestImages/WideColorGamut.exr");
        assert_sample_extracts_tile(&root, "Tiles/GoldenGate.exr");
        assert_sample_extracts_tile(&root, "Chromaticities/Rec709_YC.exr");
    }

    #[test]
    fn openexr_standard_samples_generate_sdr_previews() {
        let Some(root) = openexr_images_root() else {
            eprintln!(
                "skipping OpenEXR sample corpus test; set SIV_OPENEXR_IMAGES_DIR to openexr-images"
            );
            return;
        };

        assert_sample_generates_preview(&root, "ScanLines/Carrots.exr");
        assert_sample_generates_preview(&root, "TestImages/WideColorGamut.exr");
        assert_sample_generates_preview(&root, "Tiles/GoldenGate.exr");
        assert_sample_generates_preview(&root, "Chromaticities/Rec709_YC.exr");
    }
}
