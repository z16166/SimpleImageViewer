//! PSD/PSB ZIP and ZIP+prediction channel decompression (compression codes 2 and 3).
//!
//! Uses `miniz_oxide` (zlib-compatible inflate). Prediction undo follows Adobe's
//! horizontal differencing: 8/16-bit per sample; 32-bit uses byte-plane packing.
//!
//! 8-bit prediction undo uses a SIMD inclusive prefix-sum (doubling shifts) per
//! scanline chunk, with a scalar carry across chunks.

use miniz_oxide::inflate::{DecompressError, decompress_to_vec_zlib_with_limit};

#[cfg(target_arch = "x86_64")]
const PREFIX_SUM_SSE_BYTES: usize = 16;
#[cfg(target_arch = "x86_64")]
const PREFIX_SUM_AVX2_BYTES: usize = 32;
#[cfg(target_arch = "aarch64")]
const PREFIX_SUM_NEON_BYTES: usize = 16;
/// SSE/NEON process 8 big-endian u16 samples (16 bytes) per chunk.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
const PREFIX_SUM_U16_SSE_SAMPLES: usize = 8;
/// AVX2 processes 16 big-endian u16 samples (32 bytes) per chunk.
#[cfg(target_arch = "x86_64")]
const PREFIX_SUM_U16_AVX2_SAMPLES: usize = 16;

/// Inflate a zlib-wrapped DEFLATE stream into exactly `expected_len` bytes.
pub(crate) fn inflate_zlib_exact(
    compressed: &[u8],
    expected_len: usize,
) -> Result<Vec<u8>, String> {
    if expected_len == 0 {
        return Ok(Vec::new());
    }
    let out = decompress_to_vec_zlib_with_limit(compressed, expected_len).map_err(inflate_err)?;
    if out.len() != expected_len {
        return Err(format!(
            "PSD/PSB ZIP inflate size mismatch: got {} bytes, expected {expected_len}",
            out.len()
        ));
    }
    Ok(out)
}

fn inflate_err(e: DecompressError) -> String {
    format!("PSD/PSB ZIP inflate failed: {e}")
}

/// Undo ZIP-with-prediction differencing in-place on planar channel bytes.
///
/// `width` is samples per scanline. `depth` is 8, 16, or 32. The buffer must
/// contain a whole number of scanlines.
pub(crate) fn undo_zip_prediction(buf: &mut [u8], width: usize, depth: u16) -> Result<(), String> {
    if width == 0 || buf.is_empty() {
        return Ok(());
    }
    match depth {
        8 => {
            let row_bytes = width;
            if !buf.len().is_multiple_of(row_bytes) {
                return Err("PSD/PSB ZIP prediction 8-bit buffer length mismatch".into());
            }
            for row in buf.chunks_exact_mut(row_bytes) {
                prefix_sum_u8_inplace(row);
            }
            Ok(())
        }
        16 => {
            let row_bytes = width
                .checked_mul(2)
                .ok_or("PSD/PSB ZIP prediction overflow")?;
            if !buf.len().is_multiple_of(row_bytes) {
                return Err("PSD/PSB ZIP prediction 16-bit buffer length mismatch".into());
            }
            for row in buf.chunks_exact_mut(row_bytes) {
                prefix_sum_u16be_inplace(row);
            }
            Ok(())
        }
        32 => undo_zip_prediction_32(buf, width),
        _ => Err(format!("PSD/PSB ZIP prediction unsupported depth: {depth}")),
    }
}

/// 32-bit ZIP prediction stores each scanline as four delta-encoded byte planes
/// (all byte0, then byte1, byte2, byte3), then must be re-interleaved to BE floats.
fn undo_zip_prediction_32(buf: &mut [u8], width: usize) -> Result<(), String> {
    let row_bytes = width
        .checked_mul(4)
        .ok_or("PSD/PSB ZIP prediction overflow")?;
    if !buf.len().is_multiple_of(row_bytes) {
        return Err("PSD/PSB ZIP prediction 32-bit buffer length mismatch".into());
    }
    let mut scratch = vec![0u8; row_bytes];
    for row in buf.chunks_exact_mut(row_bytes) {
        // Undo delta on each of the 4 packed planes.
        for plane in 0..4 {
            let start = plane * width;
            let end = start + width;
            prefix_sum_u8_inplace(&mut row[start..end]);
        }
        // Re-interleave: plane p sample x -> sample bytes [x*4 + p]
        scratch.fill(0);
        for x in 0..width {
            scratch[x * 4] = row[x];
            scratch[x * 4 + 1] = row[width + x];
            scratch[x * 4 + 2] = row[width * 2 + x];
            scratch[x * 4 + 3] = row[width * 3 + x];
        }
        row.copy_from_slice(&scratch);
    }
    Ok(())
}

/// Inclusive wrapping prefix-sum on a big-endian u16 scanline (Adobe 16-bit ZIP prediction).
fn prefix_sum_u16be_inplace(row: &mut [u8]) {
    debug_assert!(row.len().is_multiple_of(2));
    if row.is_empty() {
        return;
    }

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            unsafe {
                prefix_sum_u16be_avx2(row);
            }
            return;
        }
        if is_x86_feature_detected!("sse2") {
            unsafe {
                prefix_sum_u16be_sse2(row);
            }
            return;
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        unsafe {
            prefix_sum_u16be_neon(row);
        }
        return;
    }

    prefix_sum_u16be_scalar(row);
}

#[inline]
fn prefix_sum_u16be_scalar(row: &mut [u8]) {
    let mut acc = 0u16;
    for sample in row.chunks_exact_mut(2) {
        let delta = u16::from_be_bytes([sample[0], sample[1]]);
        acc = acc.wrapping_add(delta);
        let be = acc.to_be_bytes();
        sample[0] = be[0];
        sample[1] = be[1];
    }
}

/// Swap bytes within each u16 lane (BE <-> LE on little-endian hosts).
#[cfg(target_arch = "x86_64")]
#[inline]
unsafe fn swap_u16_bytes_sse2(v: core::arch::x86_64::__m128i) -> core::arch::x86_64::__m128i {
    use core::arch::x86_64::*;
    unsafe { _mm_or_si128(_mm_slli_epi16(v, 8), _mm_srli_epi16(v, 8)) }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn prefix_sum_u16be_sse2(row: &mut [u8]) {
    use core::arch::x86_64::*;
    let n = row.len() / 2;
    let mut i = 0usize;
    let mut carry = 0u16;
    while i + PREFIX_SUM_U16_SSE_SAMPLES <= n {
        unsafe {
            let be = _mm_loadu_si128(row.as_ptr().add(i * 2).cast());
            let mut v = swap_u16_bytes_sse2(be);
            // Fold previous chunk carry into lane 0.
            v = _mm_add_epi16(v, _mm_cvtsi32_si128(carry as i32));
            // Hillis-Steele inclusive scan over 8 u16 lanes.
            v = _mm_add_epi16(v, _mm_slli_si128(v, 2));
            v = _mm_add_epi16(v, _mm_slli_si128(v, 4));
            v = _mm_add_epi16(v, _mm_slli_si128(v, 8));
            // Last lane (bytes 14..16) becomes the next carry.
            carry = _mm_extract_epi16::<7>(v) as u16;
            let out = swap_u16_bytes_sse2(v);
            _mm_storeu_si128(row.as_mut_ptr().add(i * 2).cast(), out);
        }
        i += PREFIX_SUM_U16_SSE_SAMPLES;
    }
    while i < n {
        let off = i * 2;
        let delta = u16::from_be_bytes([row[off], row[off + 1]]);
        carry = carry.wrapping_add(delta);
        let be = carry.to_be_bytes();
        row[off] = be[0];
        row[off + 1] = be[1];
        i += 1;
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn prefix_sum_u16be_avx2(row: &mut [u8]) {
    use core::arch::x86_64::*;
    let n = row.len() / 2;
    let mut i = 0usize;
    let mut carry = 0u16;
    while i + PREFIX_SUM_U16_AVX2_SAMPLES <= n {
        unsafe {
            let be = _mm256_loadu_si256(row.as_ptr().add(i * 2).cast());
            // Byte-swap each u16 lane across both 128-bit halves.
            let mut v = _mm256_or_si256(_mm256_slli_epi16(be, 8), _mm256_srli_epi16(be, 8));
            v = _mm256_add_epi16(v, _mm256_castsi128_si256(_mm_cvtsi32_si128(carry as i32)));

            let mut lo = _mm256_castsi256_si128(v);
            lo = _mm_add_epi16(lo, _mm_slli_si128(lo, 2));
            lo = _mm_add_epi16(lo, _mm_slli_si128(lo, 4));
            lo = _mm_add_epi16(lo, _mm_slli_si128(lo, 8));
            let lo_last = _mm_extract_epi16::<7>(lo) as u16;

            let mut hi = _mm256_extracti128_si256::<1>(v);
            hi = _mm_add_epi16(hi, _mm_cvtsi32_si128(lo_last as i32));
            hi = _mm_add_epi16(hi, _mm_slli_si128(hi, 2));
            hi = _mm_add_epi16(hi, _mm_slli_si128(hi, 4));
            hi = _mm_add_epi16(hi, _mm_slli_si128(hi, 8));
            carry = _mm_extract_epi16::<7>(hi) as u16;

            let scanned = _mm256_set_m128i(hi, lo);
            let out = _mm256_or_si256(_mm256_slli_epi16(scanned, 8), _mm256_srli_epi16(scanned, 8));
            _mm256_storeu_si256(row.as_mut_ptr().add(i * 2).cast(), out);
        }
        i += PREFIX_SUM_U16_AVX2_SAMPLES;
    }
    // Remainder: reuse SSE path when possible, else scalar with current carry.
    if i + PREFIX_SUM_U16_SSE_SAMPLES <= n {
        // Keep carry continuity via scalar for the tail (avoids re-entering SSE with carry=0).
        while i < n {
            let off = i * 2;
            let delta = u16::from_be_bytes([row[off], row[off + 1]]);
            carry = carry.wrapping_add(delta);
            let be = carry.to_be_bytes();
            row[off] = be[0];
            row[off + 1] = be[1];
            i += 1;
        }
    } else {
        while i < n {
            let off = i * 2;
            let delta = u16::from_be_bytes([row[off], row[off + 1]]);
            carry = carry.wrapping_add(delta);
            let be = carry.to_be_bytes();
            row[off] = be[0];
            row[off + 1] = be[1];
            i += 1;
        }
    }
}

#[cfg(target_arch = "aarch64")]
unsafe fn prefix_sum_u16be_neon(row: &mut [u8]) {
    use core::arch::aarch64::*;
    let n = row.len() / 2;
    let mut i = 0usize;
    let mut carry = 0u16;
    while i + PREFIX_SUM_U16_SSE_SAMPLES <= n {
        unsafe {
            let be_bytes = vld1q_u8(row.as_ptr().add(i * 2));
            // BE [hi,lo] -> LE lane value via rev16.
            let mut v = vreinterpretq_u16_u8(vrev16q_u8(be_bytes));
            v = vaddq_u16(v, vsetq_lane_u16::<0>(carry, vdupq_n_u16(0)));
            // Hillis-Steele: shift left by 1/2/4 u16 lanes via vext on bytes.
            v = vaddq_u16(
                v,
                vreinterpretq_u16_u8(vextq_u8(vdupq_n_u8(0), vreinterpretq_u8_u16(v), 14)),
            );
            v = vaddq_u16(
                v,
                vreinterpretq_u16_u8(vextq_u8(vdupq_n_u8(0), vreinterpretq_u8_u16(v), 12)),
            );
            v = vaddq_u16(
                v,
                vreinterpretq_u16_u8(vextq_u8(vdupq_n_u8(0), vreinterpretq_u8_u16(v), 8)),
            );
            carry = vgetq_lane_u16::<7>(v);
            let out = vrev16q_u8(vreinterpretq_u8_u16(v));
            vst1q_u8(row.as_mut_ptr().add(i * 2), out);
        }
        i += PREFIX_SUM_U16_SSE_SAMPLES;
    }
    while i < n {
        let off = i * 2;
        let delta = u16::from_be_bytes([row[off], row[off + 1]]);
        carry = carry.wrapping_add(delta);
        let be = carry.to_be_bytes();
        row[off] = be[0];
        row[off + 1] = be[1];
        i += 1;
    }
}

/// Inclusive wrapping prefix-sum on a scanline (Adobe 8-bit ZIP prediction undo).
fn prefix_sum_u8_inplace(row: &mut [u8]) {
    if row.is_empty() {
        return;
    }

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            unsafe {
                prefix_sum_u8_avx2(row);
            }
            return;
        }
        if is_x86_feature_detected!("sse2") {
            unsafe {
                prefix_sum_u8_sse2(row);
            }
            return;
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        unsafe {
            prefix_sum_u8_neon(row);
        }
        return;
    }

    prefix_sum_u8_scalar(row);
}

#[inline]
fn prefix_sum_u8_scalar(row: &mut [u8]) {
    let mut acc = row[0];
    for px in row.iter_mut().skip(1) {
        acc = acc.wrapping_add(*px);
        *px = acc;
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn prefix_sum_u8_sse2(row: &mut [u8]) {
    use core::arch::x86_64::*;
    let mut i = 0usize;
    let n = row.len();
    let mut carry = 0u8;
    while i + PREFIX_SUM_SSE_BYTES <= n {
        unsafe {
            let mut v = _mm_loadu_si128(row.as_ptr().add(i).cast());
            // Fold previous chunk's last prefix into this chunk's first delta.
            v = _mm_add_epi8(v, _mm_cvtsi32_si128(carry as i32));
            // Hillis-Steele inclusive scan via doubling byte shifts.
            v = _mm_add_epi8(v, _mm_slli_si128(v, 1));
            v = _mm_add_epi8(v, _mm_slli_si128(v, 2));
            v = _mm_add_epi8(v, _mm_slli_si128(v, 4));
            v = _mm_add_epi8(v, _mm_slli_si128(v, 8));
            _mm_storeu_si128(row.as_mut_ptr().add(i).cast(), v);
            carry = _mm_cvtsi128_si32(_mm_srli_si128(v, 15)) as u8;
        }
        i += PREFIX_SUM_SSE_BYTES;
    }
    while i < n {
        carry = carry.wrapping_add(row[i]);
        row[i] = carry;
        i += 1;
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn prefix_sum_u8_avx2(row: &mut [u8]) {
    use core::arch::x86_64::*;
    let mut i = 0usize;
    let n = row.len();
    let mut carry = 0u8;
    while i + PREFIX_SUM_AVX2_BYTES <= n {
        unsafe {
            let mut v = _mm256_loadu_si256(row.as_ptr().add(i).cast());
            // Fold previous chunk carry into lane0 byte0.
            v = _mm256_add_epi8(v, _mm256_castsi128_si256(_mm_cvtsi32_si128(carry as i32)));
            // Lane-local doubling (16B each); then add low-lane total into high lane.
            let mut lo = _mm256_castsi256_si128(v);
            lo = _mm_add_epi8(lo, _mm_slli_si128(lo, 1));
            lo = _mm_add_epi8(lo, _mm_slli_si128(lo, 2));
            lo = _mm_add_epi8(lo, _mm_slli_si128(lo, 4));
            lo = _mm_add_epi8(lo, _mm_slli_si128(lo, 8));
            let lo_last = _mm_cvtsi128_si32(_mm_srli_si128(lo, 15)) as u8;

            let mut hi = _mm256_extracti128_si256::<1>(v);
            hi = _mm_add_epi8(hi, _mm_cvtsi32_si128(lo_last as i32));
            hi = _mm_add_epi8(hi, _mm_slli_si128(hi, 1));
            hi = _mm_add_epi8(hi, _mm_slli_si128(hi, 2));
            hi = _mm_add_epi8(hi, _mm_slli_si128(hi, 4));
            hi = _mm_add_epi8(hi, _mm_slli_si128(hi, 8));
            carry = _mm_cvtsi128_si32(_mm_srli_si128(hi, 15)) as u8;

            let out = _mm256_set_m128i(hi, lo);
            _mm256_storeu_si256(row.as_mut_ptr().add(i).cast(), out);
        }
        i += PREFIX_SUM_AVX2_BYTES;
    }
    if i + PREFIX_SUM_SSE_BYTES <= n {
        // Reuse SSE path for the remainder with current carry -- fall back to scalar
        // from here so carry stays correct without re-entering a fresh SSE carry=0.
        while i < n {
            carry = carry.wrapping_add(row[i]);
            row[i] = carry;
            i += 1;
        }
    } else {
        while i < n {
            carry = carry.wrapping_add(row[i]);
            row[i] = carry;
            i += 1;
        }
    }
}

#[cfg(target_arch = "aarch64")]
unsafe fn prefix_sum_u8_neon(row: &mut [u8]) {
    use core::arch::aarch64::*;
    let mut i = 0usize;
    let n = row.len();
    let mut carry = 0u8;
    while i + PREFIX_SUM_NEON_BYTES <= n {
        unsafe {
            let mut v = vld1q_u8(row.as_ptr().add(i));
            v = vaddq_u8(v, vsetq_lane_u8::<0>(carry, vdupq_n_u8(0)));
            v = vaddq_u8(v, vextq_u8(vdupq_n_u8(0), v, 15)); // shift left 1
            v = vaddq_u8(v, vextq_u8(vdupq_n_u8(0), v, 14)); // shift left 2
            v = vaddq_u8(v, vextq_u8(vdupq_n_u8(0), v, 12)); // shift left 4
            v = vaddq_u8(v, vextq_u8(vdupq_n_u8(0), v, 8)); // shift left 8
            vst1q_u8(row.as_mut_ptr().add(i), v);
            carry = vgetq_lane_u8::<15>(v);
        }
        i += PREFIX_SUM_NEON_BYTES;
    }
    while i < n {
        carry = carry.wrapping_add(row[i]);
        row[i] = carry;
        i += 1;
    }
}

/// Inflate ZIP / ZIP+prediction payload into raw planar samples.
pub(crate) fn decode_zip_channel_bytes(
    compressed: &[u8],
    width: usize,
    height: usize,
    depth: u16,
    with_prediction: bool,
) -> Result<Vec<u8>, String> {
    let bps = match depth {
        8 => 1usize,
        16 => 2,
        32 => 4,
        _ => return Err(format!("PSD/PSB ZIP unsupported depth: {depth}")),
    };
    let expected = width
        .checked_mul(height)
        .and_then(|n| n.checked_mul(bps))
        .ok_or_else(|| "PSD/PSB ZIP output size overflow".to_string())?;
    let mut raw = inflate_zlib_exact(compressed, expected)?;
    if with_prediction {
        undo_zip_prediction(&mut raw, width, depth)?;
    }
    Ok(raw)
}

#[cfg(test)]
mod tests {
    use super::{decode_zip_channel_bytes, undo_zip_prediction};
    use miniz_oxide::deflate::compress_to_vec_zlib;

    #[test]
    fn zip_roundtrip_8bit_without_prediction() {
        let width = 4usize;
        let height = 2usize;
        let raw = vec![10u8, 20, 30, 40, 50, 60, 70, 80];
        let compressed = compress_to_vec_zlib(&raw, 6);
        let out = decode_zip_channel_bytes(&compressed, width, height, 8, false).unwrap();
        assert_eq!(out, raw);
    }

    #[test]
    fn zip_prediction_8bit_undo() {
        // Encoded deltas for row [10, 20, 30, 40] => [10, 10, 10, 10]
        let mut encoded = vec![10u8, 10, 10, 10, 5, 5, 5, 5];
        undo_zip_prediction(&mut encoded, 4, 8).unwrap();
        assert_eq!(encoded, vec![10, 20, 30, 40, 5, 10, 15, 20]);
    }

    #[test]
    fn zip_prediction_8bit_undo_long_row() {
        let width = 100usize;
        let mut encoded: Vec<u8> = (0..width).map(|i| (i % 17) as u8).collect();
        let mut expected = encoded.clone();
        let mut acc = expected[0];
        for px in expected.iter_mut().skip(1) {
            acc = acc.wrapping_add(*px);
            *px = acc;
        }
        undo_zip_prediction(&mut encoded, width, 8).unwrap();
        assert_eq!(encoded, expected);
    }

    #[test]
    fn zip_prediction_roundtrip_8bit() {
        let width = 3usize;
        let height = 2usize;
        let original = vec![1u8, 3, 7, 2, 2, 9];
        let mut predicted = original.clone();
        // Apply prediction (encode).
        for row in predicted.chunks_exact_mut(width) {
            for x in (1..width).rev() {
                row[x] = row[x].wrapping_sub(row[x - 1]);
            }
        }
        let compressed = compress_to_vec_zlib(&predicted, 6);
        let out = decode_zip_channel_bytes(&compressed, width, height, 8, true).unwrap();
        assert_eq!(out, original);
    }

    #[test]
    fn zip_prediction_16bit_undo_short() {
        // BE deltas [0x0001, 0x0002, 0x00FF] -> [1, 3, 0x102]
        let mut encoded = vec![0x00, 0x01, 0x00, 0x02, 0x00, 0xFF];
        undo_zip_prediction(&mut encoded, 3, 16).unwrap();
        assert_eq!(encoded, vec![0x00, 0x01, 0x00, 0x03, 0x01, 0x02]);
    }

    #[test]
    fn zip_prediction_16bit_undo_long_row_matches_scalar() {
        let width = 257usize;
        let mut encoded = Vec::with_capacity(width * 2);
        for i in 0..width {
            let v = ((i * 37) as u16).wrapping_mul(3);
            encoded.extend_from_slice(&v.to_be_bytes());
        }
        let mut expected = encoded.clone();
        super::prefix_sum_u16be_scalar(&mut expected);
        undo_zip_prediction(&mut encoded, width, 16).unwrap();
        assert_eq!(encoded, expected);
    }

    #[test]
    fn zip_prediction_roundtrip_16bit() {
        let width = 5usize;
        let height = 2usize;
        let mut original = Vec::new();
        for i in 0..(width * height) {
            original.extend_from_slice(&((i as u16) * 1000).to_be_bytes());
        }
        let mut predicted = original.clone();
        for row in predicted.chunks_exact_mut(width * 2) {
            let mut prev = 0u16;
            for sample in row.chunks_exact_mut(2) {
                let cur = u16::from_be_bytes([sample[0], sample[1]]);
                let delta = cur.wrapping_sub(prev);
                let be = delta.to_be_bytes();
                sample[0] = be[0];
                sample[1] = be[1];
                prev = cur;
            }
        }
        let compressed = compress_to_vec_zlib(&predicted, 6);
        let out = decode_zip_channel_bytes(&compressed, width, height, 16, true).unwrap();
        assert_eq!(out, original);
    }
}
