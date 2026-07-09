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

pub(crate) const ISO_GAIN_MAP_NAMESPACE: &[u8] = b"urn:iso:std:iso:ts:21496:-1\0";

const ISO_MULTI_CHANNEL_FLAG: u8 = 1 << 7;
const ISO_BACKWARD_DIRECTION_FLAG: u8 = 1 << 2;
const ISO_COMMON_DENOMINATOR_FLAG: u8 = 1 << 3;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct GainMapMetadata {
    pub(crate) gain_map_min: [f32; 3],
    pub(crate) gain_map_max: [f32; 3],
    pub(crate) gamma: [f32; 3],
    pub(crate) offset_sdr: [f32; 3],
    pub(crate) offset_hdr: [f32; 3],
    pub(crate) hdr_capacity_min: f32,
    pub(crate) hdr_capacity_max: f32,
    /// ISO `ISO_BACKWARD_DIRECTION_FLAG` or Adobe `hdrgm:BaseRenditionIsHDR=True`: primary stores HDR base.
    pub(crate) backward_direction: bool,
}

pub(crate) fn iso_gain_map_metadata(payload: &[u8]) -> Option<Result<GainMapMetadata, String>> {
    payload
        .strip_prefix(ISO_GAIN_MAP_NAMESPACE)
        .map(parse_iso_gain_map_metadata)
}

pub(crate) fn parse_iso_gain_map_metadata(metadata: &[u8]) -> Result<GainMapMetadata, String> {
    let mut reader = ByteReader::new(metadata);
    let min_version = reader.read_u16()?;
    if min_version != 0 {
        return Err(format!(
            "unsupported ISO 21496-1 gain map metadata minimum version {min_version}"
        ));
    }
    let _writer_version = reader.read_u16()?;
    let flags = reader.read_u8()?;

    let channel_count = if flags & ISO_MULTI_CHANNEL_FLAG != 0 {
        3
    } else {
        1
    };
    let common_denominator = flags & ISO_COMMON_DENOMINATOR_FLAG != 0;
    let mut fraction = IsoGainMapFraction::default();

    if common_denominator {
        let denominator = reader.read_u32()?;
        fraction.base_hdr_headroom = (reader.read_u32()?, denominator);
        fraction.alternate_hdr_headroom = (reader.read_u32()?, denominator);
        for channel in 0..channel_count {
            fraction.gain_map_min[channel] = (reader.read_i32()?, denominator);
            fraction.gain_map_max[channel] = (reader.read_i32()?, denominator);
            fraction.gamma[channel] = (reader.read_u32()?, denominator);
            fraction.base_offset[channel] = (reader.read_i32()?, denominator);
            fraction.alternate_offset[channel] = (reader.read_i32()?, denominator);
        }
    } else {
        fraction.base_hdr_headroom = (reader.read_u32()?, reader.read_u32()?);
        fraction.alternate_hdr_headroom = (reader.read_u32()?, reader.read_u32()?);
        for channel in 0..channel_count {
            fraction.gain_map_min[channel] = (reader.read_i32()?, reader.read_u32()?);
            fraction.gain_map_max[channel] = (reader.read_i32()?, reader.read_u32()?);
            fraction.gamma[channel] = (reader.read_u32()?, reader.read_u32()?);
            fraction.base_offset[channel] = (reader.read_i32()?, reader.read_u32()?);
            fraction.alternate_offset[channel] = (reader.read_i32()?, reader.read_u32()?);
        }
    }

    if channel_count == 1 {
        for channel in 1..3 {
            fraction.gain_map_min[channel] = fraction.gain_map_min[0];
            fraction.gain_map_max[channel] = fraction.gain_map_max[0];
            fraction.gamma[channel] = fraction.gamma[0];
            fraction.base_offset[channel] = fraction.base_offset[0];
            fraction.alternate_offset[channel] = fraction.alternate_offset[0];
        }
    }

    fraction.into_gain_map_metadata(flags)
}

/// Skip forward SDR→HDR compose; display the primary codestream as the HDR base rendition.
pub(crate) fn iso_gain_map_skips_forward_compose(metadata: GainMapMetadata) -> bool {
    metadata.backward_direction || iso_gain_map_primary_is_precomposed_hdr(metadata)
}

/// Primary codestream already stores the high-headroom rendition (Adobe `*_base_hdr.jxl` layout:
/// ISO `base_hdr_headroom` ≫ `alternate_hdr_headroom`). Forward SDR→HDR compose would treat those
/// floats as an SDR baseline and recover HDR again.
pub(crate) fn iso_gain_map_primary_is_precomposed_hdr(metadata: GainMapMetadata) -> bool {
    metadata.hdr_capacity_min > metadata.hdr_capacity_max * 1.001
}

pub(crate) fn validate_gain_map_metadata(
    metadata: GainMapMetadata,
) -> Result<GainMapMetadata, String> {
    validate_finite_triplet("GainMapMin", metadata.gain_map_min)?;
    validate_finite_triplet("GainMapMax", metadata.gain_map_max)?;
    validate_finite_triplet("OffsetSDR", metadata.offset_sdr)?;
    validate_finite_triplet("OffsetHDR", metadata.offset_hdr)?;
    for gamma in metadata.gamma {
        if !gamma.is_finite() || gamma <= 0.0 {
            return Err("gain map metadata has non-positive Gamma".to_string());
        }
    }
    if !metadata.hdr_capacity_min.is_finite() || !metadata.hdr_capacity_max.is_finite() {
        return Err("gain map metadata has non-finite HDRCapacity".to_string());
    }
    Ok(metadata)
}

pub(crate) fn gain_map_metadata_diagnostic(
    metadata: GainMapMetadata,
    target_hdr_capacity: f32,
) -> String {
    format!(
        "GainMapMin={} GainMapMax={} Gamma={} OffsetSDR={} OffsetHDR={} HDRCapacity=[{:.3},{:.3}] target={:.3} weight={:.3}",
        format_rgb_triplet(metadata.gain_map_min),
        format_rgb_triplet(metadata.gain_map_max),
        format_rgb_triplet(metadata.gamma),
        format_rgb_triplet(metadata.offset_sdr),
        format_rgb_triplet(metadata.offset_hdr),
        metadata.hdr_capacity_min,
        metadata.hdr_capacity_max,
        target_hdr_capacity,
        gain_map_weight(metadata, target_hdr_capacity),
    )
}

pub(crate) fn compose_gain_map_pixel(
    sdr_rgba: [u8; 4],
    gain_value: [f32; 3],
    metadata: GainMapMetadata,
    target_hdr_capacity: f32,
) -> [f32; 4] {
    [
        recover_hdr_channel_from_sdr_and_gain(
            sdr_rgba[0],
            gain_value[0],
            metadata,
            0,
            target_hdr_capacity,
        ),
        recover_hdr_channel_from_sdr_and_gain(
            sdr_rgba[1],
            gain_value[1],
            metadata,
            1,
            target_hdr_capacity,
        ),
        recover_hdr_channel_from_sdr_and_gain(
            sdr_rgba[2],
            gain_value[2],
            metadata,
            2,
            target_hdr_capacity,
        ),
        f32::from(sdr_rgba[3]) / 255.0,
    ]
}

pub(crate) fn append_hdr_pixel_from_sdr_and_gain(
    rgba_f32: &mut Vec<f32>,
    sdr_rgba: &[u8],
    gain_value: [f32; 3],
    metadata: GainMapMetadata,
    target_hdr_capacity: f32,
) {
    let pixel = compose_gain_map_pixel(
        [sdr_rgba[0], sdr_rgba[1], sdr_rgba[2], sdr_rgba[3]],
        gain_value,
        metadata,
        target_hdr_capacity,
    );
    rgba_f32.extend_from_slice(&pixel);
}

/// Recovers one **alternate-rendition** (HDR-side) linear channel from the baseline SDR sample,
/// one normalized gain-map sample, and decoded **ISO/IEC 21496-1** descriptor fields (`GainMapMin` /
/// `GainMapMax` / `Gamma` / `OffsetSDR` / `OffsetHDR`; headroom capacities map to [`GainMapMetadata`]).
///
/// Formal clause numbering varies by TS/IS edition; align field spellings with the serialized metadata
/// in the ISO document (**alternate vs. baseline rendition**, logarithmic boosts, offsets).
///
/// The maths matches the interoperability profile used alongside MPF JPEG gain maps (Ultra HDR tooling,
/// libavif `avifImageApplyGainMap`, etc.). Steps:
///
/// 1. **Display headroom blend `w`** ([`gain_map_weight`]): map `HDRCapacityMin` / `HDRCapacityMax` and the
///    viewer peak linear ratio [`target_hdr_capacity`] onto a clamped **`[0,1]`** scalar by interpolating in
///    **log₂** headroom—not in linear luminance ratio.
///
/// 2. **`Gamma`** shaping on the coefficient: `gain_value.powf(1.0 / gamma)` turns encoded map samples into
///    the perceptual weight multiplied by the `GainMapMin`…`GainMapMax` span.
///
/// 3. **Exponent in log₂ (stops)**: `gain_map_min + (gain_map_max - gain_map_min) * shaped * w`, then
///    **`boost = 2^log_boost`**—so `GainMapMin`/`GainMapMax` act as logarithmic boosts between baseline and
///    alternate light levels.
///
/// 4. **Offsets + clamp**: invert the baseline sRGB curve to scene-linear, add **OffsetSDR**, multiply by
///    **`boost`**, subtract **OffsetHDR**, discard negative results (alternate cannot go below mapped black).
pub(crate) fn recover_hdr_channel_from_sdr_and_gain(
    sdr_channel: u8,
    gain_value: f32,
    metadata: GainMapMetadata,
    channel_index: usize,
    target_hdr_capacity: f32,
) -> f32 {
    let channel_index = channel_index.min(2);

    let gain_weight = gain_map_weight(metadata, target_hdr_capacity);

    // Log₂ exponent → `boost`, then alternate linear sample (doc steps 3–4).
    let log_boost = metadata.gain_map_min[channel_index]
        + (metadata.gain_map_max[channel_index] - metadata.gain_map_min[channel_index])
            * gain_value.powf(1.0 / metadata.gamma[channel_index].max(f32::MIN_POSITIVE))
            * gain_weight;
    let boost = 2.0_f32.powf(log_boost);

    let linear_sdr = srgb_u8_to_linear_f32(sdr_channel);
    ((linear_sdr + metadata.offset_sdr[channel_index]) * boost - metadata.offset_hdr[channel_index])
        .max(0.0)
}

/// Maps display **linear luminance ratio** (peak nits / SDR white, e.g. `HdrToneMapSettings::target_hdr_capacity`)
/// to the gain-map application weight.
///
/// AOMedia **libavif** (`avifGetGainMapWeight`) and ISO 21496-1 interpolate in **log₂ headroom** space
/// using the metadata headrooms stored as ratios `2^log2` in [`GainMapMetadata::hdr_capacity_*`].
/// Interpolating in linear ratio space is **not** equivalent and skews brightness mid-range.
pub(crate) fn gain_map_weight(metadata: GainMapMetadata, target_hdr_capacity: f32) -> f32 {
    let base_log2 = metadata.hdr_capacity_min.max(f32::MIN_POSITIVE).log2();
    let alt_log2 = metadata.hdr_capacity_max.max(f32::MIN_POSITIVE).log2();
    let denom = alt_log2 - base_log2;
    if denom.abs() <= 1e-5 {
        return 0.0;
    }
    let display_log2 = target_hdr_capacity.max(f32::MIN_POSITIVE).log2();
    let w = ((display_log2 - base_log2) / denom).clamp(0.0, 1.0);
    if alt_log2 < base_log2 { -w } else { w }
}

/// ST 2086 hints from ISO / XMP Ultra HDR gain-map headroom (linear peak / SDR white ratios).
pub(crate) fn luminance_hints_from_gain_map(
    metadata: GainMapMetadata,
) -> crate::hdr::types::HdrLuminanceMetadata {
    use crate::hdr::types::{DEFAULT_SDR_WHITE_NITS, HdrLuminanceMetadata};

    let peak_ratio = if metadata.backward_direction {
        metadata.hdr_capacity_min.max(f32::MIN_POSITIVE)
    } else {
        metadata.hdr_capacity_max.max(f32::MIN_POSITIVE)
    };
    let peak_nits = peak_ratio * DEFAULT_SDR_WHITE_NITS;
    HdrLuminanceMetadata {
        mastering_max_nits: Some(peak_nits),
        ..HdrLuminanceMetadata::default()
    }
}

pub(crate) fn sample_gain_map_rgb(
    gain_rgba: &[u8],
    gain_width: u32,
    gain_height: u32,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
) -> [f32; 3] {
    if gain_width == 0 || gain_height == 0 || width == 0 || height == 0 {
        return [0.0; 3];
    }
    if validate_gain_map_rgba_len(gain_rgba, gain_width, gain_height).is_err() {
        return [0.0; 3];
    }

    let (x0, x1, y0, y1, tx, ty) =
        gain_map_bilinear_coords(x, y, width, height, gain_width, gain_height);
    sample_gain_map_rgb_bilinear(
        gain_rgba,
        gain_width,
        GainMapBilinearTaps {
            x0,
            x1,
            y0,
            y1,
            tx,
            ty,
        },
    )
}

fn format_rgb_triplet(values: [f32; 3]) -> String {
    format!("[{:.3},{:.3},{:.3}]", values[0], values[1], values[2])
}

fn validate_finite_triplet(name: &str, values: [f32; 3]) -> Result<(), String> {
    if values.iter().all(|value| value.is_finite()) {
        Ok(())
    } else {
        Err(format!("gain map metadata has non-finite {name}"))
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct IsoGainMapFraction {
    pub(crate) gain_map_min: [(i32, u32); 3],
    pub(crate) gain_map_max: [(i32, u32); 3],
    pub(crate) gamma: [(u32, u32); 3],
    pub(crate) base_offset: [(i32, u32); 3],
    pub(crate) alternate_offset: [(i32, u32); 3],
    pub(crate) base_hdr_headroom: (u32, u32),
    pub(crate) alternate_hdr_headroom: (u32, u32),
}

impl Default for IsoGainMapFraction {
    fn default() -> Self {
        Self {
            gain_map_min: [(0, 1); 3],
            gain_map_max: [(0, 1); 3],
            gamma: [(1, 1); 3],
            base_offset: [(0, 1); 3],
            alternate_offset: [(0, 1); 3],
            base_hdr_headroom: (0, 1),
            alternate_hdr_headroom: (0, 1),
        }
    }
}

pub(crate) fn primary_srgb_rgba8_to_linear_rgba_f32(rgba: &[u8]) -> Vec<f32> {
    let mut out = Vec::with_capacity(rgba.len());
    for chunk in rgba.chunks_exact(4) {
        out.push(srgb_u8_to_linear_f32(chunk[0]));
        out.push(srgb_u8_to_linear_f32(chunk[1]));
        out.push(srgb_u8_to_linear_f32(chunk[2]));
        out.push(f32::from(chunk[3]) / 255.0);
    }
    out
}

impl IsoGainMapFraction {
    pub(crate) fn into_gain_map_metadata(self, flags: u8) -> Result<GainMapMetadata, String> {
        let mut gain_map_min = [0.0; 3];
        let mut gain_map_max = [0.0; 3];
        let mut gamma = [1.0; 3];
        let mut offset_sdr = [0.0; 3];
        let mut offset_hdr = [0.0; 3];

        for channel in 0..3 {
            gain_map_min[channel] = signed_fraction(self.gain_map_min[channel])?;
            gain_map_max[channel] = signed_fraction(self.gain_map_max[channel])?;
            gamma[channel] = unsigned_fraction(self.gamma[channel])?;
            offset_sdr[channel] = signed_fraction(self.base_offset[channel])?;
            offset_hdr[channel] = signed_fraction(self.alternate_offset[channel])?;
        }

        validate_gain_map_metadata(GainMapMetadata {
            gain_map_min,
            gain_map_max,
            gamma,
            offset_sdr,
            offset_hdr,
            hdr_capacity_min: 2.0_f32.powf(unsigned_fraction(self.base_hdr_headroom)?),
            hdr_capacity_max: 2.0_f32.powf(unsigned_fraction(self.alternate_hdr_headroom)?),
            backward_direction: flags & ISO_BACKWARD_DIRECTION_FLAG != 0,
        })
    }
}

fn signed_fraction((numerator, denominator): (i32, u32)) -> Result<f32, String> {
    if denominator == 0 {
        return Err("ISO 21496-1 gain map metadata has zero denominator".to_string());
    }
    Ok(numerator as f32 / denominator as f32)
}

fn unsigned_fraction((numerator, denominator): (u32, u32)) -> Result<f32, String> {
    if denominator == 0 {
        return Err("ISO 21496-1 gain map metadata has zero denominator".to_string());
    }
    Ok(numerator as f32 / denominator as f32)
}

fn srgb_u8_to_linear_f32(value: u8) -> f32 {
    let encoded = f32::from(value) / 255.0;
    if encoded <= 0.04045 {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    }
}

pub(crate) fn gain_map_rgba_byte_len(gain_width: u32, gain_height: u32) -> Option<usize> {
    gain_width
        .checked_mul(gain_height)?
        .checked_mul(4)
        .map(|n| n as usize)
}

pub(crate) fn validate_gain_map_rgba_len(
    gain_rgba: &[u8],
    gain_width: u32,
    gain_height: u32,
) -> Result<(), String> {
    if gain_width == 0 || gain_height == 0 {
        return Err(format!(
            "gain map dimensions must be non-zero: {gain_width}x{gain_height}"
        ));
    }
    let expected = gain_map_rgba_byte_len(gain_width, gain_height)
        .ok_or_else(|| format!("gain map dimensions overflow: {gain_width}x{gain_height}"))?;
    if gain_rgba.len() != expected {
        return Err(format!(
            "gain map RGBA length mismatch: got {}, expected {} for {}x{}",
            gain_rgba.len(),
            expected,
            gain_width,
            gain_height
        ));
    }
    Ok(())
}

pub(crate) fn validate_iso_deferred_planes(
    width: u32,
    height: u32,
    sdr_rgba: &[u8],
    gain_width: u32,
    gain_height: u32,
    gain_rgba: &[u8],
) -> Result<(), String> {
    if width == 0 || height == 0 {
        return Err(format!(
            "primary dimensions must be non-zero: {width}x{height}"
        ));
    }
    let primary_expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|p| p.checked_mul(4))
        .ok_or_else(|| format!("primary dimension overflow: {width}x{height}"))?;
    if sdr_rgba.len() != primary_expected {
        return Err(format!(
            "SDR baseline RGBA length mismatch: got {}, expected {} for {}x{}",
            sdr_rgba.len(),
            primary_expected,
            width,
            height
        ));
    }
    validate_gain_map_rgba_len(gain_rgba, gain_width, gain_height)
}

fn gain_map_channel(
    gain_rgba: &[u8],
    gain_width: u32,
    x: u32,
    y: u32,
    channel_index: usize,
) -> f32 {
    let ch = channel_index.min(2);
    let index = (y as usize * gain_width as usize + x as usize) * 4 + ch;
    if index >= gain_rgba.len() {
        return 0.0;
    }
    f32::from(gain_rgba[index]) / 255.0
}

/// Horizontal and vertical bilinear tap indices/weights for one primary pixel.
///
/// **Keep in sync** with the `gx`/`gy` mapping in
/// [`sample_gain_map_row_nonlinear`](crate::hdr::heif_apple_gain_map_compose_simd::sample_gain_map_row_nonlinear)
/// (same center-aligned formula and clamp). `precompute_gain_row_matches_legacy_reference` catches drift.
#[inline]
pub(crate) fn gain_map_bilinear_coords(
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    gain_width: u32,
    gain_height: u32,
) -> (u32, u32, u32, u32, f32, f32) {
    let gx = ((x as f32 + 0.5) * gain_width as f32 / width as f32 - 0.5)
        .clamp(0.0, gain_width.saturating_sub(1) as f32);
    let gy = ((y as f32 + 0.5) * gain_height as f32 / height as f32 - 0.5)
        .clamp(0.0, gain_height.saturating_sub(1) as f32);
    let x0 = gx.floor() as u32;
    let y0 = gy.floor() as u32;
    let x1 = (x0 + 1).min(gain_width - 1);
    let y1 = (y0 + 1).min(gain_height - 1);
    let tx = gx - x0 as f32;
    let ty = gy - y0 as f32;
    (x0, x1, y0, y1, tx, ty)
}

/// Bilinear sample from precomputed tap coordinates (encoded 0–1, not yet BT.709-linear).
pub(crate) struct GainMapBilinearTaps {
    pub(crate) x0: u32,
    pub(crate) x1: u32,
    pub(crate) y0: u32,
    pub(crate) y1: u32,
    pub(crate) tx: f32,
    pub(crate) ty: f32,
}

pub(crate) fn sample_gain_map_rgb_bilinear(
    gain_rgba: &[u8],
    gain_width: u32,
    taps: GainMapBilinearTaps,
) -> [f32; 3] {
    let GainMapBilinearTaps {
        x0,
        x1,
        y0,
        y1,
        tx,
        ty,
    } = taps;
    let mut out = [0.0; 3];
    for (channel_index, channel) in out.iter_mut().enumerate() {
        let top = lerp(
            gain_map_channel(gain_rgba, gain_width, x0, y0, channel_index),
            gain_map_channel(gain_rgba, gain_width, x1, y0, channel_index),
            tx,
        );
        let bottom = lerp(
            gain_map_channel(gain_rgba, gain_width, x0, y1, channel_index),
            gain_map_channel(gain_rgba, gain_width, x1, y1, channel_index),
            tx,
        );
        *channel = lerp(top, bottom, ty);
    }
    out
}

#[inline]
fn gain_map_rgb_at_row(row: &[u8], x: u32) -> [f32; 3] {
    let index = x as usize * 4;
    [
        f32::from(row[index]) / 255.0,
        f32::from(row[index + 1]) / 255.0,
        f32::from(row[index + 2]) / 255.0,
    ]
}

#[inline]
fn bilinear_rgb_taps_encoded(
    c00: [f32; 3],
    c10: [f32; 3],
    c01: [f32; 3],
    c11: [f32; 3],
    tx: f32,
    ty: f32,
) -> [f32; 3] {
    let mut out = [0.0; 3];
    for channel in 0..3 {
        let top = lerp(c00[channel], c10[channel], tx);
        let bottom = lerp(c01[channel], c11[channel], tx);
        out[channel] = lerp(top, bottom, ty);
    }
    out
}

/// Bilinear upsample one gain-map row to primary width (encoded 0-1, not BT.709-linear yet).
///
/// Output is planar `[R0..Rn, G0..Gn, B0..Bn]` for SIMD channel loads.
/// **Keep in sync** with
/// [`sample_gain_map_row_nonlinear`](crate::hdr::heif_apple_gain_map_compose_simd::sample_gain_map_row_nonlinear).
pub(crate) fn precompute_gain_map_row_encoded(
    gain_rgba: &[u8],
    gain_width: u32,
    gain_height: u32,
    y: u32,
    width: u32,
    height: u32,
    out: &mut [f32],
) {
    if gain_width == 0 || gain_height == 0 || width == 0 || height == 0 {
        return;
    }

    let gy = ((y as f32 + 0.5) * gain_height as f32 / height as f32 - 0.5)
        .clamp(0.0, gain_height.saturating_sub(1) as f32);
    let y0 = gy.floor() as u32;
    let y1 = (y0 + 1).min(gain_height - 1);
    let ty = gy - y0 as f32;
    let row_stride = gain_width as usize * 4;
    let row0 = &gain_rgba[y0 as usize * row_stride..][..row_stride];
    let row1 = &gain_rgba[y1 as usize * row_stride..][..row_stride];

    let mut cache_x0 = u32::MAX;
    let mut c00 = [0.0; 3];
    let mut c10 = [0.0; 3];
    let mut c01 = [0.0; 3];
    let mut c11 = [0.0; 3];

    for x in 0..width {
        let gx = ((x as f32 + 0.5) * gain_width as f32 / width as f32 - 0.5)
            .clamp(0.0, gain_width.saturating_sub(1) as f32);
        let x0 = gx.floor() as u32;
        let tx = gx - x0 as f32;

        if x0 != cache_x0 {
            let x1 = (x0 + 1).min(gain_width - 1);
            c00 = gain_map_rgb_at_row(row0, x0);
            c10 = gain_map_rgb_at_row(row0, x1);
            c01 = gain_map_rgb_at_row(row1, x0);
            c11 = gain_map_rgb_at_row(row1, x1);
            cache_x0 = x0;
        }

        let sampled = bilinear_rgb_taps_encoded(c00, c10, c01, c11, tx, ty);
        // Planar layout: [R0..Rn, G0..Gn, B0..Bn] for SIMD channel loads.
        let xi = x as usize;
        let w = width as usize;
        out[xi] = sampled[0];
        out[w + xi] = sampled[1];
        out[2 * w + xi] = sampled[2];
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

struct ByteReader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> ByteReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn read_u8(&mut self) -> Result<u8, String> {
        if self.offset >= self.bytes.len() {
            return Err("truncated ISO 21496-1 gain map metadata".to_string());
        }
        let value = self.bytes[self.offset];
        self.offset += 1;
        Ok(value)
    }

    fn read_u16(&mut self) -> Result<u16, String> {
        let bytes = self.read_array::<2>()?;
        Ok(u16::from_be_bytes(bytes))
    }

    fn read_u32(&mut self) -> Result<u32, String> {
        let bytes = self.read_array::<4>()?;
        Ok(u32::from_be_bytes(bytes))
    }

    fn read_i32(&mut self) -> Result<i32, String> {
        let bytes = self.read_array::<4>()?;
        Ok(i32::from_be_bytes(bytes))
    }

    fn read_array<const N: usize>(&mut self) -> Result<[u8; N], String> {
        if self.offset + N > self.bytes.len() {
            return Err("truncated ISO 21496-1 gain map metadata".to_string());
        }
        let mut out = [0; N];
        out.copy_from_slice(&self.bytes[self.offset..self.offset + N]);
        self.offset += N;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        GainMapMetadata, compose_gain_map_pixel, iso_gain_map_primary_is_precomposed_hdr,
        iso_gain_map_skips_forward_compose, luminance_hints_from_gain_map,
        parse_iso_gain_map_metadata,
    };
    use crate::hdr::types::DEFAULT_SDR_WHITE_NITS;

    fn minimal_iso_metadata() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&0_u16.to_be_bytes()); // minimum_version
        out.extend_from_slice(&0_u16.to_be_bytes()); // writer_version
        out.push(0b0000_1000); // single-channel, common denominator
        out.extend_from_slice(&10_u32.to_be_bytes()); // denominator
        out.extend_from_slice(&0_u32.to_be_bytes()); // base headroom = 0
        out.extend_from_slice(&20_u32.to_be_bytes()); // alternate headroom = 2 stops
        out.extend_from_slice(&0_i32.to_be_bytes()); // gain min
        out.extend_from_slice(&20_i32.to_be_bytes()); // gain max = 2 stops
        out.extend_from_slice(&10_u32.to_be_bytes()); // gamma = 1
        out.extend_from_slice(&0_i32.to_be_bytes()); // base offset
        out.extend_from_slice(&0_i32.to_be_bytes()); // alternate offset
        out
    }

    #[test]
    fn iso_gain_map_metadata_parses_backward_direction() {
        let mut blob = minimal_iso_metadata();
        blob[4] = 0b0000_1100; // backward + common denominator
        let metadata = parse_iso_gain_map_metadata(&blob).expect("parse");
        assert!(metadata.backward_direction);
        assert!(iso_gain_map_skips_forward_compose(metadata));
    }

    #[test]
    fn iso_gain_map_metadata_expands_single_channel_values() {
        let metadata = parse_iso_gain_map_metadata(&minimal_iso_metadata()).expect("parse");

        assert!(!metadata.backward_direction);
        assert_eq!(metadata.gain_map_min, [0.0; 3]);
        assert_eq!(metadata.gain_map_max, [2.0; 3]);
        assert_eq!(metadata.gamma, [1.0; 3]);
        assert_eq!(metadata.hdr_capacity_min, 1.0);
        assert_eq!(metadata.hdr_capacity_max, 4.0);
    }

    #[test]
    fn iso_gain_map_primary_is_precomposed_hdr_when_base_headroom_exceeds_alternate() {
        let metadata = parse_iso_gain_map_metadata(&minimal_iso_metadata()).expect("parse");
        assert!(!iso_gain_map_primary_is_precomposed_hdr(metadata));

        let inverted = GainMapMetadata {
            hdr_capacity_min: 16.0,
            hdr_capacity_max: 1.0,
            ..metadata
        };
        assert!(iso_gain_map_primary_is_precomposed_hdr(inverted));
    }

    #[test]
    fn compose_gain_map_pixel_uses_capacity_weight() {
        let metadata = parse_iso_gain_map_metadata(&minimal_iso_metadata()).expect("parse");

        let sdr_only = compose_gain_map_pixel([128, 128, 128, 255], [1.0; 3], metadata, 1.0);
        let full_hdr = compose_gain_map_pixel([128, 128, 128, 255], [1.0; 3], metadata, 4.0);

        assert!(full_hdr[0] > sdr_only[0] * 3.9);
        assert_eq!(full_hdr[3], 1.0);
    }

    #[test]
    fn luminance_hints_from_gain_map_maps_headroom_to_nits() {
        let metadata = parse_iso_gain_map_metadata(&minimal_iso_metadata()).expect("parse");
        let hints = luminance_hints_from_gain_map(metadata);
        assert_eq!(hints.mastering_max_nits, Some(4.0 * DEFAULT_SDR_WHITE_NITS));
    }
}
