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
//! P3 stays on the SDR path (IR JPEG is 8-bit). On hard failure the caller
//! falls back to [`crate::psb_sdr_main`].

use crate::hdr::types::{HdrImageBuffer, HdrToneMapSettings};
use crate::psb_hdr_composite::{
    composite_layers_hdr_from_index, composite_layers_hdr_with_visibility_from_index,
};
use crate::psb_hdr_flat::{read_composite_hdr_from_index, rgba_f32_is_absolutely_blank};
use crate::psb_icc_hdr::{psd_content_wants_hdr, psd_env_wants_hdr};
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
    let embedded_icc = extract_icc_profile_from_ir(bytes, index.ir_start, index.ir_end);
    if !psd_content_wants_hdr(index.depth, embedded_icc.as_deref()) {
        return Err("PSD HDR main: content gate does not want HDR".into());
    }

    let sdr_white = tone.sdr_white_nits.max(1.0);

    if !skip_flattened {
        crate::psb_reader::check_decode_cancel(cancel)?;
        match read_composite_hdr_from_index(&index, bytes, cancel, sdr_white) {
            Ok(hdr) => {
                if rgba_f32_is_absolutely_blank(&hdr.rgba_f32) {
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
                        osd: crate::loader::PsdOsdInfo::p1_flattened(),
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
    match composite_layers_hdr_from_index(&index, bytes, cancel, sdr_white) {
        Ok(hdr) => {
            if rgba_f32_is_absolutely_blank(&hdr.rgba_f32)
                || rgba_f32_is_zero_information(&hdr.rgba_f32)
            {
                crate::preload_debug!(
                    "[PreloadDebug][PsdHdrMain] stage=P2_zero_information {}x{} -> degrade_P25",
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
                    osd: crate::loader::PsdOsdInfo::p2_strict(),
                });
            }
        }
        Err(e) if e.is_cancelled() => return Err(e),
        Err(e) => {
            crate::preload_debug!("[PreloadDebug][PsdHdrMain] stage=P2_fail err={e}");
            log::debug!("PSD HDR main P2 layer composite unavailable: {e}");
        }
    }

    if let Some(main) = decode_psd_hdr_main_p25a(&index, bytes, cancel, sdr_white)? {
        return Ok(main);
    }
    match psd_hidden_layer_strategy {
        crate::settings::PsdHiddenLayerStrategy::Heuristic => {
            if let Some(main) =
                decode_psd_hdr_main_p25b_heuristic(&index, bytes, cancel, sdr_white)?
            {
                return Ok(main);
            }
        }
        crate::settings::PsdHiddenLayerStrategy::ShowAllLayers => {
            if let Some(main) = decode_psd_hdr_main_p25b_show_all(&index, bytes, cancel, sdr_white)?
            {
                return Ok(main);
            }
        }
    }

    Err("PSD HDR main: P2/P2.5 unavailable; falling back to SDR".into())
}

fn decode_psd_hdr_main_p25a(
    index: &PsdSectionIndex,
    bytes: &[u8],
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

    let layer_info = match crate::psb_layer_composite::parse_layer_records_from_index(index, bytes)
    {
        Ok(info) => info,
        Err(e) => {
            crate::preload_debug!("[PreloadDebug][PsdHdrMain] stage=P25a_parse_fail err={e}");
            return Ok(None);
        }
    };
    let Some(visible) =
        crate::psb_layer_comps::visibility_from_layer_comp(&layer_info.records, comp_id)
    else {
        crate::preload_debug!("[PreloadDebug][PsdHdrMain] stage=P25a_visibility_fail");
        return Ok(None);
    };

    match composite_layers_hdr_with_visibility_from_index(index, bytes, &visible, cancel, sdr_white)
    {
        Ok(hdr) => {
            if rgba_f32_is_absolutely_blank(&hdr.rgba_f32)
                || rgba_f32_is_zero_information(&hdr.rgba_f32)
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
            Ok(None)
        }
    }
}

fn decode_psd_hdr_main_p25b_heuristic(
    index: &PsdSectionIndex,
    bytes: &[u8],
    cancel: Option<&std::sync::atomic::AtomicBool>,
    sdr_white: f32,
) -> Result<Option<PsdHdrMainDecode>, crate::loader::DecodeError> {
    crate::psb_reader::check_decode_cancel(cancel)?;
    let layer_info = match crate::psb_layer_composite::parse_layer_records_from_index(index, bytes)
    {
        Ok(info) => info,
        Err(e) => {
            crate::preload_debug!("[PreloadDebug][PsdHdrMain] stage=P25b_parse_fail err={e}");
            return Ok(None);
        }
    };
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
        match composite_layers_hdr_with_visibility_from_index(
            index, bytes, &visible, cancel, sdr_white,
        ) {
            Ok(hdr)
                if !rgba_f32_is_absolutely_blank(&hdr.rgba_f32)
                    && !rgba_f32_is_zero_information(&hdr.rgba_f32) =>
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
            Ok(_) => {
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
            }
        }

        let visible = crate::psb_p25_reveal::visibility_force_open_subtree(
            &layer_info.records,
            &selection.member_indices,
        );
        match composite_layers_hdr_with_visibility_from_index(
            index, bytes, &visible, cancel, sdr_white,
        ) {
            Ok(hdr) => {
                if rgba_f32_is_absolutely_blank(&hdr.rgba_f32)
                    || rgba_f32_is_zero_information(&hdr.rgba_f32)
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
            }
        }
    }

    crate::preload_debug!("[PreloadDebug][PsdHdrMain] stage=P25b_exhausted");
    Ok(None)
}

fn decode_psd_hdr_main_p25b_show_all(
    index: &PsdSectionIndex,
    bytes: &[u8],
    cancel: Option<&std::sync::atomic::AtomicBool>,
    sdr_white: f32,
) -> Result<Option<PsdHdrMainDecode>, crate::loader::DecodeError> {
    crate::psb_reader::check_decode_cancel(cancel)?;
    let layer_info = match crate::psb_layer_composite::parse_layer_records_from_index(index, bytes)
    {
        Ok(info) => info,
        Err(e) => {
            crate::preload_debug!("[PreloadDebug][PsdHdrMain] stage=P25b_parse_fail err={e}");
            return Ok(None);
        }
    };
    let visible = crate::psb_p25_reveal::visibility_force_open_all(&layer_info.records);
    crate::preload_debug!(
        "[PreloadDebug][PsdHdrMain] stage=P25b_force_open_all drawable={}",
        visible.iter().filter(|v| **v).count()
    );
    log::debug!(
        "PSD HDR main P2.5b force-open-all drawable={}",
        visible.iter().filter(|v| **v).count()
    );
    drop(layer_info);

    match composite_layers_hdr_with_visibility_from_index(index, bytes, &visible, cancel, sdr_white)
    {
        Ok(hdr)
            if !rgba_f32_is_absolutely_blank(&hdr.rgba_f32)
                && !rgba_f32_is_zero_information(&hdr.rgba_f32) =>
        {
            crate::preload_debug!(
                "[PreloadDebug][PsdHdrMain] stage=P25b_force_open_all {}x{}",
                hdr.width,
                hdr.height
            );
            Ok(Some(PsdHdrMainDecode {
                hdr,
                osd: crate::loader::PsdOsdInfo::p25b_max_bbox(None, true),
            }))
        }
        Ok(_) => {
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

/// Zero-information for HDR: all alpha 0, or solid RGB (no variance) with any alpha.
fn rgba_f32_is_zero_information(pixels: &[f32]) -> bool {
    const EPS: f32 = 1e-8;
    if pixels.is_empty() || !pixels.len().is_multiple_of(4) {
        return true;
    }
    let mut any_a = false;
    let mut ref_r = 0.0f32;
    let mut ref_g = 0.0f32;
    let mut ref_b = 0.0f32;
    let mut have_ref = false;
    let mut rgb_varies = false;
    for px in pixels.chunks_exact(4) {
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
            return false;
        }
    }
    !any_a || !rgb_varies
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
        assert!(rgba_f32_is_zero_information(&solid));
        let varied = vec![0.5f32, 0.5, 0.5, 1.0, 0.6, 0.5, 0.5, 1.0];
        assert!(!rgba_f32_is_zero_information(&varied));
        let _ = DEFAULT_SDR_WHITE_NITS;
    }
}
