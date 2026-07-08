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

//! SIMD full-range planar YCbCr (10/12-bit u16 storage) -> RGBA f32 for HEIF HDR decode.

use super::ycbcr::{HeifYcbcrMatrix, bt2020_ncl_chroma_derived_constants};

#[cfg(target_arch = "x86_64")]
use core::arch::x86_64::*;

#[cfg(target_arch = "aarch64")]
use core::arch::aarch64::*;

const CHROMA_CENTER: f32 = 0.5;
#[cfg(target_arch = "x86_64")]
const PIXELS_PER_SSE41_STEP: usize = 4;
#[cfg(target_arch = "x86_64")]
const PIXELS_PER_AVX2_STEP: usize = 4;
#[cfg(target_arch = "aarch64")]
const PIXELS_PER_NEON_STEP: usize = 4;

#[derive(Clone, Copy, Debug)]
pub(crate) struct HdrYcbcrSimdConvert {
    pub inv_scale_y: f32,
    pub inv_scale_cb: f32,
    pub inv_scale_cr: f32,
    pub matrix: HeifYcbcrMatrix,
}

#[derive(Clone, Copy, Debug)]
struct YcbcrMatrixSimdCoeffs {
    pr_to_r: f32,
    pb_to_g: f32,
    pr_to_g: f32,
    pb_to_b: f32,
}

impl HdrYcbcrSimdConvert {
    fn matrix_coeffs(self) -> Option<YcbcrMatrixSimdCoeffs> {
        match self.matrix {
            HeifYcbcrMatrix::Monochrome => None,
            HeifYcbcrMatrix::Bt601 => Some(YcbcrMatrixSimdCoeffs {
                pr_to_r: 1.402,
                pb_to_g: -0.344_136,
                pr_to_g: -0.714_136,
                pb_to_b: 1.772,
            }),
            HeifYcbcrMatrix::Bt709 => Some(YcbcrMatrixSimdCoeffs {
                pr_to_r: 1.5748,
                pb_to_g: -0.187_324,
                pr_to_g: -0.468_124,
                pb_to_b: 1.8556,
            }),
            HeifYcbcrMatrix::Bt2020Ncl => {
                let (k_rr, k_bb, k_gr, k_gb) = bt2020_ncl_chroma_derived_constants();
                Some(YcbcrMatrixSimdCoeffs {
                    pr_to_r: k_rr,
                    pb_to_g: k_gb,
                    pr_to_g: k_gr,
                    pb_to_b: k_bb,
                })
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct HdrYcbcrU16RowLayout {
    pub span_y: usize,
    pub span_cb: usize,
    pub span_cr: usize,
    pub stride_y: usize,
    pub stride_cb: usize,
    pub stride_cr: usize,
    pub y_w: usize,
    pub cb_w: usize,
    pub chroma: libheif_sys::heif_chroma,
}

#[derive(Clone, Copy)]
struct HdrYcbcrRowSimdCtx {
    convert: HdrYcbcrSimdConvert,
    coeffs: YcbcrMatrixSimdCoeffs,
}

struct HdrYcbcrU16Row<'a> {
    y: &'a [u16],
    cb: &'a [u16],
    cr: &'a [u16],
    dst: &'a mut [f32],
    width: usize,
}

impl HdrYcbcrRowSimdCtx {
    fn new(convert: HdrYcbcrSimdConvert) -> Option<Self> {
        Some(Self {
            coeffs: convert.matrix_coeffs()?,
            convert,
        })
    }
}

/// Tight u16 rows, full-range pack, supported matrix/chroma only.
pub(crate) fn hdr_ycbcr_u16_simd_eligible(
    layout: HdrYcbcrU16RowLayout,
    _studio_swing: bool,
    matrix: HeifYcbcrMatrix,
) -> bool {
    hdr_ycbcr_u16_tight_row_eligible(layout) && matrix != HeifYcbcrMatrix::Monochrome
}

/// Tight u16 row layout for SIMD or inline scalar conversion (420/422/444).
pub(crate) fn hdr_ycbcr_u16_tight_row_eligible(layout: HdrYcbcrU16RowLayout) -> bool {
    if layout.span_y != 2 || layout.span_cb != 2 || layout.span_cr != 2 {
        return false;
    }
    if layout.stride_y != layout.y_w * 2
        || layout.stride_cb != layout.cb_w * 2
        || layout.stride_cr != layout.cb_w * 2
    {
        return false;
    }
    matches!(
        layout.chroma,
        c if c == libheif_sys::heif_chroma_444
            || c == libheif_sys::heif_chroma_420
            || c == libheif_sys::heif_chroma_422
    )
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct HdrYcbcrStudioSwingParams {
    pub luma_floor: f32,
    pub luma_inv_span: f32,
    pub chroma_mid: f32,
    pub chroma_inv_span: f32,
}

/// Studio swing limited-range u16 row -> RGBA f32 (444).
pub(crate) fn ycbcr_studio_swing_row_444_u16_to_rgba_f32(
    matrix: HeifYcbcrMatrix,
    swing: HdrYcbcrStudioSwingParams,
    y_row: &[u16],
    cb_row: &[u16],
    cr_row: &[u16],
    dst: &mut [f32],
    width: usize,
) {
    if y_row.len() < width
        || cb_row.len() < width
        || cr_row.len() < width
        || dst.len() < width.saturating_mul(4)
    {
        return;
    }
    let Some(coeffs) = HdrYcbcrSimdConvert {
        inv_scale_y: 1.0,
        inv_scale_cb: 1.0,
        inv_scale_cr: 1.0,
        matrix,
    }
    .matrix_coeffs() else {
        return;
    };
    let ctx = HdrYcbcrStudioRowSimdCtx { coeffs, swing };
    let mut row = HdrYcbcrU16Row {
        y: y_row,
        cb: cb_row,
        cr: cr_row,
        dst,
        width,
    };
    let mut x = 0;
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("sse4.1") {
            unsafe {
                ycbcr_studio_swing_row_444_u16_sse41(&ctx, &mut row, &mut x);
            }
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        unsafe {
            ycbcr_studio_swing_row_444_u16_neon(&ctx, &mut row, &mut x);
        }
    }
    while x < row.width {
        write_studio_swing_u16_pixel(ctx, row.dst, x, row.y[x], row.cb[x], row.cr[x], matrix);
        x += 1;
    }
}

/// Studio swing limited-range u16 row -> RGBA f32 (420).
pub(crate) fn ycbcr_studio_swing_row_420_u16_to_rgba_f32(
    matrix: HeifYcbcrMatrix,
    swing: HdrYcbcrStudioSwingParams,
    y_row: &[u16],
    cb_row: &[u16],
    cr_row: &[u16],
    dst: &mut [f32],
    width: usize,
) {
    let chroma_len = width.div_ceil(2);
    if y_row.len() < width
        || cb_row.len() < chroma_len
        || cr_row.len() < chroma_len
        || dst.len() < width.saturating_mul(4)
    {
        return;
    }
    let Some(coeffs) = HdrYcbcrSimdConvert {
        inv_scale_y: 1.0,
        inv_scale_cb: 1.0,
        inv_scale_cr: 1.0,
        matrix,
    }
    .matrix_coeffs() else {
        return;
    };
    let ctx = HdrYcbcrStudioRowSimdCtx { coeffs, swing };
    let mut row = HdrYcbcrU16Row {
        y: y_row,
        cb: cb_row,
        cr: cr_row,
        dst,
        width,
    };
    let mut x = 0;
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("sse4.1") {
            unsafe {
                ycbcr_studio_swing_row_420_u16_sse41(&ctx, &mut row, &mut x);
            }
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        unsafe {
            ycbcr_studio_swing_row_420_u16_neon(&ctx, &mut row, &mut x);
        }
    }
    while x < row.width {
        let xc = x / 2;
        write_studio_swing_u16_pixel(ctx, row.dst, x, row.y[x], row.cb[xc], row.cr[xc], matrix);
        x += 1;
    }
}

#[derive(Clone, Copy)]
struct HdrYcbcrStudioRowSimdCtx {
    coeffs: YcbcrMatrixSimdCoeffs,
    swing: HdrYcbcrStudioSwingParams,
}

#[inline]
fn write_studio_swing_u16_pixel(
    ctx: HdrYcbcrStudioRowSimdCtx,
    dst: &mut [f32],
    x: usize,
    y: u16,
    cb: u16,
    cr: u16,
    matrix: HeifYcbcrMatrix,
) {
    let yy = (y as f32 - ctx.swing.luma_floor) * ctx.swing.luma_inv_span;
    let pb = (cb as f32 - ctx.swing.chroma_mid) * ctx.swing.chroma_inv_span;
    let pr = (cr as f32 - ctx.swing.chroma_mid) * ctx.swing.chroma_inv_span;
    let [r, g, b] = super::ycbcr::ycbcr_linear_to_rgb(yy, pb, pr, matrix);
    let base = x * 4;
    dst[base] = r.clamp(0.0, 1.0);
    dst[base + 1] = g.clamp(0.0, 1.0);
    dst[base + 2] = b.clamp(0.0, 1.0);
    dst[base + 3] = 1.0;
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn ycbcr_studio_swing_row_444_u16_sse41(
    ctx: &HdrYcbcrStudioRowSimdCtx,
    row: &mut HdrYcbcrU16Row<'_>,
    x: &mut usize,
) {
    unsafe {
        let luma_floor = _mm_set1_ps(ctx.swing.luma_floor);
        let luma_inv = _mm_set1_ps(ctx.swing.luma_inv_span);
        let chroma_mid = _mm_set1_ps(ctx.swing.chroma_mid);
        let chroma_inv = _mm_set1_ps(ctx.swing.chroma_inv_span);
        let k_pr_r = _mm_set1_ps(ctx.coeffs.pr_to_r);
        let k_pb_g = _mm_set1_ps(ctx.coeffs.pb_to_g);
        let k_pr_g = _mm_set1_ps(ctx.coeffs.pr_to_g);
        let k_pb_b = _mm_set1_ps(ctx.coeffs.pb_to_b);
        let zero = _mm_setzero_ps();
        let one = _mm_set1_ps(1.0);

        while *x + PIXELS_PER_SSE41_STEP <= row.width {
            let y = _mm_cvtepi32_ps(_mm_cvtepu16_epi32(_mm_loadl_epi64(
                row.y.as_ptr().add(*x) as *const __m128i
            )));
            let cb = _mm_cvtepi32_ps(_mm_cvtepu16_epi32(_mm_loadl_epi64(
                row.cb.as_ptr().add(*x) as *const __m128i
            )));
            let cr = _mm_cvtepi32_ps(_mm_cvtepu16_epi32(_mm_loadl_epi64(
                row.cr.as_ptr().add(*x) as *const __m128i
            )));

            let yy = _mm_mul_ps(_mm_sub_ps(y, luma_floor), luma_inv);
            let pb = _mm_mul_ps(_mm_sub_ps(cb, chroma_mid), chroma_inv);
            let pr = _mm_mul_ps(_mm_sub_ps(cr, chroma_mid), chroma_inv);

            let rf = _mm_min_ps(
                _mm_max_ps(_mm_add_ps(yy, _mm_mul_ps(k_pr_r, pr)), zero),
                one,
            );
            let gf = _mm_min_ps(
                _mm_max_ps(
                    _mm_add_ps(
                        _mm_add_ps(yy, _mm_mul_ps(k_pb_g, pb)),
                        _mm_mul_ps(k_pr_g, pr),
                    ),
                    zero,
                ),
                one,
            );
            let bf = _mm_min_ps(
                _mm_max_ps(_mm_add_ps(yy, _mm_mul_ps(k_pb_b, pb)), zero),
                one,
            );

            let mut r = [0.0_f32; 4];
            let mut g = [0.0_f32; 4];
            let mut b = [0.0_f32; 4];
            _mm_storeu_ps(r.as_mut_ptr(), rf);
            _mm_storeu_ps(g.as_mut_ptr(), gf);
            _mm_storeu_ps(b.as_mut_ptr(), bf);
            store_rgba_f32x4(row.dst, *x, r, g, b);
            *x += PIXELS_PER_SSE41_STEP;
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn ycbcr_studio_swing_row_420_u16_sse41(
    ctx: &HdrYcbcrStudioRowSimdCtx,
    row: &mut HdrYcbcrU16Row<'_>,
    x: &mut usize,
) {
    unsafe {
        let luma_floor = _mm_set1_ps(ctx.swing.luma_floor);
        let luma_inv = _mm_set1_ps(ctx.swing.luma_inv_span);
        let chroma_mid = _mm_set1_ps(ctx.swing.chroma_mid);
        let chroma_inv = _mm_set1_ps(ctx.swing.chroma_inv_span);
        let k_pr_r = _mm_set1_ps(ctx.coeffs.pr_to_r);
        let k_pb_g = _mm_set1_ps(ctx.coeffs.pb_to_g);
        let k_pr_g = _mm_set1_ps(ctx.coeffs.pr_to_g);
        let k_pb_b = _mm_set1_ps(ctx.coeffs.pb_to_b);
        let zero = _mm_setzero_ps();
        let one = _mm_set1_ps(1.0);
        let chroma_len = row.width.div_ceil(2);
        while *x < row.width {
            if *x + PIXELS_PER_SSE41_STEP <= row.width && ycbcr420_chroma_load2_fits(*x, chroma_len)
            {
                let xc = *x / 2;
                let cb0 = row.cb[xc];
                let cb1 = row.cb[xc + 1];
                let cr0 = row.cr[xc];
                let cr1 = row.cr[xc + 1];
                let y = _mm_cvtepi32_ps(_mm_cvtepu16_epi32(_mm_loadl_epi64(
                    row.y.as_ptr().add(*x) as *const __m128i,
                )));
                let cb = _mm_cvtepi32_ps(_mm_cvtepu16_epi32(_mm_setr_epi16(
                    cb0 as i16, cb0 as i16, cb1 as i16, cb1 as i16, 0, 0, 0, 0,
                )));
                let cr = _mm_cvtepi32_ps(_mm_cvtepu16_epi32(_mm_setr_epi16(
                    cr0 as i16, cr0 as i16, cr1 as i16, cr1 as i16, 0, 0, 0, 0,
                )));

                let yy = _mm_mul_ps(_mm_sub_ps(y, luma_floor), luma_inv);
                let pb = _mm_mul_ps(_mm_sub_ps(cb, chroma_mid), chroma_inv);
                let pr = _mm_mul_ps(_mm_sub_ps(cr, chroma_mid), chroma_inv);

                let rf = _mm_min_ps(
                    _mm_max_ps(_mm_add_ps(yy, _mm_mul_ps(k_pr_r, pr)), zero),
                    one,
                );
                let gf = _mm_min_ps(
                    _mm_max_ps(
                        _mm_add_ps(
                            _mm_add_ps(yy, _mm_mul_ps(k_pb_g, pb)),
                            _mm_mul_ps(k_pr_g, pr),
                        ),
                        zero,
                    ),
                    one,
                );
                let bf = _mm_min_ps(
                    _mm_max_ps(_mm_add_ps(yy, _mm_mul_ps(k_pb_b, pb)), zero),
                    one,
                );

                let mut r = [0.0_f32; 4];
                let mut g = [0.0_f32; 4];
                let mut b = [0.0_f32; 4];
                _mm_storeu_ps(r.as_mut_ptr(), rf);
                _mm_storeu_ps(g.as_mut_ptr(), gf);
                _mm_storeu_ps(b.as_mut_ptr(), bf);
                store_rgba_f32x4(row.dst, *x, r, g, b);
                *x += PIXELS_PER_SSE41_STEP;
            } else {
                break;
            }
        }
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn ycbcr_studio_swing_row_444_u16_neon(
    ctx: &HdrYcbcrStudioRowSimdCtx,
    row: &mut HdrYcbcrU16Row<'_>,
    x: &mut usize,
) {
    let luma_floor = vdupq_n_f32(ctx.swing.luma_floor);
    let luma_inv = vdupq_n_f32(ctx.swing.luma_inv_span);
    let chroma_mid = vdupq_n_f32(ctx.swing.chroma_mid);
    let chroma_inv = vdupq_n_f32(ctx.swing.chroma_inv_span);
    let k_pr_r = vdupq_n_f32(ctx.coeffs.pr_to_r);
    let k_pb_g = vdupq_n_f32(ctx.coeffs.pb_to_g);
    let k_pr_g = vdupq_n_f32(ctx.coeffs.pr_to_g);
    let k_pb_b = vdupq_n_f32(ctx.coeffs.pb_to_b);
    let zero = vdupq_n_f32(0.0);
    let one = vdupq_n_f32(1.0);

    unsafe {
        while *x + PIXELS_PER_NEON_STEP <= row.width {
            let y = vcvtq_f32_u32(vmovl_u16(vget_low_u16(vld1q_u16(row.y.as_ptr().add(*x)))));
            let cb = vcvtq_f32_u32(vmovl_u16(vget_low_u16(vld1q_u16(row.cb.as_ptr().add(*x)))));
            let cr = vcvtq_f32_u32(vmovl_u16(vget_low_u16(vld1q_u16(row.cr.as_ptr().add(*x)))));

            let yy = vmulq_f32(vsubq_f32(y, luma_floor), luma_inv);
            let pb = vmulq_f32(vsubq_f32(cb, chroma_mid), chroma_inv);
            let pr = vmulq_f32(vsubq_f32(cr, chroma_mid), chroma_inv);

            let rf = vminq_f32(vmaxq_f32(vaddq_f32(yy, vmulq_f32(k_pr_r, pr)), zero), one);
            let gf = vminq_f32(
                vmaxq_f32(
                    vaddq_f32(vaddq_f32(yy, vmulq_f32(k_pb_g, pb)), vmulq_f32(k_pr_g, pr)),
                    zero,
                ),
                one,
            );
            let bf = vminq_f32(vmaxq_f32(vaddq_f32(yy, vmulq_f32(k_pb_b, pb)), zero), one);

            let mut r = [0.0_f32; 4];
            let mut g = [0.0_f32; 4];
            let mut b = [0.0_f32; 4];
            vst1q_f32(r.as_mut_ptr(), rf);
            vst1q_f32(g.as_mut_ptr(), gf);
            vst1q_f32(b.as_mut_ptr(), bf);
            store_rgba_f32x4(row.dst, *x, r, g, b);
            *x += PIXELS_PER_NEON_STEP;
        }
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn ycbcr_studio_swing_row_420_u16_neon(
    ctx: &HdrYcbcrStudioRowSimdCtx,
    row: &mut HdrYcbcrU16Row<'_>,
    x: &mut usize,
) {
    let luma_floor = vdupq_n_f32(ctx.swing.luma_floor);
    let luma_inv = vdupq_n_f32(ctx.swing.luma_inv_span);
    let chroma_mid = vdupq_n_f32(ctx.swing.chroma_mid);
    let chroma_inv = vdupq_n_f32(ctx.swing.chroma_inv_span);
    let k_pr_r = vdupq_n_f32(ctx.coeffs.pr_to_r);
    let k_pb_g = vdupq_n_f32(ctx.coeffs.pb_to_g);
    let k_pr_g = vdupq_n_f32(ctx.coeffs.pr_to_g);
    let k_pb_b = vdupq_n_f32(ctx.coeffs.pb_to_b);
    let zero = vdupq_n_f32(0.0);
    let one = vdupq_n_f32(1.0);
    let chroma_len = row.width.div_ceil(2);
    unsafe {
        while *x < row.width {
            if *x + PIXELS_PER_NEON_STEP <= row.width && ycbcr420_chroma_load4_fits(*x, chroma_len)
            {
                let xc = *x / 2;
                let y = vcvtq_f32_u32(vmovl_u16(vget_low_u16(vld1q_u16(row.y.as_ptr().add(*x)))));
                let cb = vcvtq_f32_u32(vmovl_u16(load_u16x4_420_chroma_neon(
                    row.cb.as_ptr().add(xc),
                )));
                let cr = vcvtq_f32_u32(vmovl_u16(load_u16x4_420_chroma_neon(
                    row.cr.as_ptr().add(xc),
                )));

                let yy = vmulq_f32(vsubq_f32(y, luma_floor), luma_inv);
                let pb = vmulq_f32(vsubq_f32(cb, chroma_mid), chroma_inv);
                let pr = vmulq_f32(vsubq_f32(cr, chroma_mid), chroma_inv);

                let rf = vminq_f32(vmaxq_f32(vaddq_f32(yy, vmulq_f32(k_pr_r, pr)), zero), one);
                let gf = vminq_f32(
                    vmaxq_f32(
                        vaddq_f32(vaddq_f32(yy, vmulq_f32(k_pb_g, pb)), vmulq_f32(k_pr_g, pr)),
                        zero,
                    ),
                    one,
                );
                let bf = vminq_f32(vmaxq_f32(vaddq_f32(yy, vmulq_f32(k_pb_b, pb)), zero), one);

                let mut r = [0.0_f32; 4];
                let mut g = [0.0_f32; 4];
                let mut b = [0.0_f32; 4];
                vst1q_f32(r.as_mut_ptr(), rf);
                vst1q_f32(g.as_mut_ptr(), gf);
                vst1q_f32(b.as_mut_ptr(), bf);
                store_rgba_f32x4(row.dst, *x, r, g, b);
                *x += PIXELS_PER_NEON_STEP;
            } else {
                break;
            }
        }
    }
}

/// Full-range u16 YCbCr 4:4:4 row -> RGBA f32 (`width * 4` floats written).
pub(crate) fn ycbcr_full_range_row_444_u16_to_rgba_f32(
    convert: HdrYcbcrSimdConvert,
    y_row: &[u16],
    cb_row: &[u16],
    cr_row: &[u16],
    dst: &mut [f32],
    width: usize,
) {
    if y_row.len() < width
        || cb_row.len() < width
        || cr_row.len() < width
        || dst.len() < width.saturating_mul(4)
    {
        return;
    }
    let Some(ctx) = HdrYcbcrRowSimdCtx::new(convert) else {
        return;
    };
    let mut row = HdrYcbcrU16Row {
        y: y_row,
        cb: cb_row,
        cr: cr_row,
        dst,
        width,
    };

    let mut x = 0;
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            unsafe {
                ycbcr_full_range_row_444_u16_avx2(&ctx, &mut row, &mut x);
            }
        } else if is_x86_feature_detected!("sse4.1") {
            unsafe {
                ycbcr_full_range_row_444_u16_sse41(&ctx, &mut row, &mut x);
            }
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        unsafe {
            ycbcr_full_range_row_444_u16_neon(&ctx, &mut row, &mut x);
        }
    }

    while x < row.width {
        write_full_range_u16_pixel(
            ctx.convert,
            ctx.coeffs,
            row.dst,
            x,
            row.y[x],
            row.cb[x],
            row.cr[x],
        );
        x += 1;
    }
}

/// Full-range u16 YCbCr 4:2:0 row -> RGBA f32.
pub(crate) fn ycbcr_full_range_row_420_u16_to_rgba_f32(
    convert: HdrYcbcrSimdConvert,
    y_row: &[u16],
    cb_row: &[u16],
    cr_row: &[u16],
    dst: &mut [f32],
    width: usize,
) {
    let chroma_len = width.div_ceil(2);
    if y_row.len() < width
        || cb_row.len() < chroma_len
        || cr_row.len() < chroma_len
        || dst.len() < width.saturating_mul(4)
    {
        return;
    }
    let Some(ctx) = HdrYcbcrRowSimdCtx::new(convert) else {
        return;
    };
    let mut row = HdrYcbcrU16Row {
        y: y_row,
        cb: cb_row,
        cr: cr_row,
        dst,
        width,
    };

    let mut x = 0;
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            unsafe {
                ycbcr_full_range_row_420_u16_avx2(&ctx, &mut row, &mut x);
            }
        } else if is_x86_feature_detected!("sse4.1") {
            unsafe {
                ycbcr_full_range_row_420_u16_sse41(&ctx, &mut row, &mut x);
            }
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        unsafe {
            ycbcr_full_range_row_420_u16_neon(&ctx, &mut row, &mut x);
        }
    }

    while x < row.width {
        let xc = x / 2;
        write_full_range_u16_pixel(
            ctx.convert,
            ctx.coeffs,
            row.dst,
            x,
            row.y[x],
            row.cb[xc],
            row.cr[xc],
        );
        x += 1;
    }
}

#[inline]
fn write_full_range_u16_pixel(
    convert: HdrYcbcrSimdConvert,
    coeffs: YcbcrMatrixSimdCoeffs,
    dst: &mut [f32],
    x: usize,
    y: u16,
    cb: u16,
    cr: u16,
) {
    let yy = y as f32 * convert.inv_scale_y;
    let pb = cb as f32 * convert.inv_scale_cb - CHROMA_CENTER;
    let pr = cr as f32 * convert.inv_scale_cr - CHROMA_CENTER;
    let r = (yy + coeffs.pr_to_r * pr).clamp(0.0, 1.0);
    let g = (yy + coeffs.pb_to_g * pb + coeffs.pr_to_g * pr).clamp(0.0, 1.0);
    let b = (yy + coeffs.pb_to_b * pb).clamp(0.0, 1.0);
    let base = x * 4;
    dst[base] = r;
    dst[base + 1] = g;
    dst[base + 2] = b;
    dst[base + 3] = 1.0;
}

#[inline]
fn store_rgba_f32x4(dst: &mut [f32], x: usize, r: [f32; 4], g: [f32; 4], b: [f32; 4]) {
    for i in 0..4 {
        let base = (x + i) * 4;
        dst[base] = r[i].clamp(0.0, 1.0);
        dst[base + 1] = g[i].clamp(0.0, 1.0);
        dst[base + 2] = b[i].clamp(0.0, 1.0);
        dst[base + 3] = 1.0;
    }
}

#[inline]
#[cfg(target_arch = "x86_64")]
fn ycbcr420_chroma_load2_fits(x: usize, chroma_len: usize) -> bool {
    x / 2 + 2 <= chroma_len
}

/// 4:2:0 NEON `vld1_u16` loads four chroma samples from `cb_row[xc..]`.
#[cfg(target_arch = "aarch64")]
#[inline]
fn ycbcr420_chroma_load4_fits(x: usize, chroma_len: usize) -> bool {
    x / 2 + 4 <= chroma_len
}

/// 4:2:0 chroma upsample: `[c0, c0, c1, c1]` as `uint16x4_t`.
#[cfg(target_arch = "aarch64")]
#[inline]
unsafe fn load_u16x4_420_chroma_neon(ptr: *const u16) -> uint16x4_t {
    unsafe {
        let c0 = *ptr;
        let c1 = *ptr.add(1);
        vld1_u16([c0, c0, c1, c1].as_ptr())
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn ycbcr_full_range_row_444_u16_sse41(
    ctx: &HdrYcbcrRowSimdCtx,
    row: &mut HdrYcbcrU16Row<'_>,
    x: &mut usize,
) {
    unsafe {
        let convert = ctx.convert;
        let coeffs = ctx.coeffs;
        let inv_y = _mm_set1_ps(convert.inv_scale_y);
        let inv_cb = _mm_set1_ps(convert.inv_scale_cb);
        let inv_cr = _mm_set1_ps(convert.inv_scale_cr);
        let center = _mm_set1_ps(CHROMA_CENTER);
        let k_pr_r = _mm_set1_ps(coeffs.pr_to_r);
        let k_pb_g = _mm_set1_ps(coeffs.pb_to_g);
        let k_pr_g = _mm_set1_ps(coeffs.pr_to_g);
        let k_pb_b = _mm_set1_ps(coeffs.pb_to_b);
        let zero = _mm_setzero_ps();
        let one = _mm_set1_ps(1.0);

        while *x + PIXELS_PER_SSE41_STEP <= row.width {
            let y = _mm_cvtepi32_ps(_mm_cvtepu16_epi32(_mm_loadl_epi64(
                row.y.as_ptr().add(*x) as *const __m128i
            )));
            let cb = _mm_cvtepi32_ps(_mm_cvtepu16_epi32(_mm_loadl_epi64(
                row.cb.as_ptr().add(*x) as *const __m128i
            )));
            let cr = _mm_cvtepi32_ps(_mm_cvtepu16_epi32(_mm_loadl_epi64(
                row.cr.as_ptr().add(*x) as *const __m128i
            )));

            let yy = _mm_mul_ps(y, inv_y);
            let pb = _mm_sub_ps(_mm_mul_ps(cb, inv_cb), center);
            let pr = _mm_sub_ps(_mm_mul_ps(cr, inv_cr), center);

            let rf = _mm_min_ps(
                _mm_max_ps(_mm_add_ps(yy, _mm_mul_ps(k_pr_r, pr)), zero),
                one,
            );
            let gf = _mm_min_ps(
                _mm_max_ps(
                    _mm_add_ps(
                        _mm_add_ps(yy, _mm_mul_ps(k_pb_g, pb)),
                        _mm_mul_ps(k_pr_g, pr),
                    ),
                    zero,
                ),
                one,
            );
            let bf = _mm_min_ps(
                _mm_max_ps(_mm_add_ps(yy, _mm_mul_ps(k_pb_b, pb)), zero),
                one,
            );

            let mut r = [0.0_f32; 4];
            let mut g = [0.0_f32; 4];
            let mut b = [0.0_f32; 4];
            _mm_storeu_ps(r.as_mut_ptr(), rf);
            _mm_storeu_ps(g.as_mut_ptr(), gf);
            _mm_storeu_ps(b.as_mut_ptr(), bf);
            store_rgba_f32x4(row.dst, *x, r, g, b);
            *x += PIXELS_PER_SSE41_STEP;
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn ycbcr_full_range_row_420_u16_sse41(
    ctx: &HdrYcbcrRowSimdCtx,
    row: &mut HdrYcbcrU16Row<'_>,
    x: &mut usize,
) {
    unsafe {
        let convert = ctx.convert;
        let coeffs = ctx.coeffs;
        let inv_y = _mm_set1_ps(convert.inv_scale_y);
        let inv_cb = _mm_set1_ps(convert.inv_scale_cb);
        let inv_cr = _mm_set1_ps(convert.inv_scale_cr);
        let center = _mm_set1_ps(CHROMA_CENTER);
        let k_pr_r = _mm_set1_ps(coeffs.pr_to_r);
        let k_pb_g = _mm_set1_ps(coeffs.pb_to_g);
        let k_pr_g = _mm_set1_ps(coeffs.pr_to_g);
        let k_pb_b = _mm_set1_ps(coeffs.pb_to_b);
        let zero = _mm_setzero_ps();
        let one = _mm_set1_ps(1.0);

        while *x + PIXELS_PER_SSE41_STEP <= row.width
            && ycbcr420_chroma_load2_fits(*x, row.cb.len())
        {
            let xc = *x / 2;
            let cb0 = row.cb[xc];
            let cb1 = row.cb[xc + 1];
            let cr0 = row.cr[xc];
            let cr1 = row.cr[xc + 1];
            let y = _mm_loadl_epi64(row.y.as_ptr().add(*x) as *const __m128i);
            let cb = _mm_setr_epi16(cb0 as i16, cb0 as i16, cb1 as i16, cb1 as i16, 0, 0, 0, 0);
            let cr = _mm_setr_epi16(cr0 as i16, cr0 as i16, cr1 as i16, cr1 as i16, 0, 0, 0, 0);

            let yy = _mm_mul_ps(_mm_cvtepi32_ps(_mm_cvtepu16_epi32(y)), inv_y);
            let pb = _mm_sub_ps(
                _mm_mul_ps(_mm_cvtepi32_ps(_mm_cvtepu16_epi32(cb)), inv_cb),
                center,
            );
            let pr = _mm_sub_ps(
                _mm_mul_ps(_mm_cvtepi32_ps(_mm_cvtepu16_epi32(cr)), inv_cr),
                center,
            );

            let rf = _mm_min_ps(
                _mm_max_ps(_mm_add_ps(yy, _mm_mul_ps(k_pr_r, pr)), zero),
                one,
            );
            let gf = _mm_min_ps(
                _mm_max_ps(
                    _mm_add_ps(
                        _mm_add_ps(yy, _mm_mul_ps(k_pb_g, pb)),
                        _mm_mul_ps(k_pr_g, pr),
                    ),
                    zero,
                ),
                one,
            );
            let bf = _mm_min_ps(
                _mm_max_ps(_mm_add_ps(yy, _mm_mul_ps(k_pb_b, pb)), zero),
                one,
            );

            let mut r = [0.0_f32; 4];
            let mut g = [0.0_f32; 4];
            let mut b = [0.0_f32; 4];
            _mm_storeu_ps(r.as_mut_ptr(), rf);
            _mm_storeu_ps(g.as_mut_ptr(), gf);
            _mm_storeu_ps(b.as_mut_ptr(), bf);
            store_rgba_f32x4(row.dst, *x, r, g, b);
            *x += PIXELS_PER_SSE41_STEP;
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn convert_u16x4_sse41(
    ctx: &HdrYcbcrRowSimdCtx,
    y: __m128i,
    cb: __m128i,
    cr: __m128i,
    dst: &mut [f32],
    x: usize,
) {
    unsafe {
        let convert = ctx.convert;
        let coeffs = ctx.coeffs;
        let inv_y = _mm_set1_ps(convert.inv_scale_y);
        let inv_cb = _mm_set1_ps(convert.inv_scale_cb);
        let inv_cr = _mm_set1_ps(convert.inv_scale_cr);
        let center = _mm_set1_ps(CHROMA_CENTER);
        let k_pr_r = _mm_set1_ps(coeffs.pr_to_r);
        let k_pb_g = _mm_set1_ps(coeffs.pb_to_g);
        let k_pr_g = _mm_set1_ps(coeffs.pr_to_g);
        let k_pb_b = _mm_set1_ps(coeffs.pb_to_b);
        let zero = _mm_setzero_ps();
        let one = _mm_set1_ps(1.0);

        let yy = _mm_mul_ps(_mm_cvtepi32_ps(_mm_cvtepu16_epi32(y)), inv_y);
        let pb = _mm_sub_ps(
            _mm_mul_ps(_mm_cvtepi32_ps(_mm_cvtepu16_epi32(cb)), inv_cb),
            center,
        );
        let pr = _mm_sub_ps(
            _mm_mul_ps(_mm_cvtepi32_ps(_mm_cvtepu16_epi32(cr)), inv_cr),
            center,
        );

        let rf = _mm_min_ps(
            _mm_max_ps(_mm_add_ps(yy, _mm_mul_ps(k_pr_r, pr)), zero),
            one,
        );
        let gf = _mm_min_ps(
            _mm_max_ps(
                _mm_add_ps(
                    _mm_add_ps(yy, _mm_mul_ps(k_pb_g, pb)),
                    _mm_mul_ps(k_pr_g, pr),
                ),
                zero,
            ),
            one,
        );
        let bf = _mm_min_ps(
            _mm_max_ps(_mm_add_ps(yy, _mm_mul_ps(k_pb_b, pb)), zero),
            one,
        );

        let mut r = [0.0_f32; 4];
        let mut g = [0.0_f32; 4];
        let mut b = [0.0_f32; 4];
        _mm_storeu_ps(r.as_mut_ptr(), rf);
        _mm_storeu_ps(g.as_mut_ptr(), gf);
        _mm_storeu_ps(b.as_mut_ptr(), bf);
        store_rgba_f32x4(dst, x, r, g, b);
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn ycbcr_full_range_row_444_u16_avx2(
    ctx: &HdrYcbcrRowSimdCtx,
    row: &mut HdrYcbcrU16Row<'_>,
    x: &mut usize,
) {
    unsafe {
        while *x + PIXELS_PER_AVX2_STEP <= row.width {
            let y = _mm_loadl_epi64(row.y.as_ptr().add(*x) as *const __m128i);
            let cb = _mm_loadl_epi64(row.cb.as_ptr().add(*x) as *const __m128i);
            let cr = _mm_loadl_epi64(row.cr.as_ptr().add(*x) as *const __m128i);
            convert_u16x4_sse41(ctx, y, cb, cr, row.dst, *x);
            *x += PIXELS_PER_AVX2_STEP;
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn ycbcr_full_range_row_420_u16_avx2(
    ctx: &HdrYcbcrRowSimdCtx,
    row: &mut HdrYcbcrU16Row<'_>,
    x: &mut usize,
) {
    unsafe {
        ycbcr_full_range_row_420_u16_sse41(ctx, row, x);
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn ycbcr_full_range_row_444_u16_neon(
    ctx: &HdrYcbcrRowSimdCtx,
    row: &mut HdrYcbcrU16Row<'_>,
    x: &mut usize,
) {
    unsafe {
        let convert = ctx.convert;
        let coeffs = ctx.coeffs;
        let inv_y = vdupq_n_f32(convert.inv_scale_y);
        let inv_cb = vdupq_n_f32(convert.inv_scale_cb);
        let inv_cr = vdupq_n_f32(convert.inv_scale_cr);
        let center = vdupq_n_f32(CHROMA_CENTER);
        let k_pr_r = vdupq_n_f32(coeffs.pr_to_r);
        let k_pb_g = vdupq_n_f32(coeffs.pb_to_g);
        let k_pr_g = vdupq_n_f32(coeffs.pr_to_g);
        let k_pb_b = vdupq_n_f32(coeffs.pb_to_b);
        let zero = vdupq_n_f32(0.0);
        let one = vdupq_n_f32(1.0);

        while *x + PIXELS_PER_NEON_STEP <= row.width {
            let y = vld1_u16(row.y.as_ptr().add(*x));
            let cb = vld1_u16(row.cb.as_ptr().add(*x));
            let cr = vld1_u16(row.cr.as_ptr().add(*x));

            let yy = vmulq_f32(vcvtq_f32_u32(vmovl_u16(y)), inv_y);
            let pb = vsubq_f32(vmulq_f32(vcvtq_f32_u32(vmovl_u16(cb)), inv_cb), center);
            let pr = vsubq_f32(vmulq_f32(vcvtq_f32_u32(vmovl_u16(cr)), inv_cr), center);

            let rf = vminq_f32(vmaxq_f32(vaddq_f32(yy, vmulq_f32(k_pr_r, pr)), zero), one);
            let gf = vminq_f32(
                vmaxq_f32(
                    vaddq_f32(vaddq_f32(yy, vmulq_f32(k_pb_g, pb)), vmulq_f32(k_pr_g, pr)),
                    zero,
                ),
                one,
            );
            let bf = vminq_f32(vmaxq_f32(vaddq_f32(yy, vmulq_f32(k_pb_b, pb)), zero), one);

            let mut r = [0.0_f32; 4];
            let mut g = [0.0_f32; 4];
            let mut b = [0.0_f32; 4];
            vst1q_f32(r.as_mut_ptr(), rf);
            vst1q_f32(g.as_mut_ptr(), gf);
            vst1q_f32(b.as_mut_ptr(), bf);
            store_rgba_f32x4(row.dst, *x, r, g, b);
            *x += PIXELS_PER_NEON_STEP;
        }
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn ycbcr_full_range_row_420_u16_neon(
    ctx: &HdrYcbcrRowSimdCtx,
    row: &mut HdrYcbcrU16Row<'_>,
    x: &mut usize,
) {
    unsafe {
        let convert = ctx.convert;
        let coeffs = ctx.coeffs;
        let inv_y = vdupq_n_f32(convert.inv_scale_y);
        let inv_cb = vdupq_n_f32(convert.inv_scale_cb);
        let inv_cr = vdupq_n_f32(convert.inv_scale_cr);
        let center = vdupq_n_f32(CHROMA_CENTER);
        let k_pr_r = vdupq_n_f32(coeffs.pr_to_r);
        let k_pb_g = vdupq_n_f32(coeffs.pb_to_g);
        let k_pr_g = vdupq_n_f32(coeffs.pr_to_g);
        let k_pb_b = vdupq_n_f32(coeffs.pb_to_b);
        let zero = vdupq_n_f32(0.0);
        let one = vdupq_n_f32(1.0);

        while *x + PIXELS_PER_NEON_STEP <= row.width && ycbcr420_chroma_load4_fits(*x, row.cb.len())
        {
            let xc = *x / 2;
            let y = vld1_u16(row.y.as_ptr().add(*x));
            let cb = load_u16x4_420_chroma_neon(row.cb.as_ptr().add(xc));
            let cr = load_u16x4_420_chroma_neon(row.cr.as_ptr().add(xc));

            let yy = vmulq_f32(vcvtq_f32_u32(vmovl_u16(y)), inv_y);
            let pb = vsubq_f32(vmulq_f32(vcvtq_f32_u32(vmovl_u16(cb)), inv_cb), center);
            let pr = vsubq_f32(vmulq_f32(vcvtq_f32_u32(vmovl_u16(cr)), inv_cr), center);

            let rf = vminq_f32(vmaxq_f32(vaddq_f32(yy, vmulq_f32(k_pr_r, pr)), zero), one);
            let gf = vminq_f32(
                vmaxq_f32(
                    vaddq_f32(vaddq_f32(yy, vmulq_f32(k_pb_g, pb)), vmulq_f32(k_pr_g, pr)),
                    zero,
                ),
                one,
            );
            let bf = vminq_f32(vmaxq_f32(vaddq_f32(yy, vmulq_f32(k_pb_b, pb)), zero), one);

            let mut r = [0.0_f32; 4];
            let mut g = [0.0_f32; 4];
            let mut b = [0.0_f32; 4];
            vst1q_f32(r.as_mut_ptr(), rf);
            vst1q_f32(g.as_mut_ptr(), gf);
            vst1q_f32(b.as_mut_ptr(), bf);
            store_rgba_f32x4(row.dst, *x, r, g, b);
            *x += PIXELS_PER_NEON_STEP;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::ycbcr::ycbcr_linear_to_rgb;
    use super::*;

    fn scalar_row_444_u16(
        convert: HdrYcbcrSimdConvert,
        y_row: &[u16],
        cb_row: &[u16],
        cr_row: &[u16],
        width: usize,
    ) -> Vec<f32> {
        let coeffs = convert.matrix_coeffs().unwrap();
        let mut dst = vec![0.0_f32; width * 4];
        for x in 0..width {
            write_full_range_u16_pixel(
                convert, coeffs, &mut dst, x, y_row[x], cb_row[x], cr_row[x],
            );
        }
        dst
    }

    fn scalar_row_420_u16(
        convert: HdrYcbcrSimdConvert,
        y_row: &[u16],
        cb_row: &[u16],
        cr_row: &[u16],
        width: usize,
    ) -> Vec<f32> {
        let mut dst = vec![0.0_f32; width * 4];
        for (x, &y_value) in y_row.iter().take(width).enumerate() {
            let xc = x / 2;
            let yy = y_value as f32 * convert.inv_scale_y;
            let pb = cb_row[xc] as f32 * convert.inv_scale_cb - CHROMA_CENTER;
            let pr = cr_row[xc] as f32 * convert.inv_scale_cr - CHROMA_CENTER;
            let [r, g, b] = ycbcr_linear_to_rgb(yy, pb, pr, convert.matrix);
            let base = x * 4;
            dst[base] = r.clamp(0.0, 1.0);
            dst[base + 1] = g.clamp(0.0, 1.0);
            dst[base + 2] = b.clamp(0.0, 1.0);
            dst[base + 3] = 1.0;
        }
        dst
    }

    fn convert_10bit_bt709() -> HdrYcbcrSimdConvert {
        HdrYcbcrSimdConvert {
            inv_scale_y: 1.0 / 1023.0,
            inv_scale_cb: 1.0 / 1023.0,
            inv_scale_cr: 1.0 / 1023.0,
            matrix: HeifYcbcrMatrix::Bt709,
        }
    }

    fn convert_12bit_bt601() -> HdrYcbcrSimdConvert {
        HdrYcbcrSimdConvert {
            inv_scale_y: 1.0 / 4095.0,
            inv_scale_cb: 1.0 / 4095.0,
            inv_scale_cr: 1.0 / 4095.0,
            matrix: HeifYcbcrMatrix::Bt601,
        }
    }

    #[test]
    fn ycbcr_full_range_u16_444_matches_scalar() {
        let convert = convert_10bit_bt709();
        for width in [0_usize, 1, 3, 4, 7, 8, 9, 16] {
            let y: Vec<u16> = (0..width).map(|i| ((i * 137 + 11) % 1024) as u16).collect();
            let cb: Vec<u16> = (0..width).map(|i| ((i * 211 + 31) % 1024) as u16).collect();
            let cr: Vec<u16> = (0..width).map(|i| ((i * 317 + 43) % 1024) as u16).collect();
            let expected = scalar_row_444_u16(convert, &y, &cb, &cr, width);
            let mut simd = vec![0.0_f32; width * 4];
            ycbcr_full_range_row_444_u16_to_rgba_f32(convert, &y, &cb, &cr, &mut simd, width);
            assert_eq!(simd, expected, "width={width}");
        }
    }

    #[test]
    fn ycbcr_full_range_u16_420_matches_scalar() {
        let convert = convert_12bit_bt601();
        for width in [0_usize, 1, 2, 3, 4, 7, 8, 9, 16] {
            let chroma_len = width.div_ceil(2);
            let y: Vec<u16> = (0..width).map(|i| ((i * 97 + 7) % 4096) as u16).collect();
            let cb: Vec<u16> = (0..chroma_len)
                .map(|i| ((i * 151 + 5) % 4096) as u16)
                .collect();
            let cr: Vec<u16> = (0..chroma_len)
                .map(|i| ((i * 223 + 3) % 4096) as u16)
                .collect();
            let expected = scalar_row_420_u16(convert, &y, &cb, &cr, width);
            let mut simd = vec![0.0_f32; width * 4];
            ycbcr_full_range_row_420_u16_to_rgba_f32(convert, &y, &cb, &cr, &mut simd, width);
            assert_eq!(simd, expected, "width={width}");
        }
    }

    fn hdr_u16_layout(chroma: libheif_sys::heif_chroma) -> HdrYcbcrU16RowLayout {
        HdrYcbcrU16RowLayout {
            span_y: 2,
            span_cb: 2,
            span_cr: 2,
            stride_y: 8,
            stride_cb: 4,
            stride_cr: 4,
            y_w: 4,
            cb_w: 2,
            chroma,
        }
    }

    #[test]
    fn hdr_ycbcr_u16_simd_eligible_requires_tight_u16_rows() {
        assert!(hdr_ycbcr_u16_simd_eligible(
            hdr_u16_layout(libheif_sys::heif_chroma_420),
            false,
            HeifYcbcrMatrix::Bt709,
        ));
        assert!(!hdr_ycbcr_u16_simd_eligible(
            HdrYcbcrU16RowLayout {
                span_y: 1,
                ..hdr_u16_layout(libheif_sys::heif_chroma_444)
            },
            false,
            HeifYcbcrMatrix::Bt709,
        ));
        assert!(hdr_ycbcr_u16_simd_eligible(
            hdr_u16_layout(libheif_sys::heif_chroma_420),
            true,
            HeifYcbcrMatrix::Bt601,
        ));
        assert!(!hdr_ycbcr_u16_simd_eligible(
            hdr_u16_layout(libheif_sys::heif_chroma_420),
            false,
            HeifYcbcrMatrix::Monochrome,
        ));
    }
}
