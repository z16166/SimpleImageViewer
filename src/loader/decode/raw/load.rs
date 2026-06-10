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

use super::develop::{develop_full_resolution, develop_hq_preview};
use super::preview::{extract_embedded_preview, raw_embedded_preview_meets_hq_requirement};

use crate::hdr::types::HdrToneMapSettings;
#[cfg(feature = "preload-debug")]
use crate::loader::preview_caps::hq_preview_max_side;
use crate::loader::raw_osd::RawOsdContext;
#[cfg(any(target_os = "windows", target_os = "macos"))]
use crate::loader::raw_osd::RawOsdInfo;
use crate::loader::tiled_sources::{RawHdrRefiningSource, RawImageSource};
use crate::loader::{DecodedImage, ImageData, RawLoadOutput, RefinementRequest};
use crate::raw_processor::RawProcessor;
use crossbeam_channel::Sender;
use parking_lot::RwLock as PLRwLock;
use std::path::PathBuf;
use std::sync::Arc;

use crate::loader::decode::assemble::{make_hdr_image_data, make_image_data};

fn load_raw_hq_static_hdr(
    processor: &mut RawProcessor,
    path: &PathBuf,
    hdr_target_capacity: f32,
    hdr_tone_map: &HdrToneMapSettings,
    osd_ctx: &RawOsdContext,
) -> Option<Result<RawLoadOutput, String>> {
    crate::preload_debug!(
        "[PreloadDebug][RAW] path={:?} hq_static_preview -> StaticHdrToneMap hdr_cap={:.3}",
        path.file_name().unwrap_or_default(),
        hdr_target_capacity
    );
    match processor.develop_scene_linear_hdr() {
        Ok(hdr) => {
            let width = hdr.width;
            let height = hdr.height;
            let fallback_pixels = match crate::loader::hdr_sdr_fallback_rgba8_eager_or_placeholder(
                &hdr,
                hdr_target_capacity,
                hdr_tone_map,
            ) {
                Ok(pixels) => pixels,
                Err(err) => return Some(Err(err)),
            };
            let fallback = DecodedImage::from_arc(hdr.width, hdr.height, fallback_pixels);
            Some(Ok(RawLoadOutput {
                image: make_hdr_image_data(hdr, fallback),
                osd: osd_ctx.full_develop(width, height),
            }))
        }
        Err(err) => {
            log::error!(
                "[Loader] RAW scene-linear HDR develop failed for {:?}: {}. Falling back to embedded SDR preview.",
                path.file_name().unwrap_or_default(),
                err
            );
            None
        }
    }
}

fn load_raw_with_embedded_bootstrap(
    path: PathBuf,
    preview: DecodedImage,
    width: u32,
    height: u32,
    refine_tx: Sender<RefinementRequest>,
    final_lr_flip: i32,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
    osd_ctx: &RawOsdContext,
) -> Result<RawLoadOutput, String> {
    // High-quality RAW preview always uses the scene-linear HDR pipeline
    // to support exposure adjustments and tone mapping consistently.
    let hdr_buffer_slot = Some(Arc::new(PLRwLock::new(None)));

    let bootstrap_w = preview.width;
    let bootstrap_h = preview.height;

    let source = Arc::new(RawImageSource::new(
        path.clone(),
        preview,
        width,
        height,
        refine_tx,
        final_lr_flip,
        true,
        hdr_target_capacity,
        hdr_tone_map,
        hdr_buffer_slot.clone(),
    )?);

    crate::preload_debug!(
        "[PreloadDebug][RAW] TiledBootstrap logical={}x{} refine=true hdr=true hdr_cap={:.3}",
        width,
        height,
        hdr_target_capacity
    );

    let hdr_slot = hdr_buffer_slot.expect("hdr slot when use_hdr");
    let hdr_source = Arc::new(RawHdrRefiningSource::new(hdr_slot, width, height))
        as Arc<dyn crate::hdr::tiled::HdrTiledSource>;
    Ok(RawLoadOutput {
        image: ImageData::HdrTiled {
            hdr: hdr_source,
            fallback: source,
        },
        osd: osd_ctx.hq_bootstrap_dims(bootstrap_w, bootstrap_h),
    })
}

pub(crate) fn load_raw(
    _index: usize,
    _generation: u64,
    path: &PathBuf,
    refine_tx: Sender<RefinementRequest>,
    high_quality: bool,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
) -> Result<RawLoadOutput, String> {
    let mut processor =
        RawProcessor::new().ok_or_else(|| rust_i18n::t!("error.libraw_init").to_string())?;
    if let Err(e) = processor.open(path) {
        log::warn!(
            "[Loader] LibRaw could not open {:?}: {}. Falling back to Rule 2 (WIC/ImageIO).",
            path,
            e
        );
        #[cfg(target_os = "windows")]
        return crate::wic::load_via_wic(path, high_quality, None).map(|image| RawLoadOutput {
            image,
            osd: RawOsdInfo::empty(),
        });
        #[cfg(target_os = "macos")]
        return crate::macos_image_io::load_via_image_io(path, high_quality, None).map(|image| {
            RawLoadOutput {
                image,
                osd: RawOsdInfo::empty(),
            }
        });
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        return Err(format!(
            "LibRaw failed and no platform fallback available: {}",
            e
        ));
    }

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

    let preview_opt = extract_embedded_preview(&mut processor, path, final_orientation);
    let (width, height) = processor.developed_output_dimensions(preview_opt.as_ref());
    let area = width as u64 * height as u64;
    let threshold = crate::tile_cache::TILED_THRESHOLD.load(std::sync::atomic::Ordering::Relaxed);
    let osd_ctx = RawOsdContext::new((width, height), preview_opt.as_ref());

    if !high_quality {
        if let Some(p) = preview_opt {
            crate::preload_debug!(
                "[PreloadDebug][RAW] path={:?} mode=performance embedded={}x{} output={}x{} → StaticSdr",
                path.file_name().unwrap_or_default(),
                p.width,
                p.height,
                width,
                height
            );
            log::debug!(
                "[Loader] Performance mode: embedded preview for {:?} ({}x{}, sensor {}x{})",
                path.file_name().unwrap_or_default(),
                p.width,
                p.height,
                width,
                height
            );
            return Ok(RawLoadOutput {
                image: make_image_data(p.clone()),
                osd: osd_ctx.embedded_render(&p),
            });
        }
        crate::preload_debug!(
            "[PreloadDebug][RAW] path={:?} mode=performance no_embedded output={}x{} → full develop",
            path.file_name().unwrap_or_default(),
            width,
            height
        );
        return develop_full_resolution(
            &mut processor,
            path,
            width,
            height,
            area,
            threshold,
            refine_tx,
            final_lr_flip,
            hdr_target_capacity,
            hdr_tone_map,
            &osd_ctx,
        );
    }

    // High-quality mode: use embedded preview when it already meets HQ requirements.
    if let Some(ref p) = preview_opt {
        if raw_embedded_preview_meets_hq_requirement(p, width, height) {
            if let Some(result) = load_raw_hq_static_hdr(
                &mut processor,
                path,
                hdr_target_capacity,
                &hdr_tone_map,
                &osd_ctx,
            ) {
                return result;
            }
            crate::preload_debug!(
                "[PreloadDebug][RAW] path={:?} mode=hq embedded={}x{} output={}x{} hq_side={} meets_hq=true → StaticSdr",
                path.file_name().unwrap_or_default(),
                p.width,
                p.height,
                width,
                height,
                hq_preview_max_side()
            );
            log::debug!(
                "[Loader] HQ mode: embedded preview meets size requirement for {:?} ({}x{} vs output {}x{})",
                path.file_name().unwrap_or_default(),
                p.width,
                p.height,
                width,
                height
            );
            return Ok(RawLoadOutput {
                image: make_image_data(p.clone()),
                osd: osd_ctx.embedded_render(p),
            });
        }
        crate::preload_debug!(
            "[PreloadDebug][RAW] path={:?} mode=hq embedded={}x{} output={}x{} hq_side={} meets_hq=false → TiledBootstrap+Refine",
            path.file_name().unwrap_or_default(),
            p.width,
            p.height,
            width,
            height,
            hq_preview_max_side()
        );
        log::debug!(
            "[Loader] HQ mode: embedded preview {}x{} insufficient for output {}x{} — HQ demosaic queued",
            p.width,
            p.height,
            width,
            height
        );
    }

    // HQ mode needs demosaic. Bootstrap with embedded preview when available.
    if let Some(p) = preview_opt {
        return load_raw_with_embedded_bootstrap(
            path.clone(),
            p,
            width,
            height,
            refine_tx,
            final_lr_flip,
            hdr_target_capacity,
            hdr_tone_map,
            &osd_ctx,
        );
    }

    crate::preload_debug!(
        "[PreloadDebug][RAW] path={:?} mode=hq no_embedded output={}x{} hq_side={} → sync HQ develop",
        path.file_name().unwrap_or_default(),
        width,
        height,
        hq_preview_max_side()
    );
    develop_hq_preview(
        &mut processor,
        path,
        hdr_target_capacity,
        hdr_tone_map,
        &osd_ctx,
    )
}
