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

use crate::hdr::types::{HdrImageBuffer, HdrToneMapSettings};

#[inline]
pub(crate) fn hdr_to_sdr_with_user_tone(
    buffer: &HdrImageBuffer,
    tone: &HdrToneMapSettings,
) -> Result<Vec<u8>, String> {
    crate::hdr::decode::hdr_to_sdr_rgba8_with_tone_settings(buffer, tone.exposure_ev, tone)
}

/// Display-referred peak headroom used by the loader: values `<= 1.0` (plus epsilon) mean SDR or
/// SDR tone-mapped output where an eager full-frame SDR fallback is appropriate.
#[inline]
pub(crate) fn hdr_display_requests_sdr_preview(hdr_target_capacity: f32) -> bool {
    const MAX_SDR: f32 = 1.0;
    const EPS: f32 = 0.001;
    hdr_target_capacity <= MAX_SDR + EPS
}

pub(crate) fn cheap_hdr_sdr_placeholder_rgba8(width: u32, height: u32) -> Result<Vec<u8>, String> {
    crate::hdr::decode::validate_hdr_fallback_budget(width, height)?;
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or_else(|| format!("HDR SDR placeholder dimension overflow: {width}x{height}"))?;
    let byte_len = pixels
        .checked_mul(4)
        .ok_or_else(|| format!("HDR SDR placeholder byte overflow: {width}x{height}"))?;
    let mut out = vec![0_u8; byte_len as usize];
    for px in out.chunks_exact_mut(4) {
        px[3] = 255;
    }
    Ok(out)
}

pub(crate) fn hdr_sdr_fallback_rgba8_eager_or_placeholder(
    hdr: &HdrImageBuffer,
    hdr_target_capacity: f32,
    tone: &HdrToneMapSettings,
) -> Result<Vec<u8>, String> {
    if hdr_display_requests_sdr_preview(hdr_target_capacity) {
        hdr_to_sdr_with_user_tone(hdr, tone)
    } else {
        cheap_hdr_sdr_placeholder_rgba8(hdr.width, hdr.height)
    }
}
