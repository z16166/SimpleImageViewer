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

//! Decode pipeline entry points (load_image_file), format loaders, tiled sources (RawImageSource), and unit tests.

use crate::constants::{
    BYTES_PER_GB, BYTES_PER_MB, DEFAULT_ANIMATION_DELAY_MS, DEFAULT_PREVIEW_SIZE,
    MIN_ANIMATION_DELAY_THRESHOLD_MS, RGBA_CHANNELS,
};
use crate::hdr::tiled::HdrTiledSource;
use crate::hdr::types::HdrToneMapSettings;
use crate::raw_processor::RawProcessor;
use crossbeam_channel::Sender;
use image::{DynamicImage, GenericImageView};
use parking_lot::RwLock as PLRwLock;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use super::{
    apply_exif_orientation_to_hdr_pair, apply_exif_orientation_to_image_data,
    extract_exif_thumbnail, hdr_display_requests_sdr_preview, hdr_gain_map_decode_capacity,
    hdr_sdr_fallback_rgba8_eager_or_placeholder, hdr_to_sdr_with_user_tone, hq_preview_max_side,
};
use super::{
    AnimationFrame, DecodedImage, ImageData, LoadResult, LoaderOutput, PreviewBundle, PreviewStage,
    RefinementRequest, TiledImageSource,
};

#[cfg(test)]
use super::{PixelPlaneKind, PreviewResult, RenderShape, TileDecodeSource, TilePixelKind, TileResult};

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
            "jpg" | "jpeg" => load_jpeg_with_target_capacity(path, hdr_target_capacity, hdr_tone_map),
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
        || is_hdr_capable_modern_format_path(path)
        || crate::hdr::decode::is_hdr_candidate_ext(&ext)
        || is_raw)
        && matches!(
            result,
            Ok(ImageData::Hdr { .. } | ImageData::HdrTiled { .. })
        )
}

#[cfg(test)]
fn load_jpeg(path: &PathBuf) -> Result<ImageData, String> {
    load_jpeg_with_target_capacity(
        path,
        HdrToneMapSettings::default().target_hdr_capacity(),
        HdrToneMapSettings::default(),
    )
}

fn load_jpeg_with_target_capacity(
    path: &PathBuf,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
) -> Result<ImageData, String> {
    let decode_capacity = hdr_gain_map_decode_capacity(hdr_target_capacity, &hdr_tone_map);
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mmap = unsafe { memmap2::Mmap::map(&file).map_err(|e| e.to_string())? };
    // Sole orientation pass for all JPEG decodes (baseline SDR, **JPEG_R / Ultra HDR**). Do not
    // combine with [`apply_exif_orientation_to_image_data`] — that would double-rotate.
    let orientation = crate::metadata_utils::get_exif_orientation(path);
    // Apply EXIF Orientation per TIFF/EXIF rules (same transform family as Pillow `exif_transpose`).
    // Some reference JPEGs (e.g. libavif `paris_exif_orientation_5.jpg`) store a raster that already
    // looks like a normal landscape before correction; the tag still requests transpose, so the
    // result can differ from viewers that ignore the tag or use heuristics.
    if let Ok(hdr) = crate::hdr::ultra_hdr::decode_ultra_hdr_jpeg_bytes_with_target_capacity(
        &mmap,
        decode_capacity,
    ) {
        let pixel_count = hdr.width as u64 * hdr.height as u64;
        let tiled_limit =
            crate::tile_cache::TILED_THRESHOLD.load(std::sync::atomic::Ordering::Relaxed);
        let max_side = hdr.width.max(hdr.height);
        if pixel_count >= tiled_limit || max_side > crate::constants::ABSOLUTE_MAX_TEXTURE_SIDE {
            let (mut w, mut h, mut pixels) = libjpeg_turbo::decode_to_rgba(&mmap)?;
            if orientation > 1 {
                let oriented =
                    crate::libtiff_loader::apply_orientation_buffer(pixels, w, h, orientation);
                w = oriented.0;
                h = oriented.1;
                pixels = oriented.2;
            }
            if let Ok(hdr_source) =
                crate::hdr::ultra_hdr::UltraHdrTiledImageSource::open_with_target_capacity(
                    path.clone(),
                    orientation,
                    decode_capacity,
                )
            {
                let fallback = Arc::new(MemoryImageSource::new_with_hdr_sdr_fallback(
                    w,
                    h,
                    Arc::new(pixels),
                    true,
                ));
                return Ok(ImageData::HdrTiled {
                    hdr: Arc::new(hdr_source),
                    fallback,
                });
            }
        }

        let hdr = crate::hdr::ultra_hdr::apply_orientation_to_hdr_buffer(hdr, orientation);
        let fallback_pixels = hdr_sdr_fallback_rgba8_eager_or_placeholder(
            &hdr,
            hdr_target_capacity,
            &hdr_tone_map,
        )?;
        let fallback = DecodedImage::new(hdr.width, hdr.height, fallback_pixels);
        return Ok(make_hdr_image_data(hdr, fallback));
    }

    let (mut w, mut h, mut pixels) = libjpeg_turbo::decode_to_rgba(&mmap)?;

    if orientation > 1 {
        let (out_w, out_h, out_pixels) =
            crate::libtiff_loader::apply_orientation_buffer(pixels, w, h, orientation);
        w = out_w;
        h = out_h;
        pixels = out_pixels;
    }

    Ok(make_image_data(DecodedImage::new(w, h, pixels)))
}

// Centralized in metadata_utils.rs

fn load_static(
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

    Ok(make_image_data(DecodedImage::new(width, height, pixels)))
}

#[allow(dead_code)]
fn is_avif_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("avif") || ext.eq_ignore_ascii_case("avifs"))
}

#[allow(dead_code)]
fn is_heif_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            ext.eq_ignore_ascii_case("heic")
                || ext.eq_ignore_ascii_case("heif")
                || ext.eq_ignore_ascii_case("hif")
        })
}

#[allow(dead_code)]
fn is_jxl_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("jxl"))
}

fn is_hdr_capable_modern_format_path(path: &Path) -> bool {
    is_avif_path(path) || is_heif_path(path) || is_jxl_path(path)
}

fn load_avif_with_target_capacity(
    path: &PathBuf,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
) -> Result<ImageData, String> {
    #[cfg(feature = "avif-native")]
    {
        let mmap = crate::mmap_util::map_file(path)
            .map_err(|e| format!("Failed to read AVIF: {e}"))?;

        match crate::hdr::avif::try_decode_avif_image_sequence_sdr(&mmap[..]) {
            Ok(Some(raw)) if raw.len() > 1 => {
                let frames: Vec<AnimationFrame> = raw
                    .into_iter()
                    .map(|(delay, w, h, px)| AnimationFrame::new(w, h, px, delay))
                    .collect();
                log::info!(
                    "[Loader] AVIF image sequence: {} frames (SDR RGBA8) — {}",
                    frames.len(),
                    path.display()
                );
                return Ok(apply_exif_orientation_to_image_data(
                    path.as_path(),
                    ImageData::Animated(frames),
                ));
            }
            Ok(_) => {}
            Err(e) => {
                log::debug!(
                    "[Loader] AVIF sequence decode failed for {} ({e}); trying static HDR path",
                    path.display()
                );
            }
        }

        let decode_capacity = hdr_gain_map_decode_capacity(hdr_target_capacity, &hdr_tone_map);
        match crate::hdr::avif::decode_avif_hdr_bytes_with_target_capacity(&mmap[..], decode_capacity) {
            Ok(hdr) => {
                let fallback_pixels = hdr_sdr_fallback_rgba8_eager_or_placeholder(
                    &hdr,
                    hdr_target_capacity,
                    &hdr_tone_map,
                )?;
                let fallback = DecodedImage::new(hdr.width, hdr.height, fallback_pixels);
                let (hdr, fallback) =
                    apply_exif_orientation_to_hdr_pair(path.as_path(), hdr, fallback);
                Ok(make_hdr_image_data(hdr, fallback))
            }
            Err(err) => {
                log::warn!(
                    "[Loader] libavif decode failed for {}: {err}",
                    path.display()
                );
                #[cfg(all(feature = "avif-native", feature = "heif-native"))]
                {
                    let lower = err.to_ascii_lowercase();
                    if lower.contains("invalid ftyp")
                        || lower.contains("ftyp")
                        || lower.contains("file type box")
                    {
                        log::info!(
                            "[Loader] libavif rejected container/brands — trying libheif for {}",
                            path.display()
                        );
                        return load_heif_hdr_aware(path, hdr_target_capacity, hdr_tone_map)
                            .map_err(|heif_err| {
                                format!(
                                    "[Loader] libavif failed ({err}); HEIF fallback also failed ({heif_err})"
                                )
                            });
                    }
                }
                Err(err)
            }
        }
    }

    #[cfg(not(feature = "avif-native"))]
    {
        let _ = (path, hdr_target_capacity, hdr_tone_map);
        Err(
            "AVIF decoding requires the avif-native feature (e.g. hdr-modern-formats)."
                .to_string(),
        )
    }
}

fn load_jxl_with_target_capacity(
    path: &PathBuf,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
) -> Result<ImageData, String> {
    #[cfg(feature = "jpegxl")]
    {
        let decode_capacity = hdr_gain_map_decode_capacity(hdr_target_capacity, &hdr_tone_map);
        let data = crate::hdr::jpegxl::load_jxl_hdr_with_target_capacity(
            path,
            decode_capacity,
            hdr_target_capacity,
            hdr_tone_map,
        )?;
        Ok(apply_exif_orientation_to_image_data(path.as_path(), data))
    }

    #[cfg(not(feature = "jpegxl"))]
    {
        let _ = (path, hdr_target_capacity, hdr_tone_map);
        Err("JPEG XL support requires the jpegxl feature".to_string())
    }
}

fn load_heif_hdr_aware(
    path: &PathBuf,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
) -> Result<ImageData, String> {
    #[cfg(feature = "heif-native")]
    {
        match crate::hdr::heif::load_heif_hdr(path, hdr_target_capacity, hdr_tone_map) {
            Ok(image) => Ok(apply_exif_orientation_to_image_data(path.as_path(), image)),
            Err(err) => {
                log::warn!(
                    "[Loader] libheif decode failed for {}: {err}",
                    path.display()
                );
                Err(err)
            }
        }
    }

    #[cfg(not(feature = "heif-native"))]
    {
        let _ = (path, hdr_target_capacity, hdr_tone_map);
        Err(
            "HEIF/HEIC decoding requires the heif-native feature (e.g. hdr-modern-formats)."
                .to_string(),
        )
    }
}

fn load_hdr(
    path: &Path,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
) -> Result<ImageData, String> {
    if is_exr_path(path) {
        return load_detected_exr(path, hdr_target_capacity, hdr_tone_map);
    } else if let Some(image_data) = try_load_disk_backed_radiance_hdr(path, hdr_tone_map)? {
        return Ok(image_data);
    }

    let hdr = match crate::hdr::decode::decode_hdr_image(path) {
        Ok(hdr) => hdr,
        Err(err) if is_exr_deep_data_unsupported_error(&err) => {
            log::warn!(
                "[Loader] Deep EXR data needs custom compositing for {}; using deep decoder",
                path.display()
            );
            return load_deep_exr(path, hdr_target_capacity, hdr_tone_map);
        }
        Err(err) => return Err(err),
    };
    let pixels = hdr_sdr_fallback_rgba8_eager_or_placeholder(
        &hdr,
        hdr_target_capacity,
        &hdr_tone_map,
    )?;
    let fallback = DecodedImage::new(hdr.width, hdr.height, pixels);
    let (hdr, fallback) = apply_exif_orientation_to_hdr_pair(path, hdr, fallback);
    Ok(make_hdr_image_data(hdr, fallback))
}

fn is_exr_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("exr"))
}

fn try_load_disk_backed_exr_hdr(
    path: &Path,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
) -> Result<Option<ImageData>, String> {
    let source = match crate::hdr::exr_tiled::ExrTiledImageSource::open(path) {
        Ok(source) => source,
        Err(err) if is_exr_disk_backed_probe_fallback_error(&err) => {
            log::warn!(
                "[Loader] Disk-backed EXR tiled source unavailable for {}: {err}; falling back to full HDR decode",
                path.display()
            );
            return Ok(None);
        }
        Err(err) => return Err(err),
    };
    let pixel_count = source.width() as u64 * source.height() as u64;
    let tiled_limit = crate::tile_cache::TILED_THRESHOLD.load(std::sync::atomic::Ordering::Relaxed);
    let max_side = source.width().max(source.height());
    if pixel_count < tiled_limit && max_side <= crate::constants::ABSOLUTE_MAX_TEXTURE_SIDE {
        if source.has_subsampled_channels() {
            let hdr: Arc<dyn crate::hdr::tiled::HdrTiledSource> = Arc::new(source);
            let fallback: Arc<dyn TiledImageSource> =
                Arc::new(HdrSdrTiledFallbackSource::new(Arc::clone(&hdr), hdr_tone_map));
            log::info!(
                "[Loader] subsampled EXR {}x{} kept as disk-backed HDR tiles.",
                hdr.width(),
                hdr.height()
            );
            return Ok(Some(ImageData::HdrTiled { hdr, fallback }));
        }
        if source.requires_disk_backed_decode() {
            return exr_tiled_source_to_static_hdr(path, source, hdr_target_capacity, hdr_tone_map).map(Some);
        }
        return Ok(None);
    }

    let hdr: Arc<dyn crate::hdr::tiled::HdrTiledSource> = Arc::new(source);
    let fallback: Arc<dyn TiledImageSource> =
        Arc::new(HdrSdrTiledFallbackSource::new(Arc::clone(&hdr), hdr_tone_map));
    log::info!(
        "[Loader] EXR {}x{} routed to disk-backed HDR tiles.",
        hdr.width(),
        hdr.height()
    );
    Ok(Some(ImageData::HdrTiled { hdr, fallback }))
}

fn exr_tiled_source_to_static_hdr(
    path: &Path,
    source: crate::hdr::exr_tiled::ExrTiledImageSource,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
) -> Result<ImageData, String> {
    let tile = source.extract_tile_rgba32f_arc(0, 0, source.width(), source.height())?;
    let hdr = crate::hdr::types::HdrImageBuffer {
        width: tile.width,
        height: tile.height,
        format: crate::hdr::types::HdrPixelFormat::Rgba32Float,
        color_space: tile.color_space,
        metadata: tile.metadata.clone(),
        rgba_f32: Arc::clone(&tile.rgba_f32),
    };
    let pixels = hdr_sdr_fallback_rgba8_eager_or_placeholder(
        &hdr,
        hdr_target_capacity,
        &hdr_tone_map,
    )?;
    let fallback = DecodedImage::new(hdr.width, hdr.height, pixels);
    log::info!(
        "[Loader] EXR {}x{} routed to static HDR via disk-backed decoder: {}",
        hdr.width,
        hdr.height,
        path.display()
    );
    let (hdr, fallback) = apply_exif_orientation_to_hdr_pair(path, hdr, fallback);
    Ok(make_hdr_image_data(hdr, fallback))
}

fn try_load_disk_backed_radiance_hdr(path: &Path, hdr_tone_map: HdrToneMapSettings) -> Result<Option<ImageData>, String> {
    let source = crate::hdr::radiance_tiled::RadianceHdrTiledImageSource::open(path)?;
    let pixel_count = source.width() as u64 * source.height() as u64;
    let tiled_limit = crate::tile_cache::TILED_THRESHOLD.load(std::sync::atomic::Ordering::Relaxed);
    let max_side = source.width().max(source.height());
    if pixel_count < tiled_limit && max_side <= crate::constants::ABSOLUTE_MAX_TEXTURE_SIDE {
        return Ok(None);
    }

    let hdr: Arc<dyn crate::hdr::tiled::HdrTiledSource> = Arc::new(source);
    let fallback: Arc<dyn TiledImageSource> =
        Arc::new(HdrSdrTiledFallbackSource::new(Arc::clone(&hdr), hdr_tone_map));
    log::info!(
        "[Loader] Radiance HDR {}x{} routed to disk-backed HDR tiles.",
        hdr.width(),
        hdr.height()
    );
    Ok(Some(ImageData::HdrTiled { hdr, fallback }))
}

fn is_exr_disk_backed_probe_fallback_error(err: &str) -> bool {
    err.contains("channel subsampling not supported yet")
        || err.contains("EXR layer does not contain required")
        || err.contains("deep data not supported yet")
}

fn is_exr_deep_data_unsupported_error(err: &str) -> bool {
    err.contains("deep data not supported yet")
}

fn load_deep_exr(
    path: &Path,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
) -> Result<ImageData, String> {
    match crate::hdr::exr_tiled::decode_deep_exr_image(path) {
        Ok(hdr) => {
            let pixels = hdr_sdr_fallback_rgba8_eager_or_placeholder(
                &hdr,
                hdr_target_capacity,
                &hdr_tone_map,
            )?;
            let fallback = DecodedImage::new(hdr.width, hdr.height, pixels);
            let (hdr, fallback) = apply_exif_orientation_to_hdr_pair(path, hdr, fallback);
            Ok(make_hdr_image_data(hdr, fallback))
        }
        Err(err) => {
            log::warn!(
                "[Loader] Deep EXR compositing failed for {}: {err}; using visible placeholder",
                path.display()
            );
            make_deep_exr_placeholder(path)
        }
    }
}

fn make_deep_exr_placeholder(path: &Path) -> Result<ImageData, String> {
    let (width, height) = crate::hdr::exr_tiled::exr_dimensions_unvalidated(path)?;
    let pixel_count = width
        .checked_mul(height)
        .ok_or_else(|| format!("Deep EXR placeholder dimensions overflow: {width}x{height}"))?;
    let mut rgba_f32 = vec![0.0_f32; pixel_count as usize * 4];
    for alpha in rgba_f32.chunks_exact_mut(4).map(|pixel| &mut pixel[3]) {
        *alpha = 1.0;
    }
    let mut fallback_pixels = vec![0_u8; pixel_count as usize * 4];
    for alpha in fallback_pixels
        .chunks_exact_mut(4)
        .map(|pixel| &mut pixel[3])
    {
        *alpha = 255;
    }
    let hdr = crate::hdr::types::HdrImageBuffer {
        width,
        height,
        format: crate::hdr::types::HdrPixelFormat::Rgba32Float,
        color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
        metadata: crate::hdr::types::HdrImageMetadata::from_color_space(
            crate::hdr::types::HdrColorSpace::LinearSrgb,
        ),
        rgba_f32: Arc::new(rgba_f32),
    };
    let fallback = DecodedImage::new(width, height, fallback_pixels);
    Ok(make_hdr_image_data(hdr, fallback))
}

fn process_animation_frames(
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

fn load_gif(path: &PathBuf, hdr_target_capacity: f32, hdr_tone_map: HdrToneMapSettings) -> Result<ImageData, String> {
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

fn load_png(path: &PathBuf, hdr_target_capacity: f32, hdr_tone_map: HdrToneMapSettings) -> Result<ImageData, String> {
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

fn load_webp(path: &PathBuf, hdr_target_capacity: f32, hdr_tone_map: HdrToneMapSettings) -> Result<ImageData, String> {
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

fn load_psd(path: &PathBuf) -> Result<ImageData, String> {
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
                Ok(make_image_data(img))
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
fn is_maybe_animated(ext: &str) -> bool {
    matches!(ext, "gif" | "webp" | "apng" | "png" | "jxl")
}

/// Helper to create ImageData that respects GPU texture limits.
/// If the image is too large for a single GPU texture, it is returned as ImageData::Tiled
/// using a MemoryImageSource to avoid hardware panics while preserving full resolution.
fn make_image_data(img: DecodedImage) -> ImageData {
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

fn make_hdr_image_data(
    hdr: crate::hdr::types::HdrImageBuffer,
    fallback: DecodedImage,
) -> ImageData {
    make_hdr_image_data_for_limit(hdr, fallback, crate::constants::ABSOLUTE_MAX_TEXTURE_SIDE)
}

fn make_hdr_image_data_for_limit(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::ImageLoader;
    use crate::loader::orchestrator::TileInFlightKey;
    use crate::hdr::types::{
        HdrColorSpace, HdrImageBuffer, HdrImageMetadata, HdrPixelFormat, HdrToneMapSettings,
    };
    use std::path::{Path, PathBuf};
    use std::sync::{LazyLock, Mutex, MutexGuard};

    static TILED_THRESHOLD_TEST_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    struct TiledThresholdOverride {
        old_threshold: u64,
    }

    impl TiledThresholdOverride {
        fn set(value: u64) -> Self {
            let old_threshold =
                crate::tile_cache::TILED_THRESHOLD.load(std::sync::atomic::Ordering::Relaxed);
            crate::tile_cache::TILED_THRESHOLD.store(value, std::sync::atomic::Ordering::Relaxed);
            Self { old_threshold }
        }
    }

    impl Drop for TiledThresholdOverride {
        fn drop(&mut self) {
            crate::tile_cache::TILED_THRESHOLD
                .store(self.old_threshold, std::sync::atomic::Ordering::Relaxed);
        }
    }

    fn lock_tiled_threshold_for_test() -> MutexGuard<'static, ()> {
        TILED_THRESHOLD_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// `tests/data/paris_exif_orientation_5.jpg` from libavif: stored SOF 403×302, EXIF Orientation 5.
    /// Correct viewing swaps to 302×403 (same as Pillow `ImageOps.exif_transpose`).
    #[test]
    fn paris_exif_orientation_5_jpeg_loads_transposed_dimensions() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/data/paris_exif_orientation_5.jpg");
        if !path.is_file() {
            eprintln!("skip: tests/data/paris_exif_orientation_5.jpg missing");
            return;
        }
        assert_eq!(crate::metadata_utils::get_exif_orientation(&path), 5);
        let image_data = load_jpeg_with_target_capacity(
            &path,
            HdrToneMapSettings::default().target_hdr_capacity(),
            HdrToneMapSettings::default(),
        )
        .expect("load paris EXIF orientation 5 JPEG");
        let ImageData::Static(decoded) = image_data else {
            panic!("expected static image data for paris_exif_orientation_5.jpg");
        };
        assert_eq!(
            (decoded.width, decoded.height),
            (302, 403),
            "EXIF 5 should transpose 403×302 stored raster to 302×403 display"
        );
    }

    #[test]
    fn supported_hdr_image_data_keeps_float_buffer_with_sdr_fallback() {
        let _threshold_lock = lock_tiled_threshold_for_test();
        let _threshold_override = TiledThresholdOverride::set(u64::MAX);
        let hdr = HdrImageBuffer {
            width: 2,
            height: 1,
            format: HdrPixelFormat::Rgba32Float,
            color_space: HdrColorSpace::LinearSrgb,
            metadata: HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
            rgba_f32: Arc::new(vec![1.0; 2 * 4]),
        };
        let fallback = DecodedImage::new(2, 1, vec![255; 2 * 4]);

        let image_data = make_hdr_image_data_for_limit(hdr.clone(), fallback, 4096);

        match image_data {
            ImageData::Hdr {
                hdr: kept,
                fallback,
            } => {
                assert_eq!(kept.width, hdr.width);
                assert_eq!(kept.height, hdr.height);
                assert!(Arc::ptr_eq(&kept.rgba_f32, &hdr.rgba_f32));
                assert_eq!(fallback.width, hdr.width);
                assert_eq!(fallback.height, hdr.height);
            }
            _ => panic!("expected HDR image data"),
        }
    }

    #[test]
    fn oversized_hdr_uses_existing_sdr_fallback_routing() {
        let hdr = HdrImageBuffer {
            width: 4097,
            height: 1,
            format: HdrPixelFormat::Rgba32Float,
            color_space: HdrColorSpace::LinearSrgb,
            metadata: HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
            rgba_f32: Arc::new(vec![1.0; 4097 * 4]),
        };
        let fallback = DecodedImage::new(4097, 1, vec![255; 4097 * 4]);

        let image_data = make_hdr_image_data_for_limit(hdr, fallback, 4096);

        assert!(matches!(image_data, ImageData::HdrTiled { .. }));
    }

    #[test]
    fn tile_inflight_keys_distinguish_sdr_and_hdr_outputs() {
        let sdr = TileInFlightKey::new(7, 11, 3, 4, TilePixelKind::Sdr);
        let hdr = TileInFlightKey::new(7, 11, 3, 4, TilePixelKind::Hdr);

        assert_ne!(sdr, hdr);
    }

    #[test]
    fn tile_inflight_keys_distinguish_generations() {
        let older = TileInFlightKey::new(7, 11, 3, 4, TilePixelKind::Hdr);
        let newer = TileInFlightKey::new(7, 12, 3, 4, TilePixelKind::Hdr);

        assert_ne!(older, newer);
    }

    #[test]
    fn tile_decode_source_reports_output_kind() {
        let sdr_source: Arc<dyn TiledImageSource> =
            Arc::new(MemoryImageSource::new(1, 1, Arc::new(vec![0, 0, 0, 255])));
        let hdr_source: Arc<dyn crate::hdr::tiled::HdrTiledSource> = Arc::new(
            crate::hdr::tiled::HdrTiledImageSource::new(HdrImageBuffer {
                width: 1,
                height: 1,
                format: HdrPixelFormat::Rgba32Float,
                color_space: HdrColorSpace::LinearSrgb,
                metadata: HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
                rgba_f32: Arc::new(vec![0.0, 0.0, 0.0, 1.0]),
            })
            .expect("build HDR tiled source"),
        );

        assert_eq!(
            TileDecodeSource::Sdr(sdr_source).pixel_kind(),
            TilePixelKind::Sdr
        );
        assert_eq!(
            TileDecodeSource::Hdr(hdr_source).pixel_kind(),
            TilePixelKind::Hdr
        );
    }

    #[test]
    fn load_result_exposes_unified_preview_bundle_without_compat_fields() {
        let sdr_preview = DecodedImage::new(2, 1, vec![0, 0, 0, 255, 255, 255, 255, 255]);
        let hdr_preview = Arc::new(HdrImageBuffer {
            width: 1,
            height: 1,
            format: HdrPixelFormat::Rgba32Float,
            color_space: HdrColorSpace::LinearSrgb,
            metadata: HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
            rgba_f32: Arc::new(vec![0.0, 0.0, 0.0, 1.0]),
        });
        let bundle = PreviewBundle::initial()
            .with_sdr(sdr_preview.clone())
            .with_hdr(Arc::clone(&hdr_preview));

        let result = LoadResult {
            index: 1,
            generation: 2,
            result: Ok(ImageData::Static(sdr_preview.clone())),
            preview_bundle: bundle,
            ultra_hdr_capacity_sensitive: false,
            sdr_fallback_is_placeholder: false,
        };

        assert_eq!(result.preview_bundle.stage(), PreviewStage::Initial);
        assert_eq!(result.preview_bundle.sdr().expect("sdr preview").width, 2);
        assert_eq!(result.preview_bundle.hdr().expect("hdr preview").width, 1);
        let sdr_plane = result
            .preview_bundle
            .plane(PixelPlaneKind::Sdr)
            .expect("sdr plane");
        let hdr_plane = result
            .preview_bundle
            .plane(PixelPlaneKind::Hdr)
            .expect("hdr plane");
        assert_eq!(sdr_plane.kind(), PixelPlaneKind::Sdr);
        assert_eq!(sdr_plane.dimensions(), (2, 1));
        assert_eq!(hdr_plane.kind(), PixelPlaneKind::Hdr);
        assert_eq!(hdr_plane.dimensions(), (1, 1));
        assert_eq!(PreviewBundle::refined().stage(), PreviewStage::Refined);
    }

    #[test]
    fn image_data_exposes_render_shape_and_available_planes() {
        let sdr = DecodedImage::new(1, 1, vec![0, 0, 0, 255]);
        let hdr = HdrImageBuffer {
            width: 1,
            height: 1,
            format: HdrPixelFormat::Rgba32Float,
            color_space: HdrColorSpace::LinearSrgb,
            metadata: HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
            rgba_f32: Arc::new(vec![0.0, 0.0, 0.0, 1.0]),
        };
        let static_sdr = ImageData::Static(sdr.clone());
        let static_hdr = ImageData::Hdr {
            hdr: hdr.clone(),
            fallback: sdr.clone(),
        };
        let tiled_sdr_source: Arc<dyn TiledImageSource> =
            Arc::new(MemoryImageSource::new(1, 1, Arc::new(vec![0, 0, 0, 255])));
        let tiled_hdr_source: Arc<dyn crate::hdr::tiled::HdrTiledSource> = Arc::new(
            crate::hdr::tiled::HdrTiledImageSource::new(hdr).expect("build HDR tiled source"),
        );
        let tiled_hdr = ImageData::HdrTiled {
            hdr: Arc::clone(&tiled_hdr_source),
            fallback: Arc::clone(&tiled_sdr_source),
        };

        assert_eq!(static_sdr.preferred_render_shape(), RenderShape::Static);
        assert!(static_sdr.has_plane(PixelPlaneKind::Sdr));
        assert!(!static_sdr.has_plane(PixelPlaneKind::Hdr));
        assert!(static_sdr.static_sdr().is_some());

        assert_eq!(static_hdr.preferred_render_shape(), RenderShape::Static);
        assert!(static_hdr.has_plane(PixelPlaneKind::Sdr));
        assert!(static_hdr.has_plane(PixelPlaneKind::Hdr));
        assert!(static_hdr.static_hdr().is_some());

        assert_eq!(tiled_hdr.preferred_render_shape(), RenderShape::Tiled);
        assert!(tiled_hdr.has_plane(PixelPlaneKind::Sdr));
        assert!(tiled_hdr.has_plane(PixelPlaneKind::Hdr));
        assert!(tiled_hdr.tiled_sdr_source().is_some());
        assert!(tiled_hdr.tiled_hdr_source().is_some());
    }

    #[test]
    fn preview_result_exposes_refined_sdr_preview_bundle() {
        let preview = DecodedImage::new(2, 1, vec![0, 0, 0, 255, 255, 255, 255, 255]);
        let update = PreviewResult::from_sdr_preview(3, 5, Ok(preview.clone()));

        assert!(update.error.is_none());
        assert_eq!(update.preview_bundle.stage(), PreviewStage::Refined);
        assert_eq!(
            update
                .preview_bundle
                .plane(PixelPlaneKind::Sdr)
                .expect("sdr preview plane")
                .dimensions(),
            (2, 1)
        );
    }

    #[test]
    fn preview_result_exposes_refined_hdr_preview_bundle() {
        let hdr_preview = Arc::new(HdrImageBuffer {
            width: 2,
            height: 1,
            format: HdrPixelFormat::Rgba32Float,
            color_space: HdrColorSpace::LinearSrgb,
            metadata: HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
            rgba_f32: Arc::new(vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0, 1.0]),
        });
        let update = PreviewResult {
            index: 3,
            generation: 5,
            preview_bundle: PreviewBundle::refined().with_hdr(Arc::clone(&hdr_preview)),
            error: None,
        };

        assert!(update.error.is_none());
        assert_eq!(update.preview_bundle.stage(), PreviewStage::Refined);
        assert_eq!(
            update
                .preview_bundle
                .plane(PixelPlaneKind::Hdr)
                .expect("hdr preview plane")
                .dimensions(),
            (2, 1)
        );
        // HDR refinement results carry HDR pixels only — the SDR fallback plane is derived
        // lazily at render time by `select_render_backend`'s HDR-plane fallback (and the
        // HDR image plane shader's `SdrToneMapped` output mode). Keeping the loader side
        // HDR-only avoids tone-mapping a 4K HQ preview on systems that will only present
        // it through the native scRGB pipeline.
        assert!(update.preview_bundle.sdr().is_none());
    }

    #[test]
    fn image_request_stays_inflight_until_ui_finishes_installing_result() {
        let mut loader = ImageLoader::new();
        let index = 7;
        let generation = 11;
        loader.test_register_inflight(index, generation);

        let load_result = LoadResult {
            index,
            generation,
            result: Err("synthetic".to_string()),
            preview_bundle: PreviewBundle::initial(),
            ultra_hdr_capacity_sensitive: false,
            sdr_fallback_is_placeholder: false,
        };
        loader.test_send_loader_output(LoaderOutput::Image(load_result));

        let output = loader.poll().expect("polled image result");
        assert!(matches!(output, LoaderOutput::Image(_)));
        assert!(loader.is_loading(index, generation));

        loader.finish_image_request(index, generation);
        assert!(!loader.is_loading(index, generation));
    }

    #[test]
    fn tile_result_exposes_shared_pending_key_and_repaint_policy() {
        let result = TileResult {
            index: 7,
            generation: 11,
            col: 3,
            row: 4,
            pixel_kind: TilePixelKind::Hdr,
        };

        assert_eq!(
            result.pending_key(),
            crate::tile_cache::PendingTileKey::new(
                crate::tile_cache::TileCoord { col: 3, row: 4 },
                TilePixelKind::Hdr,
            )
        );
        assert!(result.should_request_repaint());
    }

    #[test]
    fn request_tile_decodes_hdr_source_into_hdr_cache_and_reports_hdr_ready() {
        let loader = ImageLoader::new();
        let source: Arc<dyn crate::hdr::tiled::HdrTiledSource> = Arc::new(
            crate::hdr::tiled::HdrTiledImageSource::new(HdrImageBuffer {
                width: crate::tile_cache::get_tile_size(),
                height: crate::tile_cache::get_tile_size(),
                format: HdrPixelFormat::Rgba32Float,
                color_space: HdrColorSpace::LinearSrgb,
                metadata: HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
                rgba_f32: Arc::new(vec![
                    0.25;
                    crate::tile_cache::get_tile_size() as usize
                        * crate::tile_cache::get_tile_size() as usize
                        * 4
                ]),
            })
            .expect("build HDR tiled source"),
        );

        loader.request_tile(3, 0, 1.0, TileDecodeSource::Hdr(Arc::clone(&source)), 0, 0);

        let output = loader
            .rx
            .recv_timeout(Duration::from_secs(2))
            .expect("HDR tile ready result");
        match output {
            LoaderOutput::Tile(tile) => {
                assert_eq!(tile.index, 3);
                assert_eq!(tile.generation, 0);
                assert_eq!(tile.col, 0);
                assert_eq!(tile.row, 0);
                assert_eq!(tile.pixel_kind, TilePixelKind::Hdr);
            }
            _ => panic!("expected HDR tile-ready output"),
        }

        assert!(
            source
                .cached_tile_rgba32f_arc(
                    0,
                    0,
                    crate::tile_cache::get_tile_size(),
                    crate::tile_cache::get_tile_size(),
                )
                .is_some()
        );
    }

    #[test]
    fn request_tile_reports_ready_when_hdr_tile_is_already_cached() {
        let loader = ImageLoader::new();
        let source: Arc<dyn crate::hdr::tiled::HdrTiledSource> = Arc::new(
            crate::hdr::tiled::HdrTiledImageSource::new(HdrImageBuffer {
                width: crate::tile_cache::get_tile_size(),
                height: crate::tile_cache::get_tile_size(),
                format: HdrPixelFormat::Rgba32Float,
                color_space: HdrColorSpace::LinearSrgb,
                metadata: HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
                rgba_f32: Arc::new(vec![
                    0.25;
                    crate::tile_cache::get_tile_size() as usize
                        * crate::tile_cache::get_tile_size() as usize
                        * 4
                ]),
            })
            .expect("build HDR tiled source"),
        );
        source
            .extract_tile_rgba32f_arc(
                0,
                0,
                crate::tile_cache::get_tile_size(),
                crate::tile_cache::get_tile_size(),
            )
            .expect("seed HDR tile cache");

        loader.request_tile(3, 9, 1.0, TileDecodeSource::Hdr(source), 0, 0);

        let output = loader
            .rx
            .recv_timeout(Duration::from_secs(2))
            .expect("HDR cached tile ready result");
        match output {
            LoaderOutput::Tile(tile) => {
                assert_eq!(tile.index, 3);
                assert_eq!(tile.generation, 9);
                assert_eq!(tile.col, 0);
                assert_eq!(tile.row, 0);
                assert_eq!(tile.pixel_kind, TilePixelKind::Hdr);
            }
            _ => panic!("expected HDR tile-ready output"),
        }
    }

    struct FailingHdrTiledSource;

    impl crate::hdr::tiled::HdrTiledSource for FailingHdrTiledSource {
        fn source_kind(&self) -> crate::hdr::tiled::HdrTiledSourceKind {
            crate::hdr::tiled::HdrTiledSourceKind::DiskBacked
        }

        fn width(&self) -> u32 {
            crate::tile_cache::get_tile_size()
        }

        fn height(&self) -> u32 {
            crate::tile_cache::get_tile_size()
        }

        fn color_space(&self) -> HdrColorSpace {
            HdrColorSpace::LinearSrgb
        }

        fn generate_hdr_preview(&self, _max_w: u32, _max_h: u32) -> Result<HdrImageBuffer, String> {
            Err("preview failed".to_string())
        }

        fn generate_sdr_preview(
            &self,
            _max_w: u32,
            _max_h: u32,
        ) -> Result<(u32, u32, Vec<u8>), String> {
            Err("preview failed".to_string())
        }

        fn extract_tile_rgba32f_arc(
            &self,
            _x: u32,
            _y: u32,
            _width: u32,
            _height: u32,
        ) -> Result<Arc<crate::hdr::tiled::HdrTileBuffer>, String> {
            Err("decode failed".to_string())
        }
    }

    #[test]
    fn request_tile_reports_ready_when_hdr_decode_fails() {
        let loader = ImageLoader::new();
        let source: Arc<dyn crate::hdr::tiled::HdrTiledSource> = Arc::new(FailingHdrTiledSource);

        loader.request_tile(5, 13, 1.0, TileDecodeSource::Hdr(source), 0, 0);

        let output = loader
            .rx
            .recv_timeout(Duration::from_secs(2))
            .expect("HDR failed tile ready result");
        match output {
            LoaderOutput::Tile(tile) => {
                assert_eq!(tile.index, 5);
                assert_eq!(tile.generation, 13);
                assert_eq!(tile.col, 0);
                assert_eq!(tile.row, 0);
                assert_eq!(tile.pixel_kind, TilePixelKind::Hdr);
            }
            _ => panic!("expected HDR tile-ready output"),
        }
    }

    #[test]
    fn load_hdr_routes_threshold_sized_images_to_tiled_fallback() {
        let _threshold_lock = lock_tiled_threshold_for_test();
        let path = std::env::temp_dir().join(format!(
            "simple_image_viewer_loader_hdr_route_{}.hdr",
            std::process::id()
        ));
        let bytes = b"#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n-Y 1 +X 1\n\x80\x80\x80\x81";
        std::fs::write(&path, bytes).expect("write test HDR");
        let _threshold_override = TiledThresholdOverride::set(1);

        let image_data = load_hdr(&path, 1.0, HdrToneMapSettings::default()).expect("load tiny HDR");

        let ImageData::HdrTiled { hdr, fallback } = image_data else {
            panic!("expected Radiance HDR to route to HDR tiled image data");
        };
        assert_eq!(
            hdr.source_kind(),
            crate::hdr::tiled::HdrTiledSourceKind::DiskBacked
        );
        assert!(fallback.is_hdr_sdr_fallback());
        let tile = hdr
            .extract_tile_rgba32f_arc(0, 0, 1, 1)
            .expect("extract Radiance HDR tile");
        assert_eq!((tile.width, tile.height), (1, 1));
        assert_eq!(tile.color_space, HdrColorSpace::LinearSrgb);
        assert_eq!(tile.rgba_f32.len(), 4);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_radiance_hdr_routes_small_images_to_float_image_data() {
        let _threshold_lock = lock_tiled_threshold_for_test();
        let path = std::env::temp_dir().join(format!(
            "simple_image_viewer_loader_hdr_static_route_{}.hdr",
            std::process::id()
        ));
        let bytes = b"#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n-Y 1 +X 1\n\x80\x80\x80\x81";
        std::fs::write(&path, bytes).expect("write test HDR");
        let _threshold_override = TiledThresholdOverride::set(u64::MAX);

        let image_data =
            load_hdr(&path, 1.0, HdrToneMapSettings::default()).expect("load tiny Radiance HDR");

        let ImageData::Hdr { hdr, fallback } = image_data else {
            panic!("expected small Radiance HDR to route to static HDR image data");
        };
        assert_eq!((hdr.width, hdr.height), (1, 1));
        assert_eq!((fallback.width, fallback.height), (1, 1));
        assert_eq!(hdr.color_space, HdrColorSpace::LinearSrgb);
        assert_eq!(hdr.rgba_f32.len(), 4);
        assert!(
            hdr.rgba_f32.iter().any(|value| *value > 0.0),
            "Radiance HDR float buffer should contain visible samples"
        );
        let _ = std::fs::remove_file(&path);
    }

    fn openexr_images_root() -> Option<PathBuf> {
        std::env::var_os("SIV_OPENEXR_IMAGES_DIR")
            .map(PathBuf::from)
            .or_else(|| Some(PathBuf::from(r"F:\HDR\openexr-images")))
            .filter(|path| path.is_dir())
    }

    fn assert_gray_ramp_loads_with_visible_fallback(root: &Path, relative_path: &str) {
        let path = root.join(relative_path);
        assert!(
            path.is_file(),
            "OpenEXR sample file is missing: {}",
            path.display()
        );

        let image_data = load_hdr(&path, 1.0, HdrToneMapSettings::default())
            .unwrap_or_else(|err| panic!("load {}: {err}", path.display()));
        let (hdr_max_rgb, fallback_pixels) = match image_data {
            ImageData::Hdr { hdr, fallback } => (
                max_hdr_rgb(hdr.rgba_f32.as_slice()),
                fallback.rgba().to_vec(),
            ),
            ImageData::HdrTiled { .. } => panic!(
                "{} is small enough for static HDR and should not route through tiled rendering",
                path.display()
            ),
            _ => panic!(
                "expected {} to load as static HDR image data",
                path.display()
            ),
        };
        let fallback_max_rgb = max_rgba8_rgb(&fallback_pixels);

        assert!(
            fallback_max_rgb > 0,
            "fallback display pixels should not be all black for {} (hdr_max_rgb={hdr_max_rgb:?})",
            path.display(),
        );
    }

    fn max_hdr_rgb(rgba_f32: &[f32]) -> Option<f32> {
        rgba_f32
            .chunks_exact(4)
            .map(|pixel| pixel[0].max(pixel[1]).max(pixel[2]))
            .reduce(f32::max)
    }

    fn max_rgba8_rgb(pixels: &[u8]) -> u8 {
        pixels
            .chunks_exact(4)
            .map(|pixel| pixel[0].max(pixel[1]).max(pixel[2]))
            .max()
            .unwrap_or(0)
    }

    fn collect_exr_files(root: &Path, files: &mut Vec<PathBuf>) {
        let entries = std::fs::read_dir(root).unwrap_or_else(|err| {
            panic!("read OpenEXR corpus directory {}: {err}", root.display())
        });
        for entry in entries {
            let path = entry.expect("read OpenEXR corpus entry").path();
            if path.is_dir() {
                collect_exr_files(&path, files);
            } else if path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("exr"))
            {
                files.push(path);
            }
        }
    }

    #[test]
    fn gray_ramps_load_with_visible_fallback_pixels() {
        let _threshold_lock = lock_tiled_threshold_for_test();
        let _threshold_override = TiledThresholdOverride::set(u64::MAX);
        let Some(root) = openexr_images_root() else {
            eprintln!(
                "skipping OpenEXR GrayRamps loader regression test; set SIV_OPENEXR_IMAGES_DIR to openexr-images"
            );
            return;
        };

        assert_gray_ramp_loads_with_visible_fallback(&root, "TestImages/GrayRampsDiagonal.exr");
        assert_gray_ramp_loads_with_visible_fallback(&root, "TestImages/GrayRampsHorizontal.exr");
    }

    #[test]
    fn openexr_standard_corpus_loads_every_exr_sample() {
        let Some(root) = openexr_images_root() else {
            eprintln!(
                "skipping OpenEXR corpus load test; set SIV_OPENEXR_IMAGES_DIR to openexr-images"
            );
            return;
        };

        let mut files = Vec::new();
        collect_exr_files(&root, &mut files);
        files.sort();
        assert!(!files.is_empty(), "OpenEXR corpus contains no EXR files");

        let failures: Vec<String> = files
            .iter()
            .filter_map(|path| {
                load_hdr(path, 1.0, HdrToneMapSettings::default()).err().map(|err| {
                    let relative = path.strip_prefix(&root).unwrap_or(path);
                    format!("{}: {err}", relative.display())
                })
            })
            .collect();

        assert!(
            failures.is_empty(),
            "OpenEXR corpus load failures ({}/{}):\n{}",
            failures.len(),
            files.len(),
            failures.join("\n")
        );
    }

    #[test]
    fn deep_openexr_standard_passes_decode_without_placeholder() {
        let root = std::path::PathBuf::from(r"F:\HDR\openexr-images");
        if !root.is_dir() {
            eprintln!(
                "skipping OpenEXR deep sample test; set up F:\\HDR\\openexr-images or SIV_OPENEXR_IMAGES_DIR"
            );
            return;
        }

        for relative_path in [
            "v2/LeftView/Balls.exr",
            "v2/LeftView/Ground.exr",
            "v2/LeftView/Leaves.exr",
            "v2/LeftView/Trunks.exr",
            "v2/LowResLeftView/Balls.exr",
            "v2/LowResLeftView/Ground.exr",
            "v2/LowResLeftView/Leaves.exr",
            "v2/LowResLeftView/Trunks.exr",
            "v2/Stereo/Balls.exr",
            "v2/Stereo/Ground.exr",
            "v2/Stereo/Leaves.exr",
            "v2/Stereo/Trunks.exr",
        ] {
            let path = root.join(relative_path);
            assert!(
                path.is_file(),
                "OpenEXR deep sample file is missing: {}",
                path.display()
            );

            let hdr = crate::hdr::exr_tiled::decode_deep_exr_image(&path).unwrap_or_else(|err| {
                panic!(
                    "decode deep OpenEXR sample failed for {}: {err}",
                    path.display()
                )
            });
            assert_eq!(
                hdr.rgba_f32.len(),
                hdr.width as usize * hdr.height as usize * 4
            );
            assert!(
                hdr.rgba_f32.iter().all(|value| value.is_finite()),
                "deep EXR decode should produce finite float samples: {}",
                path.display()
            );
        }
    }

    #[test]
    fn deep_openexr_standard_sample_loads_hdr_float_content() {
        let path = std::path::PathBuf::from(r"F:\HDR\openexr-images\v2\LowResLeftView\Balls.exr");
        if !path.is_file() {
            eprintln!(
                "skipping OpenEXR deep sample test; set up F:\\HDR\\openexr-images or SIV_OPENEXR_IMAGES_DIR"
            );
            return;
        }

        let image_data =
            load_hdr(&path, 1.0, HdrToneMapSettings::default()).expect("load deep OpenEXR sample");
        let ImageData::Hdr { hdr, .. } = image_data else {
            panic!("unexpected deep EXR image data");
        };
        assert!(
            hdr.rgba_f32
                .chunks_exact(4)
                .any(|pixel| pixel[0] > 0.0 || pixel[1] > 0.0 || pixel[2] > 0.0),
            "deep EXR HDR buffer should contain visible RGB content"
        );
        assert!(
            hdr.rgba_f32.chunks_exact(4).any(|pixel| pixel[3] > 0.0),
            "deep EXR HDR buffer should contain visible alpha"
        );
    }

    #[test]
    fn disk_backed_exr_probe_accepts_subsampled_yc_sample() {
        let path = std::path::PathBuf::from(r"F:\HDR\openexr-images\Chromaticities\Rec709_YC.exr");
        if !path.is_file() {
            eprintln!(
                "skipping OpenEXR YC sample test; set up F:\\HDR\\openexr-images or SIV_OPENEXR_IMAGES_DIR"
            );
            return;
        }

        let image_data = try_load_disk_backed_exr_hdr(&path, 1.0, HdrToneMapSettings::default())
            .expect("probe should load subsampled YC EXR");

        assert!(matches!(image_data, Some(ImageData::HdrTiled { .. })));
    }

    #[test]
    fn exr_extension_short_circuits_to_openexr_core_loader() {
        let path = std::env::temp_dir().join(format!(
            "simple_image_viewer_loader_exr_short_circuit_{}.exr",
            std::process::id()
        ));
        std::fs::write(&path, b"not an exr file").expect("write invalid EXR probe");
        let (tx, _rx) = crossbeam_channel::unbounded();
        let (refine_tx, _refine_rx) = crossbeam_channel::unbounded();

        let result = load_image_file(
            1,
            0,
            &path,
            tx,
            refine_tx,
            false,
            HdrToneMapSettings::default().target_hdr_capacity(),
            HdrToneMapSettings::default(),
        );
        let err = match result.result {
            Ok(_) => panic!("invalid EXR should fail in the OpenEXRCore loader"),
            Err(err) => err,
        };
        let _ = std::fs::remove_file(&path);

        assert!(
            err.contains("OpenEXRCore"),
            "EXR extension must not fall through to image-rs/static fallback: {err}"
        );
    }

    #[test]
    fn exr_magic_short_circuits_to_openexr_core_loader_even_with_wrong_extension() {
        let path = std::env::temp_dir().join(format!(
            "simple_image_viewer_loader_exr_magic_short_circuit_{}.png",
            std::process::id()
        ));
        std::fs::write(&path, [0x76, 0x2f, 0x31, 0x01, 0, 0, 0, 0])
            .expect("write invalid EXR magic probe");

        let result = super::load_via_content_detection(
            &path,
            HdrToneMapSettings::default().target_hdr_capacity(),
            HdrToneMapSettings::default(),
        );
        let err = match result {
            Ok(_) => panic!("invalid EXR magic should fail in the OpenEXRCore loader"),
            Err(err) => err,
        };
        let _ = std::fs::remove_file(&path);

        assert!(
            err.contains("OpenEXRCore"),
            "EXR magic must route to OpenEXRCore even when extension is wrong: {err}"
        );
    }

    #[test]
    fn ultra_hdr_jpeg_sample_loads_as_hdr_image_data() {
        let _threshold_lock = lock_tiled_threshold_for_test();
        let root = std::env::var_os("SIV_ULTRA_HDR_SAMPLES_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"F:\HDR\Ultra_HDR_Samples"));
        let path = root
            .join("Originals")
            .join("Ultra_HDR_Samples_Originals_01.jpg");
        if !path.is_file() {
            eprintln!("skipping Ultra HDR loader test; sample missing");
            return;
        }

        let image_data = load_jpeg(&path).expect("load Ultra HDR JPEG_R sample");

        let ImageData::Hdr { hdr, fallback } = image_data else {
            panic!("expected Ultra HDR JPEG_R to load as HDR image data");
        };
        assert_eq!((hdr.width, hdr.height), (4080, 3072));
        assert_eq!((fallback.width, fallback.height), (4080, 3072));
        assert!(
            hdr.rgba_f32
                .chunks_exact(4)
                .any(|pixel| pixel[0] > 1.0 || pixel[1] > 1.0 || pixel[2] > 1.0),
            "Ultra HDR loader should preserve HDR highlights"
        );
    }

    #[test]
    fn ultra_hdr_loader_uses_target_hdr_capacity() {
        let _threshold_lock = lock_tiled_threshold_for_test();
        let root = std::env::var_os("SIV_ULTRA_HDR_SAMPLES_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"F:\HDR\Ultra_HDR_Samples"));
        let path = root
            .join("Originals")
            .join("Ultra_HDR_Samples_Originals_01.jpg");
        if !path.is_file() {
            eprintln!("skipping Ultra HDR loader target capacity test; sample missing");
            return;
        }

        let low = load_jpeg_with_target_capacity(&path, 1.0, HdrToneMapSettings::default())
            .expect("load low-capacity Ultra HDR JPEG_R sample");
        // `hdr_gain_map_decode_capacity` clamps to `HdrToneMapSettings::target_hdr_capacity()`;
        // raise the configured peak so an 8× probe survives the min() and exercises strong gain.
        let high_tone = HdrToneMapSettings {
            max_display_nits: HdrToneMapSettings::default().sdr_white_nits * 8.0,
            ..HdrToneMapSettings::default()
        };
        let high = load_jpeg_with_target_capacity(&path, 8.0, high_tone)
            .expect("load high-capacity Ultra HDR JPEG_R sample");

        let ImageData::Hdr { hdr: low, .. } = low else {
            panic!("expected low-capacity Ultra HDR JPEG_R to load as HDR image data");
        };
        let ImageData::Hdr { hdr: high, .. } = high else {
            panic!("expected high-capacity Ultra HDR JPEG_R to load as HDR image data");
        };

        let low_peak = low
            .rgba_f32
            .chunks_exact(4)
            .map(|pixel| pixel[0].max(pixel[1]).max(pixel[2]))
            .fold(0.0_f32, f32::max);
        let high_peak = high
            .rgba_f32
            .chunks_exact(4)
            .map(|pixel| pixel[0].max(pixel[1]).max(pixel[2]))
            .fold(0.0_f32, f32::max);

        assert!(
            high_peak > low_peak,
            "loader should pass target HDR capacity into JPEG_R gain-map recovery"
        );
    }

    #[test]
    fn ultra_hdr_load_result_is_capacity_sensitive() {
        let _threshold_lock = lock_tiled_threshold_for_test();
        let root = std::env::var_os("SIV_ULTRA_HDR_SAMPLES_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"F:\HDR\Ultra_HDR_Samples"));
        let path = root
            .join("Originals")
            .join("Ultra_HDR_Samples_Originals_01.jpg");
        if !path.is_file() {
            eprintln!("skipping Ultra HDR load result marker test; sample missing");
            return;
        }

        let (tx, _rx) = crossbeam_channel::unbounded();
        let (refine_tx, _refine_rx) = crossbeam_channel::unbounded();
        let result = load_image_file(
            1,
            7,
            &path,
            tx,
            refine_tx,
            false,
            HdrToneMapSettings::default().target_hdr_capacity(),
            HdrToneMapSettings::default(),
        );

        assert!(
            result.ultra_hdr_capacity_sensitive,
            "JPEG_R load results should be marked for capacity-based invalidation"
        );
    }

    #[test]
    fn ultra_hdr_original_corpus_loads_as_hdr_image_data() {
        let _threshold_lock = lock_tiled_threshold_for_test();
        let root = std::env::var_os("SIV_ULTRA_HDR_SAMPLES_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"F:\HDR\Ultra_HDR_Samples"));
        let originals = root.join("Originals");
        if !originals.is_dir() {
            eprintln!("skipping Ultra HDR corpus loader test; Originals directory missing");
            return;
        }

        let failures = (1..=10)
            .filter_map(|index| {
                let path = originals.join(format!("Ultra_HDR_Samples_Originals_{index:02}.jpg"));
                if !path.is_file() {
                    return Some(format!("{}: missing", path.display()));
                }

                match load_jpeg(&path) {
                    Ok(ImageData::Hdr { hdr, fallback }) => {
                        let has_hdr_highlight = hdr
                            .rgba_f32
                            .chunks_exact(4)
                            .any(|pixel| pixel[0] > 1.0 || pixel[1] > 1.0 || pixel[2] > 1.0);
                        if hdr.width == 0
                            || hdr.height == 0
                            || fallback.width != hdr.width
                            || fallback.height != hdr.height
                            || !has_hdr_highlight
                        {
                            Some(format!("{}: invalid HDR output", path.display()))
                        } else {
                            None
                        }
                    }
                    Ok(_) => Some(format!("{}: loaded as non-HDR image data", path.display())),
                    Err(err) => Some(format!("{}: {err}", path.display())),
                }
            })
            .collect::<Vec<_>>();

        assert!(
            failures.is_empty(),
            "Ultra HDR corpus failures:\n{}",
            failures.join("\n")
        );
    }

    #[test]
    fn ultra_hdr_threshold_sized_jpeg_routes_to_file_backed_hdr_tiles() {
        let _threshold_lock = lock_tiled_threshold_for_test();
        let root = std::env::var_os("SIV_ULTRA_HDR_SAMPLES_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"F:\HDR\Ultra_HDR_Samples"));
        let path = root
            .join("Originals")
            .join("Ultra_HDR_Samples_Originals_01.jpg");
        if !path.is_file() {
            eprintln!("skipping Ultra HDR tiled loader test; sample missing");
            return;
        }
        let _threshold_override = TiledThresholdOverride::set(1);

        let image_data = load_jpeg(&path).expect("load Ultra HDR JPEG_R sample as tiled HDR");

        let ImageData::HdrTiled { hdr, fallback } = image_data else {
            panic!("expected Ultra HDR JPEG_R to route to HDR tiled image data");
        };
        assert_eq!(
            hdr.source_kind(),
            crate::hdr::tiled::HdrTiledSourceKind::DiskBacked
        );
        assert!(fallback.is_hdr_sdr_fallback());
        let tile = hdr
            .extract_tile_rgba32f_arc(0, 0, 64, 64)
            .expect("extract Ultra HDR tile");
        assert_eq!((tile.width, tile.height), (64, 64));
        assert!(
            tile.rgba_f32
                .chunks_exact(4)
                .any(|pixel| pixel[0] > 1.0 || pixel[1] > 1.0 || pixel[2] > 1.0),
            "Ultra HDR tiled source should preserve HDR highlights"
        );
    }

    #[test]
    fn oversized_hdr_tiled_fallback_remembers_hdr_source() {
        let hdr = HdrImageBuffer {
            width: 4097,
            height: 1,
            format: HdrPixelFormat::Rgba32Float,
            color_space: HdrColorSpace::LinearSrgb,
            metadata: HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
            rgba_f32: Arc::new(vec![1.0; 4097 * 4]),
        };
        let fallback = DecodedImage::new(4097, 1, vec![255; 4097 * 4]);

        let image_data = make_hdr_image_data_for_limit(hdr, fallback, 4096);

        let ImageData::HdrTiled { hdr, fallback } = image_data else {
            panic!("expected HDR tiled image data");
        };
        assert_eq!(hdr.width(), 4097);
        assert_eq!(hdr.height(), 1);
        assert!(fallback.is_hdr_sdr_fallback());
    }

    #[test]
    fn modern_hdr_format_path_helpers_detect_supported_extensions() {
        assert!(is_avif_path(Path::new("sample.avif")));
        assert!(is_avif_path(Path::new("sample.avifs")));
        assert!(is_heif_path(Path::new("sample.HEIC")));
        assert!(is_jxl_path(Path::new("sample.jxl")));
        assert!(is_hdr_capable_modern_format_path(Path::new("sample.heif")));
        assert!(!is_hdr_capable_modern_format_path(Path::new("sample.png")));
    }

    /// Set `SIV_PSD_SAMPLES_DIR` to a folder that contains `colors.psd` and `seine.psd`
    /// (for example `libavif/tests/data/sources` inside a libavif source checkout) to regression-test the `psd` crate composite
    /// path: it must not unwind (historical `psd_channel` index OOB panics).
    ///
    /// When the variable is unset or files are missing, this test is a no-op so CI stays green.
    #[test]
    fn optional_psd_libavif_sources_load_without_panic() {
        let Some(dir) = std::env::var("SIV_PSD_SAMPLES_DIR")
            .ok()
            .filter(|p| Path::new(p).is_dir())
        else {
            return;
        };
        let dir = PathBuf::from(dir);
        for name in ["colors.psd", "seine.psd"] {
            let path = dir.join(name);
            if !path.is_file() {
                continue;
            }
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| load_psd(&path)));
            assert!(
                outcome.is_ok(),
                "load_psd must not panic for {}",
                path.display()
            );
            match outcome.unwrap() {
                Ok(data) => match &data {
                    ImageData::Static(img) => {
                        assert!(img.width > 0 && img.height > 0, "{name}: static dims");
                    }
                    ImageData::Tiled(src) => {
                        assert!(src.width() > 0 && src.height() > 0, "{name}: tiled dims");
                    }
                    _ => panic!("{name}: unexpected PSD ImageData shape"),
                },
                Err(_msg) => {
                    // OOM guard, `psd` parse error, or composite `Err` after catch_unwind — all OK.
                }
            }
        }
    }
}
const DETECTION_BUFFER_SIZE: usize = 16;

fn load_by_image_format(
    format: image::ImageFormat,
    path: &PathBuf,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
) -> Result<ImageData, String> {
    match format {
        image::ImageFormat::Png => load_png(path, hdr_target_capacity, hdr_tone_map),
        image::ImageFormat::Gif => load_gif(path, hdr_target_capacity, hdr_tone_map),
        image::ImageFormat::WebP => load_webp(path, hdr_target_capacity, hdr_tone_map),
        image::ImageFormat::Tiff => crate::libtiff_loader::load_via_libtiff(
            path,
            hdr_target_capacity,
            hdr_tone_map,
        ),
        // Standard single-frame formats handled by load_static
        image::ImageFormat::Jpeg => {
            load_jpeg_with_target_capacity(path, hdr_target_capacity, hdr_tone_map)
        }
        image::ImageFormat::Bmp
        | image::ImageFormat::Ico
        | image::ImageFormat::Pnm
        | image::ImageFormat::Tga
        | image::ImageFormat::Dds
        | image::ImageFormat::Farbfeld
        | image::ImageFormat::Qoi => load_static(path, hdr_target_capacity, hdr_tone_map),
        // `image` is built without `avif` (ravif); libavif-only (`load_avif_with_target_capacity`).
        image::ImageFormat::Avif => load_avif_with_target_capacity(path, hdr_target_capacity, hdr_tone_map),
        image::ImageFormat::Hdr => load_hdr(path, hdr_target_capacity, hdr_tone_map),
        image::ImageFormat::OpenExr => load_detected_exr(path, hdr_target_capacity, hdr_tone_map),
        _ => Err(rust_i18n::t!(
            "error.unsupported_detected_format",
            format = format!("{:?}", format)
        )
        .to_string()),
    }
}

fn load_detected_exr(
    path: &Path,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
) -> Result<ImageData, String> {
    if let Some(image_data) =
        try_load_disk_backed_exr_hdr(path, hdr_target_capacity, hdr_tone_map)?
    {
        return Ok(image_data);
    }

    let hdr = match crate::hdr::decode::decode_exr_display_image(path) {
        Ok(hdr) => hdr,
        Err(err) if is_exr_deep_data_unsupported_error(&err) => {
            log::warn!(
                "[Loader] Deep EXR data needs custom compositing for {}; using deep decoder",
                path.display()
            );
            return load_deep_exr(path, hdr_target_capacity, hdr_tone_map);
        }
        Err(err) => return Err(err),
    };
    let pixels = hdr_sdr_fallback_rgba8_eager_or_placeholder(
        &hdr,
        hdr_target_capacity,
        &hdr_tone_map,
    )?;
    let fallback = DecodedImage::new(hdr.width, hdr.height, pixels);
    let (hdr, fallback) = apply_exif_orientation_to_hdr_pair(path, hdr, fallback);
    Ok(make_hdr_image_data(hdr, fallback))
}

fn load_via_content_detection(
    path: &PathBuf,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
) -> Result<ImageData, String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;

    // Use constant for buffer size
    let mut header = [0u8; DETECTION_BUFFER_SIZE];
    let n = file.read(&mut header).unwrap_or(0);

    // 1. Try standard image-rs detection
    if let Ok(guessed) = image::guess_format(&header[..n]) {
        return load_by_image_format(guessed, path, hdr_target_capacity, hdr_tone_map);
    }

    if crate::hdr::jpegxl::is_jxl_header(&header[..n]) {
        return load_jxl_with_target_capacity(path, hdr_target_capacity, hdr_tone_map);
    }

    // 2. Manual HEIC detection (since image-rs 0.25 doesn't natively guess it)
    // HEIF/HEIC signature: "ftyp" (at offset 4) followed by various brands.
    if n >= 12 && &header[4..8] == b"ftyp" {
        let sub = &header[8..12];
        if crate::hdr::avif::is_avif_brand(sub) {
            return load_avif_with_target_capacity(path, hdr_target_capacity, hdr_tone_map);
        }
        if crate::hdr::heif::is_heif_brand(sub) {
            return load_heif_hdr_aware(path, hdr_target_capacity, hdr_tone_map);
        }
    }

    Err(rust_i18n::t!("error.detection_failed").to_string())
}
// ---------------------------------------------------------------------------
// RAW Image Support (LibRaw)
// ---------------------------------------------------------------------------

pub struct RawImageSource {
    path: PathBuf,
    /// True RAW sensor dimensions (not thumbnail dimensions).
    width: u32,
    height: u32,
    /// Initially holds the system preview at its ORIGINAL resolution (NOT upscaled).
    /// The refinement worker replaces this with the full-res LibRaw demosaiced image.
    /// extract_tile() dynamically maps coordinates between RAW space and preview space.
    developed_image: Arc<PLRwLock<Option<DynamicImage>>>,
    /// Channel to send refinement requests. Kept here so `request_refinement()` can
    /// be called later (only when the image becomes active) instead of eagerly in the
    /// constructor, preventing prefetched images from spawning ~400MB develop tasks.
    refine_tx: Sender<RefinementRequest>,
    orientation_override: i32,
}

impl RawImageSource {
    pub fn new(
        path: PathBuf,
        preview: DecodedImage,
        raw_width: u32,
        raw_height: u32,
        refine_tx: Sender<RefinementRequest>,
        orientation_override: i32,
    ) -> Self {
        // IMPORTANT: Store preview at its ORIGINAL resolution — NO upscaling!
        // Previously this called resize_exact(raw_width, raw_height) which allocated
        // ~400MB per image (e.g. 11648×8736×4). With rapid switching and prefetching,
        // multiple concurrent allocations of this size caused OOM crashes.
        // Instead, extract_tile() maps coordinates from RAW space to preview space on demand.
        //
        // ALSO: We do NOT send a refinement request here. Refinement is deferred until
        // the image becomes the actively-viewed one (via request_refinement()). This
        // prevents prefetched images from each spawning ~400MB LibRaw develop tasks.

        let rgba = preview.into_rgba8_image();
        let developed_image = Arc::new(PLRwLock::new(Some(DynamicImage::ImageRgba8(rgba))));

        let refine_tx = refine_tx.clone();

        Self {
            path,
            width: raw_width,
            height: raw_height,
            developed_image,
            refine_tx,
            orientation_override,
        }
    }
}

impl TiledImageSource for RawImageSource {
    fn width(&self) -> u32 {
        self.width
    }

    fn height(&self) -> u32 {
        self.height
    }

    fn extract_tile(&self, x: u32, y: u32, w: u32, h: u32) -> Arc<Vec<u8>> {
        let img_lock = self.developed_image.read();
        if let Some(ref img) = *img_lock {
            let (iw, ih) = img.dimensions();
            if iw == self.width && ih == self.height {
                // Full-res developed image available — direct crop, no scaling needed.
                if let Some(rgba) = img.as_rgba8() {
                    let mut result = vec![0u8; (w * h * 4) as usize];
                    for row in 0..h {
                        let src_y = y + row;
                        let src_offset = (src_y * iw + x) as usize * 4;
                        let dst_offset = (row * w) as usize * 4;
                        let len =
                            (w as usize * 4).min(rgba.as_raw().len().saturating_sub(src_offset));
                        if len > 0 {
                            result[dst_offset..dst_offset + len]
                                .copy_from_slice(&rgba.as_raw()[src_offset..src_offset + len]);
                        }
                    }
                    Arc::new(result)
                } else {
                    let crop = img.crop_imm(x, y, w, h);
                    Arc::new(crop.into_rgba8().into_raw())
                }
            } else {
                // Preview image (smaller than RAW dimensions).
                let scale_x = iw as f64 / self.width as f64;
                let scale_y = ih as f64 / self.height as f64;
                let px = (x as f64 * scale_x) as u32;
                let py = (y as f64 * scale_y) as u32;
                let pw = ((w as f64 * scale_x).ceil() as u32)
                    .min(iw.saturating_sub(px))
                    .max(1);
                let ph = ((h as f64 * scale_y).ceil() as u32)
                    .min(ih.saturating_sub(py))
                    .max(1);
                let crop = img.crop_imm(px, py, pw, ph);
                let resized = crop.resize_exact(w, h, image::imageops::FilterType::Triangle);
                Arc::new(resized.into_rgba8().into_raw())
            }
        } else {
            Arc::new(vec![0; (w * h * RGBA_CHANNELS as u32) as usize])
        }
    }

    fn generate_preview(&self, max_w: u32, max_h: u32) -> (u32, u32, Vec<u8>) {
        let img_lock = self.developed_image.read();
        if let Some(ref img) = *img_lock {
            let scaled = img.thumbnail(max_w, max_h);
            let rgba = scaled.to_rgba8();
            (rgba.width(), rgba.height(), rgba.into_raw())
        } else {
            (0, 0, Vec::new())
        }
    }

    fn full_pixels(&self) -> Option<Arc<Vec<u8>>> {
        let img_lock = self.developed_image.read();
        if let Some(ref img) = *img_lock {
            let (iw, ih) = img.dimensions();
            // Only return pixels when we have the full-res developed image.
            // If it's still the small preview, the stride would mismatch
            // self.width/self.height and corrupt downstream consumers (e.g. printing).
            if iw == self.width && ih == self.height {
                Some(Arc::new(img.to_rgba8().into_raw()))
            } else {
                None
            }
        } else {
            None
        }
    }

    fn request_refinement(&self, index: usize, generation: u64) {
        log::info!(
            "[RawImageSource] Triggering refinement for index={}, gen={}",
            index,
            generation
        );
        let _ = self.refine_tx.send(RefinementRequest {
            path: self.path.clone(),
            index,
            generation,
            orientation_override: Some(self.orientation_override),
            developed_image: self.developed_image.clone(),
        });
    }
}

fn load_raw(
    _index: usize,
    _generation: u64,
    path: &PathBuf,
    refine_tx: Sender<RefinementRequest>,
    high_quality: bool,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
) -> Result<ImageData, String> {
    // 1. Initialize LibRaw Processor and attempt to open the file header.
    let mut processor =
        RawProcessor::new().ok_or_else(|| rust_i18n::t!("error.libraw_init").to_string())?;
    if let Err(e) = processor.open(path) {
        log::warn!(
            "[Loader] LibRaw could not open {:?}: {}. Falling back to Rule 2 (WIC/ImageIO).",
            path,
            e
        );
        #[cfg(target_os = "windows")]
        return crate::wic::load_via_wic(path, high_quality, None);
        #[cfg(target_os = "macos")]
        return crate::macos_image_io::load_via_image_io(path, high_quality, None);
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        return Err(format!(
            "LibRaw failed and no platform fallback available: {}",
            e
        ));
    }

    let (width, height) = (processor.width() as u32, processor.height() as u32);
    let area = width as u64 * height as u64;
    let threshold = crate::tile_cache::TILED_THRESHOLD.load(std::sync::atomic::Ordering::Relaxed);

    // 1. Determine the authoritative orientation once and for all.
    // We prioritize LibRaw's flip metadata, falling back to the exif crate only if LibRaw's value is unknown.
    let lr_flip = processor.flip();
    let final_orientation = match lr_flip {
        0 => 1,
        1 => 2,
        2 => 4,
        3 => 3,
        4 => 5,
        5 => 8,
        6 => 6,
        7 => 7,
        _ => crate::metadata_utils::get_exif_orientation(path),
    };

    // Ensure LibRaw's develop() pipeline uses the SAME orientation as our preview logic.
    // We explicitly set user_flip based on our authoritative decision.
    let final_lr_flip = match final_orientation {
        1 => 0,
        2 => 1,
        3 => 3,
        4 => 2,
        5 => 4,
        6 => 6,
        7 => 7,
        8 => 5,
        _ => 0,
    };
    processor.set_user_flip(final_lr_flip);

    // --- Performance Optimization: Try to use embedded preview to avoid expensive demosaicing ---
    let mut preview_opt = {
        // Step 1: Try platform-native loaders (WIC/ImageIO).
        // We pass Some(final_orientation) to force the system loader to respect our authoritative choice.
        #[cfg(target_os = "windows")]
        let res = crate::wic::load_via_wic(path, high_quality, Some(final_orientation));
        #[cfg(target_os = "macos")]
        let res =
            crate::macos_image_io::load_via_image_io(path, high_quality, Some(final_orientation));
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        let res: Result<ImageData, String> = Err("Unsupported".to_string());

        match res {
            Ok(ImageData::Static(img)) => Some(img),
            Ok(ImageData::Tiled(source)) => {
                let lim = hq_preview_max_side();
                let (pw, ph, p) = source.generate_preview(lim, lim);
                Some(DecodedImage::new(pw, ph, p))
            }
            Ok(ImageData::HdrTiled { fallback, .. }) => {
                let lim = hq_preview_max_side();
                let (pw, ph, p) = fallback.generate_preview(lim, lim);
                Some(DecodedImage::new(pw, ph, p))
            }
            _ => {
                // Step 2: Fallback to LibRaw's native thumbnail extraction if platform loader failed.
                // We use the same final_orientation to ensure perfect consistency.
                if let Ok(mut p) = processor.unpack_thumb() {
                    if final_orientation > 1 {
                        let pixels = p.take_rgba_owned();
                        if let Some(rgba) = image::RgbaImage::from_raw(p.width, p.height, pixels) {
                            let mut img = image::DynamicImage::ImageRgba8(rgba);
                            match final_orientation {
                                2 => img = img.fliph(),
                                3 => img = img.rotate180(),
                                4 => img = img.flipv(),
                                5 => img = img.fliph().rotate270(),
                                6 => img = img.rotate90(),
                                7 => img = img.fliph().rotate90(),
                                8 => img = img.rotate270(),
                                _ => {}
                            }
                            let rgba_rotated = img.to_rgba8();
                            p.set_rgba_buffer(
                                rgba_rotated.width(),
                                rgba_rotated.height(),
                                rgba_rotated.into_raw(),
                            );
                        }
                    }
                    Some(p)
                } else {
                    None
                }
            }
        }
    };

    // Sanitize: A zero-dimension image will cause a validation error in wgpu (Dimension X is zero).
    if let Some(ref p) = preview_opt {
        if p.width == 0 || p.height == 0 {
            log::warn!(
                "[Loader] Preview path returned a zero-dimension image for {:?}. Invalidate and fallback.",
                path.file_name().unwrap_or_default()
            );
            preview_opt = None;
        }
    }

    if let Some(p) = preview_opt.clone() {
        let hq_lim = hq_preview_max_side();
        let is_hq = p.width >= hq_lim || p.height >= hq_lim;
        // If !high_quality (performance mode), we use any preview to save energy/fans.
        // If high_quality is true, we only use it if it's large enough (HQ).
        if !high_quality || is_hq {
            log::debug!(
                "[Loader] Using embedded preview for {:?} ({}x{}, HQ={})",
                path,
                p.width,
                p.height,
                is_hq
            );
            return Ok(make_image_data(p));
        }
        // If we reach here, high_quality is true but preview is not HQ, so we fall through to develop.
    }

    // 2. Rule 1: High-Performance Synchronous Development for Small Images (< 64MP).
    if area < threshold
        && width <= crate::constants::ABSOLUTE_MAX_TEXTURE_SIDE
        && height <= crate::constants::ABSOLUTE_MAX_TEXTURE_SIDE
    {
        log::info!(
            "[Loader] RAW {}x{} ({:.1} MP) matches Rule 1 (Small). Synchronously extracting pixels...",
            width,
            height,
            area as f64 / 1_000_000.0
        );

        if let Ok(hdr) = processor.develop_scene_linear_hdr() {
            let warnings = processor.process_warnings();
            if warnings != 0 {
                log::info!(
                    "[Loader] LibRaw reported informational warnings (0x{:x}) for {:?}, proceeding with native pixels.",
                    warnings,
                    path
                );
            }

            if hdr.width == 0 || hdr.height == 0 {
                log::error!(
                    "[Loader] LibRaw developed a zero-dimension image for {:?}. Falling through to Rule 2.",
                    path
                );
            } else {
                let fallback_pixels = hdr_sdr_fallback_rgba8_eager_or_placeholder(
                    &hdr,
                    hdr_target_capacity,
                    &hdr_tone_map,
                )?;
                let fallback = DecodedImage::new(hdr.width, hdr.height, fallback_pixels);
                return Ok(make_hdr_image_data(hdr, fallback));
            }
        } else {
            log::error!(
                "[Loader] Failed to develop Rule 1 RAW HDR pixels. Falling through to Rule 2."
            );
        }
    }

    // 3. Rule 2: Asynchronous Tiled Pipeline for Large Images (>= 64MP) or fallback.
    let preview = if let Some(p) = preview_opt {
        p
    } else {
        log::warn!(
            "[Loader] All fast RAW thumbnail paths failed for {:?}. Falling back to slow development...",
            path.file_name().unwrap_or_default()
        );
        processor.develop()?.to_rgba8().into()
    };

    let source = Arc::new(RawImageSource::new(
        path.clone(),
        preview.clone(),
        width,
        height,
        refine_tx,
        final_lr_flip,
    ));

    log::info!(
        "[Loader] RAW {}x{} ({:.1} MP) >= 64MP - Falling back to Async Tiled preview refinement.",
        width,
        height,
        area as f64 / 1_000_000.0
    );
    Ok(ImageData::Tiled(source))
}

/// A TiledImageSource that serves tiles from an in-memory byte buffer.
/// Primarily used for common formats (PNG, JPEG, etc.) that exceed the GPU's single texture limit.
pub struct MemoryImageSource {
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

struct HdrSdrTiledFallbackSource {
    source: Arc<dyn crate::hdr::tiled::HdrTiledSource>,
    tone_map: HdrToneMapSettings,
}

impl HdrSdrTiledFallbackSource {
    fn new(source: Arc<dyn crate::hdr::tiled::HdrTiledSource>, tone_map: HdrToneMapSettings) -> Self {
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
