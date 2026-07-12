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

//! PSD/PSB HDR main-image decode state machine (Approach A).
//!
//! P1 flattened HDR -> P2 linear-light layer composite -> P2.5a Layer Comp
//! reveal -> P2.5b hidden-layer strategy (heuristic top-N or force-open-all).
//! Hard failure falls through to [`crate::psb_sdr_main`] (full SDR P1/P2/P3).
//! Note: PSD/PSB do not embed an IR JPEG suitable as an HDR recovery preview;
//! P3 on the SDR path may use the 8-bit IR thumbnail, but the HDR state machine
//! itself never synthesizes an IR JPEG fallback.
//!
//! P1 and P2 intentionally use different blank barriers. P1 rejects only an
//! absolute blank, while P2 rejects any zero-information solid composite.
//! Therefore, a solid white or gray flattened Image Data preview may remain at
//! P1 by design even when it looks like a placeholder and layers differ.

use crate::hdr::types::{HdrImageBuffer, HdrToneMapSettings};
use crate::psb_hdr_composite::composite_layers_hdr_with_visibility_from_info;
use crate::psb_hdr_flat::{
    read_composite_hdr_from_index, rgba_f32_is_absolutely_blank_with_cancel,
};
use crate::psb_icc_hdr::{
    log_16bit_transfer_assumption, probe_icc_hdr, psd_content_wants_hdr, psd_env_wants_hdr,
    transfer_assumption_uncertain,
};
use crate::psb_reader::extract_icc_profile_from_ir;
use crate::psb_section_index::PsdSectionIndex;

#[derive(Debug)]
pub struct PsdHdrMainDecode {
    pub hdr: HdrImageBuffer,
    pub osd: crate::loader::PsdOsdInfo,
}

/// True when both environment and content gates select the HDR path.
pub fn psd_should_try_hdr(
    depth: u16,
    embedded_icc: Option<&[u8]>,
    hdr_target_capacity: f32,
) -> bool {
    psd_env_wants_hdr(hdr_target_capacity) && psd_content_wants_hdr(depth, embedded_icc)
}

/// HDR main-image: P1 flattened -> P2 layer composite -> P2.5a/b. No P3 HDR.
///
/// P1 accepts a structurally valid flattened buffer only when it is not an
/// absolute blank (all-alpha-0, or for Gray/RGB also all-RGB-0). P2 accepts a
/// strict-visibility composite only when it is not zero-information (all-alpha-0
/// or solid RGB with variance 0). Solid white or gray flats may therefore stay
/// at P1 by design when flattened Image Data is a placeholder-looking solid
/// that is not absolute blank. P2.5a applies IR 1065 Layer Comp visibility when
/// present. P2.5b follows [`crate::settings::PsdHiddenLayerStrategy`]:
/// heuristic top-N max-bbox reveal, or force-open-all drawable leaves. Neither
/// P2.5 path uses fuzzy pixel heuristics. All barriers are full-buffer SIMD
/// scans.
///
/// `skip_flattened`: when an oversized PSB disk-tiled probe already rejected a
/// blank flat, skip P1 and go straight to P2 HDR.
pub fn decode_psd_hdr_main_from_bytes_with_cancel(
    bytes: &[u8],
    cancel: Option<&std::sync::atomic::AtomicBool>,
    tone: &HdrToneMapSettings,
    skip_flattened: bool,
    psd_hidden_layer_strategy: crate::settings::PsdHiddenLayerStrategy,
) -> Result<PsdHdrMainDecode, crate::loader::DecodeError> {
    let index = PsdSectionIndex::parse(bytes)?;
    decode_psd_hdr_main_from_index_with_cancel(
        &index,
        bytes,
        cancel,
        tone,
        skip_flattened,
        psd_hidden_layer_strategy,
    )
}

/// Same as [`decode_psd_hdr_main_from_bytes_with_cancel`], but reuses an
/// already-parsed [`PsdSectionIndex`].
pub fn decode_psd_hdr_main_from_index_with_cancel(
    index: &PsdSectionIndex,
    bytes: &[u8],
    cancel: Option<&std::sync::atomic::AtomicBool>,
    tone: &HdrToneMapSettings,
    skip_flattened: bool,
    psd_hidden_layer_strategy: crate::settings::PsdHiddenLayerStrategy,
) -> Result<PsdHdrMainDecode, crate::loader::DecodeError> {
    decode_psd_hdr_main_from_index_with_layer_info(
        index,
        bytes,
        cancel,
        tone,
        skip_flattened,
        psd_hidden_layer_strategy,
        None,
    )
}

/// Same as [`decode_psd_hdr_main_from_index_with_cancel`], with an optional
/// shared [`crate::psb_layer_composite::LayerInfo`] to avoid a second layer-record walk
/// when the caller also runs the SDR fallback path.
pub fn decode_psd_hdr_main_from_index_with_layer_info(
    index: &PsdSectionIndex,
    bytes: &[u8],
    cancel: Option<&std::sync::atomic::AtomicBool>,
    tone: &HdrToneMapSettings,
    skip_flattened: bool,
    psd_hidden_layer_strategy: crate::settings::PsdHiddenLayerStrategy,
    preparsed_layers: Option<&crate::psb_layer_composite::LayerInfo<'_>>,
) -> Result<PsdHdrMainDecode, crate::loader::DecodeError> {
    let embedded_icc = extract_icc_profile_from_ir(bytes, index.ir_start, index.ir_end);
    if !psd_content_wants_hdr(index.depth, embedded_icc.as_deref()) {
        return Err(crate::loader::DecodeError::PsdHdrNotWanted);
    }

    let icc_probe = embedded_icc
        .as_deref()
        .map(probe_icc_hdr)
        .unwrap_or_default();
    log_16bit_transfer_assumption(&icc_probe, index.depth);
    let mark_transfer = |osd: crate::loader::PsdOsdInfo| {
        if transfer_assumption_uncertain(&icc_probe, index.depth) {
            osd.with_transfer_uncertain()
        } else {
            osd
        }
    };

    let sdr_white = tone.sdr_white_nits.max(1.0);

    if !skip_flattened {
        crate::psb_reader::check_decode_cancel(cancel)?;
        match read_composite_hdr_from_index(index, bytes, cancel, sdr_white) {
            Ok(hdr) => {
                if rgba_f32_is_absolutely_blank_with_cancel(&hdr.rgba_f32, cancel)? {
                    crate::preload_debug!(
                        "[PreloadDebug][PsdHdrMain] stage=P1_absolute_blank {}x{} -> degrade_P2",
                        hdr.width,
                        hdr.height
                    );
                    log::debug!(
                        "PSD HDR main: P1 flattened {}x{} is absolute blank; degrading to P2",
                        hdr.width,
                        hdr.height
                    );
                } else {
                    crate::preload_debug!(
                        "[PreloadDebug][PsdHdrMain] stage=P1_flattened {}x{}",
                        hdr.width,
                        hdr.height
                    );
                    return Ok(PsdHdrMainDecode {
                        hdr,
                        osd: mark_transfer(crate::loader::PsdOsdInfo::p1_flattened()),
                    });
                }
            }
            Err(e) if e.is_cancelled() => return Err(e),
            Err(e) => {
                crate::preload_debug!("[PreloadDebug][PsdHdrMain] stage=P1_fail err={e}");
                log::debug!("PSD HDR main P1 flattened decode failed: {e}");
            }
        }
    } else {
        crate::preload_debug!("[PreloadDebug][PsdHdrMain] stage=P1_skipped -> degrade_P2");
    }

    crate::psb_reader::check_decode_cancel(cancel)?;

    // One layer-record walk shared by P2 / P2.5a / P2.5b (or caller-provided).
    let owned_layers = if preparsed_layers.is_some() {
        None
    } else {
        match crate::psb_layer_composite::parse_layer_records_from_index(index, bytes) {
            Ok(info) => Some(info),
            Err(e) => {
                crate::preload_debug!("[PreloadDebug][PsdHdrMain] stage=layer_parse_fail err={e}");
                log::debug!("PSD HDR main layer parse unavailable: {e}");
                None
            }
        }
    };
    let layer_info = preparsed_layers.or(owned_layers.as_ref());

    let mut p2_no_drawable_visible = false;
    if let Some(layer_info) = layer_info {
        let visible = crate::psb_layer_composite::compute_effective_visibility(&layer_info.records);
        match composite_layers_hdr_with_visibility_from_info(
            layer_info, bytes, index, &visible, cancel, sdr_white,
        ) {
            Ok(hdr) => {
                if rgba_f32_is_absolutely_blank_with_cancel(&hdr.rgba_f32, cancel)?
                    || rgba_f32_is_zero_information_with_cancel(&hdr.rgba_f32, cancel)?
                {
                    crate::preload_debug!(
                        "[PreloadDebug][PsdHdrMain] stage=P2_zero_information {}x{} -> degrade_P25a",
                        hdr.width,
                        hdr.height
                    );
                } else {
                    crate::preload_debug!(
                        "[PreloadDebug][PsdHdrMain] stage=P2_strict_layers {}x{}",
                        hdr.width,
                        hdr.height
                    );
                    return Ok(PsdHdrMainDecode {
                        hdr,
                        osd: mark_transfer(crate::loader::PsdOsdInfo::p2_strict()),
                    });
                }
            }
            Err(e) if e.is_cancelled() => return Err(e),
            Err(e) => {
                p2_no_drawable_visible = e.is_no_drawable_visible_layers();
                crate::preload_debug!("[PreloadDebug][PsdHdrMain] stage=P2_fail err={e}");
                log::debug!("PSD HDR main P2 layer composite unavailable: {e}");
            }
        }

        if let Some(main) = decode_psd_hdr_main_p25a(index, bytes, layer_info, cancel, sdr_white)? {
            return Ok(PsdHdrMainDecode {
                hdr: main.hdr,
                osd: mark_transfer(main.osd),
            });
        }
        match psd_hidden_layer_strategy {
            crate::settings::PsdHiddenLayerStrategy::Heuristic => {
                if let Some(main) =
                    decode_psd_hdr_main_p25b_heuristic(index, bytes, layer_info, cancel, sdr_white)?
                {
                    return Ok(PsdHdrMainDecode {
                        hdr: main.hdr,
                        osd: mark_transfer(main.osd),
                    });
                }
            }
            crate::settings::PsdHiddenLayerStrategy::ShowAllLayers => {
                if let Some(main) =
                    decode_psd_hdr_main_p25b_show_all(index, bytes, layer_info, cancel, sdr_white)?
                {
                    return Ok(PsdHdrMainDecode {
                        hdr: main.hdr,
                        osd: mark_transfer(main.osd),
                    });
                }
            }
        }
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

fn decode_psd_hdr_main_p25a(
    index: &PsdSectionIndex,
    bytes: &[u8],
    layer_info: &crate::psb_layer_composite::LayerInfo<'_>,
    cancel: Option<&std::sync::atomic::AtomicBool>,
    sdr_white: f32,
) -> Result<Option<PsdHdrMainDecode>, crate::loader::DecodeError> {
    crate::psb_reader::check_decode_cancel(cancel)?;
    let Some(comps) =
        crate::psb_layer_comps::parse_layer_comps_from_ir(bytes, index.ir_start, index.ir_end)
    else {
        crate::preload_debug!("[PreloadDebug][PsdHdrMain] stage=P25a_no_comps");
        return Ok(None);
    };
    let Some(comp) = crate::psb_layer_comps::select_layer_comp(&comps.comps, comps.last_applied)
    else {
        crate::preload_debug!("[PreloadDebug][PsdHdrMain] stage=P25a_no_selected_comp");
        return Ok(None);
    };
    let comp_id = comp.id;
    let comp_name = if comp.name.is_empty() {
        None
    } else {
        Some(comp.name.clone())
    };

    let visible = crate::psb_layer_comps::visibility_from_layer_comp(&layer_info.records, comp_id);

    match composite_layers_hdr_with_visibility_from_info(
        layer_info, bytes, index, &visible, cancel, sdr_white,
    ) {
        Ok(hdr) => {
            if rgba_f32_is_absolutely_blank_with_cancel(&hdr.rgba_f32, cancel)?
                || rgba_f32_is_zero_information_with_cancel(&hdr.rgba_f32, cancel)?
            {
                crate::preload_debug!(
                    "[PreloadDebug][PsdHdrMain] stage=P25a_zero_information -> degrade_P25b"
                );
                Ok(None)
            } else {
                crate::preload_debug!(
                    "[PreloadDebug][PsdHdrMain] stage=P25a_layer_comp {}x{}",
                    hdr.width,
                    hdr.height
                );
                Ok(Some(PsdHdrMainDecode {
                    hdr,
                    osd: crate::loader::PsdOsdInfo::p25a_layer_comp(comp_name),
                }))
            }
        }
        Err(e) if e.is_cancelled() => Err(e),
        Err(e) => {
            crate::preload_debug!("[PreloadDebug][PsdHdrMain] stage=P25a_fail err={e}");
            log::debug!("PSD HDR main P2.5a composite unavailable: {e}");
            Ok(None)
        }
    }
}

fn decode_psd_hdr_main_p25b_heuristic(
    index: &PsdSectionIndex,
    bytes: &[u8],
    layer_info: &crate::psb_layer_composite::LayerInfo<'_>,
    cancel: Option<&std::sync::atomic::AtomicBool>,
    sdr_white: f32,
) -> Result<Option<PsdHdrMainDecode>, crate::loader::DecodeError> {
    crate::psb_reader::check_decode_cancel(cancel)?;
    let candidates = crate::psb_p25_reveal::rank_max_bbox_top_level(
        &layer_info.records,
        crate::psb_p25_reveal::P25B_MAX_CANDIDATES,
    );
    if candidates.is_empty() {
        crate::preload_debug!("[PreloadDebug][PsdHdrMain] stage=P25b_no_candidate");
        return Ok(None);
    }

    for (cand_i, selection) in candidates.iter().enumerate() {
        let root_name = if selection.root_name.is_empty() {
            None
        } else {
            Some(selection.root_name.clone())
        };
        crate::preload_debug!(
            "[PreloadDebug][PsdHdrMain] stage=P25b_try cand={} root={}",
            cand_i,
            selection.root_name
        );
        log::debug!(
            "PSD HDR main P2.5b try cand={} root={}",
            cand_i,
            selection.root_name
        );

        let visible = crate::psb_p25_reveal::visibility_respect_subtree(
            &layer_info.records,
            &selection.member_indices,
        );
        match composite_layers_hdr_with_visibility_from_info(
            layer_info, bytes, index, &visible, cancel, sdr_white,
        ) {
            Ok(hdr) => {
                if !rgba_f32_is_absolutely_blank_with_cancel(&hdr.rgba_f32, cancel)?
                    && !rgba_f32_is_zero_information_with_cancel(&hdr.rgba_f32, cancel)?
                {
                    crate::preload_debug!(
                        "[PreloadDebug][PsdHdrMain] stage=P25b_max_bbox cand={} {}x{}",
                        cand_i,
                        hdr.width,
                        hdr.height
                    );
                    return Ok(Some(PsdHdrMainDecode {
                        hdr,
                        osd: crate::loader::PsdOsdInfo::p25b_max_bbox(root_name.clone(), false),
                    }));
                }
                crate::preload_debug!(
                    "[PreloadDebug][PsdHdrMain] stage=P25b_zero_information cand={} -> force_open",
                    cand_i
                );
            }
            Err(e) if e.is_cancelled() => return Err(e),
            Err(e) => {
                crate::preload_debug!(
                    "[PreloadDebug][PsdHdrMain] stage=P25b_pass1_fail cand={} err={e}",
                    cand_i
                );
                log::debug!("PSD HDR main P2.5b pass1 fail cand={cand_i}: {e}");
            }
        }

        let visible = crate::psb_p25_reveal::visibility_force_open_subtree(
            &layer_info.records,
            &selection.member_indices,
        );
        match composite_layers_hdr_with_visibility_from_info(
            layer_info, bytes, index, &visible, cancel, sdr_white,
        ) {
            Ok(hdr) => {
                if rgba_f32_is_absolutely_blank_with_cancel(&hdr.rgba_f32, cancel)?
                    || rgba_f32_is_zero_information_with_cancel(&hdr.rgba_f32, cancel)?
                {
                    crate::preload_debug!(
                        "[PreloadDebug][PsdHdrMain] stage=P25b_force_open_zero_information cand={}",
                        cand_i
                    );
                } else {
                    crate::preload_debug!(
                        "[PreloadDebug][PsdHdrMain] stage=P25b_force_open cand={} {}x{}",
                        cand_i,
                        hdr.width,
                        hdr.height
                    );
                    return Ok(Some(PsdHdrMainDecode {
                        hdr,
                        osd: crate::loader::PsdOsdInfo::p25b_max_bbox(root_name, true),
                    }));
                }
            }
            Err(e) if e.is_cancelled() => return Err(e),
            Err(e) => {
                crate::preload_debug!(
                    "[PreloadDebug][PsdHdrMain] stage=P25b_force_open_fail cand={} err={e}",
                    cand_i
                );
                log::debug!("PSD HDR main P2.5b force-open fail cand={cand_i}: {e}");
            }
        }
    }

    crate::preload_debug!("[PreloadDebug][PsdHdrMain] stage=P25b_exhausted");
    Ok(None)
}

fn decode_psd_hdr_main_p25b_show_all(
    index: &PsdSectionIndex,
    bytes: &[u8],
    layer_info: &crate::psb_layer_composite::LayerInfo<'_>,
    cancel: Option<&std::sync::atomic::AtomicBool>,
    sdr_white: f32,
) -> Result<Option<PsdHdrMainDecode>, crate::loader::DecodeError> {
    crate::psb_reader::check_decode_cancel(cancel)?;
    let visible = crate::psb_p25_reveal::visibility_force_open_all(&layer_info.records);
    crate::preload_debug!(
        "[PreloadDebug][PsdHdrMain] stage=P25b_force_open_all drawable={}",
        visible.iter().filter(|v| **v).count()
    );
    log::debug!(
        "PSD HDR main P2.5b force-open-all drawable={}",
        visible.iter().filter(|v| **v).count()
    );

    match composite_layers_hdr_with_visibility_from_info(
        layer_info, bytes, index, &visible, cancel, sdr_white,
    ) {
        Ok(hdr) => {
            if !rgba_f32_is_absolutely_blank_with_cancel(&hdr.rgba_f32, cancel)?
                && !rgba_f32_is_zero_information_with_cancel(&hdr.rgba_f32, cancel)?
            {
                crate::preload_debug!(
                    "[PreloadDebug][PsdHdrMain] stage=P25b_force_open_all {}x{}",
                    hdr.width,
                    hdr.height
                );
                return Ok(Some(PsdHdrMainDecode {
                    hdr,
                    osd: crate::loader::PsdOsdInfo::p25b_show_all(),
                }));
            }
            crate::preload_debug!(
                "[PreloadDebug][PsdHdrMain] stage=P25b_force_open_all_zero_information"
            );
            Ok(None)
        }
        Err(e) if e.is_cancelled() => Err(e),
        Err(e) => {
            crate::preload_debug!(
                "[PreloadDebug][PsdHdrMain] stage=P25b_force_open_all_fail err={e}"
            );
            log::debug!("PSD HDR main P2.5b force-open-all unavailable: {e}");
            Ok(None)
        }
    }
}

/// Per-record visibility mask plus the OSD stage that produced it, resolved
/// for the disk-backed HDR layer tiler without any full-canvas composite.
///
/// The disk tiler cannot afford a full pixel composite to decide P2 vs
/// P2.5a/P2.5b, so plan selection is geometric only (via
/// [`strict_visibility_has_drawable_output`]): the first stage whose
/// visibility yields at least one on-canvas drawable layer wins.
#[derive(Debug)]
pub(crate) struct HdrDiskVisibilityPlan {
    pub visible: Vec<bool>,
    pub osd: crate::loader::PsdOsdInfo,
}

/// Resolve the visibility mask for the disk-backed HDR layer tiler.
///
/// Mirrors the P2 -> P2.5a -> P2.5b order of
/// [`decode_psd_hdr_main_from_index_with_layer_info`] but decides each stage
/// with a geometric drawable-output check instead of compositing pixels.
pub(crate) fn resolve_hdr_disk_visibility_plan(
    index: &PsdSectionIndex,
    bytes: &[u8],
    layer_info: &crate::psb_layer_composite::LayerInfo<'_>,
    cancel: Option<&std::sync::atomic::AtomicBool>,
    strategy: crate::settings::PsdHiddenLayerStrategy,
) -> Result<HdrDiskVisibilityPlan, crate::loader::DecodeError> {
    use crate::psb_layer_composite::{
        compute_effective_visibility, strict_visibility_has_drawable_output,
    };

    crate::psb_reader::check_decode_cancel(cancel)?;
    let records = &layer_info.records;
    let w = layer_info.width;
    let h = layer_info.height;

    let embedded_icc = extract_icc_profile_from_ir(bytes, index.ir_start, index.ir_end);
    let icc_probe = embedded_icc
        .as_deref()
        .map(probe_icc_hdr)
        .unwrap_or_default();
    log_16bit_transfer_assumption(&icc_probe, index.depth);
    let mark_transfer = |osd: crate::loader::PsdOsdInfo| {
        if transfer_assumption_uncertain(&icc_probe, index.depth) {
            osd.with_transfer_uncertain()
        } else {
            osd
        }
    };

    // P2: strict Photoshop layer/group visibility.
    let strict = compute_effective_visibility(records);
    if strict_visibility_has_drawable_output(w, h, records, &strict) {
        return Ok(HdrDiskVisibilityPlan {
            visible: strict,
            osd: mark_transfer(crate::loader::PsdOsdInfo::p2_strict()),
        });
    }

    // P2.5a: selected Layer Comp reveal.
    crate::psb_reader::check_decode_cancel(cancel)?;
    if let Some(comps) =
        crate::psb_layer_comps::parse_layer_comps_from_ir(bytes, index.ir_start, index.ir_end)
        && let Some(comp) =
            crate::psb_layer_comps::select_layer_comp(&comps.comps, comps.last_applied)
    {
        let comp_name = if comp.name.is_empty() {
            None
        } else {
            Some(comp.name.clone())
        };
        let visible = crate::psb_layer_comps::visibility_from_layer_comp(records, comp.id);
        if strict_visibility_has_drawable_output(w, h, records, &visible) {
            return Ok(HdrDiskVisibilityPlan {
                visible,
                osd: mark_transfer(crate::loader::PsdOsdInfo::p25a_layer_comp(comp_name)),
            });
        }
    }

    // P2.5b: hidden-layer reveal heuristic / force-open-all.
    crate::psb_reader::check_decode_cancel(cancel)?;
    match strategy {
        crate::settings::PsdHiddenLayerStrategy::Heuristic => {
            let candidates = crate::psb_p25_reveal::rank_max_bbox_top_level(
                records,
                crate::psb_p25_reveal::P25B_MAX_CANDIDATES,
            );
            for selection in &candidates {
                let root_name = if selection.root_name.is_empty() {
                    None
                } else {
                    Some(selection.root_name.clone())
                };
                let respect = crate::psb_p25_reveal::visibility_respect_subtree(
                    records,
                    &selection.member_indices,
                );
                if strict_visibility_has_drawable_output(w, h, records, &respect) {
                    return Ok(HdrDiskVisibilityPlan {
                        visible: respect,
                        osd: mark_transfer(crate::loader::PsdOsdInfo::p25b_max_bbox(
                            root_name, false,
                        )),
                    });
                }
                let forced = crate::psb_p25_reveal::visibility_force_open_subtree(
                    records,
                    &selection.member_indices,
                );
                if strict_visibility_has_drawable_output(w, h, records, &forced) {
                    return Ok(HdrDiskVisibilityPlan {
                        visible: forced,
                        osd: mark_transfer(crate::loader::PsdOsdInfo::p25b_max_bbox(
                            root_name, true,
                        )),
                    });
                }
            }
        }
        crate::settings::PsdHiddenLayerStrategy::ShowAllLayers => {
            let visible = crate::psb_p25_reveal::visibility_force_open_all(records);
            if strict_visibility_has_drawable_output(w, h, records, &visible) {
                return Ok(HdrDiskVisibilityPlan {
                    visible,
                    osd: mark_transfer(crate::loader::PsdOsdInfo::p25b_show_all()),
                });
            }
        }
    }

    Err(crate::loader::DecodeError::NoDrawableVisibleLayers)
}

/// Zero-information for HDR: all alpha 0, or solid RGB (no variance) with any alpha.
fn rgba_f32_is_zero_information_with_cancel(
    pixels: &[f32],
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<bool, crate::loader::DecodeError> {
    const EPS: f32 = 1e-8;
    const CANCEL_POLL_PIXELS: usize = 64 * 1024;
    if pixels.is_empty() || !pixels.len().is_multiple_of(4) {
        return Ok(true);
    }
    let mut any_a = false;
    let mut ref_r = 0.0f32;
    let mut ref_g = 0.0f32;
    let mut ref_b = 0.0f32;
    let mut have_ref = false;
    let mut rgb_varies = false;
    let mut pixel_index = 0usize;
    while pixel_index < pixels.len() / 4 {
        if pixel_index.is_multiple_of(CANCEL_POLL_PIXELS) {
            crate::psb_reader::check_decode_cancel(cancel)?;
        }
        let offset = pixel_index * 4;
        let px = &pixels[offset..offset + 4];
        if px[3].abs() > EPS {
            any_a = true;
        }
        if !have_ref {
            ref_r = px[0];
            ref_g = px[1];
            ref_b = px[2];
            have_ref = true;
        } else if (px[0] - ref_r).abs() > EPS
            || (px[1] - ref_g).abs() > EPS
            || (px[2] - ref_b).abs() > EPS
        {
            rgb_varies = true;
        }
        if any_a && rgb_varies {
            return Ok(false);
        }
        pixel_index += 1;
    }
    Ok(!any_a || !rgb_varies)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hdr::types::DEFAULT_SDR_WHITE_NITS;

    #[test]
    fn should_try_hdr_gates() {
        assert!(!psd_should_try_hdr(32, None, 1.0));
        assert!(psd_should_try_hdr(32, None, 2.0));
        assert!(!psd_should_try_hdr(8, None, 2.0));
        assert!(!psd_should_try_hdr(16, None, 2.0));
    }

    #[test]
    fn zero_information_solid() {
        let solid = vec![0.5f32, 0.5, 0.5, 1.0, 0.5, 0.5, 0.5, 1.0];
        assert!(rgba_f32_is_zero_information_with_cancel(&solid, None).unwrap());
        let varied = vec![0.5f32, 0.5, 0.5, 1.0, 0.6, 0.5, 0.5, 1.0];
        assert!(!rgba_f32_is_zero_information_with_cancel(&varied, None).unwrap());
        let _ = DEFAULT_SDR_WHITE_NITS;
    }

    #[test]
    fn zero_information_honors_cancel() {
        let cancel = std::sync::atomic::AtomicBool::new(true);
        let err = rgba_f32_is_zero_information_with_cancel(&[0.0; 4], Some(&cancel))
            .expect_err("cancelled zero-information scan");
        assert!(err.is_cancelled());
    }

    #[test]
    fn content_gate_decline_is_typed_enum() {
        // Minimal 8-bit RGB PSD header: content gate must decline HDR.
        let mut bytes = vec![0u8; 50];
        bytes[0..4].copy_from_slice(b"8BPS");
        bytes[4..6].copy_from_slice(&1u16.to_be_bytes());
        bytes[12..14].copy_from_slice(&3u16.to_be_bytes());
        bytes[14..18].copy_from_slice(&1u32.to_be_bytes());
        bytes[18..22].copy_from_slice(&1u32.to_be_bytes());
        bytes[22..24].copy_from_slice(&8u16.to_be_bytes());
        bytes[24..26].copy_from_slice(&3u16.to_be_bytes());
        let index = crate::psb_section_index::PsdSectionIndex::parse(&bytes).unwrap();
        let tone = HdrToneMapSettings::default();
        let err = decode_psd_hdr_main_from_index_with_cancel(
            &index,
            &bytes,
            None,
            &tone,
            false,
            crate::settings::PsdHiddenLayerStrategy::Heuristic,
        )
        .expect_err("8-bit must decline HDR");
        assert!(err.is_psd_hdr_not_wanted());
        assert_eq!(err.as_str(), crate::loader::PSD_HDR_NOT_WANTED);
    }
}
