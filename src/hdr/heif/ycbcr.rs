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
use super::decode::{
    SendReadonlyPtr, parallel_row_chunks_mut, planar_read_sample, planar_scale_from_depth,
    planar_semantic_depth_bits, planar_storage_span_bytes,
};

use crate::hdr::types::{HdrColorProfile, HdrImageMetadata};

#[cfg(feature = "heif-native")]
use crate::hdr::types::{HdrImageBuffer, HdrPixelFormat};
#[cfg(feature = "heif-native")]
use std::sync::Arc;

#[cfg(feature = "heif-native")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HeifYcbcrMatrix {
    Bt601,
    Bt709,
    /// Rec. ITU-R BT.2020 Y'Cb'Cr' to R'G'B' via non-constant luminance Kr/Kb (CICP 9 and 10). True
    /// constant-luminance coding for MC=9 only is not split out; stills usually match the NCL matrix.
    Bt2020Ncl,
    /// CICP matrix_coefficients 0 — no colour difference; replicate luma.
    /// True Y′-only path (R=G=B=Y′) when chroma is absent — not selected from NCLX `matrix_coefficients`
    /// alone (code 0 in HEIF often means “unspecified YUV”, not monochrome video).
    #[allow(dead_code)]
    Monochrome,
}

#[cfg(feature = "heif-native")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HeifYcbcrConvertParams {
    pub matrix: HeifYcbcrMatrix,
    pub nclx_studio_swing: bool,
}

#[cfg(feature = "heif-native")]
pub(crate) fn ycbcr_matrix_from_metadata(
    metadata: &HdrImageMetadata,
    width: usize,
    height: usize,
) -> HeifYcbcrConvertParams {
    HeifYcbcrConvertParams {
        matrix: heif_ycbcr_matrix_from_nclx(metadata, width, height),
        nclx_studio_swing: nclx_limited_range_from_metadata(metadata),
    }
}

#[cfg(feature = "heif-native")]
pub(crate) fn heif_ycbcr_matrix_from_nclx(
    metadata: &HdrImageMetadata,
    y_width: usize,
    y_height: usize,
) -> HeifYcbcrMatrix {
    match &metadata.color_profile {
        HdrColorProfile::Cicp {
            matrix_coefficients: mc,
            ..
        } => match *mc {
            // H.273 matrix 0 = RGB identity (non-YCbCr); HEIF stills sometimes tag 0 / 2 when the
            // encoder meant "unspecified". Interpreting that as monochrome destroys colour, so we
            // default to BT.709 and log a warning (y_width/y_height are kept for diagnostics only).
            0 | 2 => {
                log::warn!(
                    "[heif] CICP matrix_coefficients={mc} unspecified for still image \
                     ({y_width}x{y_height}); defaulting to BT.709 YCbCr matrix"
                );
                HeifYcbcrMatrix::Bt709
            }
            5 | 6 => HeifYcbcrMatrix::Bt601,
            9 | 10 | 12 => HeifYcbcrMatrix::Bt2020Ncl,
            _ => HeifYcbcrMatrix::Bt709,
        },
        _ => HeifYcbcrMatrix::Bt709,
    }
}

#[cfg(feature = "heif-native")]
pub(crate) fn bt2020_ncl_chroma_derived_constants() -> (f32, f32, f32, f32) {
    let kr = 0.2627_f32;
    let kb = 0.0593_f32;
    let kg = 1.0_f32 - kr - kb;
    let k_rr = 2.0_f32 * (1.0_f32 - kr);
    let k_bb = 2.0_f32 * (1.0_f32 - kb);
    let k_gr = -2.0_f32 * kr * (1.0_f32 - kr) / kg;
    let k_gb = -2.0_f32 * kb * (1.0_f32 - kb) / kg;
    (k_rr, k_bb, k_gr, k_gb)
}

/// Converts **electrical** Y′ and centred chroma (**Pb/Pr**, i.e. Cb−mid / Cr−mid in normalized space —
/// JPEG full-pack uses `Cb_norm - 0.5`; narrow-range uses studio `Epb`/`Epr`) to non‑linear R′G′B′.
#[cfg(feature = "heif-native")]
pub(crate) fn ycbcr_linear_to_rgb(ey: f32, pb: f32, pr: f32, matrix: HeifYcbcrMatrix) -> [f32; 3] {
    match matrix {
        HeifYcbcrMatrix::Monochrome => [ey, ey, ey],
        HeifYcbcrMatrix::Bt601 => {
            let r = ey + 1.402_f32 * pr;
            let g = ey - 0.344_136_f32 * pb - 0.714_136_f32 * pr;
            let b = ey + 1.772_f32 * pb;
            [r, g, b]
        }
        HeifYcbcrMatrix::Bt709 => {
            let r = ey + 1.5748_f32 * pr;
            let g = ey - 0.187_324_f32 * pb - 0.468_124_f32 * pr;
            let b = ey + 1.8556_f32 * pb;
            [r, g, b]
        }
        HeifYcbcrMatrix::Bt2020Ncl => {
            let (k_rr, k_bb, k_gr, k_gb) = bt2020_ncl_chroma_derived_constants();
            let r = ey + k_rr * pr;
            let g = ey + k_gb * pb + k_gr * pr;
            let b = ey + k_bb * pb;
            [r, g, b]
        }
    }
}

#[cfg(feature = "heif-native")]
pub(crate) fn nclx_limited_range_from_metadata(metadata: &HdrImageMetadata) -> bool {
    matches!(
        &metadata.color_profile,
        HdrColorProfile::Cicp {
            full_range: false,
            ..
        }
    )
}

/// Limited-range studio swing: Ey = (Y - 16·2^(n-8)) / (219·2^(n-8)), Epb/Epr = (C - 128·2^(n-8)) / (224·2^(n-8)).
#[cfg(feature = "heif-native")]
#[derive(Clone, Copy, Debug)]
struct StudioSwingParams {
    luma_floor: f32,
    luma_span: f32,
    chroma_mid: f32,
    chroma_span: f32,
}

#[cfg(feature = "heif-native")]
fn studio_swing_params_from_semantic_bits(semantic_bits: i32) -> Result<StudioSwingParams, String> {
    let d = semantic_bits.clamp(8, 16);
    let shift = (d - 8).clamp(0, 8) as u32;
    let luma_floor = (16_i32
        .checked_shl(shift)
        .ok_or_else(|| "studio Y offset shift".to_string())?) as f32;
    let luma_span = (219_i32
        .checked_shl(shift)
        .ok_or_else(|| "studio Y span shift".to_string())?) as f32;
    let chroma_mid = (128_i32
        .checked_shl(shift)
        .ok_or_else(|| "studio chroma midpoint shift".to_string())?) as f32;
    let chroma_span = (224_i32
        .checked_shl(shift)
        .ok_or_else(|| "studio chroma span shift".to_string())?) as f32;
    if luma_span <= 0.0 {
        return Err("invalid studio Y span".to_string());
    }
    if chroma_span <= 0.0 {
        return Err("invalid studio chroma span".to_string());
    }
    Ok(StudioSwingParams {
        luma_floor,
        luma_span,
        chroma_mid,
        chroma_span,
    })
}

#[cfg(feature = "heif-native")]
#[inline]
fn studio_luma_to_normalized(code: u32, params: StudioSwingParams) -> f32 {
    (code as f32 - params.luma_floor) / params.luma_span
}

#[cfg(feature = "heif-native")]
#[inline]
fn studio_chroma_to_normalized(code: u32, params: StudioSwingParams) -> f32 {
    (code as f32 - params.chroma_mid) / params.chroma_span
}

#[cfg(feature = "heif-native")]
pub(crate) fn studio_digital_sample_to_normalized(
    code: u32,
    semantic_bits: i32,
    is_luma: bool,
) -> Result<f32, String> {
    let params = studio_swing_params_from_semantic_bits(semantic_bits)?;
    if is_luma {
        Ok(studio_luma_to_normalized(code, params))
    } else {
        Ok(studio_chroma_to_normalized(code, params))
    }
}

#[cfg(feature = "heif-native")]
pub(crate) fn chroma_column_index(
    x: usize,
    chroma: libheif_sys::heif_chroma,
    chroma_plane_w: usize,
) -> usize {
    let subsamp_h = chroma != libheif_sys::heif_chroma_444;
    let ix = if subsamp_h { x / 2 } else { x };
    ix.min(chroma_plane_w.saturating_sub(1))
}

#[cfg(feature = "heif-native")]
pub(crate) fn chroma_row_index(
    y_px: usize,
    chroma: libheif_sys::heif_chroma,
    chroma_plane_h: usize,
) -> usize {
    let subsamp_v = chroma == libheif_sys::heif_chroma_420;
    let iy = if subsamp_v { y_px / 2 } else { y_px };
    iy.min(chroma_plane_h.saturating_sub(1))
}

#[cfg(feature = "heif-native")]
#[inline]
fn fill_row_alpha_u16(
    row_alpha: Option<*const u8>,
    alpha_stride: usize,
    span_alpha: usize,
    scale_alpha: f32,
    y_w: usize,
    row_dst: &mut [f32],
) -> Result<(), String> {
    let Some(ar) = row_alpha else {
        for x_px in 0..y_w {
            row_dst[x_px * 4 + 3] = 1.0;
        }
        return Ok(());
    };
    if span_alpha == 2 && alpha_stride >= y_w * 2 {
        let a_row = unsafe { std::slice::from_raw_parts(ar as *const u16, y_w) };
        let inv = 1.0 / scale_alpha.max(1.0);
        for (x_px, &sample) in a_row.iter().enumerate().take(y_w) {
            row_dst[x_px * 4 + 3] = (sample as f32 * inv).clamp(0.0, 1.0);
        }
        return Ok(());
    }
    for x_px in 0..y_w {
        let av =
            planar_read_sample(ar, x_px, alpha_stride, span_alpha)? as f32 / scale_alpha.max(1.0);
        row_dst[x_px * 4 + 3] = av.clamp(0.0, 1.0);
    }
    Ok(())
}

#[cfg(feature = "heif-native")]
/// Planar YCbCr from libheif. NCLX `full_range: false` uses studio swing; full-pack path uses
/// `Cb/Cr` normalized to `[0, 1]` minus `0.5`. Matrix from CICP: 0 mono, 5/6 BT.601, 9/10 BT.2020 NCL,
/// else BT.709; ICC-only defaults to BT.709.
pub(crate) fn hdr_buffer_from_ycbcr(
    handle: *const libheif_sys::heif_image_handle,
    metadata: HdrImageMetadata,
    image: *const libheif_sys::heif_image,
    chroma: libheif_sys::heif_chroma,
) -> Result<HdrImageBuffer, String> {
    use libheif_sys::{heif_channel_Alpha, heif_channel_Cb, heif_channel_Cr, heif_channel_Y};

    if unsafe { libheif_sys::heif_image_has_channel(image, heif_channel_Y) } == 0 {
        return Err("YCbCr decode: missing luma".to_string());
    }
    if unsafe { libheif_sys::heif_image_has_channel(image, heif_channel_Cb) } == 0
        || unsafe { libheif_sys::heif_image_has_channel(image, heif_channel_Cr) } == 0
    {
        return Err("YCbCr decode: missing chroma plane".to_string());
    }

    let y_w = unsafe { libheif_sys::heif_image_get_width(image, heif_channel_Y) } as usize;
    let y_h = unsafe { libheif_sys::heif_image_get_height(image, heif_channel_Y) } as usize;
    if y_w == 0 || y_h == 0 {
        return Err("YCbCr: zero-sized luma".to_string());
    }
    crate::constants::validate_static_decode_dimensions(y_w as u32, y_h as u32)?;

    let cb_w = unsafe { libheif_sys::heif_image_get_width(image, heif_channel_Cb) } as usize;
    let cb_h = unsafe { libheif_sys::heif_image_get_height(image, heif_channel_Cb) } as usize;

    let mut stride_y = 0usize;
    let ptr_y = unsafe {
        libheif_sys::heif_image_get_plane_readonly2(image, heif_channel_Y, &mut stride_y)
    };
    let mut stride_cb = 0usize;
    let ptr_cb = unsafe {
        libheif_sys::heif_image_get_plane_readonly2(image, heif_channel_Cb, &mut stride_cb)
    };
    let mut stride_cr = 0usize;
    let ptr_cr = unsafe {
        libheif_sys::heif_image_get_plane_readonly2(image, heif_channel_Cr, &mut stride_cr)
    };

    let has_alpha_channel =
        unsafe { libheif_sys::heif_image_has_channel(image, heif_channel_Alpha) != 0 };
    let mut alpha_stride = 0usize;
    let alpha_ptr = if has_alpha_channel {
        unsafe {
            libheif_sys::heif_image_get_plane_readonly2(
                image,
                heif_channel_Alpha,
                &mut alpha_stride,
            )
        }
    } else {
        std::ptr::null()
    };
    let alpha_valid = has_alpha_channel && !alpha_ptr.is_null() && alpha_stride > 0;

    if ptr_y.is_null() || ptr_cb.is_null() || ptr_cr.is_null() {
        return Err("YCbCr: null plane".to_string());
    }

    let span_y = planar_storage_span_bytes(image, heif_channel_Y);
    let span_cb = planar_storage_span_bytes(image, heif_channel_Cb);
    let span_cr = planar_storage_span_bytes(image, heif_channel_Cr);

    let sem_y = planar_semantic_depth_bits(image, handle, heif_channel_Y)?;
    let sem_cb = planar_semantic_depth_bits(image, handle, heif_channel_Cb)?;
    let sem_cr = planar_semantic_depth_bits(image, handle, heif_channel_Cr)?;
    let scale_y = planar_scale_from_depth(sem_y);
    let scale_cb = planar_scale_from_depth(sem_cb);
    let scale_cr = planar_scale_from_depth(sem_cr);
    let nclx_studio_swing = nclx_limited_range_from_metadata(&metadata);
    let studio_swing = if nclx_studio_swing {
        Some((
            studio_swing_params_from_semantic_bits(sem_y)?,
            studio_swing_params_from_semantic_bits(sem_cb)?,
            studio_swing_params_from_semantic_bits(sem_cr)?,
        ))
    } else {
        None
    };

    let span_alpha = if alpha_valid {
        planar_storage_span_bytes(image, heif_channel_Alpha)
    } else {
        0
    };
    let scale_alpha = if alpha_valid {
        planar_scale_from_depth(planar_semantic_depth_bits(
            image,
            handle,
            heif_channel_Alpha,
        )?)
    } else {
        1.0
    };

    let yuv_matrix = heif_ycbcr_matrix_from_nclx(&metadata, y_w, y_h);
    let u16_layout = super::ycbcr_hdr_simd::HdrYcbcrU16RowLayout {
        span_y,
        span_cb,
        span_cr,
        stride_y,
        stride_cb,
        stride_cr,
        y_w,
        cb_w,
        chroma,
    };
    let hdr_simd = (!nclx_studio_swing
        && super::ycbcr_hdr_simd::hdr_ycbcr_u16_simd_eligible(u16_layout, false, yuv_matrix))
    .then(|| super::ycbcr_hdr_simd::HdrYcbcrSimdConvert {
        inv_scale_y: 1.0 / scale_y.max(1.0),
        inv_scale_cb: 1.0 / scale_cb.max(1.0),
        inv_scale_cr: 1.0 / scale_cr.max(1.0),
        matrix: yuv_matrix,
    });
    let hdr_studio_simd = if nclx_studio_swing
        && super::ycbcr_hdr_simd::hdr_ycbcr_u16_simd_eligible(u16_layout, true, yuv_matrix)
    {
        studio_swing.map(|(swing_y, swing_cb, _)| {
            super::ycbcr_hdr_simd::HdrYcbcrStudioSwingParams {
                luma_floor: swing_y.luma_floor,
                luma_inv_span: 1.0 / swing_y.luma_span,
                chroma_mid: swing_cb.chroma_mid,
                chroma_inv_span: 1.0 / swing_cb.chroma_span,
            }
        })
    } else {
        None
    };

    let min_y_need = span_y * y_w.max(1);
    if stride_y < min_y_need {
        return Err("YCbCr: luma stride too small".to_string());
    }
    let min_cb_w = cb_w.max(1);
    if stride_cb < span_cb * min_cb_w || stride_cr < span_cr * min_cb_w {
        return Err("YCbCr: chroma stride too small".to_string());
    }
    if alpha_valid && alpha_stride < span_alpha * y_w.max(1) {
        return Err("YCbCr: alpha stride too small".to_string());
    }

    let ptr_y = SendReadonlyPtr::new(ptr_y);
    let ptr_cb = SendReadonlyPtr::new(ptr_cb);
    let ptr_cr = SendReadonlyPtr::new(ptr_cr);
    let alpha_ptr = if alpha_valid {
        Some(SendReadonlyPtr::new(alpha_ptr))
    } else {
        None
    };

    let rgba_f32_len = super::heif_checked_rgba_buffer_len(y_w, y_h)?;
    let row_len = super::heif_checked_rgba_row_len(y_w)?;
    let mut rgba_f32 = vec![0.0_f32; rgba_f32_len];

    parallel_row_chunks_mut(y_h, row_len, &mut rgba_f32, |y_px, row_dst| {
        let row_y = unsafe { ptr_y.get().byte_add(y_px * stride_y) };

        let yc = chroma_row_index(y_px, chroma, cb_h);
        let row_cb = unsafe { ptr_cb.get().byte_add(yc * stride_cb) };
        let row_cr = unsafe { ptr_cr.get().byte_add(yc * stride_cr) };

        let row_alpha = alpha_ptr.map(|ap| unsafe { ap.get().byte_add(y_px * alpha_stride) });

        if let Some(studio_simd) = hdr_studio_simd {
            let y_u16 = unsafe { std::slice::from_raw_parts(row_y as *const u16, y_w) };
            let cb_u16 = unsafe { std::slice::from_raw_parts(row_cb as *const u16, cb_w) };
            let cr_u16 = unsafe { std::slice::from_raw_parts(row_cr as *const u16, cb_w) };
            // 4:2:0 and 4:2:2 share horizontal chroma upsample (xc = x/2);
            // vertical 4:2:0 subsampling is handled by chroma_row_index above.
            if chroma == libheif_sys::heif_chroma_420 || chroma == libheif_sys::heif_chroma_422 {
                super::ycbcr_hdr_simd::ycbcr_studio_swing_row_420_u16_to_rgba_f32(
                    yuv_matrix,
                    studio_simd,
                    y_u16,
                    cb_u16,
                    cr_u16,
                    row_dst,
                    y_w,
                );
            } else {
                super::ycbcr_hdr_simd::ycbcr_studio_swing_row_444_u16_to_rgba_f32(
                    yuv_matrix,
                    studio_simd,
                    y_u16,
                    cb_u16,
                    cr_u16,
                    row_dst,
                    y_w,
                );
            }
            if let Some(ar) = row_alpha {
                fill_row_alpha_u16(
                    Some(ar),
                    alpha_stride,
                    span_alpha,
                    scale_alpha,
                    y_w,
                    row_dst,
                )?;
            }
            return Ok(());
        }

        if let Some(simd) = hdr_simd {
            let y_u16 = unsafe { std::slice::from_raw_parts(row_y as *const u16, y_w) };
            let cb_u16 = unsafe { std::slice::from_raw_parts(row_cb as *const u16, cb_w) };
            let cr_u16 = unsafe { std::slice::from_raw_parts(row_cr as *const u16, cb_w) };
            if chroma == libheif_sys::heif_chroma_420 || chroma == libheif_sys::heif_chroma_422 {
                super::ycbcr_hdr_simd::ycbcr_full_range_row_420_u16_to_rgba_f32(
                    simd, y_u16, cb_u16, cr_u16, row_dst, y_w,
                );
            } else {
                super::ycbcr_hdr_simd::ycbcr_full_range_row_444_u16_to_rgba_f32(
                    simd, y_u16, cb_u16, cr_u16, row_dst, y_w,
                );
            }
            if let Some(ar) = row_alpha {
                fill_row_alpha_u16(
                    Some(ar),
                    alpha_stride,
                    span_alpha,
                    scale_alpha,
                    y_w,
                    row_dst,
                )?;
            }
            return Ok(());
        }

        if super::ycbcr_hdr_simd::hdr_ycbcr_u16_tight_row_eligible(u16_layout)
            && yuv_matrix != HeifYcbcrMatrix::Monochrome
        {
            let y_u16 = unsafe { std::slice::from_raw_parts(row_y as *const u16, y_w) };
            let cb_u16 = unsafe { std::slice::from_raw_parts(row_cb as *const u16, cb_w) };
            let cr_u16 = unsafe { std::slice::from_raw_parts(row_cr as *const u16, cb_w) };
            let subsamp_h = chroma != libheif_sys::heif_chroma_444;
            for (x_px, &y_sample) in y_u16.iter().enumerate() {
                let y_raw = y_sample as u32;
                let xc = if subsamp_h {
                    (x_px / 2).min(cb_w.saturating_sub(1))
                } else {
                    x_px.min(cb_w.saturating_sub(1))
                };
                let cb_raw = cb_u16[xc] as u32;
                let cr_raw = cr_u16[xc] as u32;

                let [r_, g_, b_] = if let Some((swing_y, swing_cb, swing_cr)) = studio_swing {
                    let ey = studio_luma_to_normalized(y_raw, swing_y);
                    let ecb = studio_chroma_to_normalized(cb_raw, swing_cb);
                    let ecr = studio_chroma_to_normalized(cr_raw, swing_cr);
                    ycbcr_linear_to_rgb(ey, ecb, ecr, yuv_matrix)
                } else {
                    let yv = y_raw as f32 / scale_y.max(1.0);
                    let cbv = cb_raw as f32 / scale_cb.max(1.0);
                    let crv = cr_raw as f32 / scale_cr.max(1.0);
                    ycbcr_linear_to_rgb(yv, cbv - 0.5, crv - 0.5, yuv_matrix)
                };

                let out_idx = x_px * 4;
                row_dst[out_idx] = r_.clamp(0.0, 1.0);
                row_dst[out_idx + 1] = g_.clamp(0.0, 1.0);
                row_dst[out_idx + 2] = b_.clamp(0.0, 1.0);
            }
            fill_row_alpha_u16(
                row_alpha,
                alpha_stride,
                span_alpha,
                scale_alpha,
                y_w,
                row_dst,
            )?;
            return Ok(());
        }

        for x_px in 0..y_w {
            let y_raw = planar_read_sample(row_y, x_px, stride_y, span_y)?;
            let xc = chroma_column_index(x_px, chroma, cb_w);
            let cb_raw = planar_read_sample(row_cb, xc, stride_cb, span_cb)?;
            let cr_raw = planar_read_sample(row_cr, xc, stride_cr, span_cr)?;

            let [r_, g_, b_] = if let Some((swing_y, swing_cb, swing_cr)) = studio_swing {
                let ey = studio_luma_to_normalized(y_raw, swing_y);
                let ecb = studio_chroma_to_normalized(cb_raw, swing_cb);
                let ecr = studio_chroma_to_normalized(cr_raw, swing_cr);
                ycbcr_linear_to_rgb(ey, ecb, ecr, yuv_matrix)
            } else {
                let yv = y_raw as f32 / scale_y.max(1.0);
                let cbv = cb_raw as f32 / scale_cb.max(1.0);
                let crv = cr_raw as f32 / scale_cr.max(1.0);
                ycbcr_linear_to_rgb(yv, cbv - 0.5, crv - 0.5, yuv_matrix)
            };

            let out_idx = x_px * 4;
            row_dst[out_idx] = r_.clamp(0.0, 1.0);
            row_dst[out_idx + 1] = g_.clamp(0.0, 1.0);
            row_dst[out_idx + 2] = b_.clamp(0.0, 1.0);

            if let Some(ar) = row_alpha {
                let av = planar_read_sample(ar, x_px, alpha_stride, span_alpha)? as f32
                    / scale_alpha.max(1.0);
                row_dst[out_idx + 3] = av.clamp(0.0, 1.0);
            } else {
                row_dst[out_idx + 3] = 1.0;
            }
        }
        Ok(())
    })?;

    let color_space = metadata.color_space_hint();
    Ok(HdrImageBuffer {
        width: y_w as u32,
        height: y_h as u32,
        format: HdrPixelFormat::Rgba32Float,
        color_space,
        metadata,
        rgba_f32: Arc::new(rgba_f32),
    })
}

#[cfg(feature = "heif-native")]
pub(crate) fn ycbcr_matrix_from_heif_handle(
    handle: *const libheif_sys::heif_image_handle,
) -> HeifYcbcrConvertParams {
    use super::brand::heif_nclx_to_metadata;

    let width = unsafe { libheif_sys::heif_image_handle_get_width(handle) }.max(0) as usize;
    let height = unsafe { libheif_sys::heif_image_handle_get_height(handle) }.max(0) as usize;
    let mut nclx_ptr = std::ptr::null_mut();
    let status =
        unsafe { libheif_sys::heif_image_handle_get_nclx_color_profile(handle, &mut nclx_ptr) };
    if status.code == libheif_sys::heif_error_Ok && !nclx_ptr.is_null() {
        let nclx_guard = unsafe { libheif_sys::HeifNclxProfileGuard::from_ptr(nclx_ptr) };
        let nclx = nclx_guard.as_ref();
        let metadata = heif_nclx_to_metadata(
            nclx.color_primaries as u16,
            nclx.transfer_characteristics as u16,
            nclx.matrix_coefficients as u16,
            nclx.full_range_flag != 0,
        );
        return ycbcr_matrix_from_metadata(&metadata, width, height);
    }
    // No NCLX: assume full-range JPEG-style pack (common HEIF still default).
    HeifYcbcrConvertParams {
        matrix: HeifYcbcrMatrix::Bt709,
        nclx_studio_swing: false,
    }
}

#[cfg(feature = "heif-native")]
pub(crate) fn ycbcr_image_to_rgba8(
    image: *const libheif_sys::heif_image,
    chroma: libheif_sys::heif_chroma,
    convert: HeifYcbcrConvertParams,
) -> Result<crate::loader::DecodedImage, String> {
    let y_w = unsafe { libheif_sys::heif_image_get_width(image, libheif_sys::heif_channel_Y) };
    let y_h = unsafe { libheif_sys::heif_image_get_height(image, libheif_sys::heif_channel_Y) };
    if y_w <= 0 || y_h <= 0 {
        return Err("libheif YCbCr image has zero luma size".to_string());
    }
    crate::constants::validate_static_decode_dimensions(y_w as u32, y_h as u32)?;
    let width = y_w as u32;
    let height = y_h as u32;

    let mut y_stride = 0_usize;
    let y_plane = unsafe {
        libheif_sys::heif_image_get_plane_readonly2(
            image,
            libheif_sys::heif_channel_Y,
            &mut y_stride,
        )
    };
    let mut cb_stride = 0_usize;
    let cb_plane = unsafe {
        libheif_sys::heif_image_get_plane_readonly2(
            image,
            libheif_sys::heif_channel_Cb,
            &mut cb_stride,
        )
    };
    let mut cr_stride = 0_usize;
    let cr_plane = unsafe {
        libheif_sys::heif_image_get_plane_readonly2(
            image,
            libheif_sys::heif_channel_Cr,
            &mut cr_stride,
        )
    };
    if y_plane.is_null() || cb_plane.is_null() || cr_plane.is_null() {
        return Err("libheif YCbCr image missing planes".to_string());
    }

    let HeifYcbcrConvertParams {
        matrix,
        nclx_studio_swing,
    } = convert;
    let subsample_h = chroma != libheif_sys::heif_chroma_444;
    let subsample_v = chroma == libheif_sys::heif_chroma_420;
    let chroma_row_len = if subsample_h {
        (width as usize).div_ceil(2)
    } else {
        width as usize
    };
    let w = width as usize;
    let h = height as usize;
    let rgba_len = super::heif_checked_rgba_buffer_len(w, h)?;
    let row_len = super::heif_checked_rgba_row_len(w)?;
    let mut rgba = vec![0_u8; rgba_len];
    let simd_bt709_full_range = matrix == HeifYcbcrMatrix::Bt709 && !nclx_studio_swing;
    let simd_bt709_limited_range = matrix == HeifYcbcrMatrix::Bt709 && nclx_studio_swing;
    let y_plane = SendReadonlyPtr::new(y_plane);
    let cb_plane = SendReadonlyPtr::new(cb_plane);
    let cr_plane = SendReadonlyPtr::new(cr_plane);
    parallel_row_chunks_mut(h, row_len, &mut rgba, |y, row_dst| {
        let y_row = unsafe { std::slice::from_raw_parts(y_plane.get().add(y * y_stride), w) };
        let cb_y = if subsample_v { y / 2 } else { y };
        let cb_row = unsafe {
            std::slice::from_raw_parts(cb_plane.get().add(cb_y * cb_stride), chroma_row_len)
        };
        let cr_row = unsafe {
            std::slice::from_raw_parts(cr_plane.get().add(cb_y * cr_stride), chroma_row_len)
        };

        if simd_bt709_full_range && subsample_v && subsample_h {
            super::ycbcr_simd::ycbcr_full_range_bt709_row_420_to_rgba8(
                y_row, cb_row, cr_row, row_dst, w,
            );
            return Ok(());
        }
        if simd_bt709_full_range && !subsample_h {
            super::ycbcr_simd::ycbcr_full_range_bt709_row_444_to_rgba8(
                y_row, cb_row, cr_row, row_dst, w,
            );
            return Ok(());
        }
        if simd_bt709_limited_range && subsample_v && subsample_h {
            super::ycbcr_simd::ycbcr_limited_range_bt709_row_420_to_rgba8(
                y_row, cb_row, cr_row, row_dst, w,
            );
            return Ok(());
        }
        if simd_bt709_limited_range && !subsample_h {
            super::ycbcr_simd::ycbcr_limited_range_bt709_row_444_to_rgba8(
                y_row, cb_row, cr_row, row_dst, w,
            );
            return Ok(());
        }

        for (x, &y_sample) in y_row.iter().enumerate().take(w) {
            let xc = if subsample_h { x / 2 } else { x };
            let [r, g, b] = if nclx_studio_swing {
                let yy = studio_digital_sample_to_normalized(y_sample as u32, 8, true)?;
                let cb = studio_digital_sample_to_normalized(cb_row[xc] as u32, 8, false)?;
                let cr = studio_digital_sample_to_normalized(cr_row[xc] as u32, 8, false)?;
                ycbcr_linear_to_rgb(yy, cb, cr, matrix)
            } else {
                let yy = y_sample as f32 / 255.0;
                let cb = cb_row[xc] as f32 / 255.0 - 0.5;
                let cr = cr_row[xc] as f32 / 255.0 - 0.5;
                ycbcr_linear_to_rgb(yy, cb, cr, matrix)
            };
            let dst = x * 4;
            row_dst[dst] = (r.clamp(0.0, 1.0) * 255.0).round() as u8;
            row_dst[dst + 1] = (g.clamp(0.0, 1.0) * 255.0).round() as u8;
            row_dst[dst + 2] = (b.clamp(0.0, 1.0) * 255.0).round() as u8;
            row_dst[dst + 3] = 255;
        }
        Ok(())
    })?;
    Ok(crate::loader::DecodedImage::new(width, height, rgba))
}
