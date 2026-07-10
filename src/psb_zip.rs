//! PSD/PSB ZIP and ZIP+prediction channel decompression (compression codes 2 and 3).
//!
//! Uses `miniz_oxide` (zlib-compatible inflate). Prediction undo follows Adobe's
//! horizontal differencing: 8/16-bit per sample; 32-bit uses byte-plane packing.

use miniz_oxide::inflate::{DecompressError, decompress_to_vec_zlib_with_limit};

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
                let mut acc = row[0];
                for px in row.iter_mut().skip(1) {
                    acc = acc.wrapping_add(*px);
                    *px = acc;
                }
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
                let mut acc = 0u16;
                for sample in row.chunks_exact_mut(2) {
                    let delta = u16::from_be_bytes([sample[0], sample[1]]);
                    acc = acc.wrapping_add(delta);
                    let be = acc.to_be_bytes();
                    sample[0] = be[0];
                    sample[1] = be[1];
                }
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
            let plane_bytes = &mut row[start..end];
            let mut acc = plane_bytes[0];
            for b in plane_bytes.iter_mut().skip(1) {
                acc = acc.wrapping_add(*b);
                *b = acc;
            }
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
}
