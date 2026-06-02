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

//! Decode pipeline (`load_image_file`) and submodule graph.
//!
//! On Windows, [`crate::wic::load_via_wic`] expects COM on the calling thread; [`crate::loader::ImageLoader`]
//! installs [`crate::wic::ComGuard`] on loader and tile worker threads before invoking this pipeline.

mod assemble;
mod detect;
mod hdr_formats;
mod jpeg;
mod modern;
mod raster;
mod raw;

use crate::constants::{BYTES_PER_MB, DEFAULT_PREVIEW_SIZE};
use crate::hdr::types::HdrToneMapSettings;
use crossbeam_channel::Sender;
use std::path::{Path, PathBuf};

use super::{
    DecodedImage, ImageData, LoadResult, LoaderOutput, PreviewBundle, PreviewStage,
    RefinementRequest,
};
use super::{extract_exif_thumbnail, hdr_display_requests_sdr_preview};

use assemble::{make_hdr_image_data, make_image_data};
use detect::{load_primary_with_detection_fallback, recover_via_platform_and_content_detection};
use hdr_formats::load_hdr;
use jpeg::load_jpeg_with_target_capacity;
use modern::{load_avif_with_target_capacity, load_heif_hdr_aware, load_jxl_with_target_capacity};
use raster::{is_maybe_animated, load_gif, load_png, load_psd, load_static, load_webp};
use raw::load_raw;

pub(crate) fn load_image_file(
    generation: u64,
    index: usize,
    path: &PathBuf,
    _tx: Sender<LoaderOutput>,
    refine_tx: Sender<RefinementRequest>,
    high_quality: bool,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
) -> LoadResult {
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");

    let result = (|| -> Result<ImageData, String> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_default();
        let is_system_native = if let Ok(reg) = crate::formats::get_registry().read() {
            reg.extensions.contains(&ext)
        } else {
            false
        };

        if ext == "exr" {
            return load_primary_with_detection_fallback(
                path,
                file_name,
                hdr_target_capacity,
                hdr_tone_map,
                high_quality,
                || load_hdr(path, hdr_target_capacity, hdr_tone_map),
            );
        }

        if crate::hdr::decode::is_hdr_candidate_ext(&ext) {
            match load_hdr(path, hdr_target_capacity, hdr_tone_map) {
                Ok(img) => return Ok(img),
                Err(e) => {
                    log::debug!(
                        "[{}] HDR float decode failed, continuing with standard fallback chain: {}",
                        file_name,
                        e
                    );
                }
            }
        }

        // PSD/PSB: only `load_psd` (do not fall through — image-rs would invoke `psd` again without catch_unwind).
        if ext == "psd" || ext == "psb" {
            return load_psd(path);
        }

        let is_raw = crate::raw_processor::is_raw_extension(&ext);

        if is_raw {
            return load_raw(
                index,
                generation,
                path,
                refine_tx.clone(),
                high_quality,
                hdr_target_capacity,
                hdr_tone_map,
            );
        }

        if ext == "jpg" || ext == "jpeg" {
            return load_primary_with_detection_fallback(
                path,
                file_name,
                hdr_target_capacity,
                hdr_tone_map,
                high_quality,
                || load_jpeg_with_target_capacity(path, hdr_target_capacity, hdr_tone_map),
            );
        }
        if ext == "tif" || ext == "tiff" {
            return load_primary_with_detection_fallback(
                path,
                file_name,
                hdr_target_capacity,
                hdr_tone_map,
                high_quality,
                || crate::libtiff_loader::load_via_libtiff(path, hdr_target_capacity, hdr_tone_map),
            );
        }

        if ext == "avif" || ext == "avifs" {
            return load_primary_with_detection_fallback(
                path,
                file_name,
                hdr_target_capacity,
                hdr_tone_map,
                high_quality,
                || load_avif_with_target_capacity(path, hdr_target_capacity, hdr_tone_map),
            );
        }

        if ext == "jxl" {
            return load_primary_with_detection_fallback(
                path,
                file_name,
                hdr_target_capacity,
                hdr_tone_map,
                high_quality,
                || load_jxl_with_target_capacity(path, hdr_target_capacity, hdr_tone_map),
            );
        }

        if ext == "heif" || ext == "heic" || ext == "hif" {
            return load_primary_with_detection_fallback(
                path,
                file_name,
                hdr_target_capacity,
                hdr_tone_map,
                high_quality,
                || load_heif_hdr_aware(path, hdr_target_capacity, hdr_tone_map),
            );
        }

        if is_system_native && !is_maybe_animated(&ext) {
            #[cfg(target_os = "windows")]
            if let Ok(img) = crate::wic::load_via_wic(path, high_quality, None) {
                return Ok(img);
            }
            #[cfg(target_os = "macos")]
            if let Ok(img) = crate::macos_image_io::load_via_image_io(path, high_quality, None) {
                return Ok(img);
            }
        }

        let result = match ext.as_str() {
            "gif" => load_gif(path, hdr_target_capacity, hdr_tone_map),
            "png" | "apng" => load_png(path, hdr_target_capacity, hdr_tone_map),
            "webp" => load_webp(path, hdr_target_capacity, hdr_tone_map),
            _ => load_static(path, hdr_target_capacity, hdr_tone_map),
        };
        match result {
            Ok(image) => Ok(image),
            Err(primary_err) => recover_via_platform_and_content_detection(
                path,
                file_name,
                hdr_target_capacity,
                hdr_tone_map,
                high_quality,
                primary_err,
            ),
        }
    })();

    let mut preview: Option<DecodedImage> = None;
    let mut hdr_preview: Option<std::sync::Arc<crate::hdr::types::HdrImageBuffer>> = None;

    let final_result = match result {
        Ok(ImageData::Tiled(source)) => {
            log::info!(
                "[{}] Tiled image source active: {}x{} ({:.1} MP)",
                file_name,
                source.width(),
                source.height(),
                (source.width() as f64 * source.height() as f64) / 1_000_000.0
            );

            let t0 = std::time::Instant::now();
            let exif_thumb = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                extract_exif_thumbnail(path)
            }));
            match exif_thumb {
                Ok(Some(thumb)) => {
                    log::info!(
                        "[{}] EXIF thumbnail extracted in {:?}",
                        file_name,
                        t0.elapsed()
                    );
                    preview = Some(thumb);
                }
                Ok(None) => {
                    log::info!(
                        "[{}] No EXIF thumbnail found (took {:?}), generating {}px preview...",
                        file_name,
                        t0.elapsed(),
                        DEFAULT_PREVIEW_SIZE
                    );
                    let t1 = std::time::Instant::now();
                    let gen_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        source.generate_preview(DEFAULT_PREVIEW_SIZE, DEFAULT_PREVIEW_SIZE)
                    }));
                    match gen_result {
                        Ok((pw, ph, p_pixels)) if pw > 0 && ph > 0 => {
                            log::info!(
                                "[{}] {}px preview generated ({}x{}) in {:?}",
                                file_name,
                                DEFAULT_PREVIEW_SIZE,
                                pw,
                                ph,
                                t1.elapsed()
                            );
                            preview = Some(DecodedImage::new(pw, ph, p_pixels));
                        }
                        Ok(_) => {
                            log::warn!(
                                "[{}] generate_preview returned empty/zero-size result in {:?}",
                                file_name,
                                t1.elapsed()
                            );
                        }
                        Err(e) => {
                            log::error!(
                                "[{}] generate_preview PANICKED: {:?} in {:?}",
                                file_name,
                                e,
                                t1.elapsed()
                            );
                        }
                    }
                }
                Err(e) => {
                    log::error!("[{}] extract_exif_thumbnail PANICKED: {:?}", file_name, e);
                }
            }

            Ok(ImageData::Tiled(source))
        }
        Ok(ImageData::HdrTiled { hdr, fallback }) => {
            log::info!(
                "[{}] HDR tiled image source active: {}x{} ({:.1} MP)",
                file_name,
                hdr.width(),
                hdr.height(),
                (hdr.width() as f64 * hdr.height() as f64) / 1_000_000.0
            );
            if !hdr_display_requests_sdr_preview(hdr_target_capacity) {
                // HDR mode: generate HDR preview primarily, fallback to SDR if it fails
                let t0 = std::time::Instant::now();
                let hdr_preview_result =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        hdr.generate_hdr_preview(DEFAULT_PREVIEW_SIZE, DEFAULT_PREVIEW_SIZE)
                    }));
                match hdr_preview_result {
                    Ok(Ok(image)) if image.width > 0 && image.height > 0 => {
                        log::info!(
                            "[{}] HDR {}px preview generated ({}x{}) in {:?}",
                            file_name,
                            DEFAULT_PREVIEW_SIZE,
                            image.width,
                            image.height,
                            t0.elapsed()
                        );
                        hdr_preview = Some(std::sync::Arc::new(image));
                    }
                    _ => {
                        log::warn!(
                            "[{}] HDR preview generation failed or zero-sized in {:?}, trying source SDR preview fallback",
                            file_name,
                            t0.elapsed()
                        );
                        let sdr_result =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                hdr.generate_sdr_preview(DEFAULT_PREVIEW_SIZE, DEFAULT_PREVIEW_SIZE)
                            }));
                        match sdr_result {
                            Ok(Ok((pw, ph, p_pixels))) if pw > 0 && ph > 0 => {
                                log::info!(
                                    "[{}] Source SDR fallback preview generated ({}x{}) in {:?}",
                                    file_name,
                                    pw,
                                    ph,
                                    t0.elapsed()
                                );
                                let hdr_buf = sdr_preview_to_hdr_preview(
                                    pw,
                                    ph,
                                    &p_pixels,
                                    hdr.color_space(),
                                );
                                hdr_preview = Some(std::sync::Arc::new(hdr_buf));
                            }
                            _ => {
                                log::warn!(
                                    "[{}] Source SDR preview fallback failed, trying fallback.generate_preview",
                                    file_name
                                );
                                let gen_result =
                                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                        fallback.generate_preview(
                                            DEFAULT_PREVIEW_SIZE,
                                            DEFAULT_PREVIEW_SIZE,
                                        )
                                    }));
                                match gen_result {
                                    Ok((pw, ph, p_pixels)) if pw > 0 && ph > 0 => {
                                        log::info!(
                                            "[{}] Fallback SDR preview generated ({}x{}) in {:?}",
                                            file_name,
                                            pw,
                                            ph,
                                            t0.elapsed()
                                        );
                                        let hdr_buf = sdr_preview_to_hdr_preview(
                                            pw,
                                            ph,
                                            &p_pixels,
                                            hdr.color_space(),
                                        );
                                        hdr_preview = Some(std::sync::Arc::new(hdr_buf));
                                    }
                                    _ => {
                                        log::error!(
                                            "[{}] All preview paths exhausted (HDR + source SDR + fallback). No preview available.",
                                            file_name
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            } else {
                // SDR mode: generate SDR preview primarily, fallback to HDR if it fails
                let t0 = std::time::Instant::now();
                let sdr_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    hdr.generate_sdr_preview(DEFAULT_PREVIEW_SIZE, DEFAULT_PREVIEW_SIZE)
                }));
                match sdr_result {
                    Ok(Ok((pw, ph, p_pixels))) if pw > 0 && ph > 0 => {
                        log::info!(
                            "[{}] Source SDR {}px preview generated ({}x{}) in {:?}",
                            file_name,
                            DEFAULT_PREVIEW_SIZE,
                            pw,
                            ph,
                            t0.elapsed()
                        );
                        preview = Some(DecodedImage::new(pw, ph, p_pixels));
                    }
                    _ => {
                        log::warn!(
                            "[{}] Source SDR preview generation failed or zero-sized in {:?}, trying fallback.generate_preview",
                            file_name,
                            t0.elapsed()
                        );
                        let gen_result =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                fallback
                                    .generate_preview(DEFAULT_PREVIEW_SIZE, DEFAULT_PREVIEW_SIZE)
                            }));
                        match gen_result {
                            Ok((pw, ph, p_pixels)) if pw > 0 && ph > 0 => {
                                log::info!(
                                    "[{}] Fallback SDR preview generated ({}x{}) in {:?}",
                                    file_name,
                                    pw,
                                    ph,
                                    t0.elapsed()
                                );
                                preview = Some(DecodedImage::new(pw, ph, p_pixels));
                            }
                            _ => {
                                log::warn!(
                                    "[{}] Fallback generate_preview failed, trying emergency HDR preview fallback",
                                    file_name
                                );
                                let hdr_preview_result =
                                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                        hdr.generate_hdr_preview(
                                            DEFAULT_PREVIEW_SIZE,
                                            DEFAULT_PREVIEW_SIZE,
                                        )
                                    }));
                                match hdr_preview_result {
                                    Ok(Ok(image)) if image.width > 0 && image.height > 0 => {
                                        log::info!(
                                            "[{}] Emergency HDR fallback preview generated ({}x{}) in {:?}",
                                            file_name,
                                            image.width,
                                            image.height,
                                            t0.elapsed()
                                        );
                                        match crate::hdr::tiled::sdr_preview_from_hdr_preview(
                                            &image,
                                        ) {
                                            Ok((pw, ph, p_pixels)) => {
                                                preview = Some(DecodedImage::new(pw, ph, p_pixels));
                                            }
                                            Err(err) => {
                                                log::error!(
                                                    "[{}] Emergency HDR to SDR preview conversion failed: {}",
                                                    file_name,
                                                    err
                                                );
                                            }
                                        }
                                    }
                                    _ => {
                                        log::error!(
                                            "[{}] All preview paths exhausted (source SDR + fallback + HDR). No preview available.",
                                            file_name
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }

            Ok(ImageData::HdrTiled { hdr, fallback })
        }
        Ok(ImageData::Static(decoded)) => Ok(make_image_data(decoded)),
        Ok(ImageData::Hdr { hdr, fallback }) => Ok(make_hdr_image_data(hdr, fallback)),
        Ok(ImageData::Animated(frames)) => {
            if let Some(first) = frames.first() {
                let width = first.width;
                let height = first.height;
                let max_side = width.max(height);
                let limit = crate::tile_cache::get_max_texture_side();

                let total_bytes: usize = frames.iter().map(|f| f.rgba().len()).sum();
                let mb = total_bytes as f64 / (BYTES_PER_MB as f64);

                if max_side > limit {
                    log::warn!(
                        "[{}] Animated image ({}x{}) exceeds GPU limits. Falling back to tiled static mode.",
                        file_name,
                        width,
                        height
                    );
                    Ok(make_image_data(DecodedImage::from_arc(
                        width,
                        height,
                        first.arc_pixels(),
                    )))
                } else {
                    log::info!(
                        "[{}] Decoded {}x{} ({} frames, {:.1} MB) - Animated Mode",
                        file_name,
                        width,
                        height,
                        frames.len(),
                        mb
                    );
                    Ok(ImageData::Animated(frames))
                }
            } else {
                Ok(ImageData::Animated(frames))
            }
        }
        Ok(ImageData::HdrAnimated(frames)) => {
            if let Some(first) = frames.first() {
                let width = first.width();
                let height = first.height();
                let max_side = width.max(height);
                let limit = crate::tile_cache::get_max_texture_side();

                let total_bytes: usize = frames
                    .iter()
                    .map(|f| f.fallback.rgba().len() + f.hdr.rgba_f32.len() * 4)
                    .sum();
                let mb = total_bytes as f64 / (BYTES_PER_MB as f64);

                if max_side > limit {
                    log::warn!(
                        "[{}] HDR animated image ({}x{}) exceeds GPU limits. Using first-frame SDR fallback.",
                        file_name,
                        width,
                        height
                    );
                    Ok(make_image_data(DecodedImage::new(
                        width,
                        height,
                        first.fallback.rgba().to_vec(),
                    )))
                } else {
                    log::info!(
                        "[{}] Decoded {}x{} ({} HDR frames, {:.1} MB) - HDR Animated Mode",
                        file_name,
                        width,
                        height,
                        frames.len(),
                        mb
                    );
                    Ok(ImageData::HdrAnimated(frames))
                }
            } else {
                Ok(ImageData::HdrAnimated(frames))
            }
        }
        Err(e) => {
            log::error!("[{}] Failed to load: {}", file_name, e);
            Err(e)
        }
    };

    let preview_bundle =
        PreviewBundle::from_planes(PreviewStage::Initial, preview.clone(), hdr_preview.clone());

    let sdr_fallback_is_placeholder = matches!(
        &final_result,
        Ok(ImageData::Hdr { .. } | ImageData::HdrAnimated(_))
    ) && !hdr_display_requests_sdr_preview(hdr_target_capacity);

    LoadResult {
        index,
        generation,
        ultra_hdr_capacity_sensitive: is_hdr_capacity_sensitive_load(path, &final_result),
        result: final_result,
        preview_bundle,
        sdr_fallback_is_placeholder,
        target_hdr_capacity: hdr_target_capacity,
    }
}
fn is_hdr_capacity_sensitive_load(path: &Path, result: &Result<ImageData, String>) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default();
    let is_jpeg = ext == "jpg" || ext == "jpeg";
    let is_raw = crate::raw_processor::is_raw_extension(&ext);
    (is_jpeg
        || modern::is_hdr_capable_modern_format_path(path)
        || crate::hdr::decode::is_hdr_candidate_ext(&ext)
        || is_raw)
        && matches!(
            result,
            Ok(ImageData::Hdr { .. } | ImageData::HdrTiled { .. } | ImageData::HdrAnimated(_))
        )
}

fn sdr_preview_to_hdr_preview(
    w: u32,
    h: u32,
    rgba_u8: &[u8],
    color_space: crate::hdr::types::HdrColorSpace,
) -> crate::hdr::types::HdrImageBuffer {
    let mut rgba_f32 = Vec::with_capacity(rgba_u8.len());
    for chunk in rgba_u8.chunks_exact(4) {
        let r = (chunk[0] as f32 / 255.0).powf(2.2);
        let g = (chunk[1] as f32 / 255.0).powf(2.2);
        let b = (chunk[2] as f32 / 255.0).powf(2.2);
        let a = chunk[3] as f32 / 255.0;
        rgba_f32.extend_from_slice(&[r, g, b, a]);
    }
    crate::hdr::types::HdrImageBuffer {
        width: w,
        height: h,
        format: crate::hdr::types::HdrPixelFormat::Rgba32Float,
        color_space,
        metadata: crate::hdr::types::HdrImageMetadata {
            transfer_function: crate::hdr::types::HdrTransferFunction::Linear,
            reference: crate::hdr::types::HdrReference::SceneLinear,
            color_profile: crate::hdr::types::HdrColorProfile::LinearSrgb,
            ..Default::default()
        },
        rgba_f32: std::sync::Arc::new(rgba_f32),
    }
}

#[cfg(test)]
mod tests;
