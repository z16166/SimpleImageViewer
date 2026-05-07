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

use super::{extract_exif_thumbnail, hdr_display_requests_sdr_preview};
use super::{
    DecodedImage, ImageData, LoadResult, LoaderOutput, PreviewBundle, PreviewStage,
    RefinementRequest,
};

use assemble::{make_hdr_image_data, make_image_data};
use detect::load_via_content_detection;
use hdr_formats::load_hdr;
use jpeg::load_jpeg_with_target_capacity;
use modern::{load_avif_with_target_capacity, load_heif_hdr_aware, load_jxl_with_target_capacity};
use raster::{
    load_gif, load_png, load_psd, load_static, load_webp, is_maybe_animated,
};
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
            return load_hdr(path, hdr_target_capacity, hdr_tone_map);
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
            return load_jpeg_with_target_capacity(path, hdr_target_capacity, hdr_tone_map);
        }
        if ext == "tif" || ext == "tiff" {
            return crate::libtiff_loader::load_via_libtiff(
                path,
                hdr_target_capacity,
                hdr_tone_map,
            );
        }

        if ext == "avif" || ext == "avifs" {
            return load_avif_with_target_capacity(path, hdr_target_capacity, hdr_tone_map);
        }

        if ext == "jxl" {
            return load_jxl_with_target_capacity(path, hdr_target_capacity, hdr_tone_map);
        }

        if ext == "heif" || ext == "heic" || ext == "hif" {
            return load_heif_hdr_aware(path, hdr_target_capacity, hdr_tone_map);
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
        if result.is_err() {
            #[cfg(target_os = "windows")]
            if let Ok(img) = crate::wic::load_via_wic(path, high_quality, None) {
                return Ok(img);
            }
            #[cfg(target_os = "macos")]
            if let Ok(img) = crate::macos_image_io::load_via_image_io(path, high_quality, None) {
                return Ok(img);
            }

            // Last resort: Detect format by content (magic bytes)
            if let Ok(retry_img) =
                load_via_content_detection(path, hdr_target_capacity, hdr_tone_map)
            {
                log::info!(
                    "[{}] Successfully recovered via content-based detection",
                    file_name
                );
                return Ok(retry_img);
            }
        }

        result
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

            let t0 = std::time::Instant::now();
            let hdr_preview_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
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
                Ok(Err(err)) => {
                    log::warn!(
                        "[{}] HDR preview generation failed in {:?}: {}",
                        file_name,
                        t0.elapsed(),
                        err
                    );
                }
                Err(err) => {
                    log::error!(
                        "[{}] HDR preview generation PANICKED: {:?} in {:?}",
                        file_name,
                        err,
                        t0.elapsed()
                    );
                }
                _ => {}
            }

            if hdr_preview.is_none() {
                let t0 = std::time::Instant::now();
                let gen_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    fallback.generate_preview(DEFAULT_PREVIEW_SIZE, DEFAULT_PREVIEW_SIZE)
                }));
                match gen_result {
                    Ok((pw, ph, p_pixels)) if pw > 0 && ph > 0 => {
                        log::info!(
                            "[{}] HDR fallback {}px preview generated ({}x{}) in {:?}",
                            file_name,
                            DEFAULT_PREVIEW_SIZE,
                            pw,
                            ph,
                            t0.elapsed()
                        );
                        preview = Some(DecodedImage::new(pw, ph, p_pixels));
                    }
                    Ok(_) => {
                        log::warn!(
                            "[{}] HDR fallback generate_preview returned empty/zero-size result in {:?}",
                            file_name,
                            t0.elapsed()
                        );
                    }
                    Err(e) => {
                        log::error!(
                            "[{}] HDR fallback generate_preview PANICKED: {:?} in {:?}",
                            file_name,
                            e,
                            t0.elapsed()
                        );
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
        Err(e) => {
            log::error!("[{}] Failed to load: {}", file_name, e);
            Err(e)
        }
    };

    let preview_bundle =
        PreviewBundle::from_planes(PreviewStage::Initial, preview.clone(), hdr_preview.clone());

    let sdr_fallback_is_placeholder = matches!(&final_result, Ok(ImageData::Hdr { .. }))
        && !hdr_display_requests_sdr_preview(hdr_target_capacity);

    LoadResult {
        index,
        generation,
        ultra_hdr_capacity_sensitive: is_hdr_capacity_sensitive_load(path, &final_result),
        result: final_result,
        preview_bundle,
        sdr_fallback_is_placeholder,
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
            Ok(ImageData::Hdr { .. } | ImageData::HdrTiled { .. })
        )
}


#[cfg(test)]
mod tests;

