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

//! LibRAW and raw tiled refinement.

use crate::hdr::types::HdrToneMapSettings;
use crate::loader::{hq_preview_max_side, hdr_sdr_fallback_rgba8_eager_or_placeholder};
use crate::loader::{DecodedImage, ImageData, RefinementRequest};
use crate::loader::tiled_sources::RawImageSource;
use crate::raw_processor::RawProcessor;
use crossbeam_channel::Sender;
use std::path::PathBuf;
use std::sync::Arc;

use super::assemble::{make_hdr_image_data, make_image_data};

pub(crate) fn load_raw(
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
        // WIC requires COM on this thread (`ComGuard` — see `decode` module docs).
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
        let res = {
            // WIC requires COM on this thread (`ComGuard` — see `decode` module docs).
            crate::wic::load_via_wic(path, high_quality, Some(final_orientation))
        };
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
