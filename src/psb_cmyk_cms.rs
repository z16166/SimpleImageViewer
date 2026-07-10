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

//! CMYK -> display sRGB for PSD/PSB via lcms2.
//!
//! Photoshop stores CMYK bytes as `0 = 100% ink`. lcms2 8-bit CMYK uses
//! `0 = no ink`. This module bridges that polarity, prefers an embedded ICC
//! (IR 1039), and otherwise uses the bundled CGATS001-compatible default.

/// Bundled default when the PSD has no embedded ICC (see `assets/icc/README.txt`).
pub const DEFAULT_CMYK_ICC: &[u8] = include_bytes!("../assets/icc/CGATS001Compat-v2-micro.icc");

/// Planar Adobe-polarity CMYK (+ optional alpha) for one CMS convert.
pub struct AdobeCmykSpan<'a> {
    pub c: &'a [u8],
    pub m: &'a [u8],
    pub y: &'a [u8],
    pub k: &'a [u8],
    pub alpha: Option<&'a [u8]>,
}

/// Choose embedded ICC when non-empty; otherwise the bundled default.
#[inline]
pub fn resolve_cmyk_icc(embedded: Option<&[u8]>) -> &[u8] {
    match embedded {
        Some(bytes) if !bytes.is_empty() => bytes,
        _ => DEFAULT_CMYK_ICC,
    }
}

/// Convert Adobe-polarity planar CMYK (+ optional alpha) to straight-alpha RGBA8.
///
/// Returns `None` when the `jpegxl` feature is off (no lcms link) or when lcms
/// rejects the profile / transform -- callers should fall back to naive
/// [`crate::psb_reader::cmyk_to_rgb`].
pub fn planar_cmyk_adobe_to_rgba8(span: &AdobeCmykSpan<'_>, icc: &[u8]) -> Option<Vec<u8>> {
    let n = span
        .c
        .len()
        .min(span.m.len())
        .min(span.y.len())
        .min(span.k.len());
    if n == 0 {
        return Some(Vec::new());
    }
    #[cfg(feature = "jpegxl")]
    {
        planar_cmyk_adobe_to_rgba8_lcms(span, icc, n)
    }
    #[cfg(not(feature = "jpegxl"))]
    {
        let _ = (span, icc);
        None
    }
}

/// Convert a CMYK span (Adobe polarity) into `dst_rgba` via lcms.
/// Returns `false` to signal naive fallback.
pub fn cmyk_span_adobe_to_rgba8(span: &AdobeCmykSpan<'_>, icc: &[u8], dst_rgba: &mut [u8]) -> bool {
    let n = span
        .c
        .len()
        .min(span.m.len())
        .min(span.y.len())
        .min(span.k.len())
        .min(dst_rgba.len() / 4);
    if n == 0 {
        return true;
    }
    #[cfg(feature = "jpegxl")]
    {
        cmyk_span_adobe_to_rgba8_lcms(span, icc, dst_rgba, n)
    }
    #[cfg(not(feature = "jpegxl"))]
    {
        let _ = (span, icc, dst_rgba);
        false
    }
}

#[cfg(feature = "jpegxl")]
fn planar_cmyk_adobe_to_rgba8_lcms(
    span: &AdobeCmykSpan<'_>,
    icc: &[u8],
    n: usize,
) -> Option<Vec<u8>> {
    let mut rgba = vec![0u8; n.checked_mul(4)?];
    if cmyk_span_adobe_to_rgba8_lcms(span, icc, &mut rgba, n) {
        Some(rgba)
    } else {
        None
    }
}

#[cfg(feature = "jpegxl")]
fn cmyk_span_adobe_to_rgba8_lcms(
    span: &AdobeCmykSpan<'_>,
    icc: &[u8],
    dst_rgba: &mut [u8],
    n: usize,
) -> bool {
    use libjxl_sys::{
        CmsProfile, CmsTransform, LCMS_INTENT_PERCEPTUAL, LCMS_TYPE_CMYK_8, LCMS_TYPE_RGB_8,
    };

    if icc.is_empty() {
        return false;
    }
    let Some(in_profile) = CmsProfile::open_from_mem(icc) else {
        log::warn!(
            "PSD CMYK ICC: lcms2 could not parse profile ({} bytes)",
            icc.len()
        );
        return false;
    };
    let Some(out_profile) = CmsProfile::new_srgb() else {
        log::warn!("PSD CMYK ICC: lcms2 could not build sRGB profile");
        return false;
    };
    let Some(transform) = CmsTransform::new(
        &in_profile,
        LCMS_TYPE_CMYK_8,
        &out_profile,
        LCMS_TYPE_RGB_8,
        LCMS_INTENT_PERCEPTUAL,
        0,
    ) else {
        log::warn!(
            "PSD CMYK ICC: lcms2 could not build CMYK->sRGB transform ({} bytes)",
            icc.len()
        );
        return false;
    };

    // Chunk to keep the temporary CMYK/RGB buffers bounded on huge canvases.
    const CHUNK: usize = 16_384;
    let mut cmyk_buf = vec![0u8; CHUNK * 4];
    let mut rgb_buf = vec![0u8; CHUNK * 3];

    let mut offset = 0usize;
    while offset < n {
        let count = (n - offset).min(CHUNK);
        for i in 0..count {
            let src = offset + i;
            let dst = i * 4;
            // Adobe 0=100% ink -> lcms 0=no ink.
            cmyk_buf[dst] = 255u8.wrapping_sub(span.c[src]);
            cmyk_buf[dst + 1] = 255u8.wrapping_sub(span.m[src]);
            cmyk_buf[dst + 2] = 255u8.wrapping_sub(span.y[src]);
            cmyk_buf[dst + 3] = 255u8.wrapping_sub(span.k[src]);
        }
        transform.do_transform(
            cmyk_buf.as_ptr().cast(),
            rgb_buf.as_mut_ptr().cast(),
            count as u32,
        );
        for i in 0..count {
            let src = i * 3;
            let dst = (offset + i) * 4;
            dst_rgba[dst] = rgb_buf[src];
            dst_rgba[dst + 1] = rgb_buf[src + 1];
            dst_rgba[dst + 2] = rgb_buf[src + 2];
            dst_rgba[dst + 3] = span
                .alpha
                .and_then(|a| a.get(offset + i).copied())
                .unwrap_or(255);
        }
        offset += count;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_icc_is_non_empty() {
        assert!(DEFAULT_CMYK_ICC.len() > 128);
        // ICC magic "acsp" lives at offset 36 in the profile header.
        assert_eq!(&DEFAULT_CMYK_ICC[36..40], b"acsp");
    }

    #[test]
    fn resolve_prefers_embedded() {
        let emb = b"not-a-real-profile-but-non-empty";
        assert_eq!(resolve_cmyk_icc(Some(emb)), emb.as_slice());
        assert_eq!(resolve_cmyk_icc(None), DEFAULT_CMYK_ICC);
        assert_eq!(resolve_cmyk_icc(Some(&[])), DEFAULT_CMYK_ICC);
    }

    #[cfg(feature = "jpegxl")]
    #[test]
    fn default_profile_maps_paper_white_near_white() {
        let span = AdobeCmykSpan {
            c: &[255],
            m: &[255],
            y: &[255],
            k: &[255],
            alpha: None,
        };
        let rgba = planar_cmyk_adobe_to_rgba8(&span, DEFAULT_CMYK_ICC).expect("lcms");
        assert_eq!(rgba.len(), 4);
        assert!(rgba[0] > 240 && rgba[1] > 240 && rgba[2] > 240, "{rgba:?}");
        assert_eq!(rgba[3], 255);
    }

    #[cfg(feature = "jpegxl")]
    #[test]
    fn default_profile_maps_full_black_near_black() {
        let span = AdobeCmykSpan {
            c: &[0],
            m: &[0],
            y: &[0],
            k: &[0],
            alpha: Some(&[200]),
        };
        let rgba = planar_cmyk_adobe_to_rgba8(&span, DEFAULT_CMYK_ICC).expect("lcms");
        assert!(rgba[0] < 40 && rgba[1] < 40 && rgba[2] < 40, "{rgba:?}");
        assert_eq!(rgba[3], 200);
    }
}
