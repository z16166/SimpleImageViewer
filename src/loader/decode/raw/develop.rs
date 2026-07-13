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
//!
//! `raw_high_quality` controls whether LibRaw's expensive demosaic runs:
//! - **Off:** use embedded previews whenever present (SDR pipeline on all displays).
//!   Full develop only when the file has no embedded preview; on HDR displays that
//!   develop result uses the HDR pipeline.
//! - **On:** use embedded previews when they meet HQ size requirements; otherwise demosaic at
//!   full sensor resolution. Developed pixels always use the HDR pipeline (even on SDR displays to support exposure adjustments).

use crate::hdr::types::HdrToneMapSettings;
use crate::loader::preview_caps::finalize_raw_hq_hdr_buffer;
#[cfg(feature = "preload-debug")]
use crate::loader::preview_caps::hq_preview_max_side;
use crate::loader::raw_osd::{RawDemosaicBackend, RawOsdContext};
use crate::loader::tiled_sources::{RawImageSource, RawImageSourceParams};
use crate::loader::{
    DecodedImage, ImageData, RawDevelopedImageRank, RawLoadOutput, RefinementRequest,
    hdr_display_requests_sdr_preview, hdr_sdr_fallback_rgba8_or_placeholder,
};
use crate::raw_processor::RawProcessor;
use crossbeam_channel::Sender;
use std::path::Path;
use std::sync::Arc;

use crate::loader::decode::assemble::{make_hdr_image_data, make_image_data};

pub(crate) fn develop_scene_linear_hdr_timed(
    processor: &mut RawProcessor,
) -> Result<(crate::hdr::types::HdrImageBuffer, u32), String> {
    let started = std::time::Instant::now();
    let hdr = processor.develop_scene_linear_hdr()?;
    Ok((hdr, crate::loader::elapsed_ms_u32(started)))
}

/// Demosaic at full sensor resolution (only when no embedded preview exists).
pub(crate) struct FullResolutionRawDevelopRequest<'a> {
    pub(crate) path: &'a Path,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) area: u64,
    pub(crate) threshold: u64,
    pub(crate) refine_tx: Sender<RefinementRequest>,
    pub(crate) final_lr_flip: i32,
    pub(crate) hdr_target_capacity: f32,
    pub(crate) hdr_tone_map: HdrToneMapSettings,
    pub(crate) osd_ctx: &'a RawOsdContext,
    pub(crate) cancel: &'a crate::loader::DecodeCancelFlag,
}

pub(crate) fn develop_full_resolution(
    processor: &mut RawProcessor,
    request: FullResolutionRawDevelopRequest<'_>,
) -> Result<RawLoadOutput, String> {
    let FullResolutionRawDevelopRequest {
        path,
        width,
        height,
        area,
        threshold,
        refine_tx,
        final_lr_flip,
        hdr_target_capacity,
        hdr_tone_map,
        osd_ctx,
        cancel,
    } = request;
    if area < threshold
        && width <= crate::constants::ABSOLUTE_MAX_TEXTURE_SIDE
        && height <= crate::constants::ABSOLUTE_MAX_TEXTURE_SIDE
    {
        log::info!(
            "[Loader] RAW {}x{} ({:.1} MP) — full develop (no embedded preview).",
            width,
            height,
            area as f64 / 1_000_000.0
        );

        if !hdr_display_requests_sdr_preview(hdr_target_capacity) {
            // Stage boundary before LibRaw unpack / dcraw_process.
            crate::loader::check_decode_cancel_str(Some(cancel.as_atomic()))?;
            if let Ok((hdr, cpu_ms)) = develop_scene_linear_hdr_timed(processor) {
                crate::loader::check_decode_cancel_str(Some(cancel.as_atomic()))?;
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
                        "[Loader] LibRaw developed a zero-dimension HDR image for {:?}. Falling through.",
                        path
                    );
                } else {
                    let hw = hdr.width;
                    let hh = hdr.height;
                    let fallback = DecodedImage::from_hdr_sdr_fallback(
                        hw,
                        hh,
                        hdr_sdr_fallback_rgba8_or_placeholder(&hdr)?,
                    );
                    return Ok(RawLoadOutput {
                        image: make_hdr_image_data(hdr, fallback),
                        osd: osd_ctx
                            .full_develop(hw, hh, RawDemosaicBackend::Host)
                            .with_cpu_demosaic_ms(cpu_ms),
                    });
                }
            } else {
                log::error!(
                    "[Loader] RAW scene-linear HDR develop failed for {:?}. Falling back to SDR develop.",
                    path
                );
            }
        }

        crate::loader::check_decode_cancel_str(Some(cancel.as_atomic()))?;
        match processor.develop() {
            Ok(img) => {
                crate::loader::check_decode_cancel_str(Some(cancel.as_atomic()))?;
                let rgba = img.to_rgba8();
                let rw = rgba.width();
                let rh = rgba.height();
                return Ok(RawLoadOutput {
                    image: make_image_data(DecodedImage::from(rgba)),
                    osd: osd_ctx.full_develop(rw, rh, RawDemosaicBackend::Host),
                });
            }
            Err(e) => {
                log::error!(
                    "[Loader] LibRaw develop failed for {:?}: {}. Falling through to tiled fallback.",
                    path,
                    e
                );
            }
        }
    }

    log::warn!(
        "[Loader] All fast RAW thumbnail paths failed for {:?}. Falling back to slow development...",
        path.file_name().unwrap_or_default()
    );
    crate::loader::check_decode_cancel_str(Some(cancel.as_atomic()))?;
    let preview = processor.develop()?.to_rgba8().into();
    crate::loader::check_decode_cancel_str(Some(cancel.as_atomic()))?;
    // Performance mode only (`load_raw` with `!high_quality`). Never queue HQ refinement.
    let source = Arc::new(RawImageSource::new(
        path.to_path_buf(),
        preview,
        RawImageSourceParams {
            raw_width: width,
            raw_height: height,
            refine_tx,
            initial_image_rank: RawDevelopedImageRank::FullResolutionDeveloped,
            orientation_override: final_lr_flip,
            needs_refinement: false,
            hdr_target_capacity,
            hdr_tone_map,
            hdr_developed_image: None,
        },
    )?);

    log::info!(
        "[Loader] RAW {}x{} ({:.1} MP) — tiled fallback after failed full develop.",
        width,
        height,
        area as f64 / 1_000_000.0
    );
    Ok(RawLoadOutput {
        image: ImageData::Tiled(source),
        osd: osd_ctx.full_develop(width, height, RawDemosaicBackend::Host),
    })
}

/// Demosaic once at full sensor resolution. Used when HQ mode needs better pixels
/// than the embedded preview provides, or when HQ mode has no embedded preview at all.
///
/// Intentionally **does not** check [`crate::tile_cache::TILED_THRESHOLD`]: HQ without an
/// embedded bootstrap is a rare sync path where quality beats loader latency. Very large sensors
/// may block the loader thread for several seconds — prefer [`load_raw_with_embedded_bootstrap`]
/// when an embedded thumb exists.
pub(crate) fn develop_hq_preview(
    processor: &mut RawProcessor,
    _path: &Path,
    _raw_demosaic_mode: crate::settings::RawDemosaicMode,
    _hdr_target_capacity: f32,
    _hdr_tone_map: HdrToneMapSettings,
    osd_ctx: &RawOsdContext,
    cancel: &crate::loader::DecodeCancelFlag,
) -> Result<RawLoadOutput, String> {
    crate::preload_debug!(
        "[PreloadDebug][RAW] sync HQ develop path={:?} limit={} hdr=true",
        _path.file_name().unwrap_or_default(),
        hq_preview_max_side()
    );

    // High-quality RAW preview always uses the scene-linear HDR pipeline
    // to support exposure adjustments and tone mapping consistently.
    // Stage boundary before LibRaw unpack / dcraw_process.
    crate::loader::check_decode_cancel_str(Some(cancel.as_atomic()))?;
    let (hdr, cpu_ms) = develop_scene_linear_hdr_timed(processor)?;
    crate::loader::check_decode_cancel_str(Some(cancel.as_atomic()))?;
    let (logical_w, logical_h) = processor.developed_output_dimensions();
    let hdr = finalize_raw_hq_hdr_buffer(hdr, logical_w, logical_h)?;
    let fallback = DecodedImage::from_hdr_sdr_fallback(
        hdr.width,
        hdr.height,
        hdr_sdr_fallback_rgba8_or_placeholder(&hdr)?,
    );
    let osd = osd_ctx
        .full_develop(hdr.width, hdr.height, RawDemosaicBackend::Host)
        .with_cpu_demosaic_ms(cpu_ms);
    Ok(RawLoadOutput {
        image: make_hdr_image_data(hdr, fallback),
        osd,
    })
}
