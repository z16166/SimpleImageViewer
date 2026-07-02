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

mod animation_bootstrap;
mod assemble;
mod detect;
mod directory_tree_thumb;
mod gain_map_strip;
mod hdr_formats;
mod hdr_strip_fast;
mod jpeg;
mod modern;
mod raster;
mod raw;
pub(crate) use raw::open_raw_processor_with_preview;
mod strip_downsample;
mod tiff_raw_sniff;

pub(crate) use directory_tree_thumb::{
    DirectoryTreeThumbDecodeOptions, STRIP_DEFER_SLOW_EMBEDDED_SDR,
    generate_directory_tree_thumb_decode_from_path,
};
pub(crate) use raster::is_maybe_animated;
pub(crate) use strip_downsample::downsample_decoded_for_strip;
pub(crate) use tiff_raw_sniff::tiff_may_be_camera_raw;

use crate::constants::{BYTES_PER_MB, DEFAULT_PREVIEW_SIZE};
use crate::hdr::types::HdrToneMapSettings;
use crossbeam_channel::Sender;
use std::path::Path;

use super::{
    DecodedImage, ImageData, LoadResult, PreviewBundle, PreviewStage, RefinementRequest,
    source_key_for_path,
};
use super::{
    extract_exif_thumbnail, hdr_display_requests_sdr_preview,
    hdr_sdr_fallback_is_placeholder_for_load,
};

use animation_bootstrap::{
    load_gif_with_bootstrap, load_png_with_bootstrap, load_webp_with_bootstrap,
    spawn_raster_animation_remainder_decode,
};
use assemble::{make_hdr_image_data, make_image_data};
use detect::{load_primary_with_detection_fallback, recover_via_platform_and_content_detection};
use hdr_formats::load_hdr;
use jpeg::load_jpeg_with_target_capacity;
use modern::{
    load_avif_with_target_capacity_outcome, load_heif_hdr_aware,
    load_jxl_with_target_capacity_outcome, spawn_avif_sequence_remainder_decode,
    spawn_jxl_animation_remainder_decode,
};
use raster::{load_psd, load_static};
use raw::load_raw;

pub(crate) struct ImageLoadRequest<'a> {
    pub(crate) index: usize,
    pub(crate) path: &'a Path,
    pub(crate) tx: crate::loader::orchestrator::LoaderOutputSender,
    pub(crate) refine_tx: Sender<RefinementRequest>,
    pub(crate) decode_profile: crate::loader::DecodeProfile,
    pub(crate) high_quality: bool,
    pub(crate) raw_demosaic_mode: crate::settings::RawDemosaicMode,
    pub(crate) hdr_target_capacity: f32,
    pub(crate) hdr_tone_map: HdrToneMapSettings,
    pub(crate) raw_open_prefetch: Option<&'a crate::loader::orchestrator::RawOpenPrefetch>,
    pub(crate) prefer_embedded_sdr_master: bool,
}

pub(crate) fn load_image_file(request: ImageLoadRequest<'_>) -> LoadResult {
    let ImageLoadRequest {
        index,
        path,
        tx,
        refine_tx,
        decode_profile,
        high_quality,
        raw_demosaic_mode,
        hdr_target_capacity,
        hdr_tone_map,
        raw_open_prefetch,
        prefer_embedded_sdr_master,
    } = request;
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");

    let mut raw_osd_info: Option<crate::loader::RawOsdInfo> = None;

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
            let out = load_raw(raw::RawLoadRequest {
                index,
                path,
                refine_tx: refine_tx.clone(),
                load_tx: tx.clone(),
                decode_profile: decode_profile.clone(),
                high_quality,
                raw_demosaic_mode,
                hdr_target_capacity,
                hdr_tone_map,
                raw_open_prefetch,
            })?;
            if out.osd.sensor_size.0 > 0 {
                raw_osd_info = Some(out.osd);
            }
            return Ok(out.image);
        }

        if ext == "jpg" || ext == "jpeg" {
            return load_primary_with_detection_fallback(
                path,
                file_name,
                hdr_target_capacity,
                hdr_tone_map,
                high_quality,
                || {
                    load_jpeg_with_target_capacity(
                        path,
                        hdr_target_capacity,
                        hdr_tone_map,
                        prefer_embedded_sdr_master,
                    )
                },
            );
        }
        if ext == "tif" || ext == "tiff" {
            if tiff_raw_sniff::tiff_may_be_camera_raw(path)
                && crate::raw_processor::probe_libraw_can_open(path)
            {
                log::info!(
                    "[{}] TIFF IFD0 looks like camera RAW and LibRaw opened it; using RAW pipeline",
                    file_name
                );
                return load_raw(raw::RawLoadRequest {
                    index,
                    path,
                    refine_tx: refine_tx.clone(),
                    load_tx: tx.clone(),
                    decode_profile: decode_profile.clone(),
                    high_quality,
                    raw_demosaic_mode,
                    hdr_target_capacity,
                    hdr_tone_map,
                    raw_open_prefetch,
                })
                .map(|out| {
                    if out.osd.sensor_size.0 > 0 {
                        raw_osd_info = Some(out.osd);
                    }
                    out.image
                });
            }
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
                || {
                    let outcome = load_avif_with_target_capacity_outcome(
                        path,
                        hdr_target_capacity,
                        hdr_tone_map,
                        prefer_embedded_sdr_master,
                        true,
                    )?;
                    if let Some(job) = outcome.sequence_remainder {
                        spawn_avif_sequence_remainder_decode(
                            job,
                            tx.clone(),
                            index,
                            decode_profile.clone(),
                        );
                    }
                    Ok(outcome.image)
                },
            );
        }

        if ext == "jxl" {
            return load_primary_with_detection_fallback(
                path,
                file_name,
                hdr_target_capacity,
                hdr_tone_map,
                high_quality,
                || {
                    let outcome = load_jxl_with_target_capacity_outcome(
                        path,
                        hdr_target_capacity,
                        hdr_tone_map,
                        prefer_embedded_sdr_master,
                        true,
                    )?;
                    if let Some(job) = outcome.remainder_job {
                        spawn_jxl_animation_remainder_decode(
                            job,
                            tx.clone(),
                            index,
                            decode_profile.clone(),
                        );
                    }
                    Ok(outcome.image)
                },
            );
        }

        if ext == "heif" || ext == "heic" || ext == "hif" {
            return load_primary_with_detection_fallback(
                path,
                file_name,
                hdr_target_capacity,
                hdr_tone_map,
                high_quality,
                || {
                    load_heif_hdr_aware(
                        path,
                        hdr_target_capacity,
                        hdr_tone_map,
                        crate::hdr::heif::HeifHdrDecodeDiag {
                            idx: Some(index),
                            path: Some(path),
                        },
                        prefer_embedded_sdr_master,
                    )
                },
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

        if matches!(ext.as_str(), "gif" | "png" | "apng" | "webp") {
            return load_primary_with_detection_fallback(
                path,
                file_name,
                hdr_target_capacity,
                hdr_tone_map,
                high_quality,
                || {
                    let outcome = match ext.as_str() {
                        "gif" => {
                            load_gif_with_bootstrap(path, hdr_target_capacity, hdr_tone_map, true)
                        }
                        "png" | "apng" => {
                            load_png_with_bootstrap(path, hdr_target_capacity, hdr_tone_map, true)
                        }
                        "webp" => {
                            load_webp_with_bootstrap(path, hdr_target_capacity, hdr_tone_map, true)
                        }
                        _ => unreachable!("matched gif/png/apng/webp above"),
                    }?;
                    if let Some(job) = outcome.remainder {
                        spawn_raster_animation_remainder_decode(
                            job,
                            tx.clone(),
                            index,
                            decode_profile.clone(),
                        );
                    }
                    Ok(outcome.image)
                },
            );
        }

        let result = load_static(path, hdr_target_capacity, hdr_tone_map);
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
            let logical_w = source.width();
            let logical_h = source.height();
            let mut used_exif = false;
            match exif_thumb {
                Ok(Some(thumb))
                    if super::preview_aspect_matches_logical(
                        thumb.width,
                        thumb.height,
                        logical_w,
                        logical_h,
                    ) =>
                {
                    log::info!(
                        "[{}] EXIF thumbnail extracted in {:?}",
                        file_name,
                        t0.elapsed()
                    );
                    preview = Some(thumb);
                    used_exif = true;
                }
                Ok(Some(_)) => {
                    log::info!(
                        "[{}] Skipping EXIF thumbnail due to aspect mismatch ({}x{})",
                        file_name,
                        logical_w,
                        logical_h
                    );
                }
                Ok(None) => {}
                Err(e) => {
                    log::error!("[{}] extract_exif_thumbnail PANICKED: {:?}", file_name, e);
                }
            }
            if !used_exif {
                log::info!(
                    "[{}] Generating {}px preview...",
                    file_name,
                    DEFAULT_PREVIEW_SIZE
                );
                let t1 = std::time::Instant::now();
                let gen_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    source.generate_full_image_preview(DEFAULT_PREVIEW_SIZE, DEFAULT_PREVIEW_SIZE)
                }));
                match gen_result {
                    Ok((pw, ph, p_pixels)) if pw > 0 && ph > 0 => {
                        if super::preview_aspect_matches_logical(pw, ph, logical_w, logical_h) {
                            log::info!(
                                "[{}] {}px preview generated ({}x{}) in {:?}",
                                file_name,
                                DEFAULT_PREVIEW_SIZE,
                                pw,
                                ph,
                                t1.elapsed()
                            );
                            crate::preload_debug!(
                                "[PreloadDebug][Strip] tiled bootstrap preview file={} out={}x{} logical={}x{} aspect_ok=true",
                                file_name,
                                pw,
                                ph,
                                logical_w,
                                logical_h
                            );
                            preview = Some(DecodedImage::new(pw, ph, p_pixels));
                        } else {
                            log::warn!(
                                "[{}] Rejecting {}x{} preview: aspect ratio does not match logical {}x{}",
                                file_name,
                                pw,
                                ph,
                                logical_w,
                                logical_h
                            );
                            crate::preload_debug!(
                                "[PreloadDebug][Strip] tiled bootstrap preview file={} out={}x{} logical={}x{} aspect_ok=false",
                                file_name,
                                pw,
                                ph,
                                logical_w,
                                logical_h
                            );
                        }
                    }
                    Ok(_) => {
                        log::warn!(
                            "[{}] generate_full_image_preview returned empty/zero-size result in {:?}",
                            file_name,
                            t1.elapsed()
                        );
                    }
                    Err(e) => {
                        log::error!(
                            "[{}] generate_full_image_preview PANICKED: {:?} in {:?}",
                            file_name,
                            e,
                            t1.elapsed()
                        );
                    }
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
            let (tiled_preview, tiled_hdr_preview) =
                compute_hdr_tiled_initial_preview(file_name, &hdr, &fallback, hdr_target_capacity);
            preview = tiled_preview;
            hdr_preview = tiled_hdr_preview;

            Ok(ImageData::HdrTiled { hdr, fallback })
        }
        Ok(ImageData::Static(decoded)) => Ok(make_image_data(decoded)),
        Ok(ImageData::Hdr { hdr, fallback }) => Ok(make_hdr_image_data(*hdr, fallback)),
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

    let sdr_fallback_is_placeholder = match &final_result {
        Ok(ImageData::Hdr { hdr, .. }) => {
            hdr_sdr_fallback_is_placeholder_for_load(hdr, hdr_target_capacity)
        }
        Ok(ImageData::HdrAnimated(_)) => !hdr_display_requests_sdr_preview(hdr_target_capacity),
        _ => false,
    };

    LoadResult {
        index,
        decode_profile,
        source_key: source_key_for_path(path),
        ultra_hdr_capacity_sensitive: is_hdr_capacity_sensitive_load(path, &final_result),
        result: final_result,
        preview_bundle,
        sdr_fallback_is_placeholder,
        target_hdr_capacity: hdr_target_capacity,
        raw_osd: raw_osd_info,
        uploaded_planes: None,
        device_id: None,
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

fn fallback_sdr_preview_as_hdr(
    file_name: &str,
    fallback: &std::sync::Arc<dyn crate::loader::TiledImageSource>,
) -> Option<std::sync::Arc<crate::hdr::types::HdrImageBuffer>> {
    let gen_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        fallback.generate_preview(DEFAULT_PREVIEW_SIZE, DEFAULT_PREVIEW_SIZE)
    }));
    match gen_result {
        Ok((pw, ph, p_pixels)) if pw > 0 && ph > 0 => {
            match sdr_preview_to_hdr_preview(pw, ph, &p_pixels) {
                Ok(hdr_buf) => Some(std::sync::Arc::new(hdr_buf)),
                Err(conv_err) => {
                    log::error!(
                        "[{}] Fallback SDR->HDR preview conversion rejected malformed buffer: {}",
                        file_name,
                        conv_err
                    );
                    None
                }
            }
        }
        Ok(_) => {
            log::error!(
                "[{}] Fallback SDR preview returned zero-sized image",
                file_name
            );
            None
        }
        Err(panic) => {
            log::error!(
                "[{}] fallback.generate_preview PANICKED: {:?}",
                file_name,
                panic
            );
            None
        }
    }
}

fn compute_hdr_tiled_initial_preview(
    file_name: &str,
    hdr: &std::sync::Arc<dyn crate::hdr::tiled::HdrTiledSource>,
    fallback: &std::sync::Arc<dyn crate::loader::TiledImageSource>,
    hdr_target_capacity: f32,
) -> (
    Option<crate::loader::DecodedImage>,
    Option<std::sync::Arc<crate::hdr::types::HdrImageBuffer>>,
) {
    let mut preview = None;
    let mut hdr_preview = None;

    if !hdr_display_requests_sdr_preview(hdr_target_capacity) {
        let hdr_preview_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            hdr.generate_hdr_preview(DEFAULT_PREVIEW_SIZE, DEFAULT_PREVIEW_SIZE)
        }));
        match hdr_preview_result {
            Ok(Ok(image)) if image.width > 0 && image.height > 0 => {
                hdr_preview = Some(std::sync::Arc::new(image));
            }
            Ok(Ok(_)) => {
                log::warn!(
                    "[{}] HDR preview returned zero-sized image; trying source SDR preview fallback",
                    file_name
                );
                let sdr_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    hdr.generate_sdr_preview(DEFAULT_PREVIEW_SIZE, DEFAULT_PREVIEW_SIZE)
                }));
                match sdr_result {
                    Ok(Ok((pw, ph, p_pixels))) if pw > 0 && ph > 0 => {
                        match sdr_preview_to_hdr_preview(pw, ph, &p_pixels) {
                            Ok(hdr_buf) => hdr_preview = Some(std::sync::Arc::new(hdr_buf)),
                            Err(conv_err) => {
                                log::error!(
                                    "[{}] Source SDR->HDR preview conversion rejected malformed buffer: {}",
                                    file_name,
                                    conv_err
                                );
                            }
                        }
                    }
                    _ => {
                        hdr_preview = fallback_sdr_preview_as_hdr(file_name, fallback);
                    }
                }
            }
            Ok(Err(err)) => {
                log::warn!(
                    "[{}] HDR preview generation failed: {}; trying fallback.generate_preview",
                    file_name,
                    err
                );
                hdr_preview = fallback_sdr_preview_as_hdr(file_name, fallback);
            }
            Err(panic) => {
                log::error!(
                    "[{}] HDR preview generation PANICKED: {:?}; trying fallback.generate_preview",
                    file_name,
                    panic
                );
                hdr_preview = fallback_sdr_preview_as_hdr(file_name, fallback);
            }
        }
    } else {
        let sdr_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            hdr.generate_sdr_preview(DEFAULT_PREVIEW_SIZE, DEFAULT_PREVIEW_SIZE)
        }));
        match sdr_result {
            Ok(Ok((pw, ph, p_pixels))) if pw > 0 && ph > 0 => {
                preview = Some(crate::loader::DecodedImage::new(pw, ph, p_pixels));
            }
            _ => {
                let gen_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    fallback.generate_preview(DEFAULT_PREVIEW_SIZE, DEFAULT_PREVIEW_SIZE)
                }));
                match gen_result {
                    Ok((pw, ph, p_pixels)) if pw > 0 && ph > 0 => {
                        preview = Some(crate::loader::DecodedImage::new(pw, ph, p_pixels));
                    }
                    _ => {
                        log::warn!(
                            "[{}] SDR preview paths failed; trying emergency HDR preview fallback",
                            file_name
                        );
                        let hdr_preview_result =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                hdr.generate_hdr_preview(DEFAULT_PREVIEW_SIZE, DEFAULT_PREVIEW_SIZE)
                            }));
                        if let Ok(Ok(image)) = hdr_preview_result
                            && image.width > 0
                            && image.height > 0
                        {
                            match crate::hdr::tiled::sdr_preview_from_hdr_preview(&image) {
                                Ok((pw, ph, p_pixels)) => {
                                    preview =
                                        Some(crate::loader::DecodedImage::new(pw, ph, p_pixels));
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
                    }
                }
            }
        }
    }

    (preview, hdr_preview)
}

fn sdr_preview_to_hdr_preview(
    w: u32,
    h: u32,
    rgba_u8: &[u8],
) -> Result<crate::hdr::types::HdrImageBuffer, String> {
    if w == 0 || h == 0 {
        return Err("SDR preview dimensions must be non-zero".to_string());
    }
    let expected_len = (w as usize)
        .checked_mul(h as usize)
        .and_then(|px| px.checked_mul(4))
        .ok_or_else(|| format!("SDR preview dimensions overflow RGBA length: {}x{}", w, h))?;
    if rgba_u8.len() != expected_len {
        return Err(format!(
            "SDR preview RGBA length mismatch for {}x{}: got {}, expected {}",
            w,
            h,
            rgba_u8.len(),
            expected_len
        ));
    }

    let mut rgba_f32 = Vec::with_capacity(expected_len);
    for chunk in rgba_u8.chunks_exact(4) {
        let r = crate::hdr::decode::srgb_nonlinear_channel_to_linear(chunk[0] as f32 / 255.0);
        let g = crate::hdr::decode::srgb_nonlinear_channel_to_linear(chunk[1] as f32 / 255.0);
        let b = crate::hdr::decode::srgb_nonlinear_channel_to_linear(chunk[2] as f32 / 255.0);
        let a = chunk[3] as f32 / 255.0;
        rgba_f32.extend_from_slice(&[r, g, b, a]);
    }
    let color_space = crate::hdr::types::HdrColorSpace::LinearSrgb;
    Ok(crate::hdr::types::HdrImageBuffer {
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
    })
}

#[cfg(test)]
pub(super) fn compute_hdr_tiled_initial_preview_for_test(
    file_name: &str,
    image_data: &crate::loader::ImageData,
    hdr_target_capacity: f32,
) -> (
    Option<crate::loader::DecodedImage>,
    Option<std::sync::Arc<crate::hdr::types::HdrImageBuffer>>,
) {
    let crate::loader::ImageData::HdrTiled { hdr, fallback } = image_data else {
        panic!("compute_hdr_tiled_initial_preview_for_test requires ImageData::HdrTiled");
    };

    compute_hdr_tiled_initial_preview(file_name, hdr, fallback, hdr_target_capacity)
}

#[cfg(test)]
mod tests;
