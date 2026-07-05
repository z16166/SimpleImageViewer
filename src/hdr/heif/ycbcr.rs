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
    planar_read_sample, planar_scale_from_depth, planar_semantic_depth_bits,
    planar_storage_span_bytes,
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
            // H.273 matrix 0 = RGB identity (non‑YCbCr); **HEIF stills** sometimes tag 0 / 2 when the
            // encoder meant “unspecified”. Interpreting that as monochrome destroys colour — use a
            // simple SD vs HD **luma resolution** split (common broadcast rule of thumb).
            0 | 2 => {
                let hdish = y_width >= 1280 || y_height >= 720;
                if hdish {
                    HeifYcbcrMatrix::Bt709
                } else {
                    HeifYcbcrMatrix::Bt601
                }
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
pub(crate) fn studio_digital_sample_to_normalized(
    code: u32,
    semantic_bits: i32,
    is_luma: bool,
) -> Result<f32, String> {
    let d = semantic_bits.clamp(8, 16);
    let shift = (d - 8).clamp(0, 8) as u32;
    let y_floor = (16_i32
        .checked_shl(shift)
        .ok_or_else(|| "studio Y offset shift".to_string())?) as f32;
    let y_span = (219_i32
        .checked_shl(shift)
        .ok_or_else(|| "studio Y span shift".to_string())?) as f32;
    let c_mid = (128_i32
        .checked_shl(shift)
        .ok_or_else(|| "studio chroma midpoint shift".to_string())?) as f32;
    let c_span = (224_i32
        .checked_shl(shift)
        .ok_or_else(|| "studio chroma span shift".to_string())?) as f32;

    if is_luma {
        if y_span <= 0.0 {
            return Err("invalid studio Y span".to_string());
        }
        Ok((code as f32 - y_floor) / y_span)
    } else if c_span <= 0.0 {
        Err("invalid studio chroma span".to_string())
    } else {
        Ok((code as f32 - c_mid) / c_span)
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
/// Planar YCbCr from libheif. NCLX `full_range: false` uses studio swing; full-pack path uses
/// `Cb/Cr` normalized to `[0, 1]` minus `0.5`. Matrix from CICP: 0 mono, 5/6 BT.601, 9/10 BT.2020 NCL,
/// else BT.709; ICC-only defaults to BT.709.
pub(crate) fn hdr_buffer_from_ycbcr(
    handle: *const libheif_sys::heif_image_handle,
    metadata: &HdrImageMetadata,
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

    let scale_y =
        planar_scale_from_depth(planar_semantic_depth_bits(image, handle, heif_channel_Y)?);
    let scale_cb =
        planar_scale_from_depth(planar_semantic_depth_bits(image, handle, heif_channel_Cb)?);
    let scale_cr =
        planar_scale_from_depth(planar_semantic_depth_bits(image, handle, heif_channel_Cr)?);

    let sem_y = planar_semantic_depth_bits(image, handle, heif_channel_Y)?;
    let sem_cb = planar_semantic_depth_bits(image, handle, heif_channel_Cb)?;
    let sem_cr = planar_semantic_depth_bits(image, handle, heif_channel_Cr)?;
    let nclx_studio_swing = nclx_limited_range_from_metadata(metadata);

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

    let yuv_matrix = heif_ycbcr_matrix_from_nclx(metadata, y_w, y_h);

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

    let mut rgba_f32 = Vec::with_capacity(y_w * y_h * 4);

    for y_px in 0..y_h {
        let row_y = unsafe { ptr_y.byte_add(y_px * stride_y) };

        let yc = chroma_row_index(y_px, chroma, cb_h);
        let row_cb = unsafe { ptr_cb.byte_add(yc * stride_cb) };
        let row_cr = unsafe { ptr_cr.byte_add(yc * stride_cr) };

        let row_alpha = alpha_valid.then(|| unsafe { alpha_ptr.byte_add(y_px * alpha_stride) });

        for x_px in 0..y_w {
            let y_raw = planar_read_sample(row_y, x_px, stride_y, span_y)?;
            let xc = chroma_column_index(x_px, chroma, cb_w);
            let cb_raw = planar_read_sample(row_cb, xc, stride_cb, span_cb)?;
            let cr_raw = planar_read_sample(row_cr, xc, stride_cr, span_cr)?;

            let [r_, g_, b_] = if nclx_studio_swing {
                let ey = studio_digital_sample_to_normalized(y_raw, sem_y, true)?;
                let ecb = studio_digital_sample_to_normalized(cb_raw, sem_cb, false)?;
                let ecr = studio_digital_sample_to_normalized(cr_raw, sem_cr, false)?;
                ycbcr_linear_to_rgb(ey, ecb, ecr, yuv_matrix)
            } else {
                let yv = y_raw as f32 / scale_y.max(1.0);
                let cbv = cb_raw as f32 / scale_cb.max(1.0);
                let crv = cr_raw as f32 / scale_cr.max(1.0);
                ycbcr_linear_to_rgb(yv, cbv - 0.5, crv - 0.5, yuv_matrix)
            };

            rgba_f32.push(r_.clamp(0.0, 1.0));
            rgba_f32.push(g_.clamp(0.0, 1.0));
            rgba_f32.push(b_.clamp(0.0, 1.0));

            if let Some(ar) = row_alpha {
                let av = planar_read_sample(ar, x_px, alpha_stride, span_alpha)? as f32
                    / scale_alpha.max(1.0);
                rgba_f32.push(av.clamp(0.0, 1.0));
            } else {
                rgba_f32.push(1.0);
            }
        }
    }

    let color_space = metadata.color_space_hint();
    Ok(HdrImageBuffer {
        width: y_w as u32,
        height: y_h as u32,
        format: HdrPixelFormat::Rgba32Float,
        color_space,
        metadata: metadata.clone(),
        rgba_f32: Arc::new(rgba_f32),
    })
}

#[cfg(feature = "heif-native")]
pub(crate) fn ycbcr_matrix_from_heif_handle(
    handle: *const libheif_sys::heif_image_handle,
) -> (HeifYcbcrMatrix, bool) {
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
        return (
            heif_ycbcr_matrix_from_nclx(&metadata, width, height),
            nclx_limited_range_from_metadata(&metadata),
        );
    }
    // No NCLX: assume full-range JPEG-style pack (common HEIF still default).
    (HeifYcbcrMatrix::Bt709, false)
}

#[cfg(feature = "heif-native")]
pub(crate) fn ycbcr_image_to_rgba8(
    handle: *const libheif_sys::heif_image_handle,
    image: *const libheif_sys::heif_image,
    chroma: libheif_sys::heif_chroma,
) -> Result<crate::loader::DecodedImage, String> {
    let y_w = unsafe { libheif_sys::heif_image_get_width(image, libheif_sys::heif_channel_Y) };
    let y_h = unsafe { libheif_sys::heif_image_get_height(image, libheif_sys::heif_channel_Y) };
    if y_w <= 0 || y_h <= 0 {
        return Err("libheif YCbCr image has zero luma size".to_string());
    }
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

    let (matrix, nclx_studio_swing) = ycbcr_matrix_from_heif_handle(handle);
    let subsample_h = chroma != libheif_sys::heif_chroma_444;
    let subsample_v = chroma == libheif_sys::heif_chroma_420;
    let chroma_row_len = if subsample_h {
        (width as usize).div_ceil(2)
    } else {
        width as usize
    };
    let mut rgba = vec![0_u8; width as usize * height as usize * 4];
    for y in 0..height as usize {
        let y_row =
            unsafe { std::slice::from_raw_parts(y_plane.add(y * y_stride), width as usize) };
        let cb_y = if subsample_v { y / 2 } else { y };
        let cb_row =
            unsafe { std::slice::from_raw_parts(cb_plane.add(cb_y * cb_stride), chroma_row_len) };
        let cr_row =
            unsafe { std::slice::from_raw_parts(cr_plane.add(cb_y * cr_stride), chroma_row_len) };
        for (x, &y_sample) in y_row.iter().enumerate().take(width as usize) {
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
            let dst = (y * width as usize + x) * 4;
            rgba[dst] = (r.clamp(0.0, 1.0) * 255.0).round() as u8;
            rgba[dst + 1] = (g.clamp(0.0, 1.0) * 255.0).round() as u8;
            rgba[dst + 2] = (b.clamp(0.0, 1.0) * 255.0).round() as u8;
            rgba[dst + 3] = 255;
        }
    }
    Ok(crate::loader::DecodedImage::new(width, height, rgba))
}
