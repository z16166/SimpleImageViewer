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
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use exr::block::BlockIndex;
use exr::block::reader::ChunksReader;
use exr::math::Vec2;
use exr::meta::attribute::{ChannelList, Chromaticities, SampleType};

use crate::hdr::tiled::{
    HdrTileBuffer, HdrTileCache, HdrTiledSource, HdrTiledSourceKind,
    configured_hdr_tile_cache_max_bytes,
};
use crate::hdr::types::HdrColorSpace;

#[derive(Debug)]
pub struct ExrTiledImageSource {
    path: PathBuf,
    width: u32,
    height: u32,
    color_space: HdrColorSpace,
    tile_cache: Mutex<HdrTileCache>,
}

impl ExrTiledImageSource {
    pub fn open(path: &Path) -> Result<Self, String> {
        Self::open_with_cache_budget(path, configured_hdr_tile_cache_max_bytes())
    }

    pub fn open_with_cache_budget(path: &Path, max_cache_bytes: usize) -> Result<Self, String> {
        let file = File::open(path).map_err(|err| err.to_string())?;
        let reader =
            exr::block::read(BufReader::new(file), false).map_err(|err| err.to_string())?;
        let header = reader
            .headers()
            .first()
            .ok_or_else(|| "EXR file has no image layers".to_string())?;
        let width = u32::try_from(header.layer_size.width())
            .map_err(|_| "EXR width exceeds u32".to_string())?;
        let height = u32::try_from(header.layer_size.height())
            .map_err(|_| "EXR height exceeds u32".to_string())?;
        validate_required_rgba_channels(&header.channels)?;
        let color_space =
            hdr_color_space_from_exr_chromaticities(header.shared_attributes.chromaticities);

        Ok(Self {
            path: path.to_path_buf(),
            width,
            height,
            color_space,
            tile_cache: Mutex::new(HdrTileCache::new(max_cache_bytes)),
        })
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
        let preview = exr::prelude::read_first_rgba_layer_from_file(
            &self.path,
            move |resolution, _channels| {
                PreviewAccumulator::new(
                    resolution.width() as u32,
                    resolution.height() as u32,
                    max_w,
                    max_h,
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

pub(crate) fn exr_color_space(path: &Path) -> Result<HdrColorSpace, String> {
    let file = File::open(path).map_err(|err| err.to_string())?;
    let reader = exr::block::read(BufReader::new(file), false).map_err(|err| err.to_string())?;
    let header = reader
        .headers()
        .first()
        .ok_or_else(|| "EXR file has no image layers".to_string())?;
    Ok(hdr_color_space_from_exr_chromaticities(
        header.shared_attributes.chromaticities,
    ))
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
    } else {
        HdrColorSpace::Unknown
    }
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
    rgba_sum: Vec<f32>,
    sample_counts: Vec<u32>,
}

impl PreviewAccumulator {
    fn new(source_width: u32, source_height: u32, max_w: u32, max_h: u32) -> Self {
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

fn channel_roles(channels: &ChannelList) -> Vec<Option<usize>> {
    channels
        .list
        .iter()
        .map(|channel| {
            if channel.name.eq_case_insensitive("R") {
                Some(0)
            } else if channel.name.eq_case_insensitive("G") {
                Some(1)
            } else if channel.name.eq_case_insensitive("B") {
                Some(2)
            } else if channel.name.eq_case_insensitive("A") {
                Some(3)
            } else {
                None
            }
        })
        .collect()
}

fn validate_required_rgba_channels(channels: &ChannelList) -> Result<(), String> {
    let roles = channel_roles(channels);
    for (name, index) in [("R", 0), ("G", 1), ("B", 2)] {
        if !roles.iter().any(|role| *role == Some(index)) {
            return Err(format!(
                "EXR layer does not contain required {name} channel"
            ));
        }
    }
    Ok(())
}

fn copy_line_channel_to_tile(
    line: exr::block::lines::LineRef<'_>,
    sample_type: SampleType,
    channel: usize,
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
        let dest = (dest_y * tile.width as usize + dest_x) * 4 + channel;
        rgba[dest] = samples[source_x];
    }

    Ok(())
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
        let mut preview = super::PreviewAccumulator::new(2, 1, 1, 1);

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
            super::hdr_color_space_from_exr_chromaticities(Some(unknown)),
            HdrColorSpace::Unknown
        );
    }
}
