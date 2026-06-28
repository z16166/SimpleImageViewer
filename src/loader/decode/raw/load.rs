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

use super::develop::{develop_full_resolution, develop_hq_preview, develop_scene_linear_hdr_timed};
use super::preview::{extract_embedded_preview, raw_embedded_preview_meets_hq_requirement};

use crate::hdr::types::HdrToneMapSettings;
const GPU_DEMOSAIC_MAX_DIMENSION: u32 = crate::constants::ABSOLUTE_MAX_TEXTURE_SIDE;
use crate::loader::RawOsdInfo;
#[cfg(feature = "preload-debug")]
use crate::loader::preview_caps::hq_preview_max_side;
use crate::loader::raw_osd::RawDemosaicBackend;
use crate::loader::raw_osd::RawOsdContext;

use crate::loader::tiled_sources::{RawHdrRefiningSource, RawImageSource};
use crate::loader::{
    DecodeProfile, DecodedImage, ImageData, LoaderOutput, PreviewBundle, PreviewResult,
    RawLoadOutput, RefinementRequest, source_key_for_path,
};
use crate::raw_processor::RawProcessor;
use crossbeam_channel::Sender;
use parking_lot::RwLock as PLRwLock;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::loader::decode::assemble::{make_hdr_image_data, make_image_data};
use crate::loader::orchestrator::{RawOpenPhaseTimings, RawOpenPrefetch};

pub(crate) fn open_raw_processor_with_preview(
    path: &Path,
) -> Result<(RawProcessor, Option<DecodedImage>, RawOpenPhaseTimings, i32), String> {
    let open_started = std::time::Instant::now();
    let mut processor =
        RawProcessor::new().ok_or_else(|| rust_i18n::t!("error.libraw_init").to_string())?;
    processor.open(path)?;
    let open_ms = crate::loader::elapsed_ms_u32(open_started);

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

    let thumb_started = std::time::Instant::now();
    let preview_opt = extract_embedded_preview(&mut processor, path, final_orientation);
    let thumb_ms = crate::loader::elapsed_ms_u32(thumb_started);

    Ok((
        processor,
        preview_opt,
        RawOpenPhaseTimings { open_ms, thumb_ms },
        final_lr_flip,
    ))
}

fn load_raw_hq_static_hdr(
    processor: &mut RawProcessor,
    path: &Path,
    hdr_target_capacity: f32,
    hdr_tone_map: &HdrToneMapSettings,
    osd_ctx: &RawOsdContext,
) -> Option<Result<RawLoadOutput, String>> {
    crate::preload_debug!(
        "[PreloadDebug][RAW] path={:?} hq_static_preview -> StaticHdrToneMap hdr_cap={:.3}",
        path.file_name().unwrap_or_default(),
        hdr_target_capacity
    );
    match develop_scene_linear_hdr_timed(processor) {
        Ok((hdr, cpu_ms)) => {
            let width = hdr.width;
            let height = hdr.height;
            let fallback_pixels = match crate::loader::hdr_sdr_fallback_rgba8_eager_or_placeholder(
                &hdr,
                hdr_target_capacity,
                hdr_tone_map,
            ) {
                Ok(fb) => fb,
                Err(err) => return Some(Err(err)),
            };
            let fallback =
                DecodedImage::from_hdr_sdr_fallback(hdr.width, hdr.height, fallback_pixels);
            Some(Ok(RawLoadOutput {
                image: make_hdr_image_data(hdr, fallback),
                osd: osd_ctx
                    .full_develop(width, height, RawDemosaicBackend::Host)
                    .with_cpu_demosaic_ms(cpu_ms),
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
    let hdr_buffer_slot = Arc::new(PLRwLock::new(None));

    let bootstrap_w = preview.width;
    let bootstrap_h = preview.height;

    let source = Arc::new(RawImageSource::new(
            path.to_path_buf(),
        preview,
        width,
        height,
        refine_tx,
        final_lr_flip,
        true,
        hdr_target_capacity,
        hdr_tone_map,
        Some(Arc::clone(&hdr_buffer_slot)),
    )?);

    crate::preload_debug!(
        "[PreloadDebug][RAW] TiledBootstrap logical={}x{} refine=true hdr=true hdr_cap={:.3}",
        width,
        height,
        hdr_target_capacity
    );

    let hdr_source = Arc::new(RawHdrRefiningSource::new(hdr_buffer_slot, width, height))
        as Arc<dyn crate::hdr::tiled::HdrTiledSource>;
    Ok(RawLoadOutput {
        image: ImageData::HdrTiled {
            hdr: hdr_source,
            fallback: source,
        },
        osd: osd_ctx.hq_bootstrap_dims(bootstrap_w, bootstrap_h),
    })
}

pub(crate) const RAW_HQ_BOOTSTRAP_PREVIEW: bool = true;

fn emit_raw_hq_bootstrap_preview(
    load_tx: &crate::loader::orchestrator::LoaderOutputSender,
    index: usize,
    path: &Path,
    preview: &DecodedImage,
    decode_profile: DecodeProfile,
    raw_bootstrap_osd: Option<RawOsdInfo>,
    #[cfg_attr(not(feature = "preload-debug"), allow(unused_variables))] log_tag: &str,
) {
    crate::preload_debug!(
        "[PreloadDebug][RAW-{}] bootstrap preview early idx={} {}x{} path={:?}",
        log_tag,
        index,
        preview.width,
        preview.height,
        path.file_name().unwrap_or_default()
    );
    let _ = load_tx.send(LoaderOutput::Preview(PreviewResult {
        index,
        decode_profile,
        source_key: source_key_for_path(path),
        preview_bundle: PreviewBundle::refined().with_sdr(preview.clone()),
        error: None,
        cpu_demosaic_ms: None,
        raw_bootstrap_osd,
    }));
}

pub(crate) fn load_raw(
    index: usize,
    path: &Path,
    refine_tx: Sender<RefinementRequest>,
    load_tx: crate::loader::orchestrator::LoaderOutputSender,
    decode_profile: DecodeProfile,
    high_quality: bool,
    raw_demosaic_mode: crate::settings::RawDemosaicMode,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
    raw_open_prefetch: Option<&RawOpenPrefetch>,
) -> Result<RawLoadOutput, String> {
    let (mut processor, preview_opt, open_timings, final_lr_flip, prefetched) = if let Some(
        session,
    ) =
        raw_open_prefetch.and_then(|cache| cache.take_or_wait(path))
    {
        let final_lr_flip = session.final_lr_flip;
        (
            session.processor,
            session.preview,
            session.timings,
            final_lr_flip,
            true,
        )
    } else {
        match open_raw_processor_with_preview(path) {
            Ok((processor, preview, timings, final_lr_flip)) => {
                (processor, preview, timings, final_lr_flip, false)
            }
            Err(e) => {
                log::warn!("[Loader] LibRaw could not open {:?}: {}.", path, e);
                if high_quality {
                    return Err(format!(
                        "{} (high-quality RAW requires LibRaw; rebuild vcpkg libraw or check the file)",
                        e
                    ));
                }
                log::warn!(
                    "[Loader] Falling back to Rule 2 (WIC/ImageIO) for performance-mode RAW."
                );
                #[cfg(target_os = "windows")]
                return crate::wic::load_via_wic(path, high_quality, None).map(|image| {
                    RawLoadOutput {
                        image,
                        osd: RawOsdInfo::empty(),
                    }
                });
                #[cfg(target_os = "macos")]
                return crate::macos_image_io::load_via_image_io(path, high_quality, None).map(
                    |image| RawLoadOutput {
                        image,
                        osd: RawOsdInfo::empty(),
                    },
                );
                #[cfg(not(any(target_os = "windows", target_os = "macos")))]
                return Err(format!(
                    "LibRaw failed and no platform fallback available: {}",
                    e
                ));
            }
        }
    };

    let _ = (open_timings.open_ms, open_timings.thumb_ms, prefetched);
    #[cfg(feature = "preload-debug")]
    crate::preload_debug!(
        "[PreloadDebug][RAW] open phases idx={} open_ms={} thumb_ms={} prefetched={} path={:?}",
        index,
        open_timings.open_ms,
        open_timings.thumb_ms,
        prefetched,
        path.file_name().unwrap_or_default()
    );

    let (width, height) = {
        if high_quality {
            processor.unpack()?;
        }
        processor.developed_output_dimensions()
    };
    let area = width as u64 * height as u64;
    let threshold = crate::tile_cache::TILED_THRESHOLD.load(std::sync::atomic::Ordering::Relaxed);
    let osd_ctx = RawOsdContext::new(
        (processor.raw_width(), processor.raw_height()),
        preview_opt.as_ref(),
    );

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
            let osd = osd_ctx.embedded_render(&p);
            return Ok(RawLoadOutput {
                image: make_image_data(p),
                osd,
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
            raw_demosaic_mode,
            hdr_target_capacity,
            hdr_tone_map,
            &osd_ctx,
        );
    }

    // GPU demosaic is subject to several conditions:
    // 1. high_quality must be enabled.
    // 2. raw_demosaic_mode must be set to Gpu.
    // 3. final_lr_flip must be 0 (GPU demosaic does not support orientation flip; rotation should be handled in display shader).
    // 4. The raw processor must report compatibility with the GPU demosaic Bayer pattern.
    // 5. Image dimensions must not exceed the maximum GPU texture dimension (GPU_DEMOSAIC_MAX_DIMENSION) to prevent rendering failures.
    // 6. Image area must be under the threshold to avoid exceeding GPU memory budgets.
    // 7. Device/backend must support RAW demosaic compute (set at app startup).
    let use_gpu_demosaic = high_quality
        && raw_demosaic_mode == crate::settings::RawDemosaicMode::Gpu
        && crate::loader::GPU_DEMOSAIC_SUPPORTED.load(std::sync::atomic::Ordering::Relaxed)
        && final_lr_flip == 0
        && processor.is_gpu_demosaic_compatible()
        && width <= GPU_DEMOSAIC_MAX_DIMENSION
        && height <= GPU_DEMOSAIC_MAX_DIMENSION
        && area < threshold;

    if high_quality
        && raw_demosaic_mode == crate::settings::RawDemosaicMode::Gpu
        && !use_gpu_demosaic
        && !processor.is_gpu_demosaic_compatible()
    {
        log::debug!(
            "[Loader] GPU demosaic skipped for {:?}: CFA not compatible with GPU Bayer demosaic (e.g. Fuji X-Trans/Super-CCD or non-square pixels); using CPU tiled develop",
            path.file_name().unwrap_or_default()
        );
    }

    if use_gpu_demosaic {
        if RAW_HQ_BOOTSTRAP_PREVIEW && let Some(ref p) = preview_opt {
            emit_raw_hq_bootstrap_preview(
                &load_tx,
                index,
                path,
                p,
                decode_profile.clone(),
                Some(osd_ctx.gpu_bootstrap_dims(p.width, p.height)),
                "GPU",
            );
        }
        let extract_started = std::time::Instant::now();
        match processor.extract_raw_gpu_source(crate::settings::RawDemosaicMethod::Ppg) {
            Ok(mut raw_gpu_source) => {
                let extract_ms = crate::loader::elapsed_ms_u32(extract_started);
                // scene_color_scale stays [1,1,1]: linear baseline matches CPU develop (no auto_bright).
                log::debug!(
                    "[Loader] RAW GPU load {:?}: extract={extract_ms}ms (linear scene scale; demosaic on GPU)",
                    path.file_name().unwrap_or_default()
                );
                #[cfg(feature = "preload-debug")]
                crate::preload_debug!(
                    "[PreloadDebug][RAW-GPU] load {:?} extract={extract_ms}ms demosaic_pending",
                    path.file_name().unwrap_or_default(),
                );
                raw_gpu_source.bootstrap_preview = preview_opt.clone();
                let mut metadata = crate::raw_processor::raw_scene_linear_metadata();
                metadata.raw_gpu_source = Some(raw_gpu_source);

                let hdr = crate::hdr::types::HdrImageBuffer {
                    width,
                    height,
                    format: crate::hdr::types::HdrPixelFormat::Rgba32Float,
                    color_space: metadata.color_space_hint(),
                    metadata,
                    rgba_f32: std::sync::Arc::new(Vec::new()),
                };

                let fallback = if RAW_HQ_BOOTSTRAP_PREVIEW && let Some(ref p) = preview_opt {
                    p.clone()
                } else {
                    let fallback_pixels =
                        crate::loader::cheap_hdr_sdr_placeholder_rgba8(width, height)?;
                    DecodedImage::from_arc_sdr_deferred_placeholder(
                        width,
                        height,
                        std::sync::Arc::new(fallback_pixels),
                    )
                };

                let mut osd = if RAW_HQ_BOOTSTRAP_PREVIEW
                    && let Some(p) = preview_opt.as_ref()
                {
                    osd_ctx.gpu_bootstrap_dims(p.width, p.height)
                } else {
                    osd_ctx.full_develop(width, height, RawDemosaicBackend::Video)
                };
                osd = osd.with_gpu_extract_ms(extract_ms);
                return Ok(RawLoadOutput {
                    image: make_hdr_image_data(hdr, fallback),
                    osd,
                });
            }
            Err(err) => {
                log::error!(
                    "[Loader] GPU raw extract failed: {}. Falling back to CPU.",
                    err
                );
            }
        }
    }

    // High-quality mode: try a fast synchronous full develop when the embedded thumb is already
    // large enough, but never treat the embedded JPEG as the final image — demosaic (CPU or GPU)
    // must still run via bootstrap/refine when develop fails or the user chose a demosaic backend.
    if let Some(ref p) = preview_opt {
        if raw_embedded_preview_meets_hq_requirement(p, width, height) {
            if RAW_HQ_BOOTSTRAP_PREVIEW {
                emit_raw_hq_bootstrap_preview(
                    &load_tx,
                    index,
                    path,
                    p,
                    decode_profile.clone(),
                    Some(osd_ctx.hq_bootstrap_dims(p.width, p.height)),
                    "CPU",
                );
            }
            if let Some(result) = load_raw_hq_static_hdr(
                &mut processor,
                path,
                hdr_target_capacity,
                &hdr_tone_map,
                &osd_ctx,
            ) {
                return result;
            }
            log::debug!(
                "[Loader] HQ mode: embedded preview {}x{} meets size cap but full develop failed for {:?} — queued demosaic refine",
                p.width,
                p.height,
                path.file_name().unwrap_or_default()
            );
        } else {
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
    }

    // HQ mode needs demosaic. Bootstrap with embedded preview when available.
    if let Some(p) = preview_opt {
        return load_raw_with_embedded_bootstrap(
            path.to_path_buf(),
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
        raw_demosaic_mode,
        hdr_target_capacity,
        hdr_tone_map,
        &osd_ctx,
    )
}
