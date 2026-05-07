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

use crate::constants::{BYTES_PER_GB, BYTES_PER_MB, DEFAULT_ANIMATION_DELAY_MS, MIN_ANIMATION_DELAY_THRESHOLD_MS};
use crate::hdr::types::HdrToneMapSettings;
use crate::loader::{apply_exif_orientation_to_image_data, AnimationFrame, DecodedImage, ImageData};
use std::path::PathBuf;
use std::time::Duration;

use super::assemble::make_image_data;
use super::hdr_formats::{is_exr_path, load_hdr};

pub(crate) fn load_static(
    path: &PathBuf,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
) -> Result<ImageData, String> {
    use image::ImageReader;

    if is_exr_path(path) {
        return load_hdr(path, hdr_target_capacity, hdr_tone_map);
    }

    let reader = ImageReader::open(path).map_err(|e| e.to_string())?;
    let mut decoder = reader.with_guessed_format().map_err(|e| e.to_string())?;
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
        path.as_path(),
        make_image_data(DecodedImage::new(width, height, pixels)),
    ))
}
pub(crate) fn process_animation_frames(
    raw_frames: Vec<image::Frame>,
    path: &PathBuf,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
) -> Result<ImageData, String> {
    if raw_frames.len() <= 1 {
        return load_static(path, hdr_target_capacity, hdr_tone_map);
    }

    let frames: Vec<AnimationFrame> = raw_frames
        .into_iter()
        .map(|frame| {
            let (numer, denom) = frame.delay().numer_denom_ms();
            let delay_ms = if denom == 0 {
                DEFAULT_ANIMATION_DELAY_MS
            } else {
                numer / denom
            };
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
        path.as_path(),
        ImageData::Animated(frames),
    ))
}

pub(crate) fn load_gif(path: &PathBuf, hdr_target_capacity: f32, hdr_tone_map: HdrToneMapSettings) -> Result<ImageData, String> {
    use image::AnimationDecoder;
    use image::codecs::gif::GifDecoder;
    use std::io::BufReader;

    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let reader = BufReader::new(file);
    let decoder = GifDecoder::new(reader).map_err(|e| e.to_string())?;
    let raw_frames = decoder
        .into_frames()
        .collect_frames()
        .map_err(|e| e.to_string())?;

    process_animation_frames(raw_frames, path, hdr_target_capacity, hdr_tone_map)
}

pub(crate) fn load_png(path: &PathBuf, hdr_target_capacity: f32, hdr_tone_map: HdrToneMapSettings) -> Result<ImageData, String> {
    use image::AnimationDecoder;
    use image::codecs::png::PngDecoder;
    use std::io::BufReader;

    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let reader = BufReader::new(file);
    let decoder = PngDecoder::new(reader).map_err(|e| e.to_string())?;

    if !decoder.is_apng().map_err(|e| e.to_string())? {
        return load_static(path, hdr_target_capacity, hdr_tone_map);
    }

    let raw_frames = decoder
        .apng()
        .map_err(|e| e.to_string())?
        .into_frames()
        .collect_frames()
        .map_err(|e| e.to_string())?;

    process_animation_frames(raw_frames, path, hdr_target_capacity, hdr_tone_map)
}

// ---------------------------------------------------------------------------
// Animated WebP
// ---------------------------------------------------------------------------

pub(crate) fn load_webp(path: &PathBuf, hdr_target_capacity: f32, hdr_tone_map: HdrToneMapSettings) -> Result<ImageData, String> {
    use image::AnimationDecoder;
    use image::codecs::webp::WebPDecoder;
    use std::io::BufReader;

    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let reader = BufReader::new(file);
    let decoder = WebPDecoder::new(reader).map_err(|e| e.to_string())?;
    let raw_frames = decoder
        .into_frames()
        .collect_frames()
        .map_err(|e| e.to_string())?;

    process_animation_frames(raw_frames, path, hdr_target_capacity, hdr_tone_map)
}

// ---------------------------------------------------------------------------
// PSD / PSB (Photoshop Document / Large Document)
// ---------------------------------------------------------------------------

pub(crate) fn load_psd(path: &PathBuf) -> Result<ImageData, String> {
    // Step 1: Estimate memory requirement from header
    let (width, height, _channels, estimated_bytes) = crate::psb_reader::estimate_memory(path)?;
    let estimated_mb = estimated_bytes / BYTES_PER_MB;

    // Step 2: Check available RAM
    use sysinfo::System;
    let mut sys = System::new();
    sys.refresh_memory();
    let available_mb = sys.available_memory() / BYTES_PER_MB;

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

    // Step 3: Detect version and choose decoder
    let mut sig_buf = [0u8; 6];
    {
        use std::io::Read;
        let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
        f.read_exact(&mut sig_buf).map_err(|e| e.to_string())?;
    }
    let version = u16::from_be_bytes([sig_buf[4], sig_buf[5]]);

    if version == 2 {
        // PSB v2: Use tiled source for large files
        log::info!("Using custom PSB tiled source for v2 format");
        let source = crate::psb_reader::open_tiled_source(path)?;
        let arc_source = std::sync::Arc::new(source);
        Ok(ImageData::Tiled(arc_source))
    } else {
        // PSD v1: use the psd crate (mmap bitstream; `psd` still allocates its own structures).
        // Decode on a dedicated thread: `join()` turns any unwinding panic into `Err`, which is
        // more reliable than `catch_unwind` alone when the loader runs on worker pools / mixed stacks.
        let mmap = crate::mmap_util::map_file(path)
            .map_err(|e| format!("Failed to read PSD: {e}"))?;

        let handle = std::thread::Builder::new()
            .name("siv-psd-v1".to_string())
            .spawn(move || {
                // Must use the same panic-hook suppression as EXR: `setup_panic_hook` calls
                // `process::exit(1)` on every panic; without suppression, a caught decoder panic
                // still runs the hook and terminates before `join()` can turn it into `Err`.
                crate::hdr::exr_tiled::catch_exr_panic("PSD v1 decode", || {
                    let psd_file = psd::Psd::from_bytes(&mmap[..])
                        .map_err(|e| format!("Failed to parse PSD: {e}"))?;
                    let w = psd_file.width();
                    let h = psd_file.height();
                    let pixels = psd_file.rgba();
                    Ok((w, h, pixels))
                })
            })
            .map_err(|e| format!("Failed to spawn PSD decoder thread: {e}"))?;

        match handle.join() {
            Ok(Ok((w, h, pixels))) => {
                let img = DecodedImage::new(w, h, pixels);
                Ok(apply_exif_orientation_to_image_data(
                    path.as_path(),
                    make_image_data(img),
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
            Err(panic_payload) => {
                let msg = if let Some(s) = panic_payload.downcast_ref::<&str>() {
                    (*s).to_string()
                } else if let Some(s) = panic_payload.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "unknown panic in psd decode thread".to_string()
                };
                log::error!(
                    "[Loader] PSD decode thread panicked for {}: {}",
                    path.display(),
                    msg
                );
                Err(format!(
                    "PSD decode failed (psd crate internal error — corrupt or unsupported layer data): {msg}"
                ))
            }
        }
    }
}

/// Returns true if the extension belongs to a format that we prefer to load
/// via image-rs or the native codec path to preserve animations (GIF, WebP, APNG, JPEG XL).
pub(crate) fn is_maybe_animated(ext: &str) -> bool {
    matches!(ext, "gif" | "webp" | "apng" | "png" | "jxl")
}
