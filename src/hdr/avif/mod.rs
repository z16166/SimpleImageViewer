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

mod brand;
mod metadata;

#[cfg(feature = "avif-native")]
mod decode;
#[cfg(feature = "avif-native")]
mod gain_map;
#[cfg(feature = "avif-native")]
mod orientation;
#[cfg(feature = "avif-native")]
mod sequence;
#[cfg(feature = "avif-native")]
mod strip_baseline;

#[cfg(test)]
mod tests;

pub(crate) use brand::is_avif_brand;
pub(crate) use metadata::avif_cicp_to_metadata;

#[cfg(all(test, feature = "avif-native"))]
pub(crate) use decode::decode_avif_hdr_bytes;
#[cfg(feature = "avif-native")]
pub(crate) use decode::decode_avif_hdr_bytes_with_target_capacity;
#[cfg(all(test, feature = "avif-native"))]
pub(crate) use gain_map::avif_gain_map_to_metadata;
#[cfg(feature = "avif-native")]
pub(crate) use orientation::libavif_probe_exif_orientation_from_path;
#[cfg(all(test, feature = "avif-native"))]
pub(crate) use orientation::{
    AVIF_TRANSFORM_IMIR_FLAG, AVIF_TRANSFORM_IROT_FLAG, avif_irot_imir_to_exif_orientation,
};
#[cfg(feature = "avif-native")]
pub(crate) use sequence::try_decode_avif_image_sequence_hdr;
#[cfg(feature = "avif-native")]
pub(crate) use strip_baseline::{
    decode_avif_strip_exif_thumbnail, decode_avif_strip_iso_gain_map_baseline,
    decode_avif_strip_precomposed_hdr,
};

#[cfg(feature = "avif-native")]
use std::sync::Arc;

#[cfg(feature = "avif-native")]
use crate::hdr::avif_gain_map_deferred::{
    attach_avif_gain_map_gpu_deferred, avif_build_iso_sdr_baseline_rgba8,
};
#[cfg(feature = "avif-native")]
use crate::hdr::gain_map::iso_gain_map_skips_forward_compose;
#[cfg(feature = "avif-native")]
use crate::hdr::types::{
    HdrColorProfile, HdrImageBuffer, HdrImageMetadata, HdrPixelFormat, HdrReference,
    HdrTransferFunction,
};

#[cfg(feature = "avif-native")]
use metadata::{AvifMetadataExt, avif_yuv_to_rgb_output_metadata};

#[cfg(feature = "avif-native")]
use decode::{
    apply_icc_to_srgb_via_lcms, avif_fill_opaque_alpha_f32_if_no_alpha_plane,
    avif_fill_opaque_alpha_u16_if_no_alpha_plane, avif_image_icc_bytes, decode_avif_image_rgba_u16,
    libavif_result_to_string, rgb_channel_max_f,
};

#[cfg(feature = "avif-native")]
use gain_map::decode_avif_gain_map;

#[cfg(feature = "avif-native")]
/// Convert a decoded [`libavif_sys::avifImage`] (static read or sequence frame) into an
/// [`HdrImageBuffer`]. Safe on decoder-owned images: YUV→RGB relaxations restore CICP snapshots.
#[cfg(feature = "avif-native")]
pub(crate) fn avif_image_to_hdr_buffer(
    image: *mut libavif_sys::avifImage,
    target_hdr_capacity: f32,
) -> Result<HdrImageBuffer, String> {
    let image_ref = unsafe { &*image };
    if image_ref.width == 0 || image_ref.height == 0 {
        return Err("libavif decoded zero-sized image".to_string());
    }
    if image_ref.depth == 0 || image_ref.depth > 16 {
        return Err(format!("unsupported AVIF bit depth {}", image_ref.depth));
    }

    let metadata = avif_cicp_to_metadata(
        image_ref.colorPrimaries,
        image_ref.transferCharacteristics,
        image_ref.matrixCoefficients,
        image_ref.yuvRange == libavif_sys::AVIF_RANGE_FULL,
    )
    .with_clli(image_ref.clli.maxCLL, image_ref.clli.maxPALL);

    // **BT.2020 matrix 10** (constant luminance): libavif’s `reformat.c` has **no** dedicated
    // YUV→RGB matrix for CL — it uses an **explicit fallback to BT.2020 NCL (9)** for conversion,
    // same as several other “non-matrix” CICP codes (`avif_matrix_fallback_for_yuv_to_rgb`). That is
    // upstream **design**, not a bug we are papering over.
    //
    // Separately, **Microsoft Chimera** (`…_with_HDR_metadata.avif`) is a known case where the
    // **container CICP says 10** but the **coded luma/chroma matches NCL**; strict CL inverse would
    // skew colours (see AOMediaCodec/libavif#324). Using libavif’s NCL conversion matches that payload
    // and the paired SDR Chimera asset. **True** CL-encoded streams remain theoretically wrong here;
    // they are rare; fixing them would need a normative CL path, not a different “10 vs 9” hack.
    //
    // We do **not** persist any CICP rewrite to disk — only the temporary matrix passed into
    // `avifImageYUVToRGB` for this decode (metadata still reflects the file’s declared CICP).
    if image_ref.gainMap.is_null()
        && image_ref.matrixCoefficients == libavif_sys::AVIF_MATRIX_COEFFICIENTS_BT2020_CL
        && image_ref.transferCharacteristics == libavif_sys::AVIF_TRANSFER_CHARACTERISTICS_SMPTE2084
    {
        log::debug!(
            "[AVIF] CICP matrix 10 + PQ: YUV→RGB via libavif with matrix fallback 10→NCL (reformat has no CL matrix; Chimera-class files are often NCL payload with MC=10 tag)"
        );
    }

    let (mut rgba_u16, rgb_out_depth) =
        decode_avif_image_rgba_u16(image, image_ref, &libavif_result_to_string)?;
    avif_fill_opaque_alpha_u16_if_no_alpha_plane(&mut rgba_u16, rgb_out_depth, image_ref);

    let metadata = avif_yuv_to_rgb_output_metadata(&metadata, image_ref);
    let color_space = metadata.color_space_hint();

    // ISO gain map: defer compose to GPU (SDR baseline + gain planes + `jpeg_compose_gpu`).
    // Base RGB from `avifImageYUVToRGB` uses the image CICP transfer before ISO gain-map recovery.
    if let Some((gain_metadata, gain_width, gain_height, gain_rgba)) =
        decode_avif_gain_map(image_ref, &libavif_result_to_string)
    {
        if iso_gain_map_skips_forward_compose(gain_metadata) {
            log::debug!(
                "[HDR] AVIF gain map: primary is HDR base (backward or inverted HDRCapacity); skipping forward compose"
            );
        } else {
            let sdr_rgba = avif_build_iso_sdr_baseline_rgba8(
                &rgba_u16,
                rgb_out_depth,
                image_ref.width,
                image_ref.height,
                &metadata,
                color_space,
            );
            return attach_avif_gain_map_gpu_deferred(
                crate::hdr::avif_gain_map_deferred::AvifGainMapDeferredInput {
                    width: image_ref.width,
                    height: image_ref.height,
                    sdr_rgba,
                    gain_width,
                    gain_height,
                    gain_rgba,
                    gain_metadata,
                    container_luminance: metadata.luminance,
                    target_hdr_capacity,
                },
            );
        }
    }

    // Normalize using the **output** `avifRGBImage.depth` libavif used (8/10/12/16), not the
    // source YUV bit depth: 8-bit RGB output must use 255, while 16-bit full-range uses 65535.
    let scale = rgb_channel_max_f(rgb_out_depth);
    let mut rgba_f32 = rgba_u16
        .into_iter()
        .map(|value| value as f32 / scale)
        .collect::<Vec<_>>();

    // Honour an embedded ICC profile when present (e.g. `paris_icc_exif_xmp.avif`, Display P3
    // photo). Without this we'd treat DP3-encoded pixels as sRGB primaries → desaturated colours.
    // The lcms2 transform produces **sRGB-OETF-encoded floats in [0,1]** which the WGSL shader
    // then linearises via `srgb_to_linear`. Falls through to CICP interpretation when the file
    // has no ICC, when lcms2 is unavailable (build without `jpegxl`), or when the transform fails.
    let icc_slice = avif_image_icc_bytes(image_ref);
    let hdr_transfer_from_cicp = matches!(
        metadata.transfer_function,
        HdrTransferFunction::Pq | HdrTransferFunction::Hlg
    );
    if !icc_slice.is_empty() && hdr_transfer_from_cicp {
        log::debug!(
            "[AVIF] ignoring embedded ICC ({} bytes): CICP transfer {:?} — use WGSL PQ/HLG + CICP primaries, not ICC→sRGB",
            icc_slice.len(),
            metadata.transfer_function
        );
    }
    let final_metadata = if !icc_slice.is_empty()
        && !hdr_transfer_from_cicp
        && apply_icc_to_srgb_via_lcms(&mut rgba_f32, icc_slice)
    {
        let luminance = metadata.luminance;
        HdrImageMetadata {
            transfer_function: HdrTransferFunction::Srgb,
            reference: HdrReference::Unknown,
            color_profile: HdrColorProfile::Cicp {
                color_primaries: 1,
                transfer_characteristics: 13,
                matrix_coefficients: 0,
                full_range: true,
            },
            luminance,
            gain_map: None,
            raw_gpu_source: None,
        }
    } else {
        metadata
    };
    avif_fill_opaque_alpha_f32_if_no_alpha_plane(&mut rgba_f32, image_ref);
    let out_color_space = final_metadata.color_space_hint();

    Ok(HdrImageBuffer {
        width: image_ref.width,
        height: image_ref.height,
        format: HdrPixelFormat::Rgba32Float,
        color_space: out_color_space,
        metadata: final_metadata,
        rgba_f32: Arc::new(rgba_f32),
    })
}
