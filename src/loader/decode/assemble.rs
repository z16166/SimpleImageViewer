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

//! HDR/SDR assembly and in-memory tiled sources.

use crate::hdr::types::HdrToneMapSettings;
use crate::loader::{hdr_to_sdr_with_user_tone, DecodedImage, ImageData, TiledImageSource};
use std::sync::Arc;

pub(crate) fn make_image_data(img: DecodedImage) -> ImageData {
    let pixel_count = img.width as u64 * img.height as u64;
    let max_side = img.width.max(img.height);
    // Use the conservative ABSOLUTE_MAX_TEXTURE_SIDE (8192) for the tiling decision,
    // consistent with WIC, macOS ImageIO, and Linux libtiff paths.
    // Images exceeding 8192 on any side benefit from the tiled preview pipeline
    // (instant EXIF preview + async HQ preview) regardless of GPU capability.
    // The GPU's actual texture limit (often 16384) is used only at the wgpu device
    // level to allow tile textures of any supported size.
    let limit = crate::constants::ABSOLUTE_MAX_TEXTURE_SIDE;
    let tiled_limit = crate::tile_cache::TILED_THRESHOLD.load(std::sync::atomic::Ordering::Relaxed);

    if pixel_count >= tiled_limit || max_side > limit {
        log::info!(
            "[Loader] Image {}x{} ({:.1} MP) exceeds GPU limit ({}) or threshold ({:.1} MP). Using forced tiling.",
            img.width,
            img.height,
            pixel_count as f64 / 1_000_000.0,
            limit,
            tiled_limit as f64 / 1_000_000.0
        );
        ImageData::Tiled(Arc::new(MemoryImageSource::new(
            img.width,
            img.height,
            img.into_arc_pixels(),
        )))
    } else {
        ImageData::Static(img)
    }
}

pub(crate) fn make_hdr_image_data(
    hdr: crate::hdr::types::HdrImageBuffer,
    fallback: DecodedImage,
) -> ImageData {
    make_hdr_image_data_for_limit(hdr, fallback, crate::constants::ABSOLUTE_MAX_TEXTURE_SIDE)
}

pub(crate) fn make_hdr_image_data_for_limit(
    hdr: crate::hdr::types::HdrImageBuffer,
    fallback: DecodedImage,
    max_texture_side: u32,
) -> ImageData {
    let pixel_count = hdr.width as u64 * hdr.height as u64;
    let tiled_limit = crate::tile_cache::TILED_THRESHOLD.load(std::sync::atomic::Ordering::Relaxed);
    let max_side = hdr.width.max(hdr.height);

    if pixel_count >= tiled_limit || max_side > max_texture_side {
        log::info!(
            "[Loader] HDR image {}x{} exceeds callback texture limit ({}) or threshold ({:.1} MP). Using SDR tiled fallback.",
            hdr.width,
            hdr.height,
            max_texture_side,
            tiled_limit as f64 / 1_000_000.0
        );
        let fallback_source = Arc::new(MemoryImageSource::new_with_hdr_sdr_fallback(
            fallback.width,
            fallback.height,
            fallback.into_arc_pixels(),
            true,
        ));

        match crate::hdr::tiled::HdrTiledImageSource::new(hdr) {
            Ok(hdr_source) => {
                let kind = crate::hdr::tiled::HdrTiledSource::source_kind(&hdr_source);
                log::info!(
                    "[Loader] HDR tiled source ready: kind={}, {}x{}",
                    kind.as_str(),
                    fallback_source.width(),
                    fallback_source.height()
                );
                ImageData::HdrTiled {
                    hdr: Arc::new(hdr_source),
                    fallback: fallback_source,
                }
            }
            Err(err) => {
                log::warn!("[Loader] HDR tiled source unavailable; using SDR fallback: {err}");
                ImageData::Tiled(fallback_source)
            }
        }
    } else {
        ImageData::Hdr { hdr, fallback }
    }
}
/// A TiledImageSource that serves tiles from an in-memory byte buffer.
/// Primarily used for common formats (PNG, JPEG, etc.) that exceed the GPU's single texture limit.
pub(crate) struct MemoryImageSource {
    width: u32,
    height: u32,
    pixels: Arc<Vec<u8>>,
    hdr_sdr_fallback: bool,
}

impl MemoryImageSource {
    pub fn new(width: u32, height: u32, pixels: Arc<Vec<u8>>) -> Self {
        Self::new_with_hdr_sdr_fallback(width, height, pixels, false)
    }

    pub fn new_with_hdr_sdr_fallback(
        width: u32,
        height: u32,
        pixels: Arc<Vec<u8>>,
        hdr_sdr_fallback: bool,
    ) -> Self {
        Self {
            width,
            height,
            pixels,
            hdr_sdr_fallback,
        }
    }
}

pub(crate) struct HdrSdrTiledFallbackSource {
    source: Arc<dyn crate::hdr::tiled::HdrTiledSource>,
    tone_map: HdrToneMapSettings,
}

impl HdrSdrTiledFallbackSource {
    pub(crate) fn new(
        source: Arc<dyn crate::hdr::tiled::HdrTiledSource>,
        tone_map: HdrToneMapSettings,
    ) -> Self {
        Self { source, tone_map }
    }
}

impl TiledImageSource for HdrSdrTiledFallbackSource {
    fn width(&self) -> u32 {
        self.source.width()
    }

    fn height(&self) -> u32 {
        self.source.height()
    }

    fn is_hdr_sdr_fallback(&self) -> bool {
        true
    }

    fn extract_tile(&self, x: u32, y: u32, w: u32, h: u32) -> Arc<Vec<u8>> {
        let pixels = self
            .source
            .extract_tile_rgba32f_arc(x, y, w, h)
            .and_then(|tile| {
                hdr_to_sdr_with_user_tone(
                    &crate::hdr::types::HdrImageBuffer {
                        width: tile.width,
                        height: tile.height,
                        format: crate::hdr::types::HdrPixelFormat::Rgba32Float,
                        color_space: tile.color_space,
                        metadata: tile.metadata.clone(),
                        rgba_f32: Arc::clone(&tile.rgba_f32),
                    },
                    &self.tone_map,
                )
            })
            .unwrap_or_else(|err| {
                log::warn!("[Loader] HDR SDR tile fallback failed: {err}");
                vec![0; w as usize * h as usize * 4]
            });
        Arc::new(pixels)
    }

    fn generate_preview(&self, max_w: u32, max_h: u32) -> (u32, u32, Vec<u8>) {
        self.source
            .generate_sdr_preview(max_w, max_h)
            .unwrap_or_else(|err| {
                log::warn!("[Loader] HDR SDR preview fallback failed: {err}");
                let scale = (max_w as f32 / self.width() as f32)
                    .min(max_h as f32 / self.height() as f32)
                    .min(1.0);
                let width = ((self.width() as f32 * scale).round() as u32).max(1);
                let height = ((self.height() as f32 * scale).round() as u32).max(1);
                (width, height, vec![0; width as usize * height as usize * 4])
            })
    }

    fn full_pixels(&self) -> Option<Arc<Vec<u8>>> {
        None
    }
}

impl TiledImageSource for MemoryImageSource {
    fn width(&self) -> u32 {
        self.width
    }

    fn height(&self) -> u32 {
        self.height
    }

    fn is_hdr_sdr_fallback(&self) -> bool {
        self.hdr_sdr_fallback
    }

    fn extract_tile(&self, x: u32, y: u32, w: u32, h: u32) -> Arc<Vec<u8>> {
        let mut tile_pixels = Vec::with_capacity((w * h * 4) as usize);
        let stride = self.width as usize * 4;

        for row in y..(y + h) {
            let start = (row as usize * stride) + (x as usize * 4);
            let end = start + (w as usize * 4);
            if end <= self.pixels.len() {
                tile_pixels.extend_from_slice(&self.pixels[start..end]);
            } else {
                // Safety fallback for out-of-bounds
                tile_pixels.resize(tile_pixels.len() + (w * 4) as usize, 0);
            }
        }
        Arc::new(tile_pixels)
    }

    fn generate_preview(&self, max_w: u32, max_h: u32) -> (u32, u32, Vec<u8>) {
        // Since we already have the full image in memory, we can use the image crate
        // to generate a high-quality downscaled preview.
        // OPTIMIZATION: Use ImageBuffer with reference (slice) to avoid cloning giant pixel buffer.
        if let Some(buf) = image::ImageBuffer::<image::Rgba<u8>, &[u8]>::from_raw(
            self.width,
            self.height,
            &self.pixels,
        ) {
            let img = image::imageops::thumbnail(&buf, max_w, max_h);
            (img.width(), img.height(), img.into_raw())
        } else {
            (0, 0, Vec::new())
        }
    }

    fn full_pixels(&self) -> Option<Arc<Vec<u8>>> {
        Some(Arc::clone(&self.pixels))
    }
}
