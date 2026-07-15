//! Mask density scaling and Gaussian-feather blur (SIMD-accelerated).

/// Apply density scaling (0-255) to a layer-sized mask buffer.
/// Density 255 = no change, density 0 = all zero (fully transparent).
pub(crate) fn apply_mask_density(mask: &mut [u8], density: u8) {
    if density >= 255 {
        return;
    }
    if density == 0 {
        mask.fill(0);
        return;
    }
    // Scale each byte: mask[i] = mask[i] * density / 255
    let d = u16::from(density);
    for b in mask.iter_mut() {
        *b = ((u16::from(*b) * d + 127) / 255) as u8;
    }
}

/// Maximum mask feather radius in pixels.
///
/// Photoshop's Layer Mask feather UI caps at 250 px; the "Feather Selection"
/// dialog allows up to 1000 px.  Our convolution runs in O(w×h×kernel_len) —
/// with `kernel_len = 2·ceil(1.25·feather) + 1` — so an unbounded feather can
/// stall the UI for minutes on large documents.  We clamp to this safe upper
/// bound and log a warning when it triggers.
pub(crate) const MAX_MASK_FEATHER: f64 = 1024.0;

/// Apply a separable Gaussian blur as a Photoshop-compatible mask feather.
/// `feather` is the pixel radius; values ≤ 0.5 produce a no-op.
///
/// Sigma is derived as `feather / 2.0`; the kernel spans ±2.5σ (capturing
/// >99 % of the Gaussian mass), clamped to the image edge.  The feather value
/// is clamped to [`MAX_MASK_FEATHER`] to prevent pathological runtime.
pub(crate) fn apply_mask_feather(mask: &mut [u8], w: u32, h: u32, feather: f64) {
    let feather = if feather > MAX_MASK_FEATHER {
        log::warn!(
            "PSD/PSB mask feather {feather:.1} exceeds max {MAX_MASK_FEATHER}; clamping to \
             {MAX_MASK_FEATHER} to prevent excessive composite time"
        );
        MAX_MASK_FEATHER
    } else {
        feather
    };
    if feather <= 0.5 || w < 2 || h < 2 {
        return;
    }
    let sigma = feather / 2.0;
    let radius = (sigma * 2.5).ceil() as u32;
    if radius == 0 {
        return;
    }
    let kernel_len = (radius * 2 + 1) as usize;

    // Precompute Gaussian kernel weights and normalize (directly in f32 — the
    // extra precision of f64 is unnecessary for u8 feather output and avoiding
    // the f64→f32 conversion saves one Vec allocation).
    let mut kernel = Vec::with_capacity(kernel_len);
    let s2 = -(2.0_f32 * (sigma as f32) * (sigma as f32));
    let mut total = 0.0_f32;
    for i in 0..kernel_len {
        let x = (i as f32) - radius as f32;
        let w = (x * x / s2).exp();
        kernel.push(w);
        total += w;
    }
    let inv_total = 1.0 / total;
    for w in &mut kernel {
        *w *= inv_total;
    }

    let wp = w as usize;
    let hp = h as usize;
    let mut tmp = vec![0.0f32; wp * hp];

    feather_horizontal_pass(mask, &mut tmp, wp, hp, radius as usize, &kernel);
    feather_vertical_pass(&tmp, mask, wp, hp, radius as usize, &kernel);
}

// ── SIMD-accelerated horizontal / vertical passes ────────────────────────

#[cfg(target_arch = "x86_64")]
fn feather_horizontal_pass(
    mask: &[u8],
    tmp: &mut [f32],
    wp: usize,
    hp: usize,
    radius: usize,
    kernel: &[f32],
) {
    #[target_feature(enable = "avx2")]
    unsafe fn run_avx2(
        mask: &[u8],
        tmp: &mut [f32],
        wp: usize,
        hp: usize,
        radius: usize,
        kernel: &[f32],
    ) {
        unsafe {
            feather_horizontal_avx2(mask, tmp, wp, hp, radius, kernel);
        }
    }
    #[target_feature(enable = "sse4.1")]
    unsafe fn run_sse41(
        mask: &[u8],
        tmp: &mut [f32],
        wp: usize,
        hp: usize,
        radius: usize,
        kernel: &[f32],
    ) {
        unsafe {
            feather_horizontal_sse41(mask, tmp, wp, hp, radius, kernel);
        }
    }
    if is_x86_feature_detected!("avx2") {
        unsafe {
            run_avx2(mask, tmp, wp, hp, radius, kernel);
        }
    } else if is_x86_feature_detected!("sse4.1") {
        unsafe {
            run_sse41(mask, tmp, wp, hp, radius, kernel);
        }
    } else {
        feather_horizontal_scalar(mask, tmp, wp, hp, radius, kernel);
    }
}

#[cfg(not(target_arch = "x86_64"))]
fn feather_horizontal_pass(
    mask: &[u8],
    tmp: &mut [f32],
    wp: usize,
    hp: usize,
    radius: usize,
    kernel: &[f32],
) {
    #[cfg(target_arch = "aarch64")]
    {
        if std::arch::is_aarch64_feature_detected!("neon") {
            unsafe {
                feather_horizontal_neon(mask, tmp, wp, hp, radius, kernel);
            }
            return;
        }
    }
    feather_horizontal_scalar(mask, tmp, wp, hp, radius, kernel);
}

#[cfg(target_arch = "x86_64")]
fn feather_vertical_pass(
    tmp: &[f32],
    mask: &mut [u8],
    wp: usize,
    hp: usize,
    radius: usize,
    kernel: &[f32],
) {
    #[target_feature(enable = "avx2")]
    unsafe fn run_avx2(
        tmp: &[f32],
        mask: &mut [u8],
        wp: usize,
        hp: usize,
        radius: usize,
        kernel: &[f32],
    ) {
        unsafe {
            feather_vertical_avx2(tmp, mask, wp, hp, radius, kernel);
        }
    }
    #[target_feature(enable = "sse4.1")]
    unsafe fn run_sse41(
        tmp: &[f32],
        mask: &mut [u8],
        wp: usize,
        hp: usize,
        radius: usize,
        kernel: &[f32],
    ) {
        unsafe {
            feather_vertical_sse41(tmp, mask, wp, hp, radius, kernel);
        }
    }
    if is_x86_feature_detected!("avx2") {
        unsafe {
            run_avx2(tmp, mask, wp, hp, radius, kernel);
        }
    } else if is_x86_feature_detected!("sse4.1") {
        unsafe {
            run_sse41(tmp, mask, wp, hp, radius, kernel);
        }
    } else {
        feather_vertical_scalar(tmp, mask, wp, hp, radius, kernel);
    }
}

#[cfg(not(target_arch = "x86_64"))]
fn feather_vertical_pass(
    tmp: &[f32],
    mask: &mut [u8],
    wp: usize,
    hp: usize,
    radius: usize,
    kernel: &[f32],
) {
    #[cfg(target_arch = "aarch64")]
    {
        if std::arch::is_aarch64_feature_detected!("neon") {
            unsafe {
                feather_vertical_neon(tmp, mask, wp, hp, radius, kernel);
            }
            return;
        }
    }
    feather_vertical_scalar(tmp, mask, wp, hp, radius, kernel);
}

// ── Scalar reference ────────────────────────────────────────────────────

fn feather_horizontal_scalar(
    mask: &[u8],
    tmp: &mut [f32],
    wp: usize,
    hp: usize,
    radius: usize,
    kernel: &[f32],
) {
    for row in 0..hp {
        let row_off = row * wp;
        for col in 0..wp {
            let mut accum = 0.0f32;
            for (k, &kw) in kernel.iter().enumerate() {
                let sx = (col as i64 + k as i64 - radius as i64).clamp(0, wp as i64 - 1) as usize;
                accum += f32::from(mask[row_off + sx]) * kw;
            }
            tmp[row_off + col] = accum;
        }
    }
}

fn feather_vertical_scalar(
    tmp: &[f32],
    mask: &mut [u8],
    wp: usize,
    hp: usize,
    radius: usize,
    kernel: &[f32],
) {
    for col in 0..wp {
        for row in 0..hp {
            let mut accum = 0.0f32;
            for (k, &kw) in kernel.iter().enumerate() {
                let sy = (row as i64 + k as i64 - radius as i64).clamp(0, hp as i64 - 1) as usize;
                accum += tmp[sy * wp + col] * kw;
            }
            let v = (accum + 0.5) as u32;
            mask[row * wp + col] = v.min(255) as u8;
        }
    }
}

// ── SSE4.1 (4-wide) ────────────────────────────────────────────────────

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
#[allow(unsafe_op_in_unsafe_fn)]
unsafe fn feather_horizontal_sse41(
    mask: &[u8],
    tmp: &mut [f32],
    wp: usize,
    hp: usize,
    radius: usize,
    kernel: &[f32],
) {
    use core::arch::x86_64::*;
    for row in 0..hp {
        let row_off = row * wp;
        // Scalar left edge (col < radius).
        for col in 0..radius.min(wp) {
            let mut accum = 0.0f32;
            for (k, &kw) in kernel.iter().enumerate() {
                let sx = (col as i64 + k as i64 - radius as i64).clamp(0, wp as i64 - 1) as usize;
                accum += f32::from(mask[row_off + sx]) * kw;
            }
            tmp[row_off + col] = accum;
        }
        // SIMD interior: all 4 output pixels and all source taps are in-bounds.
        let interior_end = wp.saturating_sub(radius + 3);
        let mut col = radius;
        while col + 4 <= interior_end {
            let mut acc = _mm_setzero_ps();
            for (k, &kw) in kernel.iter().enumerate() {
                let src_base = row_off + col + k - radius;
                let u32bits = u32::from_le_bytes([
                    mask[src_base],
                    mask[src_base + 1],
                    mask[src_base + 2],
                    mask[src_base + 3],
                ]);
                let u8vec = _mm_cvtsi32_si128(u32bits as i32);
                let f32vals = _mm_cvtepi32_ps(_mm_cvtepu8_epi32(u8vec));
                let kw4 = _mm_set1_ps(kw);
                acc = _mm_add_ps(acc, _mm_mul_ps(f32vals, kw4));
            }
            _mm_storeu_ps(tmp.as_mut_ptr().add(row_off + col), acc);
            col += 4;
        }
        // Scalar right edge (col >= interior_end).
        for col in col..wp {
            let mut accum = 0.0f32;
            for (k, &kw) in kernel.iter().enumerate() {
                let sx = (col as i64 + k as i64 - radius as i64).clamp(0, wp as i64 - 1) as usize;
                accum += f32::from(mask[row_off + sx]) * kw;
            }
            tmp[row_off + col] = accum;
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
#[allow(unsafe_op_in_unsafe_fn)]
unsafe fn feather_vertical_sse41(
    tmp: &[f32],
    mask: &mut [u8],
    wp: usize,
    hp: usize,
    radius: usize,
    kernel: &[f32],
) {
    use core::arch::x86_64::*;
    for row in 0..hp {
        let use_simd = row >= radius && row + radius + 3 < hp;
        let interior_end = if use_simd { wp.saturating_sub(3) } else { 0 };
        let mut col = 0usize;
        while col + 4 <= interior_end {
            let mut acc = _mm_setzero_ps();
            for (k, &kw) in kernel.iter().enumerate() {
                let src_row = row + k - radius;
                let f32vals = _mm_loadu_ps(tmp.as_ptr().add(src_row * wp + col));
                let kw4 = _mm_set1_ps(kw);
                acc = _mm_add_ps(acc, _mm_mul_ps(f32vals, kw4));
            }
            let rounded = _mm_add_ps(acc, _mm_set1_ps(0.5));
            let i32vals = _mm_cvttps_epi32(rounded);
            let clamped = _mm_min_epi32(i32vals, _mm_set1_epi32(255));
            let u8vals = _mm_packus_epi16(
                _mm_packs_epi32(clamped, _mm_setzero_si128()),
                _mm_setzero_si128(),
            );
            let u32bits = _mm_cvtsi128_si32(u8vals) as u32;
            let bytes = u32bits.to_le_bytes();
            mask[row * wp + col] = bytes[0];
            mask[row * wp + col + 1] = bytes[1];
            mask[row * wp + col + 2] = bytes[2];
            mask[row * wp + col + 3] = bytes[3];
            col += 4;
        }
        for col in col..wp {
            let mut accum = 0.0f32;
            for (k, &kw) in kernel.iter().enumerate() {
                let sy = (row as i64 + k as i64 - radius as i64).clamp(0, hp as i64 - 1) as usize;
                accum += tmp[sy * wp + col] * kw;
            }
            let v = (accum + 0.5) as u32;
            mask[row * wp + col] = v.min(255) as u8;
        }
    }
}

// ── AVX2 (8-wide) ──────────────────────────────────────────────────────

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
#[allow(unsafe_op_in_unsafe_fn)]
unsafe fn feather_horizontal_avx2(
    mask: &[u8],
    tmp: &mut [f32],
    wp: usize,
    hp: usize,
    radius: usize,
    kernel: &[f32],
) {
    use core::arch::x86_64::*;
    for row in 0..hp {
        let row_off = row * wp;
        // Scalar left edge.
        for col in 0..radius.min(wp) {
            let mut accum = 0.0f32;
            for (k, &kw) in kernel.iter().enumerate() {
                let sx = (col as i64 + k as i64 - radius as i64).clamp(0, wp as i64 - 1) as usize;
                accum += f32::from(mask[row_off + sx]) * kw;
            }
            tmp[row_off + col] = accum;
        }
        // SIMD interior: 8-wide.
        let interior_end = wp.saturating_sub(radius + 7);
        let mut col = radius;
        while col + 8 <= interior_end {
            let mut acc = _mm256_setzero_ps();
            for (k, &kw) in kernel.iter().enumerate() {
                let src_base = row_off + col + k - radius;
                // Load 8 u8 values and convert to f32.
                let u64bits = u64::from_le_bytes([
                    mask[src_base],
                    mask[src_base + 1],
                    mask[src_base + 2],
                    mask[src_base + 3],
                    mask[src_base + 4],
                    mask[src_base + 5],
                    mask[src_base + 6],
                    mask[src_base + 7],
                ]);
                // Zero-extend u8 to u16, then u16 to u32 via shuffle, then cvtdq2ps.
                let u8vec = _mm_cvtsi64x_si128(u64bits as i64);
                let u16vec = _mm_cvtepu8_epi16(u8vec);
                let u32lo = _mm_cvtepu16_epi32(u16vec);
                let u32hi = _mm_cvtepu16_epi32(_mm_srli_si128(u16vec, 8));
                let f32lo = _mm_cvtepi32_ps(u32lo);
                let f32hi = _mm_cvtepi32_ps(u32hi);
                let f32vals = _mm256_set_m128(f32hi, f32lo);
                let kw8 = _mm256_set1_ps(kw);
                acc = _mm256_add_ps(acc, _mm256_mul_ps(f32vals, kw8));
            }
            _mm256_storeu_ps(tmp.as_mut_ptr().add(row_off + col), acc);
            col += 8;
        }
        // Scalar remainder.
        for col in col..wp {
            let mut accum = 0.0f32;
            for (k, &kw) in kernel.iter().enumerate() {
                let sx = (col as i64 + k as i64 - radius as i64).clamp(0, wp as i64 - 1) as usize;
                accum += f32::from(mask[row_off + sx]) * kw;
            }
            tmp[row_off + col] = accum;
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
#[allow(unsafe_op_in_unsafe_fn)]
unsafe fn feather_vertical_avx2(
    tmp: &[f32],
    mask: &mut [u8],
    wp: usize,
    hp: usize,
    radius: usize,
    kernel: &[f32],
) {
    use core::arch::x86_64::*;
    for row in 0..hp {
        let use_simd = row >= radius && row + radius + 7 < hp;
        let interior_end = if use_simd { wp.saturating_sub(7) } else { 0 };
        let mut col = 0usize;
        while col + 8 <= interior_end {
            let mut acc = _mm256_setzero_ps();
            for (k, &kw) in kernel.iter().enumerate() {
                let src_row = row + k - radius;
                let f32vals = _mm256_loadu_ps(tmp.as_ptr().add(src_row * wp + col));
                let kw8 = _mm256_set1_ps(kw);
                acc = _mm256_add_ps(acc, _mm256_mul_ps(f32vals, kw8));
            }
            let rounded = _mm256_add_ps(acc, _mm256_set1_ps(0.5));
            let i32vals = _mm256_cvttps_epi32(rounded);
            let clamped = _mm256_min_epi32(i32vals, _mm256_set1_epi32(255));
            let lo16 = _mm_packs_epi32(
                _mm256_castsi256_si128(clamped),
                _mm256_extracti128_si256(clamped, 1),
            );
            let u8vals = _mm_packus_epi16(lo16, _mm_setzero_si128());
            let u64bits = _mm_cvtsi128_si64(u8vals) as u64;
            let bytes = u64bits.to_le_bytes();
            for j in 0..8 {
                mask[row * wp + col + j] = bytes[j];
            }
            col += 8;
        }
        for col in col..wp {
            let mut accum = 0.0f32;
            for (k, &kw) in kernel.iter().enumerate() {
                let sy = (row as i64 + k as i64 - radius as i64).clamp(0, hp as i64 - 1) as usize;
                accum += tmp[sy * wp + col] * kw;
            }
            let v = (accum + 0.5) as u32;
            mask[row * wp + col] = v.min(255) as u8;
        }
    }
}

// ── NEON (4-wide) ──────────────────────────────────────────────────────

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
#[allow(unsafe_op_in_unsafe_fn)]
unsafe fn feather_horizontal_neon(
    mask: &[u8],
    tmp: &mut [f32],
    wp: usize,
    hp: usize,
    radius: usize,
    kernel: &[f32],
) {
    use core::arch::aarch64::*;
    for row in 0..hp {
        let row_off = row * wp;
        // Scalar left edge.
        for col in 0..radius.min(wp) {
            let mut accum = 0.0f32;
            for (k, &kw) in kernel.iter().enumerate() {
                let sx = (col as i64 + k as i64 - radius as i64).clamp(0, wp as i64 - 1) as usize;
                accum += f32::from(mask[row_off + sx]) * kw;
            }
            tmp[row_off + col] = accum;
        }
        // NEON interior: 4-wide.
        let interior_end = wp.saturating_sub(radius + 3);
        let mut col = radius;
        while col + 4 <= interior_end {
            let mut acc = vdupq_n_f32(0.0);
            for (k, &kw) in kernel.iter().enumerate() {
                let src_base = row_off + col + k - radius;
                let u8vals = vld1_u8(mask.as_ptr().add(src_base));
                // 4 u8 → u16 → u32 → f32 via single widening chain
                let f32vals = vcvtq_f32_u32(vmovl_u16(vget_low_u16(vmovl_u8(u8vals))));
                let kw4 = vdupq_n_f32(kw);
                acc = vmlaq_f32(acc, f32vals, kw4);
            }
            vst1q_f32(tmp.as_mut_ptr().add(row_off + col), acc);
            col += 4;
        }
        // Scalar right edge.
        for col in col..wp {
            let mut accum = 0.0f32;
            for (k, &kw) in kernel.iter().enumerate() {
                let sx = (col as i64 + k as i64 - radius as i64).clamp(0, wp as i64 - 1) as usize;
                accum += f32::from(mask[row_off + sx]) * kw;
            }
            tmp[row_off + col] = accum;
        }
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
#[allow(unsafe_op_in_unsafe_fn)]
unsafe fn feather_vertical_neon(
    tmp: &[f32],
    mask: &mut [u8],
    wp: usize,
    hp: usize,
    radius: usize,
    kernel: &[f32],
) {
    use core::arch::aarch64::*;
    for row in 0..hp {
        let use_simd = row >= radius && row + radius + 3 < hp;
        let interior_end = if use_simd { wp.saturating_sub(3) } else { 0 };
        let mut col = 0usize;
        while col + 4 <= interior_end {
            let mut acc = vdupq_n_f32(0.0);
            for (k, &kw) in kernel.iter().enumerate() {
                let src_row = row + k - radius;
                let f32vals = vld1q_f32(tmp.as_ptr().add(src_row * wp + col));
                let kw4 = vdupq_n_f32(kw);
                acc = vmlaq_f32(acc, f32vals, kw4);
            }
            let rounded = vaddq_f32(acc, vdupq_n_f32(0.5));
            let i32vals = vcvtq_s32_f32(rounded);
            let clamped = vminq_s32(i32vals, vdupq_n_s32(255));
            let u8vals = vqmovun_s16(vcombine_s16(vmovn_s32(clamped), vdup_n_s16(0)));
            let u32bits = vget_lane_u32(vreinterpret_u32_u8(u8vals), 0);
            let bytes = u32bits.to_le_bytes();
            mask[row * wp + col] = bytes[0];
            mask[row * wp + col + 1] = bytes[1];
            mask[row * wp + col + 2] = bytes[2];
            mask[row * wp + col + 3] = bytes[3];
            col += 4;
        }
        for col in col..wp {
            let mut accum = 0.0f32;
            for (k, &kw) in kernel.iter().enumerate() {
                let sy = (row as i64 + k as i64 - radius as i64).clamp(0, hp as i64 - 1) as usize;
                accum += tmp[sy * wp + col] * kw;
            }
            let v = (accum + 0.5) as u32;
            mask[row * wp + col] = v.min(255) as u8;
        }
    }
}
