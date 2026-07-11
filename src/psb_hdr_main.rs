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
//! P1 flattened HDR -> P2 linear-light layer composite. P3 stays on the SDR
//! path (IR JPEG is 8-bit). On hard failure the caller falls back to
//! [`crate::psb_sdr_main`].

use crate::hdr::types::{HdrImageBuffer, HdrToneMapSettings};
use crate::psb_hdr_composite::composite_layers_hdr_from_index;
use crate::psb_hdr_flat::{read_composite_hdr_from_index, rgba_f32_is_absolutely_blank};
use crate::psb_icc_hdr::{psd_content_wants_hdr, psd_env_wants_hdr};
use crate::psb_reader::extract_icc_profile_from_ir;
use crate::psb_section_index::PsdSectionIndex;

/// True when both environment and content gates select the HDR path.
pub fn psd_should_try_hdr(
    depth: u16,
    embedded_icc: Option<&[u8]>,
    hdr_target_capacity: f32,
) -> bool {
    psd_env_wants_hdr(hdr_target_capacity) && psd_content_wants_hdr(depth, embedded_icc)
}

/// HDR main-image: P1 flattened -> P2 layer composite. No P3 HDR.
///
/// `skip_flattened`: when an oversized PSB disk-tiled probe already rejected a
/// blank flat, skip P1 and go straight to P2 HDR.
pub fn decode_psd_hdr_main_from_bytes_with_cancel(
    bytes: &[u8],
    cancel: Option<&std::sync::atomic::AtomicBool>,
    tone: &HdrToneMapSettings,
    skip_flattened: bool,
) -> Result<HdrImageBuffer, crate::loader::DecodeError> {
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
                    return Ok(hdr);
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
                    "[PreloadDebug][PsdHdrMain] stage=P2_zero_information {}x{} -> fail",
                    hdr.width,
                    hdr.height
                );
                return Err(
                    "PSD HDR main: P2 composite is zero-information; falling back to SDR".into(),
                );
            }
            crate::preload_debug!(
                "[PreloadDebug][PsdHdrMain] stage=P2_strict_layers {}x{}",
                hdr.width,
                hdr.height
            );
            Ok(hdr)
        }
        Err(e) => {
            crate::preload_debug!("[PreloadDebug][PsdHdrMain] stage=P2_fail err={e}");
            log::debug!("PSD HDR main P2 layer composite unavailable: {e}");
            Err(e)
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
