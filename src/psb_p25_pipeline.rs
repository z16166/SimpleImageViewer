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

//! Shared P2.5a / P2.5b pipeline helpers for the SDR and HDR state machines.
//!
//! The SDR (`psb_sdr_main.rs`) and HDR (`psb_hdr_main.rs`) main-image decode
//! state machines run nearly identical P2.5a (Layer Comp reveal) and P2.5b
//! (hidden-layer heuristic / force-open-all) fallback logic; only the composite
//! function, the blank/zero-information check, and the result wrapper differ.
//! This module extracts the common control flow so both paths share a single
//! implementation, reducing the risk of missed fixes.

use crate::psb_section_index::PsdSectionIndex;

/// Try a P2.5a Layer Comp reveal pass.
///
/// Returns `Ok(Some((result, osd)))` when a Layer Comp is present, selected,
/// produces a non-blank / non-zero-information composite, and `Ok(None)` to
/// degrade to P2.5b. Propagates `Err` on cancel.
///
/// # Parameters
///
/// * `stage` – short label for diagnostics (e.g. `"PsdSdrMain"`).
/// * `composite_fn` – performs the actual composite with the given visibility.
/// * `is_blank_or_zero_info` – returns `true` when the composite result should
///   be treated as blank / zero-information (causes degrade to the next stage).
pub(crate) fn try_p25a_pass<T, C, B>(
    stage: &str,
    index: &PsdSectionIndex,
    bytes: &[u8],
    layer_info: &crate::psb_layer_composite::LayerInfo<'_>,
    cancel: Option<&std::sync::atomic::AtomicBool>,
    composite_fn: &mut C,
    is_blank_or_zero_info: &B,
) -> Result<Option<(T, crate::loader::PsdOsdInfo)>, crate::loader::DecodeError>
where
    C: FnMut(&[bool]) -> Result<T, crate::loader::DecodeError>,
    B: Fn(&T, Option<&std::sync::atomic::AtomicBool>) -> Result<bool, crate::loader::DecodeError>,
{
    crate::psb_reader::check_decode_cancel(cancel)?;
    let Some(comps) =
        crate::psb_layer_comps::parse_layer_comps_from_ir(bytes, index.ir_start, index.ir_end)
    else {
        crate::preload_debug!("[PreloadDebug][{stage}] stage=P25a_no_comps");
        return Ok(None);
    };
    let Some(comp) = crate::psb_layer_comps::select_layer_comp(&comps.comps, comps.last_applied)
    else {
        crate::preload_debug!("[PreloadDebug][{stage}] stage=P25a_no_selected_comp");
        return Ok(None);
    };
    let comp_name = if comp.name.is_empty() {
        None
    } else {
        Some(comp.name.clone())
    };

    let visible = crate::psb_layer_comps::visibility_from_layer_comp(&layer_info.records, comp.id);
    match composite_fn(&visible) {
        Ok(result) => {
            if is_blank_or_zero_info(&result, cancel)? {
                crate::preload_debug!(
                    "[PreloadDebug][{stage}] stage=P25a_zero_information -> degrade_P25b"
                );
                Ok(None)
            } else {
                crate::preload_debug!("[PreloadDebug][{stage}] stage=P25a_layer_comp",);
                Ok(Some((
                    result,
                    crate::loader::PsdOsdInfo::p25a_layer_comp(comp_name),
                )))
            }
        }
        Err(e) if e.is_cancelled() => Err(e),
        Err(e) => {
            crate::preload_debug!("[PreloadDebug][{stage}] stage=P25a_fail err={e}");
            log::debug!("{stage} P2.5a composite unavailable: {e}");
            Ok(None)
        }
    }
}

/// Try P2.5b hidden-layer heuristic (max-bbox candidate reveal).
///
/// Tries ranked max-bbox candidates: first respect-subtree, then
/// force-open-subtree. Returns `Ok(Some((result, osd)))` on the first
/// non-blank / non-zero-information match, `Ok(None)` to degrade to P3
/// (SDR) or hard fail (HDR). Propagates `Err` on cancel.
///
/// # Parameters
///
/// * `stage` – short label for diagnostics.
/// * `composite_fn` – performs the composite for each visibility trial.
/// * `is_blank_or_zero_info` – returns `true` for blank / zero-information.
/// * `reveal_err` – accumulates structural P2.5b errors for the caller's
///   final error report (see [`crate::psb_p25_reveal::remember_p25_reveal_err`]).
pub(crate) fn try_p25b_heuristic_pass<T, C, B>(
    stage: &str,
    layer_info: &crate::psb_layer_composite::LayerInfo<'_>,
    cancel: Option<&std::sync::atomic::AtomicBool>,
    composite_fn: &mut C,
    is_blank_or_zero_info: &B,
    reveal_err: &mut Option<crate::loader::DecodeError>,
) -> Result<Option<(T, crate::loader::PsdOsdInfo)>, crate::loader::DecodeError>
where
    C: FnMut(&[bool]) -> Result<T, crate::loader::DecodeError>,
    B: Fn(&T, Option<&std::sync::atomic::AtomicBool>) -> Result<bool, crate::loader::DecodeError>,
{
    crate::psb_reader::check_decode_cancel(cancel)?;
    let candidates = crate::psb_p25_reveal::rank_max_bbox_top_level(
        &layer_info.records,
        crate::psb_p25_reveal::P25B_MAX_CANDIDATES,
    );
    if candidates.is_empty() {
        crate::preload_debug!("[PreloadDebug][{stage}] stage=P25b_no_candidate");
        return Ok(None);
    }

    for (cand_i, selection) in candidates.iter().enumerate() {
        let root_name = if selection.root_name.is_empty() {
            None
        } else {
            Some(selection.root_name.clone())
        };
        crate::preload_debug!(
            "[PreloadDebug][{stage}] stage=P25b_try cand={} root={}",
            cand_i,
            selection.root_name
        );
        log::debug!(
            "{stage} P2.5b try cand={} root={}",
            cand_i,
            selection.root_name
        );

        let visible = crate::psb_p25_reveal::visibility_respect_subtree(
            &layer_info.records,
            &selection.member_indices,
        );
        match composite_fn(&visible) {
            Ok(result) => {
                if !is_blank_or_zero_info(&result, cancel)? {
                    crate::preload_debug!(
                        "[PreloadDebug][{stage}] stage=P25b_max_bbox cand={}",
                        cand_i
                    );
                    return Ok(Some((
                        result,
                        crate::loader::PsdOsdInfo::p25b_max_bbox(root_name, false),
                    )));
                }
                crate::preload_debug!(
                    "[PreloadDebug][{stage}] stage=P25b_zero_information cand={} -> force_open",
                    cand_i
                );
            }
            Err(e) if e.is_cancelled() => return Err(e),
            Err(e) => {
                crate::preload_debug!(
                    "[PreloadDebug][{stage}] stage=P25b_pass1_fail cand={} err={e}",
                    cand_i
                );
                log::debug!("{stage} P2.5b pass1 fail cand={cand_i}: {e}");
                crate::psb_p25_reveal::remember_p25_reveal_err(reveal_err, e);
            }
        }

        let visible = crate::psb_p25_reveal::visibility_force_open_subtree(
            &layer_info.records,
            &selection.member_indices,
        );
        match composite_fn(&visible) {
            Ok(result) => {
                if is_blank_or_zero_info(&result, cancel)? {
                    crate::preload_debug!(
                        "[PreloadDebug][{stage}] stage=P25b_force_open_zero_information cand={}",
                        cand_i
                    );
                } else {
                    crate::preload_debug!(
                        "[PreloadDebug][{stage}] stage=P25b_force_open cand={}",
                        cand_i
                    );
                    return Ok(Some((
                        result,
                        crate::loader::PsdOsdInfo::p25b_max_bbox(root_name, true),
                    )));
                }
            }
            Err(e) if e.is_cancelled() => return Err(e),
            Err(e) => {
                crate::preload_debug!(
                    "[PreloadDebug][{stage}] stage=P25b_force_open_fail cand={} err={e}",
                    cand_i
                );
                log::debug!("{stage} P2.5b force-open fail cand={cand_i}: {e}");
                crate::psb_p25_reveal::remember_p25_reveal_err(reveal_err, e);
            }
        }
    }

    crate::preload_debug!("[PreloadDebug][{stage}] stage=P25b_exhausted");
    Ok(None)
}

/// Try P2.5b force-open-all pass.
///
/// Returns `Ok(Some((result, osd)))` when the full force-open-all reveals a
/// non-blank / non-zero-information composite, `Ok(None)` to degrade.
pub(crate) fn try_p25b_show_all_pass<T, C, B>(
    stage: &str,
    layer_info: &crate::psb_layer_composite::LayerInfo<'_>,
    cancel: Option<&std::sync::atomic::AtomicBool>,
    composite_fn: &mut C,
    is_blank_or_zero_info: &B,
    reveal_err: &mut Option<crate::loader::DecodeError>,
) -> Result<Option<(T, crate::loader::PsdOsdInfo)>, crate::loader::DecodeError>
where
    C: FnMut(&[bool]) -> Result<T, crate::loader::DecodeError>,
    B: Fn(&T, Option<&std::sync::atomic::AtomicBool>) -> Result<bool, crate::loader::DecodeError>,
{
    crate::psb_reader::check_decode_cancel(cancel)?;
    let visible = crate::psb_p25_reveal::visibility_force_open_all(&layer_info.records);
    crate::preload_debug!(
        "[PreloadDebug][{stage}] stage=P25b_force_open_all drawable={}",
        visible.iter().filter(|v| **v).count()
    );
    log::debug!(
        "{stage} P2.5b force-open-all drawable={}",
        visible.iter().filter(|v| **v).count()
    );

    match composite_fn(&visible) {
        Ok(result) => {
            if !is_blank_or_zero_info(&result, cancel)? {
                crate::preload_debug!("[PreloadDebug][{stage}] stage=P25b_force_open_all");
                return Ok(Some((result, crate::loader::PsdOsdInfo::p25b_show_all())));
            }
            crate::preload_debug!(
                "[PreloadDebug][{stage}] stage=P25b_force_open_all_zero_information"
            );
            Ok(None)
        }
        Err(e) if e.is_cancelled() => Err(e),
        Err(e) => {
            crate::preload_debug!("[PreloadDebug][{stage}] stage=P25b_force_open_all_fail err={e}");
            log::debug!("{stage} P2.5b force-open-all unavailable: {e}");
            crate::psb_p25_reveal::remember_p25_reveal_err(reveal_err, e);
            Ok(None)
        }
    }
}
