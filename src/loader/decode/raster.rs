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

//! GIF / PNG / WebP / PSD and static raster via `image`.

use crate::constants::{
    BYTES_PER_GB, BYTES_PER_MB, DEFAULT_ANIMATION_DELAY_MS, MIN_ANIMATION_DELAY_THRESHOLD_MS,
};
use crate::hdr::types::HdrToneMapSettings;
use crate::loader::{
    AnimationFrame, DecodedImage, ImageData, apply_exif_orientation_to_image_data,
};
use std::path::Path;
use std::time::Duration;

use super::assemble::make_image_data;
use super::hdr_formats::{is_exr_path, load_hdr};

pub(crate) fn load_static_from_mmap(
    path: &Path,
    mmap: &[u8],
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
) -> Result<ImageData, String> {
    use image::{ColorType, DynamicImage, ImageDecoder, ImageReader};
    use std::io::Cursor;

    if is_exr_path(path) {
        return load_hdr(path, hdr_target_capacity, hdr_tone_map);
    }

    let mut reader = ImageReader::new(Cursor::new(mmap))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    // Remove the default memory limit (512MB) to allow gigapixel images
    reader.no_limits();

    let decoder = reader.into_decoder().map_err(|e| e.to_string())?;
    let (width, height) = decoder.dimensions();
    // Decode straight into the final RGBA8 buffer when the codec already emits
    // Rgba8/Rgb8, avoiding DynamicImage + into_rgba8 intermediate allocations.
    let pixels = match decoder.color_type() {
        ColorType::Rgba8 => {
            let len = usize::try_from(decoder.total_bytes()).map_err(|_| {
                format!("image dimensions {width}x{height} exceed addressable memory")
            })?;
            let mut buf = vec![0u8; len];
            decoder.read_image(&mut buf).map_err(|e| e.to_string())?;
            buf
        }
        ColorType::Rgb8 => {
            let rgb_len = usize::try_from(decoder.total_bytes()).map_err(|_| {
                format!("image dimensions {width}x{height} exceed addressable memory")
            })?;
            let mut rgb = vec![0u8; rgb_len];
            decoder.read_image(&mut rgb).map_err(|e| e.to_string())?;
            let rgba_len = rgb_len
                .checked_div(3)
                .and_then(|px| px.checked_mul(4))
                .ok_or_else(|| {
                    format!("image dimensions {width}x{height} exceed addressable memory")
                })?;
            let mut rgba = vec![0u8; rgba_len];
            simple_image_viewer::simd_swizzle::interleave_rgb_packed_to_rgba_packed(
                &rgb, &mut rgba,
            );
            rgba
        }
        _ => {
            let img = DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
            img.into_rgba8().into_raw()
        }
    };

    Ok(apply_exif_orientation_to_image_data(
        path,
        make_image_data(DecodedImage::new(width, height, pixels)),
        Some(mmap),
    ))
}

pub(crate) fn load_static(
    path: &Path,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
) -> Result<ImageData, String> {
    if is_exr_path(path) {
        return load_hdr(path, hdr_target_capacity, hdr_tone_map);
    }
    let (mmap, _) = crate::mmap_util::map_file(path)?;
    load_static_from_mmap(path, &mmap, hdr_target_capacity, hdr_tone_map)
}
/// Convert an already-decoded `image::Frame` into static [`ImageData`] without
/// re-running the format decoder (used for single-frame GIF/APNG/WebP).
pub(crate) fn image_frame_to_static_image_data(
    frame: image::Frame,
    path: &Path,
    mmap: Option<&[u8]>,
) -> ImageData {
    let buffer = frame.into_buffer();
    let (width, height) = buffer.dimensions();
    apply_exif_orientation_to_image_data(
        path,
        make_image_data(DecodedImage::new(width, height, buffer.into_raw())),
        mmap,
    )
}

pub(crate) fn process_animation_frames(
    raw_frames: Vec<image::Frame>,
    path: &Path,
    mmap: Option<&[u8]>,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
) -> Result<ImageData, String> {
    // One frame (or empty): reuse the decoded buffer when present; only fall back
    // to a full static decode when the decoder produced no frames at all.
    if raw_frames.len() <= 1 {
        if let Some(frame) = raw_frames.into_iter().next() {
            return Ok(image_frame_to_static_image_data(frame, path, mmap));
        }
        if let Some(bytes) = mmap {
            return load_static_from_mmap(path, bytes, hdr_target_capacity, hdr_tone_map);
        }
        return load_static(path, hdr_target_capacity, hdr_tone_map);
    }

    let frames: Vec<AnimationFrame> = raw_frames
        .into_iter()
        .map(|frame| {
            let (numer, denom) = frame.delay().numer_denom_ms();
            let delay_ms = numer
                .checked_div(denom)
                .unwrap_or(DEFAULT_ANIMATION_DELAY_MS);
            // Standard browser behavior: delays <= 10ms are treated as 100ms
            let delay_ms = if delay_ms <= MIN_ANIMATION_DELAY_THRESHOLD_MS {
                DEFAULT_ANIMATION_DELAY_MS
            } else {
                delay_ms
            };
            let buffer = frame.into_buffer();
            let (width, height) = buffer.dimensions();
            AnimationFrame::new(
                width,
                height,
                buffer.into_raw(),
                Duration::from_millis(delay_ms as u64),
            )
        })
        .collect();

    Ok(apply_exif_orientation_to_image_data(
        path,
        ImageData::Animated(frames),
        mmap,
    ))
}

pub(crate) fn load_gif_from_mmap(
    path: &Path,
    mmap: &[u8],
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
) -> Result<ImageData, String> {
    use image::AnimationDecoder;
    use image::codecs::gif::GifDecoder;
    use std::io::Cursor;

    let reader = Cursor::new(mmap);
    let decoder = GifDecoder::new(reader).map_err(|e| e.to_string())?;
    let raw_frames = decoder
        .into_frames()
        .collect_frames()
        .map_err(|e| e.to_string())?;

    process_animation_frames(
        raw_frames,
        path,
        Some(mmap),
        hdr_target_capacity,
        hdr_tone_map,
    )
}

pub(crate) fn load_gif(
    path: &Path,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
) -> Result<ImageData, String> {
    let (mmap, _) = crate::mmap_util::map_file(path)?;
    load_gif_from_mmap(path, &mmap, hdr_target_capacity, hdr_tone_map)
}

pub(crate) fn load_png_from_mmap(
    path: &Path,
    mmap: &[u8],
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
) -> Result<ImageData, String> {
    use image::AnimationDecoder;
    use image::codecs::png::PngDecoder;
    use std::io::Cursor;

    let reader = Cursor::new(mmap);
    let decoder = PngDecoder::new(reader).map_err(|e| e.to_string())?;

    if !decoder.is_apng().map_err(|e| e.to_string())? {
        return load_static_from_mmap(path, mmap, hdr_target_capacity, hdr_tone_map);
    }

    let raw_frames = decoder
        .apng()
        .map_err(|e| e.to_string())?
        .into_frames()
        .collect_frames()
        .map_err(|e| e.to_string())?;

    process_animation_frames(
        raw_frames,
        path,
        Some(mmap),
        hdr_target_capacity,
        hdr_tone_map,
    )
}

pub(crate) fn load_png(
    path: &Path,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
) -> Result<ImageData, String> {
    let (mmap, _) = crate::mmap_util::map_file(path)?;
    load_png_from_mmap(path, &mmap, hdr_target_capacity, hdr_tone_map)
}

// ---------------------------------------------------------------------------
// Animated WebP
// ---------------------------------------------------------------------------

pub(crate) fn load_webp_from_mmap(
    path: &Path,
    mmap: &[u8],
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
) -> Result<ImageData, String> {
    use image::AnimationDecoder;
    use image::codecs::webp::WebPDecoder;
    use std::io::Cursor;

    let reader = Cursor::new(mmap);
    let decoder = WebPDecoder::new(reader).map_err(|e| e.to_string())?;
    let raw_frames = decoder
        .into_frames()
        .collect_frames()
        .map_err(|e| e.to_string())?;

    process_animation_frames(
        raw_frames,
        path,
        Some(mmap),
        hdr_target_capacity,
        hdr_tone_map,
    )
}

pub(crate) fn load_webp(
    path: &Path,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
) -> Result<ImageData, String> {
    let (mmap, _) = crate::mmap_util::map_file(path)?;
    load_webp_from_mmap(path, &mmap, hdr_target_capacity, hdr_tone_map)
}

// ---------------------------------------------------------------------------
// PSD / PSB (Photoshop Document / Large Document)
// ---------------------------------------------------------------------------

pub(crate) fn load_psd(
    path: &Path,
    cancel: crate::loader::DecodeCancelFlag,
    gpu: Option<crate::psb_layer_blend_gpu::PsdGpuContext>,
) -> Result<ImageData, String> {
    // Step 1: Map the file once standardly
    let (mmap, _) =
        crate::mmap_util::map_file(path).map_err(|e| format!("Failed to read PSD: {e}"))?;

    // Step 2: Estimate memory requirement from header bytes
    let (width, height, _channels, estimated_bytes) =
        crate::psb_reader::estimate_memory_from_bytes(&mmap)?;
    let estimated_mb = estimated_bytes / BYTES_PER_MB;

    // Step 3: Check available RAM (cached snapshot; refreshed on the logic thread / monitor changes).
    let available_mb = crate::system_memory::available_memory_mb();

    // Reserve at least 1GB for the OS + app overhead
    let safe_available = available_mb.saturating_sub(BYTES_PER_GB / BYTES_PER_MB);
    if estimated_mb > safe_available {
        return Err(format!(
            "Image requires ~{estimated_mb} MB RAM but only ~{safe_available} MB is available. \
             Please close other applications or convert to a smaller format."
        ));
    }

    log::info!(
        "PSD/PSB {}x{}: estimated {estimated_mb} MB, available {available_mb} MB -- proceeding",
        width,
        height
    );

    // Step 4: Large PSB keeps on-demand disk tiling (Hubble-class files). Header size is
    // used only as an OOM guard before full decode; PSD and smaller PSB decode first, then
    // route Static vs in-memory tiled by *actual* P1/P2/P3 dimensions via make_image_data.
    let version = u16::from_be_bytes([mmap[4], mmap[5]]);
    if version == 2 && psd_header_requires_disk_tiled(width, height) {
        log::info!(
            "Using PSB disk tiled source for header {}x{} (exceeds tiled limits)",
            width,
            height
        );
        let source = crate::psb_reader::open_tiled_source(path)?;
        return Ok(ImageData::Tiled(std::sync::Arc::new(source)));
    }

    if cancel.is_cancelled() {
        return Err(crate::loader::DECODE_CANCELLED.to_string());
    }

    let composite = crate::psb_layer_composite::decode_psd_sdr_main_from_bytes_with_cancel(
        &mmap[..],
        Some(cancel.as_atomic()),
        gpu.as_ref(),
    )?;
    let img = DecodedImage::new(composite.width, composite.height, composite.pixels);
    let oriented =
        apply_exif_orientation_to_image_data(path, ImageData::Static(img), Some(&mmap[..]));
    match oriented {
        ImageData::Static(decoded) => {
            log::info!(
                "PSD/PSB decoded display size {}x{} (header was {}x{}); routing via make_image_data",
                decoded.width,
                decoded.height,
                width,
                height
            );
            Ok(make_image_data(decoded))
        }
        other => Err(format!(
            "PSD/PSB orientation produced unexpected image shape ({:?})",
            std::mem::discriminant(&other)
        )),
    }
}

/// Same limits as [`super::assemble::make_image_data`], applied to PSB header dims so
/// multi-GB documents skip full-canvas decode.
fn psd_header_requires_disk_tiled(width: u32, height: u32) -> bool {
    let pixel_count = u64::from(width) * u64::from(height);
    let max_side = width.max(height);
    pixel_count >= crate::tile_cache::get_tiled_threshold()
        || max_side > crate::constants::ABSOLUTE_MAX_TEXTURE_SIDE
}

/// Returns true if the extension belongs to a format that we prefer to load
/// via image-rs or the native codec path to preserve animations (GIF, WebP, APNG, JPEG XL).
pub(crate) fn is_maybe_animated(ext: &str) -> bool {
    matches!(ext, "gif" | "webp" | "apng" | "png" | "jxl")
}
