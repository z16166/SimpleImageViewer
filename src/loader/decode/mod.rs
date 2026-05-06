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

#[cfg(test)]
use super::{
    PixelPlaneKind, PreviewResult, RenderShape, TileDecodeSource, TilePixelKind, TileResult,
    TiledImageSource,
};
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use std::time::Duration;

use assemble::{make_hdr_image_data, make_image_data};
#[cfg(test)]
use assemble::{make_hdr_image_data_for_limit, MemoryImageSource};

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
        || modern::is_hdr_capable_modern_format_path(path)
        || crate::hdr::decode::is_hdr_candidate_ext(&ext)
        || is_raw)
        && matches!(
            result,
            Ok(ImageData::Hdr { .. } | ImageData::HdrTiled { .. })
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::hdr_formats::try_load_disk_backed_exr_hdr;
    use super::jpeg::load_jpeg;
    use super::modern::{is_avif_path, is_heif_path, is_hdr_capable_modern_format_path, is_jxl_path};
    use super::raster::load_psd;
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
