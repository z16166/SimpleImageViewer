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

//! SIMD YCbCr BT.709 -> RGBA8 for libheif 8-bit planar output (full- and limited-range).

#[cfg(target_arch = "x86_64")]
use core::arch::x86_64::*;

#[cfg(target_arch = "aarch64")]
use core::arch::aarch64::*;

const U8_TO_F32_SCALE: f32 = 1.0 / 255.0;
const CHROMA_CENTER: f32 = 0.5;
const STUDIO_LUMA_FLOOR: f32 = 16.0;
const STUDIO_LUMA_INV_SPAN: f32 = 1.0 / 219.0;
const STUDIO_CHROMA_MID: f32 = 128.0;
const STUDIO_CHROMA_INV_SPAN: f32 = 1.0 / 224.0;
const BT709_PR_TO_R: f32 = 1.5748;
const BT709_PB_TO_G: f32 = -0.187_324;
const BT709_PR_TO_G: f32 = -0.468_124;
const BT709_PB_TO_B: f32 = 1.8556;
const RGBA_ALPHA: u8 = 255;

#[cfg(target_arch = "x86_64")]
const PIXELS_PER_SSE41_STEP: usize = 4;
#[cfg(target_arch = "aarch64")]
const PIXELS_PER_NEON_STEP: usize = 4;
#[cfg(target_arch = "x86_64")]
const PIXELS_PER_AVX2_STEP: usize = 8;

/// 4:2:0 AVX2/NEON vector chroma loads read 8 bytes from `cb_row[xc..]`.
#[inline]
fn ycbcr420_chroma_load8_fits(x: usize, chroma_len: usize) -> bool {
    x / 2 + 8 <= chroma_len
}

/// `_mm_cvtepu8_epi32` only uses the low 4 bytes; avoid `_mm_loadl_epi64` (8 bytes).
#[cfg(target_arch = "x86_64")]
#[inline]
unsafe fn load_u8x4_for_cvtepu8(ptr: *const u8) -> __m128i {
    unsafe { _mm_cvtsi32_si128(*(ptr as *const i32)) }
}

/// NEON `vld1_u8` loads 8 bytes; this loads exactly 4.
#[cfg(target_arch = "aarch64")]
#[inline]
unsafe fn load_u8x4_neon(ptr: *const u8) -> uint8x8_t {
    unsafe {
        let packed = u32::from_ne_bytes([*ptr, *ptr.add(1), *ptr.add(2), *ptr.add(3)]);
        vreinterpret_u8_u32(vdup_n_u32(packed))
    }
}

#[cfg(target_arch = "aarch64")]
#[inline]
unsafe fn u8x8_to_f32x4(v: uint8x8_t) -> float32x4_t {
    unsafe { vcvtq_f32_u32(vmovl_u16(vget_low_u16(vmovl_u8(v)))) }
}

/// 4:2:0 chroma upsample: `[c0, c0, c1, c1]` in the low four lanes.
#[cfg(target_arch = "aarch64")]
#[inline]
unsafe fn load_u8x4_420_chroma_neon(ptr: *const u8) -> uint8x8_t {
    unsafe {
        let c0 = *ptr;
        let c1 = *ptr.add(1);
        let packed = u32::from_ne_bytes([c0, c0, c1, c1]);
        vreinterpret_u8_u32(vdup_n_u32(packed))
    }
}

/// Full-range BT.709 YCbCr 4:4:4 row -> RGBA8. Returns bytes written (`width * 4`).
pub(crate) fn ycbcr_full_range_bt709_row_444_to_rgba8(
    y_row: &[u8],
    cb_row: &[u8],
    cr_row: &[u8],
    dst: &mut [u8],
    width: usize,
) -> usize {
    if y_row.len() < width
        || cb_row.len() < width
        || cr_row.len() < width
        || dst.len() < width.saturating_mul(4)
    {
        return 0;
    }
    let mut x = 0;
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            unsafe {
                ycbcr_full_range_bt709_row_444_avx2(y_row, cb_row, cr_row, dst, width, &mut x);
            }
        } else if is_x86_feature_detected!("sse4.1") {
            unsafe {
                ycbcr_full_range_bt709_row_444_sse41(y_row, cb_row, cr_row, dst, width, &mut x);
            }
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        unsafe {
            ycbcr_full_range_bt709_row_444_neon(y_row, cb_row, cr_row, dst, width, &mut x);
        }
    }

    while x < width {
        write_bt709_pixel(dst, x, y_row[x], cb_row[x], cr_row[x]);
        x += 1;
    }
    width * 4
}

/// Full-range BT.709 YCbCr 4:2:0 row -> RGBA8 (`chroma_row_len = width.div_ceil(2)`).
pub(crate) fn ycbcr_full_range_bt709_row_420_to_rgba8(
    y_row: &[u8],
    cb_row: &[u8],
    cr_row: &[u8],
    dst: &mut [u8],
    width: usize,
) -> usize {
    let chroma_len = width.div_ceil(2);
    if y_row.len() < width
        || cb_row.len() < chroma_len
        || cr_row.len() < chroma_len
        || dst.len() < width.saturating_mul(4)
    {
        return 0;
    }
    let mut x = 0;
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            unsafe {
                ycbcr_full_range_bt709_row_420_avx2(y_row, cb_row, cr_row, dst, width, &mut x);
            }
        } else if is_x86_feature_detected!("sse4.1") {
            unsafe {
                ycbcr_full_range_bt709_row_420_sse41(y_row, cb_row, cr_row, dst, width, &mut x);
            }
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        unsafe {
            ycbcr_full_range_bt709_row_420_neon(y_row, cb_row, cr_row, dst, width, &mut x);
        }
    }

    while x < width {
        let xc = x / 2;
        write_bt709_pixel(dst, x, y_row[x], cb_row[xc], cr_row[xc]);
        x += 1;
    }
    width * 4
}

/// Limited-range (studio swing) BT.709 YCbCr 4:4:4 row -> RGBA8.
pub(crate) fn ycbcr_limited_range_bt709_row_444_to_rgba8(
    y_row: &[u8],
    cb_row: &[u8],
    cr_row: &[u8],
    dst: &mut [u8],
    width: usize,
) -> usize {
    if y_row.len() < width
        || cb_row.len() < width
        || cr_row.len() < width
        || dst.len() < width.saturating_mul(4)
    {
        return 0;
    }
    let mut x = 0;
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            unsafe {
                ycbcr_limited_range_bt709_row_444_avx2(y_row, cb_row, cr_row, dst, width, &mut x);
            }
        } else if is_x86_feature_detected!("sse4.1") {
            unsafe {
                ycbcr_limited_range_bt709_row_444_sse41(y_row, cb_row, cr_row, dst, width, &mut x);
            }
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        unsafe {
            ycbcr_limited_range_bt709_row_444_neon(y_row, cb_row, cr_row, dst, width, &mut x);
        }
    }

    while x < width {
        write_limited_bt709_pixel(dst, x, y_row[x], cb_row[x], cr_row[x]);
        x += 1;
    }
    width * 4
}

/// Limited-range (studio swing) BT.709 YCbCr 4:2:0 row -> RGBA8.
pub(crate) fn ycbcr_limited_range_bt709_row_420_to_rgba8(
    y_row: &[u8],
    cb_row: &[u8],
    cr_row: &[u8],
    dst: &mut [u8],
    width: usize,
) -> usize {
    let chroma_len = width.div_ceil(2);
    if y_row.len() < width
        || cb_row.len() < chroma_len
        || cr_row.len() < chroma_len
        || dst.len() < width.saturating_mul(4)
    {
        return 0;
    }
    let mut x = 0;
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            unsafe {
                ycbcr_limited_range_bt709_row_420_avx2(y_row, cb_row, cr_row, dst, width, &mut x);
            }
        } else if is_x86_feature_detected!("sse4.1") {
            unsafe {
                ycbcr_limited_range_bt709_row_420_sse41(y_row, cb_row, cr_row, dst, width, &mut x);
            }
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        unsafe {
            ycbcr_limited_range_bt709_row_420_neon(y_row, cb_row, cr_row, dst, width, &mut x);
        }
    }

    while x < width {
        let xc = x / 2;
        write_limited_bt709_pixel(dst, x, y_row[x], cb_row[xc], cr_row[xc]);
        x += 1;
    }
    width * 4
}

#[inline]
fn write_bt709_pixel(dst: &mut [u8], x: usize, y: u8, cb: u8, cr: u8) {
    let yy = y as f32 * U8_TO_F32_SCALE;
    let pb = cb as f32 * U8_TO_F32_SCALE - CHROMA_CENTER;
    let pr = cr as f32 * U8_TO_F32_SCALE - CHROMA_CENTER;
    let r = (yy + BT709_PR_TO_R * pr).clamp(0.0, 1.0);
    let g = (yy + BT709_PB_TO_G * pb + BT709_PR_TO_G * pr).clamp(0.0, 1.0);
    let b = (yy + BT709_PB_TO_B * pb).clamp(0.0, 1.0);
    let base = x * 4;
    dst[base] = (r * 255.0).round() as u8;
    dst[base + 1] = (g * 255.0).round() as u8;
    dst[base + 2] = (b * 255.0).round() as u8;
    dst[base + 3] = RGBA_ALPHA;
}

#[inline]
fn write_limited_bt709_pixel(dst: &mut [u8], x: usize, y: u8, cb: u8, cr: u8) {
    let yy = (y as f32 - STUDIO_LUMA_FLOOR) * STUDIO_LUMA_INV_SPAN;
    let pb = (cb as f32 - STUDIO_CHROMA_MID) * STUDIO_CHROMA_INV_SPAN;
    let pr = (cr as f32 - STUDIO_CHROMA_MID) * STUDIO_CHROMA_INV_SPAN;
    let r = (yy + BT709_PR_TO_R * pr).clamp(0.0, 1.0);
    let g = (yy + BT709_PB_TO_G * pb + BT709_PR_TO_G * pr).clamp(0.0, 1.0);
    let b = (yy + BT709_PB_TO_B * pb).clamp(0.0, 1.0);
    let base = x * 4;
    dst[base] = (r * 255.0).round() as u8;
    dst[base + 1] = (g * 255.0).round() as u8;
    dst[base + 2] = (b * 255.0).round() as u8;
    dst[base + 3] = RGBA_ALPHA;
}

/// Store 4 normalized RGB f32 lanes as interleaved RGBA8 (16 bytes) via one SSE4.1 store.
/// Matches scalar `(v * 255.0).round().clamp(0.0, 255.0)` for non-negative clamped inputs.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
#[inline]
unsafe fn store_bt709_f32x4_to_rgba8_sse41(
    dst: &mut [u8],
    x: usize,
    rf: __m128,
    gf: __m128,
    bf: __m128,
) {
    unsafe {
        let scale = _mm_set1_ps(255.0);
        let half = _mm_set1_ps(0.5);
        // Match scalar `(v * 255.0).round()` for non-negative v: trunc(v + 0.5).
        let ri = _mm_cvttps_epi32(_mm_add_ps(_mm_mul_ps(rf, scale), half));
        let gi = _mm_cvttps_epi32(_mm_add_ps(_mm_mul_ps(gf, scale), half));
        let bi = _mm_cvttps_epi32(_mm_add_ps(_mm_mul_ps(bf, scale), half));

        let r8 = _mm_packus_epi16(
            _mm_packus_epi32(ri, _mm_setzero_si128()),
            _mm_setzero_si128(),
        );
        let g8 = _mm_packus_epi16(
            _mm_packus_epi32(gi, _mm_setzero_si128()),
            _mm_setzero_si128(),
        );
        let b8 = _mm_packus_epi16(
            _mm_packus_epi32(bi, _mm_setzero_si128()),
            _mm_setzero_si128(),
        );
        let a8 = _mm_set1_epi8(RGBA_ALPHA as i8);

        let rg = _mm_unpacklo_epi8(r8, g8);
        let ba = _mm_unpacklo_epi8(b8, a8);
        let rgba = _mm_unpacklo_epi16(rg, ba);
        _mm_storeu_si128(dst.as_mut_ptr().add(x * 4) as *mut __m128i, rgba);
    }
}

/// Store 4 normalized RGB f32 lanes as interleaved RGBA8 (16 bytes) via NEON.
/// Matches scalar `(v * 255.0).round().clamp(0.0, 255.0)` for non-negative clamped inputs.
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
#[inline]
unsafe fn store_bt709_f32x4_to_rgba8_neon(
    dst: &mut [u8],
    x: usize,
    rf: float32x4_t,
    gf: float32x4_t,
    bf: float32x4_t,
) {
    unsafe {
        let scale = vdupq_n_f32(255.0);
        let half = vdupq_n_f32(0.5);
        // Match scalar `(v * 255.0).round()` for non-negative v: trunc(v + 0.5).
        let ru = vcvtq_u32_f32(vaddq_f32(vmulq_f32(rf, scale), half));
        let gu = vcvtq_u32_f32(vaddq_f32(vmulq_f32(gf, scale), half));
        let bu = vcvtq_u32_f32(vaddq_f32(vmulq_f32(bf, scale), half));

        let r8 = vqmovn_u16(vcombine_u16(vqmovn_u32(ru), vdup_n_u16(0)));
        let g8 = vqmovn_u16(vcombine_u16(vqmovn_u32(gu), vdup_n_u16(0)));
        let b8 = vqmovn_u16(vcombine_u16(vqmovn_u32(bu), vdup_n_u16(0)));
        let a8 = vdup_n_u8(RGBA_ALPHA);

        // Interleave 4 pixels into one 16-byte vector (vst4_u8 would write 8 pixels).
        let rg = vzip_u8(r8, g8);
        let ba = vzip_u8(b8, a8);
        let rgba16 = vzip_u16(vreinterpret_u16_u8(rg.0), vreinterpret_u16_u8(ba.0));
        let out = vcombine_u8(vreinterpret_u8_u16(rgba16.0), vreinterpret_u8_u16(rgba16.1));
        vst1q_u8(dst.as_mut_ptr().add(x * 4), out);
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn ycbcr_full_range_bt709_row_444_sse41(
    y_row: &[u8],
    cb_row: &[u8],
    cr_row: &[u8],
    dst: &mut [u8],
    width: usize,
    x: &mut usize,
) {
    unsafe {
        let scale = _mm_set1_ps(U8_TO_F32_SCALE);
        let center = _mm_set1_ps(CHROMA_CENTER);
        let store = bt709_store_consts_sse41();

        while *x + PIXELS_PER_SSE41_STEP <= width {
            let y = _mm_cvtepi32_ps(_mm_cvtepu8_epi32(load_u8x4_for_cvtepu8(
                y_row.as_ptr().add(*x),
            )));
            let cb = _mm_cvtepi32_ps(_mm_cvtepu8_epi32(load_u8x4_for_cvtepu8(
                cb_row.as_ptr().add(*x),
            )));
            let cr = _mm_cvtepi32_ps(_mm_cvtepu8_epi32(load_u8x4_for_cvtepu8(
                cr_row.as_ptr().add(*x),
            )));

            let yy = _mm_mul_ps(y, scale);
            let pb = _mm_sub_ps(_mm_mul_ps(cb, scale), center);
            let pr = _mm_sub_ps(_mm_mul_ps(cr, scale), center);
            bt709_store_normalized_sse41(yy, pb, pr, dst, *x, &store);
            *x += PIXELS_PER_SSE41_STEP;
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[repr(C)]
struct Bt709StoreConstsSse41 {
    k_pr_r: __m128,
    k_pb_g: __m128,
    k_pr_g: __m128,
    k_pb_b: __m128,
    zero: __m128,
    one: __m128,
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
#[inline]
unsafe fn bt709_store_consts_sse41() -> Bt709StoreConstsSse41 {
    Bt709StoreConstsSse41 {
        k_pr_r: _mm_set1_ps(BT709_PR_TO_R),
        k_pb_g: _mm_set1_ps(BT709_PB_TO_G),
        k_pr_g: _mm_set1_ps(BT709_PR_TO_G),
        k_pb_b: _mm_set1_ps(BT709_PB_TO_B),
        zero: _mm_setzero_ps(),
        one: _mm_set1_ps(1.0),
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn ycbcr_full_range_bt709_row_420_sse41(
    y_row: &[u8],
    cb_row: &[u8],
    cr_row: &[u8],
    dst: &mut [u8],
    width: usize,
    x: &mut usize,
) {
    unsafe {
        let scale = _mm_set1_ps(U8_TO_F32_SCALE);
        let center = _mm_set1_ps(CHROMA_CENTER);
        let store = bt709_store_consts_sse41();
        let simd_end = (width / PIXELS_PER_SSE41_STEP) * PIXELS_PER_SSE41_STEP;
        // Upsample 2 chroma bytes to 4 luma sites: [c0,c0,c1,c1].
        let chroma_dup = _mm_setr_epi8(0, 0, 1, 1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1);

        while *x + PIXELS_PER_SSE41_STEP <= simd_end {
            let xc = *x / 2;
            let y = load_u8x4_for_cvtepu8(y_row.as_ptr().add(*x));
            // Low two bytes = chroma samples; pad high bytes. LE packing matches x86_64 lane order.
            let cb2 = _mm_cvtsi32_si128(i32::from_le_bytes([
                *cb_row.as_ptr().add(xc),
                *cb_row.as_ptr().add(xc + 1),
                0,
                0,
            ]));
            let cr2 = _mm_cvtsi32_si128(i32::from_le_bytes([
                *cr_row.as_ptr().add(xc),
                *cr_row.as_ptr().add(xc + 1),
                0,
                0,
            ]));
            let cbv = _mm_shuffle_epi8(cb2, chroma_dup);
            let crv = _mm_shuffle_epi8(cr2, chroma_dup);

            let yy = _mm_mul_ps(_mm_cvtepi32_ps(_mm_cvtepu8_epi32(y)), scale);
            let pb = _mm_sub_ps(
                _mm_mul_ps(_mm_cvtepi32_ps(_mm_cvtepu8_epi32(cbv)), scale),
                center,
            );
            let pr = _mm_sub_ps(
                _mm_mul_ps(_mm_cvtepi32_ps(_mm_cvtepu8_epi32(crv)), scale),
                center,
            );
            bt709_store_normalized_sse41(yy, pb, pr, dst, *x, &store);
            *x += PIXELS_PER_SSE41_STEP;
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn bt709_store_normalized_sse41(
    yy: __m128,
    pb: __m128,
    pr: __m128,
    dst: &mut [u8],
    x: usize,
    c: &Bt709StoreConstsSse41,
) {
    unsafe {
        let rf = _mm_min_ps(
            _mm_max_ps(_mm_add_ps(yy, _mm_mul_ps(c.k_pr_r, pr)), c.zero),
            c.one,
        );
        let gf = _mm_min_ps(
            _mm_max_ps(
                _mm_add_ps(
                    _mm_add_ps(yy, _mm_mul_ps(c.k_pb_g, pb)),
                    _mm_mul_ps(c.k_pr_g, pr),
                ),
                c.zero,
            ),
            c.one,
        );
        let bf = _mm_min_ps(
            _mm_max_ps(_mm_add_ps(yy, _mm_mul_ps(c.k_pb_b, pb)), c.zero),
            c.one,
        );

        store_bt709_f32x4_to_rgba8_sse41(dst, x, rf, gf, bf);
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn ycbcr_full_range_bt709_row_444_avx2(
    y_row: &[u8],
    cb_row: &[u8],
    cr_row: &[u8],
    dst: &mut [u8],
    width: usize,
    x: &mut usize,
) {
    unsafe {
        while *x + PIXELS_PER_AVX2_STEP <= width {
            let y = _mm256_cvtepu8_epi32(_mm_loadl_epi64(y_row.as_ptr().add(*x) as *const __m128i));
            let cb =
                _mm256_cvtepu8_epi32(_mm_loadl_epi64(cb_row.as_ptr().add(*x) as *const __m128i));
            let cr =
                _mm256_cvtepu8_epi32(_mm_loadl_epi64(cr_row.as_ptr().add(*x) as *const __m128i));
            store_bt709_u8x8_from_i32(dst, *x, y, cb, cr);
            *x += PIXELS_PER_AVX2_STEP;
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn ycbcr_full_range_bt709_row_420_avx2(
    y_row: &[u8],
    cb_row: &[u8],
    cr_row: &[u8],
    dst: &mut [u8],
    width: usize,
    x: &mut usize,
) {
    unsafe {
        while *x + PIXELS_PER_AVX2_STEP <= width && ycbcr420_chroma_load8_fits(*x, cb_row.len()) {
            let xc = *x / 2;
            let cb = _mm_loadl_epi64(cb_row.as_ptr().add(xc) as *const __m128i);
            let cr = _mm_loadl_epi64(cr_row.as_ptr().add(xc) as *const __m128i);
            // Upsample 4 chroma samples to 8 luma sites: [c0,c0,c1,c1,c2,c2,c3,c3].
            let cb_dup = _mm_shuffle_epi8(
                cb,
                _mm_setr_epi8(0, 0, 1, 1, 2, 2, 3, 3, -1, -1, -1, -1, -1, -1, -1, -1),
            );
            let cr_dup = _mm_shuffle_epi8(
                cr,
                _mm_setr_epi8(0, 0, 1, 1, 2, 2, 3, 3, -1, -1, -1, -1, -1, -1, -1, -1),
            );
            let y = _mm256_cvtepu8_epi32(_mm_loadl_epi64(y_row.as_ptr().add(*x) as *const __m128i));
            let cb = _mm256_cvtepu8_epi32(cb_dup);
            let cr = _mm256_cvtepu8_epi32(cr_dup);
            store_bt709_u8x8_from_i32(dst, *x, y, cb, cr);
            *x += PIXELS_PER_AVX2_STEP;
        }
    }
}

/// Pack 8xi32 channel values in `0..=255` into the low 8 bytes of an `__m128i`.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
#[inline]
unsafe fn pack_i32x8_to_u8x8_avx2(v: __m256i) -> __m128i {
    // packus is lane-wise; permute gathers [lo4, hi4] into the low 128 bits.
    let v16 = _mm256_packus_epi32(v, _mm256_setzero_si256());
    let v16 = _mm256_permute4x64_epi64(v16, 0xD8);
    _mm_packus_epi16(_mm256_castsi256_si128(v16), _mm_setzero_si128())
}

/// Store 8 normalized RGB f32 lanes as interleaved RGBA8 (32 bytes) via one AVX2 store.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
#[inline]
unsafe fn store_bt709_f32x8_to_rgba8_avx2(
    dst: &mut [u8],
    x: usize,
    rf: __m256,
    gf: __m256,
    bf: __m256,
) {
    unsafe {
        let scale = _mm256_set1_ps(255.0);
        let half = _mm256_set1_ps(0.5);
        // Match scalar `(v * 255.0).round()` for non-negative v: trunc(v + 0.5).
        let ri = _mm256_cvttps_epi32(_mm256_add_ps(_mm256_mul_ps(rf, scale), half));
        let gi = _mm256_cvttps_epi32(_mm256_add_ps(_mm256_mul_ps(gf, scale), half));
        let bi = _mm256_cvttps_epi32(_mm256_add_ps(_mm256_mul_ps(bf, scale), half));

        let r8 = pack_i32x8_to_u8x8_avx2(ri);
        let g8 = pack_i32x8_to_u8x8_avx2(gi);
        let b8 = pack_i32x8_to_u8x8_avx2(bi);
        let a8 = _mm_set1_epi8(RGBA_ALPHA as i8);

        let rg = _mm_unpacklo_epi8(r8, g8);
        let ba = _mm_unpacklo_epi8(b8, a8);
        let rgba_lo = _mm_unpacklo_epi16(rg, ba);
        let rgba_hi = _mm_unpackhi_epi16(rg, ba);
        let out = _mm256_set_m128i(rgba_hi, rgba_lo);
        _mm256_storeu_si256(dst.as_mut_ptr().add(x * 4) as *mut __m256i, out);
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn store_bt709_u8x8_from_i32(
    dst: &mut [u8],
    x: usize,
    y: __m256i,
    cb: __m256i,
    cr: __m256i,
) {
    unsafe {
        let scale = _mm256_set1_ps(U8_TO_F32_SCALE);
        let center = _mm256_set1_ps(CHROMA_CENTER);
        let yy = _mm256_mul_ps(_mm256_cvtepi32_ps(y), scale);
        let pb = _mm256_sub_ps(_mm256_mul_ps(_mm256_cvtepi32_ps(cb), scale), center);
        let pr = _mm256_sub_ps(_mm256_mul_ps(_mm256_cvtepi32_ps(cr), scale), center);

        let rf = bt709_rgb_channel_avx2(yy, pb, pr, 0.0, BT709_PR_TO_R);
        let gf = bt709_rgb_channel_avx2(yy, pb, pr, BT709_PB_TO_G, BT709_PR_TO_G);
        let bf = bt709_rgb_channel_avx2(yy, pb, pr, BT709_PB_TO_B, 0.0);
        store_bt709_f32x8_to_rgba8_avx2(dst, x, rf, gf, bf);
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn bt709_rgb_channel_avx2(
    yy: __m256,
    pb: __m256,
    pr: __m256,
    k_pb: f32,
    k_pr: f32,
) -> __m256 {
    let zero = _mm256_setzero_ps();
    let one = _mm256_set1_ps(1.0);
    let mut out = yy;
    if k_pb != 0.0 {
        out = _mm256_add_ps(out, _mm256_mul_ps(_mm256_set1_ps(k_pb), pb));
    }
    if k_pr != 0.0 {
        out = _mm256_add_ps(out, _mm256_mul_ps(_mm256_set1_ps(k_pr), pr));
    }
    _mm256_min_ps(_mm256_max_ps(out, zero), one)
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn ycbcr_full_range_bt709_row_444_neon(
    y_row: &[u8],
    cb_row: &[u8],
    cr_row: &[u8],
    dst: &mut [u8],
    width: usize,
    x: &mut usize,
) {
    unsafe {
        let scale = vdupq_n_f32(U8_TO_F32_SCALE);
        let center = vdupq_n_f32(CHROMA_CENTER);
        let k_pr_r = vdupq_n_f32(BT709_PR_TO_R);
        let k_pb_g = vdupq_n_f32(BT709_PB_TO_G);
        let k_pr_g = vdupq_n_f32(BT709_PR_TO_G);
        let k_pb_b = vdupq_n_f32(BT709_PB_TO_B);
        let zero = vdupq_n_f32(0.0);
        let one = vdupq_n_f32(1.0);

        while *x + PIXELS_PER_NEON_STEP <= width {
            let y = load_u8x4_neon(y_row.as_ptr().add(*x));
            let cb = load_u8x4_neon(cb_row.as_ptr().add(*x));
            let cr = load_u8x4_neon(cr_row.as_ptr().add(*x));

            let yy = vmulq_f32(u8x8_to_f32x4(y), scale);
            let pb = vsubq_f32(vmulq_f32(u8x8_to_f32x4(cb), scale), center);
            let pr = vsubq_f32(vmulq_f32(u8x8_to_f32x4(cr), scale), center);

            let rf = vminq_f32(vmaxq_f32(vaddq_f32(yy, vmulq_f32(k_pr_r, pr)), zero), one);
            let gf = vminq_f32(
                vmaxq_f32(
                    vaddq_f32(vaddq_f32(yy, vmulq_f32(k_pb_g, pb)), vmulq_f32(k_pr_g, pr)),
                    zero,
                ),
                one,
            );
            let bf = vminq_f32(vmaxq_f32(vaddq_f32(yy, vmulq_f32(k_pb_b, pb)), zero), one);

            store_bt709_f32x4_to_rgba8_neon(dst, *x, rf, gf, bf);
            *x += PIXELS_PER_NEON_STEP;
        }
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn ycbcr_full_range_bt709_row_420_neon(
    y_row: &[u8],
    cb_row: &[u8],
    cr_row: &[u8],
    dst: &mut [u8],
    width: usize,
    x: &mut usize,
) {
    unsafe {
        let scale = vdupq_n_f32(U8_TO_F32_SCALE);
        let center = vdupq_n_f32(CHROMA_CENTER);
        let k_pr_r = vdupq_n_f32(BT709_PR_TO_R);
        let k_pb_g = vdupq_n_f32(BT709_PB_TO_G);
        let k_pr_g = vdupq_n_f32(BT709_PR_TO_G);
        let k_pb_b = vdupq_n_f32(BT709_PB_TO_B);
        let zero = vdupq_n_f32(0.0);
        let one = vdupq_n_f32(1.0);

        while *x + PIXELS_PER_NEON_STEP <= width && ycbcr420_chroma_load8_fits(*x, cb_row.len()) {
            let xc = *x / 2;
            let y = load_u8x4_neon(y_row.as_ptr().add(*x));
            let cb = load_u8x4_420_chroma_neon(cb_row.as_ptr().add(xc));
            let cr = load_u8x4_420_chroma_neon(cr_row.as_ptr().add(xc));

            let yy = vmulq_f32(u8x8_to_f32x4(y), scale);
            let pb = vsubq_f32(vmulq_f32(u8x8_to_f32x4(cb), scale), center);
            let pr = vsubq_f32(vmulq_f32(u8x8_to_f32x4(cr), scale), center);

            let rf = vminq_f32(vmaxq_f32(vaddq_f32(yy, vmulq_f32(k_pr_r, pr)), zero), one);
            let gf = vminq_f32(
                vmaxq_f32(
                    vaddq_f32(vaddq_f32(yy, vmulq_f32(k_pb_g, pb)), vmulq_f32(k_pr_g, pr)),
                    zero,
                ),
                one,
            );
            let bf = vminq_f32(vmaxq_f32(vaddq_f32(yy, vmulq_f32(k_pb_b, pb)), zero), one);

            store_bt709_f32x4_to_rgba8_neon(dst, *x, rf, gf, bf);
            *x += PIXELS_PER_NEON_STEP;
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn ycbcr_limited_range_bt709_row_444_sse41(
    y_row: &[u8],
    cb_row: &[u8],
    cr_row: &[u8],
    dst: &mut [u8],
    width: usize,
    x: &mut usize,
) {
    unsafe {
        let luma_floor = _mm_set1_ps(STUDIO_LUMA_FLOOR);
        let luma_inv = _mm_set1_ps(STUDIO_LUMA_INV_SPAN);
        let chroma_mid = _mm_set1_ps(STUDIO_CHROMA_MID);
        let chroma_inv = _mm_set1_ps(STUDIO_CHROMA_INV_SPAN);
        let store = bt709_store_consts_sse41();

        while *x + PIXELS_PER_SSE41_STEP <= width {
            let y = _mm_cvtepi32_ps(_mm_cvtepu8_epi32(load_u8x4_for_cvtepu8(
                y_row.as_ptr().add(*x),
            )));
            let cb = _mm_cvtepi32_ps(_mm_cvtepu8_epi32(load_u8x4_for_cvtepu8(
                cb_row.as_ptr().add(*x),
            )));
            let cr = _mm_cvtepi32_ps(_mm_cvtepu8_epi32(load_u8x4_for_cvtepu8(
                cr_row.as_ptr().add(*x),
            )));

            let yy = _mm_mul_ps(_mm_sub_ps(y, luma_floor), luma_inv);
            let pb = _mm_mul_ps(_mm_sub_ps(cb, chroma_mid), chroma_inv);
            let pr = _mm_mul_ps(_mm_sub_ps(cr, chroma_mid), chroma_inv);
            bt709_store_normalized_sse41(yy, pb, pr, dst, *x, &store);
            *x += PIXELS_PER_SSE41_STEP;
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn ycbcr_limited_range_bt709_row_420_sse41(
    y_row: &[u8],
    cb_row: &[u8],
    cr_row: &[u8],
    dst: &mut [u8],
    width: usize,
    x: &mut usize,
) {
    unsafe {
        let luma_floor = _mm_set1_ps(STUDIO_LUMA_FLOOR);
        let luma_inv = _mm_set1_ps(STUDIO_LUMA_INV_SPAN);
        let chroma_mid = _mm_set1_ps(STUDIO_CHROMA_MID);
        let chroma_inv = _mm_set1_ps(STUDIO_CHROMA_INV_SPAN);
        let store = bt709_store_consts_sse41();
        let simd_end = (width / PIXELS_PER_SSE41_STEP) * PIXELS_PER_SSE41_STEP;
        let chroma_dup = _mm_setr_epi8(0, 0, 1, 1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1);

        while *x + PIXELS_PER_SSE41_STEP <= simd_end {
            let xc = *x / 2;
            let y = load_u8x4_for_cvtepu8(y_row.as_ptr().add(*x));
            // Low two bytes = chroma samples; pad high bytes. LE packing matches x86_64 lane order.
            let cb2 = _mm_cvtsi32_si128(i32::from_le_bytes([
                *cb_row.as_ptr().add(xc),
                *cb_row.as_ptr().add(xc + 1),
                0,
                0,
            ]));
            let cr2 = _mm_cvtsi32_si128(i32::from_le_bytes([
                *cr_row.as_ptr().add(xc),
                *cr_row.as_ptr().add(xc + 1),
                0,
                0,
            ]));
            let cbv = _mm_shuffle_epi8(cb2, chroma_dup);
            let crv = _mm_shuffle_epi8(cr2, chroma_dup);

            let y_f = _mm_cvtepi32_ps(_mm_cvtepu8_epi32(y));
            let cb_f = _mm_cvtepi32_ps(_mm_cvtepu8_epi32(cbv));
            let cr_f = _mm_cvtepi32_ps(_mm_cvtepu8_epi32(crv));
            let yy = _mm_mul_ps(_mm_sub_ps(y_f, luma_floor), luma_inv);
            let pb = _mm_mul_ps(_mm_sub_ps(cb_f, chroma_mid), chroma_inv);
            let pr = _mm_mul_ps(_mm_sub_ps(cr_f, chroma_mid), chroma_inv);
            bt709_store_normalized_sse41(yy, pb, pr, dst, *x, &store);
            *x += PIXELS_PER_SSE41_STEP;
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn ycbcr_limited_range_bt709_row_444_avx2(
    y_row: &[u8],
    cb_row: &[u8],
    cr_row: &[u8],
    dst: &mut [u8],
    width: usize,
    x: &mut usize,
) {
    unsafe {
        while *x + PIXELS_PER_AVX2_STEP <= width {
            let y = _mm256_cvtepu8_epi32(_mm_loadl_epi64(y_row.as_ptr().add(*x) as *const __m128i));
            let cb =
                _mm256_cvtepu8_epi32(_mm_loadl_epi64(cb_row.as_ptr().add(*x) as *const __m128i));
            let cr =
                _mm256_cvtepu8_epi32(_mm_loadl_epi64(cr_row.as_ptr().add(*x) as *const __m128i));
            store_limited_bt709_u8x8_from_i32(dst, *x, y, cb, cr);
            *x += PIXELS_PER_AVX2_STEP;
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn ycbcr_limited_range_bt709_row_420_avx2(
    y_row: &[u8],
    cb_row: &[u8],
    cr_row: &[u8],
    dst: &mut [u8],
    width: usize,
    x: &mut usize,
) {
    unsafe {
        while *x + PIXELS_PER_AVX2_STEP <= width && ycbcr420_chroma_load8_fits(*x, cb_row.len()) {
            let xc = *x / 2;
            let cb = _mm_loadl_epi64(cb_row.as_ptr().add(xc) as *const __m128i);
            let cr = _mm_loadl_epi64(cr_row.as_ptr().add(xc) as *const __m128i);
            // Upsample 4 chroma samples to 8 luma sites: [c0,c0,c1,c1,c2,c2,c3,c3].
            let cb_dup = _mm_shuffle_epi8(
                cb,
                _mm_setr_epi8(0, 0, 1, 1, 2, 2, 3, 3, -1, -1, -1, -1, -1, -1, -1, -1),
            );
            let cr_dup = _mm_shuffle_epi8(
                cr,
                _mm_setr_epi8(0, 0, 1, 1, 2, 2, 3, 3, -1, -1, -1, -1, -1, -1, -1, -1),
            );
            let y = _mm256_cvtepu8_epi32(_mm_loadl_epi64(y_row.as_ptr().add(*x) as *const __m128i));
            let cb = _mm256_cvtepu8_epi32(cb_dup);
            let cr = _mm256_cvtepu8_epi32(cr_dup);
            store_limited_bt709_u8x8_from_i32(dst, *x, y, cb, cr);
            *x += PIXELS_PER_AVX2_STEP;
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn store_limited_bt709_u8x8_from_i32(
    dst: &mut [u8],
    x: usize,
    y: __m256i,
    cb: __m256i,
    cr: __m256i,
) {
    unsafe {
        let luma_floor = _mm256_set1_ps(STUDIO_LUMA_FLOOR);
        let luma_inv = _mm256_set1_ps(STUDIO_LUMA_INV_SPAN);
        let chroma_mid = _mm256_set1_ps(STUDIO_CHROMA_MID);
        let chroma_inv = _mm256_set1_ps(STUDIO_CHROMA_INV_SPAN);
        let y_f = _mm256_cvtepi32_ps(y);
        let cb_f = _mm256_cvtepi32_ps(cb);
        let cr_f = _mm256_cvtepi32_ps(cr);
        let yy = _mm256_mul_ps(_mm256_sub_ps(y_f, luma_floor), luma_inv);
        let pb = _mm256_mul_ps(_mm256_sub_ps(cb_f, chroma_mid), chroma_inv);
        let pr = _mm256_mul_ps(_mm256_sub_ps(cr_f, chroma_mid), chroma_inv);

        let rf = bt709_rgb_channel_avx2(yy, pb, pr, 0.0, BT709_PR_TO_R);
        let gf = bt709_rgb_channel_avx2(yy, pb, pr, BT709_PB_TO_G, BT709_PR_TO_G);
        let bf = bt709_rgb_channel_avx2(yy, pb, pr, BT709_PB_TO_B, 0.0);
        store_bt709_f32x8_to_rgba8_avx2(dst, x, rf, gf, bf);
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn bt709_store_normalized_neon(
    yy: float32x4_t,
    pb: float32x4_t,
    pr: float32x4_t,
    dst: &mut [u8],
    x: usize,
) {
    unsafe {
        let k_pr_r = vdupq_n_f32(BT709_PR_TO_R);
        let k_pb_g = vdupq_n_f32(BT709_PB_TO_G);
        let k_pr_g = vdupq_n_f32(BT709_PR_TO_G);
        let k_pb_b = vdupq_n_f32(BT709_PB_TO_B);
        let zero = vdupq_n_f32(0.0);
        let one = vdupq_n_f32(1.0);

        let rf = vminq_f32(vmaxq_f32(vaddq_f32(yy, vmulq_f32(k_pr_r, pr)), zero), one);
        let gf = vminq_f32(
            vmaxq_f32(
                vaddq_f32(vaddq_f32(yy, vmulq_f32(k_pb_g, pb)), vmulq_f32(k_pr_g, pr)),
                zero,
            ),
            one,
        );
        let bf = vminq_f32(vmaxq_f32(vaddq_f32(yy, vmulq_f32(k_pb_b, pb)), zero), one);

        store_bt709_f32x4_to_rgba8_neon(dst, x, rf, gf, bf);
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn ycbcr_limited_range_bt709_row_444_neon(
    y_row: &[u8],
    cb_row: &[u8],
    cr_row: &[u8],
    dst: &mut [u8],
    width: usize,
    x: &mut usize,
) {
    unsafe {
        let luma_floor = vdupq_n_f32(STUDIO_LUMA_FLOOR);
        let luma_inv = vdupq_n_f32(STUDIO_LUMA_INV_SPAN);
        let chroma_mid = vdupq_n_f32(STUDIO_CHROMA_MID);
        let chroma_inv = vdupq_n_f32(STUDIO_CHROMA_INV_SPAN);

        while *x + PIXELS_PER_NEON_STEP <= width {
            let y = load_u8x4_neon(y_row.as_ptr().add(*x));
            let cb = load_u8x4_neon(cb_row.as_ptr().add(*x));
            let cr = load_u8x4_neon(cr_row.as_ptr().add(*x));

            let y_f = u8x8_to_f32x4(y);
            let cb_f = u8x8_to_f32x4(cb);
            let cr_f = u8x8_to_f32x4(cr);
            let yy = vmulq_f32(vsubq_f32(y_f, luma_floor), luma_inv);
            let pb = vmulq_f32(vsubq_f32(cb_f, chroma_mid), chroma_inv);
            let pr = vmulq_f32(vsubq_f32(cr_f, chroma_mid), chroma_inv);
            bt709_store_normalized_neon(yy, pb, pr, dst, *x);
            *x += PIXELS_PER_NEON_STEP;
        }
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn ycbcr_limited_range_bt709_row_420_neon(
    y_row: &[u8],
    cb_row: &[u8],
    cr_row: &[u8],
    dst: &mut [u8],
    width: usize,
    x: &mut usize,
) {
    unsafe {
        let luma_floor = vdupq_n_f32(STUDIO_LUMA_FLOOR);
        let luma_inv = vdupq_n_f32(STUDIO_LUMA_INV_SPAN);
        let chroma_mid = vdupq_n_f32(STUDIO_CHROMA_MID);
        let chroma_inv = vdupq_n_f32(STUDIO_CHROMA_INV_SPAN);

        while *x + PIXELS_PER_NEON_STEP <= width && ycbcr420_chroma_load8_fits(*x, cb_row.len()) {
            let xc = *x / 2;
            let y = load_u8x4_neon(y_row.as_ptr().add(*x));
            let cb = load_u8x4_420_chroma_neon(cb_row.as_ptr().add(xc));
            let cr = load_u8x4_420_chroma_neon(cr_row.as_ptr().add(xc));

            let y_f = u8x8_to_f32x4(y);
            let cb_f = u8x8_to_f32x4(cb);
            let cr_f = u8x8_to_f32x4(cr);
            let yy = vmulq_f32(vsubq_f32(y_f, luma_floor), luma_inv);
            let pb = vmulq_f32(vsubq_f32(cb_f, chroma_mid), chroma_inv);
            let pr = vmulq_f32(vsubq_f32(cr_f, chroma_mid), chroma_inv);
            bt709_store_normalized_neon(yy, pb, pr, dst, *x);
            *x += PIXELS_PER_NEON_STEP;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scalar_row_444(y_row: &[u8], cb_row: &[u8], cr_row: &[u8], width: usize) -> Vec<u8> {
        let mut dst = vec![0_u8; width * 4];
        for (x, &y_value) in y_row.iter().take(width).enumerate() {
            write_bt709_pixel(dst.as_mut_slice(), x, y_value, cb_row[x], cr_row[x]);
        }
        dst
    }

    fn scalar_row_420(y_row: &[u8], cb_row: &[u8], cr_row: &[u8], width: usize) -> Vec<u8> {
        let mut dst = vec![0_u8; width * 4];
        for (x, &y_value) in y_row.iter().take(width).enumerate() {
            let xc = x / 2;
            write_bt709_pixel(dst.as_mut_slice(), x, y_value, cb_row[xc], cr_row[xc]);
        }
        dst
    }

    fn scalar_limited_row_444(y_row: &[u8], cb_row: &[u8], cr_row: &[u8], width: usize) -> Vec<u8> {
        let mut dst = vec![0_u8; width * 4];
        for (x, &y_value) in y_row.iter().take(width).enumerate() {
            write_limited_bt709_pixel(dst.as_mut_slice(), x, y_value, cb_row[x], cr_row[x]);
        }
        dst
    }

    fn scalar_limited_row_420(y_row: &[u8], cb_row: &[u8], cr_row: &[u8], width: usize) -> Vec<u8> {
        let mut dst = vec![0_u8; width * 4];
        for (x, &y_value) in y_row.iter().take(width).enumerate() {
            let xc = x / 2;
            write_limited_bt709_pixel(dst.as_mut_slice(), x, y_value, cb_row[xc], cr_row[xc]);
        }
        dst
    }

    #[test]
    fn ycbcr_full_range_bt709_444_matches_scalar() {
        for width in [0_usize, 1, 3, 4, 7, 8, 9, 16] {
            let y: Vec<u8> = (0..width).map(|i| ((i * 17 + 11) % 256) as u8).collect();
            let cb: Vec<u8> = (0..width).map(|i| ((i * 23 + 31) % 256) as u8).collect();
            let cr: Vec<u8> = (0..width).map(|i| ((i * 29 + 43) % 256) as u8).collect();
            let expected = scalar_row_444(&y, &cb, &cr, width);
            let mut simd = vec![0_u8; width * 4];
            ycbcr_full_range_bt709_row_444_to_rgba8(&y, &cb, &cr, &mut simd, width);
            assert_eq!(simd, expected, "width={width}");
        }
    }

    #[test]
    fn ycbcr_full_range_bt709_420_matches_scalar() {
        for width in [0_usize, 1, 2, 3, 4, 7, 8, 9, 16] {
            let chroma_len = width.div_ceil(2);
            let y: Vec<u8> = (0..width).map(|i| ((i * 13 + 7) % 256) as u8).collect();
            let cb: Vec<u8> = (0..chroma_len)
                .map(|i| ((i * 19 + 5) % 256) as u8)
                .collect();
            let cr: Vec<u8> = (0..chroma_len)
                .map(|i| ((i * 31 + 3) % 256) as u8)
                .collect();
            let expected = scalar_row_420(&y, &cb, &cr, width);
            let mut simd = vec![0_u8; width * 4];
            ycbcr_full_range_bt709_row_420_to_rgba8(&y, &cb, &cr, &mut simd, width);
            assert_eq!(simd, expected, "width={width}");
        }
    }

    #[test]
    fn ycbcr_limited_range_bt709_444_matches_scalar() {
        for width in [0_usize, 1, 3, 4, 7, 8, 9, 16] {
            let y: Vec<u8> = (0..width).map(|i| ((i * 17 + 11) % 256) as u8).collect();
            let cb: Vec<u8> = (0..width).map(|i| ((i * 23 + 31) % 256) as u8).collect();
            let cr: Vec<u8> = (0..width).map(|i| ((i * 29 + 43) % 256) as u8).collect();
            let expected = scalar_limited_row_444(&y, &cb, &cr, width);
            let mut simd = vec![0_u8; width * 4];
            ycbcr_limited_range_bt709_row_444_to_rgba8(&y, &cb, &cr, &mut simd, width);
            assert_eq!(simd, expected, "width={width}");
        }
    }

    #[test]
    fn ycbcr_limited_range_bt709_420_matches_scalar() {
        for width in [0_usize, 1, 2, 3, 4, 7, 8, 9, 16] {
            let chroma_len = width.div_ceil(2);
            let y: Vec<u8> = (0..width).map(|i| ((i * 13 + 7) % 256) as u8).collect();
            let cb: Vec<u8> = (0..chroma_len)
                .map(|i| ((i * 19 + 5) % 256) as u8)
                .collect();
            let cr: Vec<u8> = (0..chroma_len)
                .map(|i| ((i * 31 + 3) % 256) as u8)
                .collect();
            let expected = scalar_limited_row_420(&y, &cb, &cr, width);
            let mut simd = vec![0_u8; width * 4];
            ycbcr_limited_range_bt709_row_420_to_rgba8(&y, &cb, &cr, &mut simd, width);
            assert_eq!(simd, expected, "width={width}");
        }
    }
}
