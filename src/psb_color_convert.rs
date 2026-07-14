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

//! PSD/PSB color-mode conversion: bitmap (0), indexed (2), multichannel (7),
//! duotone (8), and Lab (9) → interleaved RGBA8.

/// Read a big-endian u32 from the byte slice.
fn read_be_u32(buf: &[u8]) -> Result<u32, ()> {
    if buf.len() < 4 {
        return Err(());
    }
    Ok(u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]))
}

// ---------------------------------------------------------------------------
// Bitmap (mode 0): 1-bit per pixel → grayscale RGBA
// ---------------------------------------------------------------------------

/// Interleave one row of Bitmap-mode data (depth=1 packed-bits) into RGBA8.
///
/// `src` holds `ceil(width / 8)` packed bytes (MSB-first pixel order).
/// Each 1 → 0xFF (white), each 0 → 0x00 (black), alpha = 0xFF.
pub fn bitmap_bits_row_to_rgba8(dst_row: &mut [u8], src: &[u8], width: usize) {
    for col in 0..width {
        let byte_idx = col >> 3; // col / 8
        let bit_idx = 7 - (col & 7); // MSB first
        let v = if byte_idx < src.len() && (src[byte_idx] >> bit_idx) & 1 != 0 {
            0xFFu8
        } else {
            0x00u8
        };
        let base = col * 4;
        dst_row[base] = v;
        dst_row[base + 1] = v;
        dst_row[base + 2] = v;
        dst_row[base + 3] = 0xFF;
    }
}

/// Expand packed 1-bit data into 8-bit samples (0 or 255) for depth=1 Bitmap.
///
/// `packed` has `ceil(pixel_count / 8)` bytes; `dst` has `pixel_count` bytes.
/// Each 1-bit → 0xFF, each 0-bit → 0x00.
pub fn bitmap_expand_bits_to_u8(dst: &mut [u8], packed: &[u8]) {
    for (i, d) in dst.iter_mut().enumerate() {
        let byte_idx = i >> 3;
        let bit_idx = 7 - (i & 7);
        *d = if byte_idx < packed.len() && (packed[byte_idx] >> bit_idx) & 1 != 0 {
            0xFF
        } else {
            0x00
        };
    }
}

/// Compute the number of raw bytes in a Bitmap (depth=1) channel.
#[inline]
pub fn bitmap_packed_byte_count(pixel_count: usize) -> usize {
    (pixel_count + 7) >> 3
}

// ---------------------------------------------------------------------------
// Indexed (mode 2): palette lookup → RGB
// ---------------------------------------------------------------------------

/// Extract the 768-byte palette from Color Mode Data section (Indexed mode only).
///
/// Returns 768 bytes = 256 × RGB (3 bytes per entry, R,G,B order).
/// Returns `None` when the section is truncated or not present.
pub fn extract_indexed_palette(bytes: &[u8]) -> Option<Vec<u8>> {
    // Header is 26 bytes, followed by 4-byte cm_len, then cm_data.
    const CM_LEN_OFFSET: usize = 26;
    if bytes.len() < CM_LEN_OFFSET + 4 {
        return None;
    }
    let cm_len = read_be_u32(&bytes[CM_LEN_OFFSET..CM_LEN_OFFSET + 4]).ok()? as usize;
    if cm_len == 0 {
        // Photoshop writes 0 for no Color Mode Data (no palette available).
        return None;
    }
    let cm_start = CM_LEN_OFFSET + 4; // = 30
    if cm_len >= 768 && bytes.len() >= cm_start + 768 {
        Some(bytes[cm_start..cm_start + 768].to_vec())
    } else {
        None
    }
}

#[allow(dead_code)]
/// Interleave one row of Indexed-mode data into RGBA8.
///
/// `src` holds 8-bit palette indices. `palette` is 768 bytes (256 × 3, RGB).
pub fn indexed_row_to_rgba8(dst_row: &mut [u8], src: &[u8], palette: &[u8], width: usize) {
    for col in 0..width {
        let idx = if col < src.len() {
            src[col] as usize
        } else {
            0
        };
        let base = col * 4;
        if idx < 256 {
            let pbase = idx * 3;
            dst_row[base] = palette[pbase];
            dst_row[base + 1] = palette[pbase + 1];
            dst_row[base + 2] = palette[pbase + 2];
        } else {
            dst_row[base] = 0;
            dst_row[base + 1] = 0;
            dst_row[base + 2] = 0;
        }
        dst_row[base + 3] = 0xFF;
    }
}

// ---------------------------------------------------------------------------
// Multichannel (mode 7): first 3 channels as RGB, no colour transform
// ---------------------------------------------------------------------------

/// Interleave one row of Multichannel-mode data into RGBA8.
///
/// Channels 0/1/2 map to R/G/B. Channel 3 (if present) is alpha.
/// No colour conversion is performed — this is a best-effort display mapping.
pub fn multichannel_row_to_rgba8(
    dst_row: &mut [u8],
    ch0: &[u8],
    ch1: &[u8],
    ch2: &[u8],
    alpha: Option<&[u8]>,
    start: usize,
    end: usize,
) {
    let width = end.saturating_sub(start);
    for col in 0..width {
        let base = col * 4;
        let r = ch0.get(start + col).copied().unwrap_or(0);
        let g = ch1.get(start + col).copied().unwrap_or(0);
        let b = ch2.get(start + col).copied().unwrap_or(0);
        let a = alpha
            .and_then(|a| a.get(start + col))
            .copied()
            .unwrap_or(0xFF);
        dst_row[base] = r;
        dst_row[base + 1] = g;
        dst_row[base + 2] = b;
        dst_row[base + 3] = a;
    }
}

// ---------------------------------------------------------------------------
// Duotone (mode 8): stored as single grayscale channel
// ---------------------------------------------------------------------------

#[allow(dead_code)]
/// Interleave one row of Duotone-mode data into RGBA8.
///
/// Duotone image data is stored identically to Grayscale: one channel of
/// 8/16/32-bit samples. The duotone curves and ink colours are stored in
/// the Color Mode Data section but are too complex to apply automatically;
/// we render as plain grayscale.
pub fn duotone_row_to_rgba8(
    dst_row: &mut [u8],
    gray: &[u8],
    alpha: Option<&[u8]>,
    start: usize,
    end: usize,
) {
    let width = end.saturating_sub(start);
    for col in 0..width {
        let base = col * 4;
        let v = gray.get(start + col).copied().unwrap_or(0);
        let a = alpha
            .and_then(|a| a.get(start + col))
            .copied()
            .unwrap_or(0xFF);
        dst_row[base] = v;
        dst_row[base + 1] = v;
        dst_row[base + 2] = v;
        dst_row[base + 3] = a;
    }
}

// ---------------------------------------------------------------------------
// Lab (mode 9): CIE L*a*b* → XYZ D50 → linear sRGB (D65-adapted)
// ---------------------------------------------------------------------------

/// Interleave one row of Lab-mode data into RGBA8.
///
/// PSD stores L*(0..255 → 0..100), a*(0..255 → -128..+127),
/// b*(0..255 → -128..+127). Alpha is channel 3 when channels ≥ 4.
pub fn lab_row_to_rgba8(
    dst_row: &mut [u8],
    l_ch: &[u8],
    a_ch: &[u8],
    b_ch: &[u8],
    alpha: Option<&[u8]>,
    start: usize,
    end: usize,
) {
    let width = end.saturating_sub(start);
    for col in 0..width {
        let base = col * 4;
        let l_raw = l_ch.get(start + col).copied().unwrap_or(0) as f32;
        let a_raw = a_ch.get(start + col).copied().unwrap_or(128) as f32;
        let b_raw = b_ch.get(start + col).copied().unwrap_or(128) as f32;
        let (r, g, b) = lab_pixel(l_raw, a_raw, b_raw);
        let a = alpha
            .and_then(|a| a.get(start + col))
            .copied()
            .unwrap_or(0xFF);
        dst_row[base] = r;
        dst_row[base + 1] = g;
        dst_row[base + 2] = b;
        dst_row[base + 3] = a;
    }
}

/// Convert one Lab pixel to sRGB ([0,255] each).
///
/// Raw values: L* 0..255 → 0..100, a* 0..255 → -128..+127, b* 0..255 → -128..+127.
pub(crate) fn lab_pixel(l_raw: f32, a_raw: f32, b_raw: f32) -> (u8, u8, u8) {
    // Scale L* to [0, 100], a*/b* to [-128, 127].
    let l = l_raw * (100.0 / 255.0);
    let a = a_raw - 128.0;
    let b = b_raw - 128.0;

    // --- Lab → XYZ (D50) ---
    let fy = (l + 16.0) / 116.0;
    let fx = a / 500.0 + fy;
    let fz = fy - b / 200.0;

    let eps3 = 0.008856_f32; // (216/24389)³⁻¹ ≈ 0.008856
    let kap = 903.3; // 24389/27 ≈ 903.3

    let x = if fx.powi(3) > eps3 {
        fx.powi(3) * 0.9642_f32 // Xn (D50)
    } else {
        ((116.0 * fx - 16.0) / kap) * 0.9642_f32
    };
    let y = if l > eps3 * kap {
        fy.powi(3) // Yn = 1.0
    } else {
        l / kap
    };
    let z = if fz.powi(3) > eps3 {
        fz.powi(3) * 0.8249_f32 // Zn (D50)
    } else {
        ((116.0 * fz - 16.0) / kap) * 0.8249_f32
    };

    // --- XYZ D50 → linear sRGB (Bradford-adapted D50→D65+sRGB matrix) ---
    // Source: http://www.brucelindbloom.com/ — "sRGB (D50)"
    let r_lin = 3.1338561_f32 * x - 1.6168667_f32 * y - 0.4906146_f32 * z;
    let g_lin = -0.9787684_f32 * x + 1.9161415_f32 * y + 0.0334540_f32 * z;
    let b_lin = 0.0719453_f32 * x - 0.2289914_f32 * y + 1.4052427_f32 * z;

    // --- sRGB gamma encode ---
    let r = srgb_encode(r_lin);
    let g = srgb_encode(g_lin);
    let b = srgb_encode(b_lin);

    (r, g, b)
}

/// Apply sRGB gamma curve to a linear-channel value (clamped to [0,1]).
fn srgb_encode(linear: f32) -> u8 {
    let v = linear.clamp(0.0, 1.0);
    let v = if v <= 0.0031308 {
        12.92 * v
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    };
    (v * 255.0).round().clamp(0.0, 255.0) as u8
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Bitmap ---

    #[test]
    fn bitmap_bits_all_white() {
        let packed = [0xFFu8; 2];
        let mut dst = [0u8; 32]; // 8 pixels × 4
        bitmap_bits_row_to_rgba8(&mut dst, &packed, 8);
        for i in 0..8 {
            assert_eq!(dst[i * 4..i * 4 + 4], [0xFF, 0xFF, 0xFF, 0xFF], "i={i}");
        }
    }

    #[test]
    fn bitmap_bits_all_black() {
        let packed = [0x00u8; 2];
        let mut dst = [0u8; 32];
        bitmap_bits_row_to_rgba8(&mut dst, &packed, 8);
        for i in 0..8 {
            assert_eq!(dst[i * 4..i * 4 + 4], [0x00, 0x00, 0x00, 0xFF], "i={i}");
        }
    }

    #[test]
    fn bitmap_bits_alternating() {
        // 0b1010_1010, 0b1010_1010 → pixels 0,2,4,6,8,10,12,14 = 1; 1,3,5,7,9,11,13,15 = 0
        let packed = [0xAAu8, 0xAAu8];
        let mut dst = [0u8; 64]; // 16 pixels × 4
        bitmap_bits_row_to_rgba8(&mut dst, &packed, 16);
        for i in 0..16 {
            let expected = if i & 1 == 0 { 0xFF } else { 0x00 };
            assert_eq!(
                dst[i * 4..i * 4 + 4],
                [expected, expected, expected, 0xFF],
                "i={i}"
            );
        }
    }

    #[test]
    fn bitmap_expand_bits_round_trip() {
        let packed = [0xF0u8, 0x0Fu8]; // 1111_0000 0000_1111
        let pixel_count = 16;
        let mut dst = vec![0u8; pixel_count];
        bitmap_expand_bits_to_u8(&mut dst, &packed);
        for i in 0..4 {
            assert_eq!(dst[i], 0xFF, "i={i}");
        }
        for i in 4..8 {
            assert_eq!(dst[i], 0x00, "i={i}");
        }
        for i in 8..12 {
            assert_eq!(dst[i], 0x00, "i={i}");
        }
        for i in 12..16 {
            assert_eq!(dst[i], 0xFF, "i={i}");
        }
    }

    #[test]
    fn bitmap_packed_byte_count_basic() {
        assert_eq!(bitmap_packed_byte_count(1), 1);
        assert_eq!(bitmap_packed_byte_count(8), 1);
        assert_eq!(bitmap_packed_byte_count(9), 2);
        assert_eq!(bitmap_packed_byte_count(16), 2);
        assert_eq!(bitmap_packed_byte_count(0), 0);
    }

    // -- Indexed ---

    #[test]
    fn extract_palette_from_minimal_indexed_psd() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"8BPS");
        bytes.extend_from_slice(&1u16.to_be_bytes());
        bytes.extend_from_slice(&[0; 6]);
        bytes.extend_from_slice(&1u16.to_be_bytes()); // channels
        bytes.extend_from_slice(&1u32.to_be_bytes()); // height
        bytes.extend_from_slice(&1u32.to_be_bytes()); // width
        bytes.extend_from_slice(&8u16.to_be_bytes()); // depth
        bytes.extend_from_slice(&2u16.to_be_bytes()); // color_mode = Indexed
        bytes.extend_from_slice(&768u32.to_be_bytes()); // cm_len
        for i in 0usize..256 {
            bytes.push(i as u8); // R
            bytes.push(0xFFu8.wrapping_sub(i as u8)); // G
            bytes.push((i as u8).wrapping_mul(2)); // B
        }
        let palette = extract_indexed_palette(&bytes).expect("should have palette");
        assert_eq!(palette.len(), 768);
        assert_eq!(palette[0], 0);
        assert_eq!(palette[1], 0xFF);
        assert_eq!(palette[2], 0);
    }

    #[test]
    fn extract_palette_truncated_returns_none() {
        let bytes = [0u8; 20];
        assert!(extract_indexed_palette(&bytes).is_none());
    }

    #[test]
    fn indexed_row_maps_colours() {
        let palette = vec![
            0xFFu8, 0x00, 0x00, // index 0 = red
            0x00, 0xFF, 0x00, // index 1 = green
            0x00, 0x00, 0xFF, // index 2 = blue
            0xFF, 0xFF, 0x00, // index 3 = yellow
        ];
        let src = [0u8, 1, 2, 3];
        let width = 4;
        let mut dst = [0u8; 16];
        indexed_row_to_rgba8(&mut dst, &src, &palette, width);
        assert_eq!(&dst[0..4], &[0xFF, 0x00, 0x00, 0xFF]);
        assert_eq!(&dst[4..8], &[0x00, 0xFF, 0x00, 0xFF]);
        assert_eq!(&dst[8..12], &[0x00, 0x00, 0xFF, 0xFF]);
        assert_eq!(&dst[12..16], &[0xFF, 0xFF, 0x00, 0xFF]);
    }

    // -- Multichannel ---

    #[test]
    fn multichannel_row_rgb_only() {
        let ch0 = [10, 20, 30];
        let ch1 = [40, 50, 60];
        let ch2 = [70, 80, 90];
        let mut dst = [0u8; 12];
        multichannel_row_to_rgba8(&mut dst, &ch0, &ch1, &ch2, None, 0, 3);
        assert_eq!(&dst[0..4], &[10, 40, 70, 0xFF]);
        assert_eq!(&dst[4..8], &[20, 50, 80, 0xFF]);
        assert_eq!(&dst[8..12], &[30, 60, 90, 0xFF]);
    }

    #[test]
    fn multichannel_row_with_alpha() {
        let ch0 = [10, 20];
        let ch1 = [30, 40];
        let ch2 = [50, 60];
        let a = [128, 200];
        let mut dst = [0u8; 8];
        multichannel_row_to_rgba8(&mut dst, &ch0, &ch1, &ch2, Some(&a), 0, 2);
        assert_eq!(&dst[0..4], &[10, 30, 50, 128]);
        assert_eq!(&dst[4..8], &[20, 40, 60, 200]);
    }

    // -- Duotone ---

    #[test]
    fn duotone_row_basic() {
        let gray = [0x00, 0x80, 0xFF];
        let mut dst = [0u8; 12];
        duotone_row_to_rgba8(&mut dst, &gray, None, 0, 3);
        assert_eq!(&dst[0..4], &[0x00, 0x00, 0x00, 0xFF]);
        assert_eq!(&dst[4..8], &[0x80, 0x80, 0x80, 0xFF]);
        assert_eq!(&dst[8..12], &[0xFF, 0xFF, 0xFF, 0xFF]);
    }

    #[test]
    fn duotone_row_with_alpha() {
        let gray = [100, 200];
        let a = [50, 150];
        let mut dst = [0u8; 8];
        duotone_row_to_rgba8(&mut dst, &gray, Some(&a), 0, 2);
        assert_eq!(&dst[0..4], &[100, 100, 100, 50]);
        assert_eq!(&dst[4..8], &[200, 200, 200, 150]);
    }

    // -- Lab ---

    /// Lab(0,0,0) → very dark, near-black but not pure black
    #[test]
    fn lab_black_pixel() {
        let (r, g, b) = lab_pixel(0.0, 128.0, 128.0); // L*=0, a*=0, b*=0
        // L*=0 should produce near-black
        assert!(r < 10 && g < 10 && b < 10, "got ({r},{g},{b})");
    }

    /// Lab(255,128,128) = L*=100, a*=0, b*=0 → white
    #[test]
    fn lab_white_pixel() {
        let (r, g, b) = lab_pixel(255.0, 128.0, 128.0);
        // L*=100 maps to approximately D50 white → close to (255,255,255)
        assert!(r >= 240 && g >= 240 && b >= 240, "got ({r},{g},{b})");
    }

    /// Lab(128,0,0) = L*=~50, a*=-128, b*=-128 → green-ish
    #[test]
    fn lab_a_negative() {
        // a* = -128 (green), b* = -128 (blue) → blue-green
        let (r, g, b) = lab_pixel(128.0, 0.0, 0.0);
        assert!(g > r || b > r, "got ({r},{g},{b})");
    }

    /// Lab(128,255,255) = L*=~50, a*=+127, b*=+127 → warm (red/yellow)
    #[test]
    fn lab_a_b_positive() {
        let (r, g, b) = lab_pixel(128.0, 255.0, 255.0);
        assert!(r >= g && r >= b, "got ({r},{g},{b})");
    }

    /// Simple midpoint: L*=50, a*=0, b*=0 → gray
    #[test]
    fn lab_mid_gray() {
        let (r, g, b) = lab_pixel(128.0, 128.0, 128.0);
        // Should be medium gray — R ≈ G ≈ B with moderate value
        let diff = (r as i16 - g as i16)
            .unsigned_abs()
            .max((r as i16 - b as i16).unsigned_abs());
        assert!(
            diff <= 5,
            "expected neutral gray, got ({r},{g},{b}) diff={diff}"
        );
        assert!(r >= 100 && r <= 200, "expected mid-gray, got r={r}");
    }

    /// Lab row conversion matches lab_pixel
    #[test]
    fn lab_row_matches_pixel_function() {
        let l_ch = [0u8, 64, 128, 192, 255];
        let a_ch = [128u8, 64, 128, 192, 128];
        let b_ch = [128u8, 192, 128, 64, 128];
        let width = 5;
        let mut dst = vec![0u8; width * 4];
        lab_row_to_rgba8(&mut dst, &l_ch, &a_ch, &b_ch, None, 0, width);
        for i in 0..width {
            let (er, eg, eb) = lab_pixel(l_ch[i] as f32, a_ch[i] as f32, b_ch[i] as f32);
            assert_eq!(&dst[i * 4..i * 4 + 4], &[er, eg, eb, 0xFF], "i={i}");
        }
    }

    #[test]
    fn srgb_encode_clamps_negative() {
        assert_eq!(srgb_encode(-0.5), 0);
    }

    #[test]
    fn srgb_encode_clamps_above_one() {
        assert_eq!(srgb_encode(2.0), 255);
    }

    #[test]
    fn srgb_encode_mid() {
        let v = srgb_encode(0.5);
        // 0.5 linear → ~0.735 gamma → ~187
        assert!((v as i16 - 188).unsigned_abs() <= 2, "got {v}");
    }
}
