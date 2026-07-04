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
    use image::ImageReader;
    use std::io::Cursor;

    if is_exr_path(path) {
        return load_hdr(path, hdr_target_capacity, hdr_tone_map);
    }

    let reader = ImageReader::new(Cursor::new(mmap))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let mut decoder = reader;
    // Remove the default memory limit (512MB) to allow gigapixel images
    decoder.no_limits();

    let img = match decoder.decode() {
        Ok(img) => img,
        Err(e) => return Err(e.to_string()),
    };
    let rgba = img.into_rgba8();
    let (width, height) = rgba.dimensions();
    let pixels = rgba.into_raw();

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
    let mmap = crate::mmap_util::map_file(path)?;
    load_static_from_mmap(path, &mmap, hdr_target_capacity, hdr_tone_map)
}
pub(crate) fn process_animation_frames(
    raw_frames: Vec<image::Frame>,
    path: &Path,
    mmap: Option<&[u8]>,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
) -> Result<ImageData, String> {
    if raw_frames.len() <= 1 {
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
    let mmap = crate::mmap_util::map_file(path)?;
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
    let mmap = crate::mmap_util::map_file(path)?;
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
    let mmap = crate::mmap_util::map_file(path)?;
    load_webp_from_mmap(path, &mmap, hdr_target_capacity, hdr_tone_map)
}

// ---------------------------------------------------------------------------
// PSD / PSB (Photoshop Document / Large Document)
// ---------------------------------------------------------------------------

pub(crate) fn load_psd(path: &Path) -> Result<ImageData, String> {
    // Step 1: Map the file once standardly
    let mmap = crate::mmap_util::map_file(path).map_err(|e| format!("Failed to read PSD: {e}"))?;

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
        "PSD/PSB {}x{}: estimated {estimated_mb} MB, available {available_mb} MB — proceeding",
        width,
        height
    );

    // Step 4: Detect version and choose decoder
    let version = u16::from_be_bytes([mmap[4], mmap[5]]);

    if version == 2 {
        // PSB v2: Use tiled source for large files
        log::info!("Using custom PSB tiled source for v2 format");
        let source = crate::psb_reader::open_tiled_source(path)?;
        let arc_source = std::sync::Arc::new(source);
        Ok(ImageData::Tiled(arc_source))
    } else {
        // PSD v1: use the psd crate (mmap bitstream; `psd` still allocates its own structures).
        // Decode on the refinement pool: `catch_exr_panic` contains unwinding panics; the pool
        // reuses workers instead of spawning a new OS thread per file.

        use crate::loader::preview_caps::REFINEMENT_POOL;
        use std::sync::mpsc::sync_channel;

        let (tx, rx) = sync_channel(1);
        REFINEMENT_POOL.spawn(move || {
            let result = crate::hdr::exr_tiled::catch_exr_panic("PSD v1 decode", || {
                let psd_file = psd::Psd::from_bytes(&mmap[..])
                    .map_err(|e| format!("Failed to parse PSD: {e}"))?;
                let w = psd_file.width();
                let h = psd_file.height();
                let pixels = psd_file.rgba();
                Ok((w, h, pixels))
            });
            let _ = tx.send(result);
        });

        match rx.recv() {
            Ok(Ok((w, h, pixels))) => {
                let img = DecodedImage::new(w, h, pixels);
                Ok(apply_exif_orientation_to_image_data(
                    path,
                    make_image_data(img),
                    None,
                ))
            }
            Ok(Err(e)) => {
                const PSD_DECODE_PANIC_PREFIX: &str = "PSD v1 decode: decoder panic: ";
                if let Some(msg) = e.strip_prefix(PSD_DECODE_PANIC_PREFIX) {
                    log::error!(
                        "[Loader] PSD decoder panicked for {}: {}",
                        path.display(),
                        msg
                    );
                    Err(format!(
                        "PSD decode failed (psd crate internal error — corrupt or unsupported layer data): {msg}"
                    ))
                } else {
                    Err(e)
                }
            }
            Err(_) => Err("PSD decode failed: refinement pool dropped the result".to_string()),
        }
    }
}

/// Returns true if the extension belongs to a format that we prefer to load
/// via image-rs or the native codec path to preserve animations (GIF, WebP, APNG, JPEG XL).
pub(crate) fn is_maybe_animated(ext: &str) -> bool {
    matches!(ext, "gif" | "webp" | "apng" | "png" | "jxl")
}
