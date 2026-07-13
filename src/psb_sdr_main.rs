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

//! PSD/PSB SDR main-image decode state machine.
//!
//! Drives the flattened-composite -> layer-composite -> P2.5a layer-comp reveal
//! -> P2.5b hidden-layer strategy (heuristic top-N or force-open-all) ->
//! IR-thumbnail fallback (see `decode_psd_sdr_main_from_bytes_with_cancel`) from
//! a single `PsdSectionIndex` structural walk, shared by P1/P2/P3, instead of
//! each stage re-parsing the header/color-mode/image-resources/layer-mask
//! sections on its own. Layer-record parsing is likewise done once and shared
//! by P2/P2.5a/P2.5b.
//!
//! 8-bit documents use the u8 GPU/CPU compositor. 16/32-bit documents (common
//! print RGB/CMYK, and float PSD when the display env is SDR-only) reuse the
//! linear-light f32 layer compositor, then tone-map to RGBA8 at the SDR
//! output boundary so layers-only files are still viewable without an HDR
//! display or IR thumbnail.
//!
//! A structural index-parse failure skips P1 and P2 (there is no verified
//! `image_data_pos`/`lm_start`/`lm_end` to use) and falls back to P3 only via
//! the self-contained (re-walking) thumbnail extractor. Image Data truncation
//! is handled by P1 after the shared index has been built, so P2 can still try
//! the verified layer/mask section.

use crate::psb_section_index::PsdSectionIndex;

#[derive(Debug)]
pub struct PsdMainDecode {
    pub composite: crate::psb_reader::PsbComposite,
    pub osd: crate::loader::PsdOsdInfo,
}

/// SDR main-image state machine: flattened composite -> strict layer composite
/// -> P2.5a layer-comp reveal -> P2.5b hidden-layer strategy -> IR thumbnail -> fail.
///
/// P1 accepts a structurally valid flattened buffer only when it is not an
/// absolute blank (all-alpha-0, or for Gray/RGB also all-RGB-0). P2 accepts a
/// strict-visibility composite only when it is not zero-information (all-alpha-0
/// or solid RGB with variance 0). P2.5a applies IR 1065 Layer Comp visibility
/// when present.
/// P2.5b follows [`crate::settings::PsdHiddenLayerStrategy`]: heuristic top-N
/// max-bbox reveal, or force-open-all drawable leaves. Neither P2.5 path uses
/// fuzzy pixel heuristics.
/// P3 accepts an IR thumbnail under the same zero-information barrier as P2.
/// All barriers are full-buffer SIMD scans.
pub fn decode_psd_sdr_main_from_bytes_with_cancel(
    bytes: &[u8],
    cancel: Option<&std::sync::atomic::AtomicBool>,
    gpu: Option<&crate::psb_layer_blend_gpu::PsdGpuContext>,
    psd_hidden_layer_strategy: crate::settings::PsdHiddenLayerStrategy,
) -> Result<PsdMainDecode, crate::loader::DecodeError> {
    decode_psd_sdr_main_inner(bytes, cancel, gpu, false, psd_hidden_layer_strategy)
}

/// Same as [`decode_psd_sdr_main_from_bytes_with_cancel`], but skips P1 flattened
/// Image Data. Used when an oversized PSB disk-tiled probe already rejected a
/// blank (or unreadable) flat and must not re-decode the full canvas.
pub fn decode_psd_sdr_main_skip_flattened_with_cancel(
    bytes: &[u8],
    cancel: Option<&std::sync::atomic::AtomicBool>,
    gpu: Option<&crate::psb_layer_blend_gpu::PsdGpuContext>,
    psd_hidden_layer_strategy: crate::settings::PsdHiddenLayerStrategy,
) -> Result<PsdMainDecode, crate::loader::DecodeError> {
    decode_psd_sdr_main_inner(bytes, cancel, gpu, true, psd_hidden_layer_strategy)
}

/// Same as [`decode_psd_sdr_main_from_bytes_with_cancel`], but reuses an
/// already-parsed [`PsdSectionIndex`] (e.g. from the raster entry path that
/// probed ICC / disk-tiled compression) instead of walking the file again.
#[allow(dead_code)]
pub fn decode_psd_sdr_main_from_index_with_cancel(
    index: &PsdSectionIndex,
    bytes: &[u8],
    cancel: Option<&std::sync::atomic::AtomicBool>,
    gpu: Option<&crate::psb_layer_blend_gpu::PsdGpuContext>,
    psd_hidden_layer_strategy: crate::settings::PsdHiddenLayerStrategy,
) -> Result<PsdMainDecode, crate::loader::DecodeError> {
    decode_psd_sdr_main_with_index(
        index,
        bytes,
        cancel,
        gpu,
        false,
        psd_hidden_layer_strategy,
        None,
    )
}

/// Same as [`decode_psd_sdr_main_from_index_with_cancel`], but reuses a
/// pre-parsed [`crate::psb_layer_composite::LayerInfo`] (e.g. after an HDR
/// attempt already walked layer records, or a shared raster parse).
pub fn decode_psd_sdr_main_from_index_with_layer_info(
    index: &PsdSectionIndex,
    bytes: &[u8],
    cancel: Option<&std::sync::atomic::AtomicBool>,
    gpu: Option<&crate::psb_layer_blend_gpu::PsdGpuContext>,
    psd_hidden_layer_strategy: crate::settings::PsdHiddenLayerStrategy,
    layer_info: Option<&crate::psb_layer_composite::LayerInfo<'_>>,
) -> Result<PsdMainDecode, crate::loader::DecodeError> {
    decode_psd_sdr_main_with_index(
        index,
        bytes,
        cancel,
        gpu,
        false,
        psd_hidden_layer_strategy,
        layer_info,
    )
}

/// Same as [`decode_psd_sdr_main_skip_flattened_with_cancel`], but reuses an
/// already-parsed [`PsdSectionIndex`].
#[allow(dead_code)]
pub fn decode_psd_sdr_main_skip_flattened_from_index_with_cancel(
    index: &PsdSectionIndex,
    bytes: &[u8],
    cancel: Option<&std::sync::atomic::AtomicBool>,
    gpu: Option<&crate::psb_layer_blend_gpu::PsdGpuContext>,
    psd_hidden_layer_strategy: crate::settings::PsdHiddenLayerStrategy,
) -> Result<PsdMainDecode, crate::loader::DecodeError> {
    decode_psd_sdr_main_with_index(
        index,
        bytes,
        cancel,
        gpu,
        true,
        psd_hidden_layer_strategy,
        None,
    )
}

/// Skip-flattened SDR decode with a shared [`crate::psb_layer_composite::LayerInfo`].
pub fn decode_psd_sdr_main_skip_flattened_from_index_with_layer_info(
    index: &PsdSectionIndex,
    bytes: &[u8],
    cancel: Option<&std::sync::atomic::AtomicBool>,
    gpu: Option<&crate::psb_layer_blend_gpu::PsdGpuContext>,
    psd_hidden_layer_strategy: crate::settings::PsdHiddenLayerStrategy,
    layer_info: Option<&crate::psb_layer_composite::LayerInfo<'_>>,
) -> Result<PsdMainDecode, crate::loader::DecodeError> {
    decode_psd_sdr_main_with_index(
        index,
        bytes,
        cancel,
        gpu,
        true,
        psd_hidden_layer_strategy,
        layer_info,
    )
}

fn decode_psd_sdr_main_inner(
    bytes: &[u8],
    cancel: Option<&std::sync::atomic::AtomicBool>,
    gpu: Option<&crate::psb_layer_blend_gpu::PsdGpuContext>,
    skip_flattened: bool,
    psd_hidden_layer_strategy: crate::settings::PsdHiddenLayerStrategy,
) -> Result<PsdMainDecode, crate::loader::DecodeError> {
    // Single structural walk feeds P1 (image_data_pos), P2 (lm_start/lm_end),
    // and P3 (ir_start/ir_end); every stage below reuses this same index.
    let index = match PsdSectionIndex::parse(bytes) {
        Ok(index) => index,
        Err(e) if e.is_structural() => {
            crate::preload_debug!("[PreloadDebug][PsdSdrMain] stage=P1_fail err={e}");
            log::debug!("PSD SDR main P1 flattened decode failed: {e}");
            // Header/structural failures cannot be recovered by P2; go straight to P3.
            crate::preload_debug!("[PreloadDebug][PsdSdrMain] stage=P1_structural_fail -> skip_P2");
            log::debug!("PSD SDR main: skipping P2 after structural header failure");
            return decode_psd_sdr_main_p3_only(bytes, cancel);
        }
        // Unexpected non-structural index failures still leave no valid shared
        // index to drive P1/P2/P3 from. Fail closed instead of re-walking the
        // file through the legacy self-contained paths.
        Err(e) => return Err(e.into()),
    };
    decode_psd_sdr_main_with_index(
        &index,
        bytes,
        cancel,
        gpu,
        skip_flattened,
        psd_hidden_layer_strategy,
        None,
    )
}

fn decode_psd_sdr_main_with_index(
    index: &PsdSectionIndex,
    bytes: &[u8],
    cancel: Option<&std::sync::atomic::AtomicBool>,
    gpu: Option<&crate::psb_layer_blend_gpu::PsdGpuContext>,
    skip_flattened: bool,
    psd_hidden_layer_strategy: crate::settings::PsdHiddenLayerStrategy,
    preparsed_layers: Option<&crate::psb_layer_composite::LayerInfo<'_>>,
) -> Result<PsdMainDecode, crate::loader::DecodeError> {
    // P1: structurally valid flattened Image Data, then absolute blank barrier.
    if skip_flattened {
        crate::preload_debug!("[PreloadDebug][PsdSdrMain] stage=P1_skipped -> degrade_P2");
        log::debug!(
            "PSD SDR main: skipping P1 flattened (caller already rejected blank/unreadable flat)"
        );
    } else {
        match crate::psb_reader::read_composite_from_index(index, bytes, cancel) {
            Ok(composite) => {
                let absolutely_blank = crate::psb_reader::rgba8_is_absolutely_blank_with_cancel(
                    &composite.pixels,
                    cancel,
                    index.color_mode,
                )?;
                if absolutely_blank {
                    crate::preload_debug!(
                        "[PreloadDebug][PsdSdrMain] stage=P1_absolute_blank {}x{} \
                         pixels={} -> degrade_P2",
                        composite.width,
                        composite.height,
                        composite.pixels.len()
                    );
                    log::debug!(
                        "PSD SDR main: P1 flattened {}x{} is absolute blank \
                         (all-transparent, or Gray/RGB all-RGB-0); degrading to P2",
                        composite.width,
                        composite.height
                    );
                } else {
                    crate::preload_debug!(
                        "[PreloadDebug][PsdSdrMain] stage=P1_flattened {}x{} pixels={}",
                        composite.width,
                        composite.height,
                        composite.pixels.len()
                    );
                    log::debug!(
                        "PSD SDR main: P1 flattened composite {}x{}",
                        composite.width,
                        composite.height
                    );
                    return Ok(PsdMainDecode {
                        composite,
                        osd: crate::loader::PsdOsdInfo::p1_flattened(),
                    });
                }
            }
            Err(e) if e.is_cancelled() => return Err(e),
            Err(e) => {
                crate::preload_debug!("[PreloadDebug][PsdSdrMain] stage=P1_fail err={e}");
                log::debug!("PSD SDR main P1 flattened decode failed: {e}");
            }
        }
    }

    // P1 -> P2: poll cancel after absolute-blank degrade (or P1 fail) before P2 work.
    crate::psb_reader::check_decode_cancel(cancel)?;

    // One layer-record walk shared by P2 / P2.5a / P2.5b. Callers may pass a
    // pre-parsed LayerInfo (HDR->SDR fallback) to skip a second structure walk.
    // `parse_ms` is attributed once to the first composite pass that logs timing
    // (Ok path); later P2.5a/P2.5b attempts pass 0 so preload_debug stays accurate.
    let parse_t0 = std::time::Instant::now();
    let owned_layers = if preparsed_layers.is_some() {
        None
    } else {
        match crate::psb_layer_composite::parse_layer_records_from_index(index, bytes) {
            Ok(info) => Some(info),
            Err(e) => {
                crate::preload_debug!("[PreloadDebug][PsdSdrMain] stage=layer_parse_fail err={e}");
                log::debug!("PSD SDR main layer parse unavailable: {e}");
                None
            }
        }
    };
    let layer_info = preparsed_layers.or(owned_layers.as_ref());
    let mut parse_ms = if preparsed_layers.is_some() {
        0.0
    } else {
        parse_t0.elapsed().as_secs_f64() * 1000.0
    };

    // P2: strict visibility layer composite, then zero-information barrier.
    let mut p2_no_drawable_visible = false;
    // Structural P2.5b failures (e.g. channel length mismatch) must not be
    // reported as "all layers hidden" after P3 also fails.
    let mut p25_reveal_err: Option<crate::loader::DecodeError> = None;
    if let Some(layer_info) = layer_info {
        match composite_sdr_layers_from_info(index, bytes, layer_info, parse_ms, cancel, gpu) {
            Ok(composite) => {
                // Timing log includes parse_ms; do not re-attribute on later stages.
                parse_ms = 0.0;
                let zero_info = crate::psb_reader::rgba8_is_zero_information_with_cancel(
                    &composite.pixels,
                    cancel,
                )?;
                if zero_info {
                    crate::preload_debug!(
                        "[PreloadDebug][PsdSdrMain] stage=P2_zero_information {}x{} \
                         pixels={} -> degrade_P25a",
                        composite.width,
                        composite.height,
                        composite.pixels.len()
                    );
                    log::debug!(
                        "PSD SDR main: P2 strict composite {}x{} is zero-information \
                         (all-transparent or solid RGB); degrading to P2.5a",
                        composite.width,
                        composite.height
                    );
                } else {
                    crate::preload_debug!(
                        "[PreloadDebug][PsdSdrMain] stage=P2_strict_layers {}x{} pixels={}",
                        composite.width,
                        composite.height,
                        composite.pixels.len()
                    );
                    log::debug!(
                        "PSD SDR main: P2 strict layer composite {}x{}",
                        composite.width,
                        composite.height
                    );
                    return Ok(PsdMainDecode {
                        composite,
                        osd: crate::loader::PsdOsdInfo::p2_strict(),
                    });
                }
            }
            Err(e) if e.is_cancelled() => return Err(e),
            Err(e) => {
                p2_no_drawable_visible = e.is_no_drawable_visible_layers();
                crate::preload_debug!("[PreloadDebug][PsdSdrMain] stage=P2_fail err={e}");
                log::debug!("PSD SDR main P2 layer composite unavailable: {e}");
            }
        }

        if let Some(main) =
            decode_psd_sdr_main_p25a(index, bytes, layer_info, &mut parse_ms, cancel, gpu)?
        {
            return Ok(main);
        }
        crate::psb_reader::check_decode_cancel(cancel)?;
        match psd_hidden_layer_strategy {
            crate::settings::PsdHiddenLayerStrategy::Heuristic => {
                if let Some(main) = decode_psd_sdr_main_p25b_heuristic(
                    index,
                    bytes,
                    layer_info,
                    &mut parse_ms,
                    cancel,
                    gpu,
                    &mut p25_reveal_err,
                )? {
                    return Ok(main);
                }
            }
            crate::settings::PsdHiddenLayerStrategy::ShowAllLayers => {
                if let Some(main) = decode_psd_sdr_main_p25b_show_all(
                    index,
                    bytes,
                    layer_info,
                    &mut parse_ms,
                    cancel,
                    gpu,
                    &mut p25_reveal_err,
                )? {
                    return Ok(main);
                }
            }
        }
    }

    // P3: embedded Photoshop IR thumbnail (via the already-parsed index's
    // ir_start/ir_end), then zero-information barrier.
    crate::psb_reader::check_decode_cancel(cancel)?;
    match crate::psb_reader::extract_photoshop_thumbnail_from_ir(
        bytes,
        index.ir_start,
        index.ir_end,
    ) {
        Some(thumb) => {
            if let Some(main) = try_accept_p3_thumbnail(thumb, cancel)? {
                return Ok(main);
            }
        }
        None => {
            crate::preload_debug!("[PreloadDebug][PsdSdrMain] stage=P3_fail no_ir_thumbnail");
            log::debug!("PSD SDR main P3: no embedded IR thumbnail");
        }
    }

    crate::preload_debug!("[PreloadDebug][PsdSdrMain] stage=fail no_p1_p2_p3");
    if let Some(e) = p25_reveal_err {
        return Err(e);
    }
    if p2_no_drawable_visible {
        return Err(rust_i18n::t!("error.psd_all_layers_hidden")
            .to_string()
            .into());
    }
    Err(rust_i18n::t!("error.psd_no_displayable_image")
        .to_string()
        .into())
}

/// Apply the P3 zero-information barrier to an IR thumbnail.
///
/// Returns `Some` when the thumb is displayable, `Ok(None)` when it is
/// zero-information (caller continues / fails), or `Err` on cancel.
fn try_accept_p3_thumbnail(
    thumb: crate::psb_reader::PsbComposite,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<Option<PsdMainDecode>, crate::loader::DecodeError> {
    crate::psb_reader::check_decode_cancel(cancel)?;
    let zero_info =
        crate::psb_reader::rgba8_is_zero_information_with_cancel(&thumb.pixels, cancel)?;
    if zero_info {
        crate::preload_debug!(
            "[PreloadDebug][PsdSdrMain] stage=P3_zero_information {}x{} \
             pixels={} -> fail",
            thumb.width,
            thumb.height,
            thumb.pixels.len()
        );
        log::debug!(
            "PSD SDR main: P3 IR thumbnail {}x{} is zero-information \
             (all-transparent or solid RGB); no displayable image",
            thumb.width,
            thumb.height
        );
        return Ok(None);
    }
    crate::preload_debug!(
        "[PreloadDebug][PsdSdrMain] stage=P3_ir_thumbnail {}x{} pixels={}",
        thumb.width,
        thumb.height,
        thumb.pixels.len()
    );
    log::debug!(
        "PSD SDR main: P3 IR thumbnail {}x{}",
        thumb.width,
        thumb.height
    );
    Ok(Some(PsdMainDecode {
        composite: thumb,
        osd: crate::loader::PsdOsdInfo::p3_ir_thumb(),
    }))
}

/// P3-only path used when [`PsdSectionIndex::parse`] fails structurally: there
/// is no valid index to resolve P1's Image Data or P2's layer/mask sections,
/// so both are skipped entirely. P3 falls back to the self-contained
/// (re-walking) thumbnail extractor since there is no `ir_start`/`ir_end` to
/// reuse from an index that never parsed.
fn decode_psd_sdr_main_p3_only(
    bytes: &[u8],
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<PsdMainDecode, crate::loader::DecodeError> {
    crate::psb_reader::check_decode_cancel(cancel)?;
    match crate::psb_reader::try_extract_photoshop_thumbnail(bytes) {
        Some(thumb) => {
            if let Some(main) = try_accept_p3_thumbnail(thumb, cancel)? {
                return Ok(main);
            }
        }
        None => {
            crate::preload_debug!("[PreloadDebug][PsdSdrMain] stage=P3_fail no_ir_thumbnail");
            log::debug!("PSD SDR main P3: no embedded IR thumbnail");
        }
    }

    crate::preload_debug!("[PreloadDebug][PsdSdrMain] stage=fail no_p1_p2_p3");
    Err(rust_i18n::t!("error.psd_no_displayable_image")
        .to_string()
        .into())
}

fn decode_psd_sdr_main_p25a(
    index: &PsdSectionIndex,
    bytes: &[u8],
    layer_info: &crate::psb_layer_composite::LayerInfo<'_>,
    parse_ms: &mut f64,
    cancel: Option<&std::sync::atomic::AtomicBool>,
    gpu: Option<&crate::psb_layer_blend_gpu::PsdGpuContext>,
) -> Result<Option<PsdMainDecode>, crate::loader::DecodeError> {
    crate::psb_reader::check_decode_cancel(cancel)?;
    let Some(comps) =
        crate::psb_layer_comps::parse_layer_comps_from_ir(bytes, index.ir_start, index.ir_end)
    else {
        crate::preload_debug!("[PreloadDebug][PsdSdrMain] stage=P25a_no_comps");
        log::debug!("PSD SDR main P2.5a: no Layer Comps resource");
        return Ok(None);
    };
    let Some(comp) = crate::psb_layer_comps::select_layer_comp(&comps.comps, comps.last_applied)
    else {
        crate::preload_debug!("[PreloadDebug][PsdSdrMain] stage=P25a_no_selected_comp");
        log::debug!("PSD SDR main P2.5a: Layer Comps list empty after select");
        return Ok(None);
    };
    let comp_id = comp.id;
    let comp_name = if comp.name.is_empty() {
        None
    } else {
        Some(comp.name.clone())
    };

    let visible = crate::psb_layer_comps::visibility_from_layer_comp(&layer_info.records, comp_id);

    match composite_p25_pass(index, bytes, layer_info, &visible, parse_ms, cancel, gpu) {
        Ok(composite) => {
            let zero_info = crate::psb_reader::rgba8_is_zero_information_with_cancel(
                &composite.pixels,
                cancel,
            )?;
            if zero_info {
                crate::preload_debug!(
                    "[PreloadDebug][PsdSdrMain] stage=P25a_zero_information {}x{} \
                     pixels={} -> degrade_P25b",
                    composite.width,
                    composite.height,
                    composite.pixels.len()
                );
                log::debug!(
                    "PSD SDR main: P2.5a layer-comp composite is zero-information; \
                     degrading to P2.5b"
                );
                Ok(None)
            } else {
                crate::preload_debug!(
                    "[PreloadDebug][PsdSdrMain] stage=P25a_layer_comp {}x{} pixels={}",
                    composite.width,
                    composite.height,
                    composite.pixels.len()
                );
                log::debug!(
                    "PSD SDR main: P2.5a layer-comp composite id={comp_id} name={:?}",
                    comp_name.as_deref()
                );
                Ok(Some(PsdMainDecode {
                    composite,
                    osd: crate::loader::PsdOsdInfo::p25a_layer_comp(comp_name),
                }))
            }
        }
        Err(e) if e.is_cancelled() => Err(e),
        Err(e) => {
            crate::preload_debug!("[PreloadDebug][PsdSdrMain] stage=P25a_fail err={e}");
            log::debug!("PSD SDR main P2.5a composite unavailable: {e}");
            Ok(None)
        }
    }
}

fn decode_psd_sdr_main_p25b_heuristic(
    index: &PsdSectionIndex,
    bytes: &[u8],
    layer_info: &crate::psb_layer_composite::LayerInfo<'_>,
    parse_ms: &mut f64,
    cancel: Option<&std::sync::atomic::AtomicBool>,
    gpu: Option<&crate::psb_layer_blend_gpu::PsdGpuContext>,
    reveal_err: &mut Option<crate::loader::DecodeError>,
) -> Result<Option<PsdMainDecode>, crate::loader::DecodeError> {
    crate::psb_reader::check_decode_cancel(cancel)?;
    let candidates = crate::psb_p25_reveal::rank_max_bbox_top_level(
        &layer_info.records,
        crate::psb_p25_reveal::P25B_MAX_CANDIDATES,
    );
    if candidates.is_empty() {
        crate::preload_debug!("[PreloadDebug][PsdSdrMain] stage=P25b_no_candidate");
        log::debug!("PSD SDR main P2.5b: no max-bbox candidate");
        return Ok(None);
    }

    for (cand_i, selection) in candidates.iter().enumerate() {
        let root_name = if selection.root_name.is_empty() {
            None
        } else {
            Some(selection.root_name.clone())
        };
        crate::preload_debug!(
            "[PreloadDebug][PsdSdrMain] stage=P25b_try cand={} root={}",
            cand_i,
            selection.root_name
        );
        log::debug!(
            "PSD SDR main P2.5b try cand={} root={}",
            cand_i,
            selection.root_name
        );

        let visible = crate::psb_p25_reveal::visibility_respect_subtree(
            &layer_info.records,
            &selection.member_indices,
        );
        match composite_p25_pass(index, bytes, layer_info, &visible, parse_ms, cancel, gpu) {
            Ok(composite) => {
                let zero_info = crate::psb_reader::rgba8_is_zero_information_with_cancel(
                    &composite.pixels,
                    cancel,
                )?;
                if !zero_info {
                    crate::preload_debug!(
                        "[PreloadDebug][PsdSdrMain] stage=P25b_max_bbox cand={} {}x{} pixels={}",
                        cand_i,
                        composite.width,
                        composite.height,
                        composite.pixels.len()
                    );
                    log::debug!(
                        "PSD SDR main: P2.5b max-bbox composite {}",
                        selection.root_name
                    );
                    return Ok(Some(PsdMainDecode {
                        composite,
                        osd: crate::loader::PsdOsdInfo::p25b_max_bbox(root_name, false),
                    }));
                }
                crate::preload_debug!(
                    "[PreloadDebug][PsdSdrMain] stage=P25b_zero_information cand={} -> force_open",
                    cand_i
                );
            }
            Err(e) if e.is_cancelled() => return Err(e),
            Err(e) => {
                crate::preload_debug!(
                    "[PreloadDebug][PsdSdrMain] stage=P25b_pass1_fail cand={} err={e}",
                    cand_i
                );
                log::debug!("PSD SDR main P2.5b pass1 fail cand={cand_i}: {e}");
                remember_p25_reveal_err(reveal_err, e);
            }
        }

        let visible = crate::psb_p25_reveal::visibility_force_open_subtree(
            &layer_info.records,
            &selection.member_indices,
        );
        match composite_p25_pass(index, bytes, layer_info, &visible, parse_ms, cancel, gpu) {
            Ok(composite) => {
                let zero_info = crate::psb_reader::rgba8_is_zero_information_with_cancel(
                    &composite.pixels,
                    cancel,
                )?;
                if zero_info {
                    crate::preload_debug!(
                        "[PreloadDebug][PsdSdrMain] stage=P25b_force_open_zero_information \
                         cand={} {}x{} -> next",
                        cand_i,
                        composite.width,
                        composite.height
                    );
                } else {
                    crate::preload_debug!(
                        "[PreloadDebug][PsdSdrMain] stage=P25b_force_open cand={} {}x{} pixels={}",
                        cand_i,
                        composite.width,
                        composite.height,
                        composite.pixels.len()
                    );
                    log::debug!(
                        "PSD SDR main: P2.5b force-open max-bbox composite {}",
                        selection.root_name
                    );
                    return Ok(Some(PsdMainDecode {
                        composite,
                        osd: crate::loader::PsdOsdInfo::p25b_max_bbox(root_name, true),
                    }));
                }
            }
            Err(e) if e.is_cancelled() => return Err(e),
            Err(e) => {
                crate::preload_debug!(
                    "[PreloadDebug][PsdSdrMain] stage=P25b_force_open_fail cand={} err={e}",
                    cand_i
                );
                log::debug!("PSD SDR main P2.5b force-open fail cand={cand_i}: {e}");
                remember_p25_reveal_err(reveal_err, e);
            }
        }
    }

    crate::preload_debug!("[PreloadDebug][PsdSdrMain] stage=P25b_exhausted -> degrade_P3");
    Ok(None)
}

fn decode_psd_sdr_main_p25b_show_all(
    index: &PsdSectionIndex,
    bytes: &[u8],
    layer_info: &crate::psb_layer_composite::LayerInfo<'_>,
    parse_ms: &mut f64,
    cancel: Option<&std::sync::atomic::AtomicBool>,
    gpu: Option<&crate::psb_layer_blend_gpu::PsdGpuContext>,
    reveal_err: &mut Option<crate::loader::DecodeError>,
) -> Result<Option<PsdMainDecode>, crate::loader::DecodeError> {
    crate::psb_reader::check_decode_cancel(cancel)?;
    let visible = crate::psb_p25_reveal::visibility_force_open_all(&layer_info.records);
    crate::preload_debug!(
        "[PreloadDebug][PsdSdrMain] stage=P25b_force_open_all drawable={}",
        visible.iter().filter(|v| **v).count()
    );
    log::debug!(
        "PSD SDR main P2.5b force-open-all drawable={}",
        visible.iter().filter(|v| **v).count()
    );

    match composite_p25_pass(index, bytes, layer_info, &visible, parse_ms, cancel, gpu) {
        Ok(composite) => {
            let zero_info = crate::psb_reader::rgba8_is_zero_information_with_cancel(
                &composite.pixels,
                cancel,
            )?;
            if zero_info {
                crate::preload_debug!(
                    "[PreloadDebug][PsdSdrMain] stage=P25b_force_open_all_zero_information \
                     {}x{} -> degrade_P3",
                    composite.width,
                    composite.height
                );
                Ok(None)
            } else {
                crate::preload_debug!(
                    "[PreloadDebug][PsdSdrMain] stage=P25b_force_open_all {}x{} pixels={}",
                    composite.width,
                    composite.height,
                    composite.pixels.len()
                );
                log::debug!("PSD SDR main: P2.5b force-open-all composite");
                Ok(Some(PsdMainDecode {
                    composite,
                    osd: crate::loader::PsdOsdInfo::p25b_show_all(),
                }))
            }
        }
        Err(e) if e.is_cancelled() => Err(e),
        Err(e) => {
            crate::preload_debug!(
                "[PreloadDebug][PsdSdrMain] stage=P25b_force_open_all_fail err={e}"
            );
            log::debug!("PSD SDR main P2.5b force-open-all unavailable: {e}");
            remember_p25_reveal_err(reveal_err, e);
            Ok(None)
        }
    }
}

/// Keep a P2.5b structural failure for the final error path; ignore blank-layer
/// soft failures so "all layers hidden" can still be reported when appropriate.
fn remember_p25_reveal_err(
    slot: &mut Option<crate::loader::DecodeError>,
    err: crate::loader::DecodeError,
) {
    if !err.is_no_drawable_visible_layers() {
        *slot = Some(err);
    }
}

/// Backdate `total_t0` so `total_ms` includes a shared `parse_ms` measured before
/// this composite pass started (matches `composite_layers_from_bytes` timing).
fn composite_total_t0(parse_ms: f64) -> std::time::Instant {
    let parse_dur = std::time::Duration::from_secs_f64((parse_ms / 1000.0).max(0.0));
    std::time::Instant::now()
        .checked_sub(parse_dur)
        .unwrap_or_else(std::time::Instant::now)
}

/// Strict-visibility composite for the SDR main state machine.
///
/// Depth 8 keeps the existing u8 compositor (optional GPU). Depth 16/32 reuses
/// the HDR f32 layer compositor and tone-maps to RGBA8 so SDR displays can
/// still show layers-only high-bit-depth documents.
fn composite_sdr_layers_from_info(
    index: &PsdSectionIndex,
    bytes: &[u8],
    layer_info: &crate::psb_layer_composite::LayerInfo<'_>,
    parse_ms: f64,
    cancel: Option<&std::sync::atomic::AtomicBool>,
    gpu: Option<&crate::psb_layer_blend_gpu::PsdGpuContext>,
) -> Result<crate::psb_reader::PsbComposite, crate::loader::DecodeError> {
    let visible = crate::psb_layer_composite::compute_effective_visibility(&layer_info.records);
    composite_sdr_layers_with_visibility(index, bytes, layer_info, &visible, parse_ms, cancel, gpu)
}

fn composite_sdr_layers_with_visibility(
    index: &PsdSectionIndex,
    bytes: &[u8],
    layer_info: &crate::psb_layer_composite::LayerInfo<'_>,
    visible: &[bool],
    parse_ms: f64,
    cancel: Option<&std::sync::atomic::AtomicBool>,
    gpu: Option<&crate::psb_layer_blend_gpu::PsdGpuContext>,
) -> Result<crate::psb_reader::PsbComposite, crate::loader::DecodeError> {
    if layer_info.depth == 8 {
        return crate::psb_layer_composite::composite_layers_with_visibility_from_info(
            layer_info,
            visible,
            parse_ms,
            composite_total_t0(parse_ms),
            cancel,
            gpu,
        );
    }
    if layer_info.depth != 16 && layer_info.depth != 32 {
        return Err(format!(
            "PSD/PSB SDR layer composite unsupported depth (found {}-bit)",
            layer_info.depth
        )
        .into());
    }

    let sdr_white = crate::hdr::types::DEFAULT_SDR_WHITE_NITS;
    let hdr = crate::psb_hdr_composite::composite_layers_hdr_with_visibility_from_info(
        layer_info, bytes, index, visible, cancel, sdr_white,
    )?;
    let pixels = crate::hdr::decode::hdr_to_sdr_rgba8(&hdr, 0.0)
        .map_err(|e| format!("PSD SDR deep-bit tone-map failed: {e}"))?;
    Ok(crate::psb_reader::PsbComposite {
        width: hdr.width,
        height: hdr.height,
        pixels,
    })
}

fn composite_p25_pass(
    index: &PsdSectionIndex,
    bytes: &[u8],
    layer_info: &crate::psb_layer_composite::LayerInfo<'_>,
    visible: &[bool],
    parse_ms: &mut f64,
    cancel: Option<&std::sync::atomic::AtomicBool>,
    gpu: Option<&crate::psb_layer_blend_gpu::PsdGpuContext>,
) -> Result<crate::psb_reader::PsbComposite, crate::loader::DecodeError> {
    let attributed_parse_ms = *parse_ms;
    let result = composite_sdr_layers_with_visibility(
        index,
        bytes,
        layer_info,
        visible,
        attributed_parse_ms,
        cancel,
        gpu,
    );
    // Ok paths emit the PsdComposite timing line; clear so later P2.5b candidates
    // do not re-report the shared layer-record parse.
    if result.is_ok() {
        *parse_ms = 0.0;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::{
        PsdSectionIndex, decode_psd_sdr_main_from_bytes_with_cancel,
        decode_psd_sdr_main_skip_flattened_with_cancel,
    };
    use crate::settings::PsdHiddenLayerStrategy;
    use std::path::Path;

    fn tiny_raw_rgb_psd(width: u32, height: u32, rgb_planar: &[u8]) -> Vec<u8> {
        assert_eq!(rgb_planar.len(), (width * height * 3) as usize);
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"8BPS");
        bytes.extend_from_slice(&1u16.to_be_bytes()); // version = PSD
        bytes.extend_from_slice(&[0u8; 6]); // reserved
        bytes.extend_from_slice(&3u16.to_be_bytes()); // channels
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&8u16.to_be_bytes()); // depth
        bytes.extend_from_slice(&3u16.to_be_bytes()); // color mode (RGB)
        bytes.extend_from_slice(&0u32.to_be_bytes()); // color mode data length
        bytes.extend_from_slice(&0u32.to_be_bytes()); // image resources length
        bytes.extend_from_slice(&0u32.to_be_bytes()); // layer and mask info length
        bytes.extend_from_slice(&0u16.to_be_bytes()); // Image Data compression = Raw
        bytes.extend_from_slice(rgb_planar);
        bytes
    }

    fn raw_layer_channel_plane(samples: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0u8, 0u8]; // compression = Raw
        bytes.extend_from_slice(samples);
        bytes
    }

    fn layer_extra_with_name(name: &[u8]) -> Vec<u8> {
        let mut extra = Vec::new();
        extra.extend_from_slice(&0u32.to_be_bytes()); // mask data length
        extra.extend_from_slice(&0u32.to_be_bytes()); // blending ranges length
        extra.push(name.len() as u8);
        extra.extend_from_slice(name);
        while extra.len() % 4 != 0 {
            extra.push(0);
        }
        extra
    }

    /// Hidden single-layer PSD with blank flattened Image Data.
    ///
    /// `r`/`g`/`b` are planar channel samples (length `width * height`). They
    /// must have RGB variance so a successful P2.5b force-open is not rejected
    /// by the zero-information barrier (solid RGB is treated as zero-info).
    fn hidden_single_layer_psd(width: u32, height: u32, r: &[u8], g: &[u8], b: &[u8]) -> Vec<u8> {
        let pixel_count = (width * height) as usize;
        assert_eq!(r.len(), pixel_count);
        assert_eq!(g.len(), pixel_count);
        assert_eq!(b.len(), pixel_count);
        let channels = [
            (0i16, raw_layer_channel_plane(r)),
            (1i16, raw_layer_channel_plane(g)),
            (2i16, raw_layer_channel_plane(b)),
        ];
        let extra = layer_extra_with_name(b"Hidden red");

        let mut layer_record = Vec::new();
        layer_record.extend_from_slice(&0i32.to_be_bytes()); // top
        layer_record.extend_from_slice(&0i32.to_be_bytes()); // left
        layer_record.extend_from_slice(&(height as i32).to_be_bytes()); // bottom
        layer_record.extend_from_slice(&(width as i32).to_be_bytes()); // right
        layer_record.extend_from_slice(&(channels.len() as u16).to_be_bytes());
        for (id, data) in &channels {
            layer_record.extend_from_slice(&id.to_be_bytes());
            layer_record.extend_from_slice(&(data.len() as u32).to_be_bytes());
        }
        layer_record.extend_from_slice(b"8BIM");
        layer_record.extend_from_slice(b"norm");
        layer_record.extend_from_slice(&[255, 0, 2, 0]); // opacity, clipping, hidden flag, filler
        layer_record.extend_from_slice(&(extra.len() as u32).to_be_bytes());
        layer_record.extend_from_slice(&extra);

        let mut layer_info = Vec::new();
        layer_info.extend_from_slice(&1i16.to_be_bytes());
        layer_info.extend_from_slice(&layer_record);
        for (_, data) in &channels {
            layer_info.extend_from_slice(data);
        }

        let mut layer_mask_info = Vec::new();
        layer_mask_info.extend_from_slice(&(layer_info.len() as u32).to_be_bytes());
        layer_mask_info.extend_from_slice(&layer_info);

        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"8BPS");
        bytes.extend_from_slice(&1u16.to_be_bytes()); // version = PSD
        bytes.extend_from_slice(&[0u8; 6]); // reserved
        bytes.extend_from_slice(&3u16.to_be_bytes()); // channels
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&8u16.to_be_bytes()); // depth
        bytes.extend_from_slice(&3u16.to_be_bytes()); // color mode (RGB)
        bytes.extend_from_slice(&0u32.to_be_bytes()); // color mode data length
        bytes.extend_from_slice(&0u32.to_be_bytes()); // image resources length
        bytes.extend_from_slice(&(layer_mask_info.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&layer_mask_info);
        bytes.extend_from_slice(&0u16.to_be_bytes()); // Image Data compression = Raw
        bytes.extend(std::iter::repeat_n(0u8, pixel_count * 3)); // blank flat
        bytes
    }

    #[test]
    fn decode_psd_sdr_main_decodes_tiny_raw_rgb_flattened() {
        let bytes = tiny_raw_rgb_psd(1, 1, &[0x10, 0x20, 0x30]);

        let main = decode_psd_sdr_main_from_bytes_with_cancel(
            &bytes,
            None,
            None,
            PsdHiddenLayerStrategy::Heuristic,
        )
        .expect("tiny raw RGB PSD should decode through P1");

        assert_eq!((main.composite.width, main.composite.height), (1, 1));
        assert_eq!(main.composite.pixels, vec![0x10, 0x20, 0x30, 0xFF]);
        assert_eq!(main.osd, crate::loader::PsdOsdInfo::p1_flattened());
    }

    #[test]
    fn decode_psd_sdr_main_degrades_to_p2_when_image_data_compression_missing() {
        // Header + color-mode + image-resources + layer-mask sections are all
        // present (zero-length), but the file ends right before the 2-byte
        // Image Data compression field. The shared index is still valid for P2
        // because layer/mask offsets are known; only P1 should fail here.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"8BPS");
        bytes.extend_from_slice(&1u16.to_be_bytes()); // version = PSD
        bytes.extend_from_slice(&[0u8; 6]); // reserved
        bytes.extend_from_slice(&3u16.to_be_bytes()); // channels
        bytes.extend_from_slice(&1u32.to_be_bytes()); // height
        bytes.extend_from_slice(&1u32.to_be_bytes()); // width
        bytes.extend_from_slice(&8u16.to_be_bytes()); // depth
        bytes.extend_from_slice(&3u16.to_be_bytes()); // color mode (RGB)
        bytes.extend_from_slice(&0u32.to_be_bytes()); // color mode data length
        bytes.extend_from_slice(&0u32.to_be_bytes()); // image resources length
        bytes.extend_from_slice(&0u32.to_be_bytes()); // layer and mask info length
        // No trailing Image Data compression u16 -- file ends here.

        let index = PsdSectionIndex::parse(&bytes)
            .expect("missing Image Data compression must not reject the shared index");
        assert_eq!(index.image_data_pos, bytes.len() as u64);
        let p1_err = index.image_data_compression(&bytes).unwrap_err();
        assert_eq!(p1_err, "PSD/PSB Image Data compression truncated");

        let err = decode_psd_sdr_main_from_bytes_with_cancel(
            &bytes,
            None,
            None,
            PsdHiddenLayerStrategy::Heuristic,
        )
        .expect_err("P1 failure should degrade to P2/P3 fallback");
        let expected = rust_i18n::t!("error.psd_all_layers_hidden").to_string();
        assert_eq!(err.as_str(), expected);
        assert_ne!(err.as_str(), p1_err);
    }

    #[test]
    fn decode_psd_sdr_main_p25b_heuristic_force_opens_hidden_max_bbox_layer() {
        // Non-uniform RGB so force-open survives the zero-information barrier.
        let bytes = hidden_single_layer_psd(
            2,
            2,
            &[200, 200, 10, 10],
            &[30, 80, 30, 80],
            &[10, 10, 200, 200],
        );

        let main = decode_psd_sdr_main_from_bytes_with_cancel(
            &bytes,
            None,
            None,
            PsdHiddenLayerStrategy::Heuristic,
        )
        .expect("hidden layer should decode through P2.5b heuristic force-open");

        assert_eq!((main.composite.width, main.composite.height), (2, 2));
        assert_eq!(&main.composite.pixels[0..4], &[200, 30, 10, 255]);
        assert_eq!(&main.composite.pixels[4..8], &[200, 80, 10, 255]);
        assert_eq!(
            main.osd,
            crate::loader::PsdOsdInfo::p25b_max_bbox(Some("Hidden red".into()), true)
        );
    }

    #[test]
    fn decode_psd_sdr_main_p25b_show_all_force_opens_hidden_layer() {
        // Non-uniform RGB so force-open survives the zero-information barrier.
        let bytes = hidden_single_layer_psd(
            2,
            2,
            &[200, 200, 10, 10],
            &[30, 80, 30, 80],
            &[10, 10, 200, 200],
        );

        let main = decode_psd_sdr_main_from_bytes_with_cancel(
            &bytes,
            None,
            None,
            PsdHiddenLayerStrategy::ShowAllLayers,
        )
        .expect("hidden layer should decode through P2.5b show-all");

        assert_eq!((main.composite.width, main.composite.height), (2, 2));
        assert_eq!(&main.composite.pixels[0..4], &[200, 30, 10, 255]);
        assert_eq!(&main.composite.pixels[4..8], &[200, 80, 10, 255]);
        assert_eq!(main.osd, crate::loader::PsdOsdInfo::p25b_show_all());
    }

    /// Two hidden layers: large solid-black (zero-info alone) + smaller variegated content.
    /// Bottom-to-top PSD order: large first, then small.
    fn two_hidden_layers_large_blank_small_content_psd() -> Vec<u8> {
        let width = 4u32;
        let height = 4u32;
        let pixel_count = (width * height) as usize;
        let large_r = vec![0u8; pixel_count];
        let large_g = vec![0u8; pixel_count];
        let large_b = vec![0u8; pixel_count];
        let small_r = [200u8, 200, 10, 10];
        let small_g = [30u8, 80, 30, 80];
        let small_b = [10u8, 10, 200, 200];

        fn one_layer(
            name: &[u8],
            top: i32,
            left: i32,
            bottom: i32,
            right: i32,
            r: &[u8],
            g: &[u8],
            b: &[u8],
        ) -> (Vec<u8>, Vec<Vec<u8>>) {
            let channels = [
                (0i16, raw_layer_channel_plane(r)),
                (1i16, raw_layer_channel_plane(g)),
                (2i16, raw_layer_channel_plane(b)),
            ];
            let extra = layer_extra_with_name(name);
            let mut layer_record = Vec::new();
            layer_record.extend_from_slice(&top.to_be_bytes());
            layer_record.extend_from_slice(&left.to_be_bytes());
            layer_record.extend_from_slice(&bottom.to_be_bytes());
            layer_record.extend_from_slice(&right.to_be_bytes());
            layer_record.extend_from_slice(&(channels.len() as u16).to_be_bytes());
            for (id, data) in &channels {
                layer_record.extend_from_slice(&id.to_be_bytes());
                layer_record.extend_from_slice(&(data.len() as u32).to_be_bytes());
            }
            layer_record.extend_from_slice(b"8BIM");
            layer_record.extend_from_slice(b"norm");
            layer_record.extend_from_slice(&[255, 0, 2, 0]);
            layer_record.extend_from_slice(&(extra.len() as u32).to_be_bytes());
            layer_record.extend_from_slice(&extra);
            let channel_data: Vec<Vec<u8>> = channels.into_iter().map(|(_, d)| d).collect();
            (layer_record, channel_data)
        }

        let (large_rec, large_ch) = one_layer(
            b"Large blank",
            0,
            0,
            height as i32,
            width as i32,
            &large_r,
            &large_g,
            &large_b,
        );
        let (small_rec, small_ch) =
            one_layer(b"Small content", 0, 0, 2, 2, &small_r, &small_g, &small_b);

        let mut layer_info = Vec::new();
        layer_info.extend_from_slice(&2i16.to_be_bytes());
        layer_info.extend_from_slice(&large_rec);
        layer_info.extend_from_slice(&small_rec);
        for data in large_ch.iter().chain(small_ch.iter()) {
            layer_info.extend_from_slice(data);
        }

        let mut layer_mask_info = Vec::new();
        layer_mask_info.extend_from_slice(&(layer_info.len() as u32).to_be_bytes());
        layer_mask_info.extend_from_slice(&layer_info);

        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"8BPS");
        bytes.extend_from_slice(&1u16.to_be_bytes());
        bytes.extend_from_slice(&[0u8; 6]);
        bytes.extend_from_slice(&3u16.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&8u16.to_be_bytes());
        bytes.extend_from_slice(&3u16.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&(layer_mask_info.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&layer_mask_info);
        bytes.extend_from_slice(&0u16.to_be_bytes());
        bytes.extend(std::iter::repeat_n(0u8, pixel_count * 3));
        bytes
    }

    #[test]
    fn decode_psd_sdr_main_p25b_heuristic_tries_second_max_bbox_candidate() {
        let bytes = two_hidden_layers_large_blank_small_content_psd();
        let main = decode_psd_sdr_main_from_bytes_with_cancel(
            &bytes,
            None,
            None,
            PsdHiddenLayerStrategy::Heuristic,
        )
        .expect("second candidate should provide content");
        assert_eq!(
            main.osd,
            crate::loader::PsdOsdInfo::p25b_max_bbox(Some("Small content".into()), true)
        );
        assert_ne!(
            main.composite.pixels[0..3],
            main.composite.pixels[4..7],
            "small layer must keep RGB variance"
        );
    }

    #[test]
    fn decode_psd_sdr_main_p25b_show_all_composites_both_hidden_layers() {
        // Both layers are force-opened; the small variegated layer keeps RGB variance.
        let bytes = two_hidden_layers_large_blank_small_content_psd();
        let main = decode_psd_sdr_main_from_bytes_with_cancel(
            &bytes,
            None,
            None,
            PsdHiddenLayerStrategy::ShowAllLayers,
        )
        .expect("show-all should composite both hidden layers");
        assert_eq!(main.osd, crate::loader::PsdOsdInfo::p25b_show_all());
        assert_ne!(
            main.composite.pixels[0..3],
            main.composite.pixels[4..7],
            "composited stack must keep RGB variance from the small layer"
        );
    }

    #[test]
    fn decode_01_02_psd_sdr_main_returns_structurally_valid_image() {
        // Flattened Image Data may be a solid-ish placeholder; under the SDR
        // state machine that is still a valid P1 result (no pixel heuristics).
        let path = Path::new(r"F:\BaiduNetdiskDownload\素材库\45套 psd企业画册模板\12\01-02.psd");
        if !path.is_file() {
            eprintln!("skipping decode_01_02_psd_sdr_main...; sample missing");
            return;
        }
        let bytes = std::fs::read(path).unwrap();
        let main = decode_psd_sdr_main_from_bytes_with_cancel(
            &bytes,
            None,
            None,
            PsdHiddenLayerStrategy::Heuristic,
        )
        .expect("main");
        assert_eq!((main.composite.width, main.composite.height), (5031, 3437));
        assert_eq!(main.composite.pixels.len(), 5031 * 3437 * 4);
    }

    #[test]
    fn decode_psd_sdr_main_1_2_hidden_layers_force_opens_with_trailing_pad() {
        // 1-2.psd has all layers hidden and a few trailing pad bytes after the
        // declared channel block. Previously the strict length check aborted
        // P2.5b and the UI falsely reported "all layers hidden". With pad
        // tolerance, heuristic force-open must yield a displayable composite.
        let path = Path::new(r"F:\BaiduNetdiskDownload\素材库\45套 psd企业画册模板\18\18\1-2.psd");
        if !path.is_file() {
            eprintln!("skipping decode_psd_sdr_main_1_2_hidden...; sample missing");
            return;
        }
        let bytes = std::fs::read(path).expect("read");
        let main = decode_psd_sdr_main_from_bytes_with_cancel(
            &bytes,
            None,
            None,
            PsdHiddenLayerStrategy::Heuristic,
        )
        .expect("hidden layers with trailing channel pad should force-open");
        assert_eq!((main.composite.width, main.composite.height), (6614, 3307));
        assert!(!main.composite.pixels.is_empty());
        assert!(
            matches!(main.osd.stage, crate::loader::PsdDecodeStage::P25b),
            "expected P2.5b reveal, got {:?}",
            main.osd.stage
        );
    }

    #[test]
    fn layer_channel_byte_ranges_tolerates_17_psd_trailing_pad() {
        // 17.psd Layer Info length is even-rounded: declared channel sum is
        // one byte short of the channel block end (a trailing 0x00 pad).
        let path = Path::new(r"F:\BaiduNetdiskDownload\素材库\45套 psd企业画册模板\17\17.psd");
        if !path.is_file() {
            eprintln!("skipping layer_channel_byte_ranges_tolerates_17...; sample missing");
            return;
        }
        let bytes = std::fs::read(path).expect("read");
        let index = PsdSectionIndex::parse(&bytes).expect("index");
        let info = crate::psb_layer_composite::parse_layer_records_from_index(&index, &bytes)
            .expect("layer records");
        let declared: usize = info
            .records
            .iter()
            .flat_map(|r| r.channels.iter())
            .map(|c| c.data_len as usize)
            .sum();
        assert_eq!(
            info.channel_data.len().saturating_sub(declared),
            1,
            "fixture expectation: Adobe even-length pad of 1 byte"
        );
        crate::psb_layer_decode::layer_channel_byte_ranges(&info.records, info.channel_data.len())
            .expect("channel ranges follow declared lengths; Layer Info pad is unused");
    }

    #[test]
    fn decode_psd_sdr_main_prefers_structurally_valid_flattened() {
        // 10.psd has a usable flattened composite -- P1 must win even if layers exist.
        let path = Path::new(r"F:\BaiduNetdiskDownload\素材库\45套 psd企业画册模板\10\10.psd");
        if !path.is_file() {
            eprintln!(
                "skipping decode_psd_sdr_main_prefers_structurally_valid_flattened; sample missing"
            );
            return;
        }
        let bytes = std::fs::read(path).expect("read");
        let flat = crate::psb_reader::read_composite_from_bytes(&bytes).expect("flat");
        let main = decode_psd_sdr_main_from_bytes_with_cancel(
            &bytes,
            None,
            None,
            PsdHiddenLayerStrategy::Heuristic,
        )
        .expect("main");
        assert_eq!(
            (main.composite.width, main.composite.height),
            (flat.width, flat.height)
        );
        assert_eq!(main.composite.pixels, flat.pixels);
    }

    /// Visible RGB layer + blank flat Image Data at the given bit depth.
    ///
    /// The layer covers only a sub-rect so the blank canvas hinterland keeps
    /// RGB/alpha variance (full-canvas solid colour is rejected by the
    /// zero-information barrier, same as 8-bit P2).
    fn visible_rgb_layers_only_psd(width: u32, height: u32, depth: u16) -> Vec<u8> {
        assert!(matches!(depth, 8 | 16 | 32));
        assert!(width >= 2 && height >= 2);
        let canvas_pixels = (width * height) as usize;
        let bps = (depth / 8) as usize;

        let left = 0i32;
        let top = 0i32;
        let right = 1i32; // 1x1 layer at origin
        let bottom = 1i32;
        let lw = (right - left) as u32;
        let lh = (bottom - top) as u32;
        let layer_pixels = (lw * lh) as usize;

        let plane = |sample_bytes: &[u8]| {
            let mut ch = vec![0u8, 0u8]; // Raw compression
            for _ in 0..layer_pixels {
                ch.extend_from_slice(sample_bytes);
            }
            ch
        };

        let (r_plane, g_plane, b_plane, a_plane) = match depth {
            8 => (plane(&[220]), plane(&[40]), plane(&[40]), plane(&[255])),
            16 => (
                plane(&220u16.wrapping_mul(257).to_be_bytes()),
                plane(&40u16.wrapping_mul(257).to_be_bytes()),
                plane(&40u16.wrapping_mul(257).to_be_bytes()),
                plane(&u16::MAX.to_be_bytes()),
            ),
            32 => (
                plane(&1.0f32.to_be_bytes()),
                plane(&(40.0f32 / 255.0).to_be_bytes()),
                plane(&(40.0f32 / 255.0).to_be_bytes()),
                plane(&1.0f32.to_be_bytes()),
            ),
            _ => unreachable!(),
        };

        // Channel order in Photoshop layer data: alpha (-1), R (0), G (1), B (2).
        let channels = [
            (-1i16, a_plane),
            (0i16, r_plane),
            (1i16, g_plane),
            (2i16, b_plane),
        ];
        let extra = layer_extra_with_name(b"Layer0");

        let mut layer_record = Vec::new();
        layer_record.extend_from_slice(&top.to_be_bytes());
        layer_record.extend_from_slice(&left.to_be_bytes());
        layer_record.extend_from_slice(&bottom.to_be_bytes());
        layer_record.extend_from_slice(&right.to_be_bytes());
        layer_record.extend_from_slice(&(channels.len() as u16).to_be_bytes());
        for (id, data) in &channels {
            layer_record.extend_from_slice(&id.to_be_bytes());
            layer_record.extend_from_slice(&(data.len() as u32).to_be_bytes());
        }
        layer_record.extend_from_slice(b"8BIM");
        layer_record.extend_from_slice(b"norm");
        layer_record.extend_from_slice(&[255, 0, 0, 0]); // visible
        layer_record.extend_from_slice(&(extra.len() as u32).to_be_bytes());
        layer_record.extend_from_slice(&extra);

        let mut layer_info = Vec::new();
        layer_info.extend_from_slice(&1i16.to_be_bytes());
        layer_info.extend_from_slice(&layer_record);
        for (_, data) in &channels {
            layer_info.extend_from_slice(data);
        }

        let mut layer_mask_info = Vec::new();
        layer_mask_info.extend_from_slice(&(layer_info.len() as u32).to_be_bytes());
        layer_mask_info.extend_from_slice(&layer_info);

        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"8BPS");
        bytes.extend_from_slice(&1u16.to_be_bytes());
        bytes.extend_from_slice(&[0u8; 6]);
        bytes.extend_from_slice(&3u16.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&depth.to_be_bytes());
        bytes.extend_from_slice(&3u16.to_be_bytes()); // RGB
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&(layer_mask_info.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&layer_mask_info);
        bytes.extend_from_slice(&0u16.to_be_bytes()); // Raw Image Data
        bytes.extend(std::iter::repeat_n(0u8, canvas_pixels * 3 * bps)); // blank flat
        bytes
    }

    #[test]
    fn decode_psd_sdr_main_p2_16bit_rgb_layers_only() {
        let bytes = visible_rgb_layers_only_psd(2, 2, 16);
        let main = decode_psd_sdr_main_from_bytes_with_cancel(
            &bytes,
            None,
            None,
            PsdHiddenLayerStrategy::Heuristic,
        )
        .expect("16-bit RGB layers-only must decode via SDR P2 (f32 composite + tone-map)");
        assert_eq!((main.composite.width, main.composite.height), (2, 2));
        assert_eq!(main.osd, crate::loader::PsdOsdInfo::p2_strict());
        // Red-dominant opaque pixel after linear->sRGB tone-map.
        let px = &main.composite.pixels[0..4];
        assert!(px[0] > px[1] && px[0] > px[2] && px[3] == 255, "got {px:?}");
        // Hinterland stays transparent (layer is only 1x1 at origin).
        assert_eq!(&main.composite.pixels[4..8], &[0, 0, 0, 0]);
    }

    #[test]
    fn decode_psd_sdr_main_p2_32bit_rgb_layers_only() {
        let bytes = visible_rgb_layers_only_psd(2, 2, 32);
        let main = decode_psd_sdr_main_from_bytes_with_cancel(
            &bytes,
            None,
            None,
            PsdHiddenLayerStrategy::Heuristic,
        )
        .expect("32-bit RGB layers-only must decode via SDR P2 even without HDR env");
        assert_eq!((main.composite.width, main.composite.height), (2, 2));
        assert_eq!(main.osd, crate::loader::PsdOsdInfo::p2_strict());
        let px = &main.composite.pixels[0..4];
        assert!(px[0] > px[1] && px[0] > px[2] && px[3] == 255, "got {px:?}");
        assert_eq!(&main.composite.pixels[4..8], &[0, 0, 0, 0]);
    }

    /// Visible CMYK layer (1x1) + blank flat at 16-bit -- common print layers-only case.
    fn visible_cmyk16_layers_only_psd(width: u32, height: u32) -> Vec<u8> {
        assert!(width >= 2 && height >= 2);
        let canvas_pixels = (width * height) as usize;
        let left = 0i32;
        let top = 0i32;
        let right = 1i32;
        let bottom = 1i32;
        let layer_pixels = 1usize;

        let plane = |v: u16| {
            let mut ch = vec![0u8, 0u8];
            for _ in 0..layer_pixels {
                ch.extend_from_slice(&v.to_be_bytes());
            }
            ch
        };
        // Adobe polarity: 0 = full ink. Strong cyan, little M/Y/K.
        let channels = [
            (0i16, plane(0)),                  // C full
            (1i16, plane(u16::MAX)),           // M none
            (2i16, plane(u16::MAX)),           // Y none
            (3i16, plane((u16::MAX / 4) * 3)), // K light
        ];
        let extra = layer_extra_with_name(b"Cyan");

        let mut layer_record = Vec::new();
        layer_record.extend_from_slice(&top.to_be_bytes());
        layer_record.extend_from_slice(&left.to_be_bytes());
        layer_record.extend_from_slice(&bottom.to_be_bytes());
        layer_record.extend_from_slice(&right.to_be_bytes());
        layer_record.extend_from_slice(&(channels.len() as u16).to_be_bytes());
        for (id, data) in &channels {
            layer_record.extend_from_slice(&id.to_be_bytes());
            layer_record.extend_from_slice(&(data.len() as u32).to_be_bytes());
        }
        layer_record.extend_from_slice(b"8BIM");
        layer_record.extend_from_slice(b"norm");
        layer_record.extend_from_slice(&[255, 0, 0, 0]);
        layer_record.extend_from_slice(&(extra.len() as u32).to_be_bytes());
        layer_record.extend_from_slice(&extra);

        let mut layer_info = Vec::new();
        layer_info.extend_from_slice(&1i16.to_be_bytes());
        layer_info.extend_from_slice(&layer_record);
        for (_, data) in &channels {
            layer_info.extend_from_slice(data);
        }

        let mut layer_mask_info = Vec::new();
        layer_mask_info.extend_from_slice(&(layer_info.len() as u32).to_be_bytes());
        layer_mask_info.extend_from_slice(&layer_info);

        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"8BPS");
        bytes.extend_from_slice(&1u16.to_be_bytes());
        bytes.extend_from_slice(&[0u8; 6]);
        bytes.extend_from_slice(&4u16.to_be_bytes()); // CMYK channels
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&16u16.to_be_bytes());
        bytes.extend_from_slice(&4u16.to_be_bytes()); // CMYK
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&(layer_mask_info.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&layer_mask_info);
        bytes.extend_from_slice(&0u16.to_be_bytes());
        // Blank CMYK flat: 0 ink (255-equivalent = max = no ink) as zeros still
        // downconverts to black RGB which P1 treats as absolute blank for...
        // actually color_mode 4 may differ. Use all-zero planes (full ink black).
        bytes.extend(std::iter::repeat_n(0u8, canvas_pixels * 4 * 2));
        bytes
    }

    #[test]
    fn decode_psd_sdr_main_p2_16bit_cmyk_layers_only() {
        // CMYK flats are rarely "absolute blank" (no RGB-0 rule), so skip P1
        // to exercise the layers-only deep-bit P2 path directly.
        let bytes = visible_cmyk16_layers_only_psd(2, 2);
        let main = decode_psd_sdr_main_skip_flattened_with_cancel(
            &bytes,
            None,
            None,
            PsdHiddenLayerStrategy::Heuristic,
        )
        .expect("16-bit CMYK layers-only must decode via SDR P2");
        assert_eq!((main.composite.width, main.composite.height), (2, 2));
        assert_eq!(main.osd, crate::loader::PsdOsdInfo::p2_strict());
        // Cyan-ish: G and B should dominate R after the device approximation.
        let px = &main.composite.pixels[0..4];
        assert!(px[3] == 255, "got {px:?}");
        assert!(
            px[1] > 32 || px[2] > 32,
            "expected cyan-ish/non-black pixel, got {px:?}"
        );
    }
}
