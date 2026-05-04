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

#[cfg(feature = "jpegxl")]
use crate::hdr::gain_map::{
    GainMapMetadata, append_hdr_pixel_from_sdr_and_gain, gain_map_metadata_diagnostic,
    parse_iso_gain_map_metadata, sample_gain_map_rgb,
};
use crate::hdr::types::{
    HdrColorProfile, HdrImageMetadata, HdrLuminanceMetadata, HdrReference, HdrTransferFunction,
};
#[cfg(feature = "jpegxl")]
use crate::hdr::types::{
    HdrColorSpace, HdrGainMapMetadata, HdrImageBuffer, HdrPixelFormat, HdrToneMapSettings,
};
#[cfg(feature = "jpegxl")]
use crate::{
    constants::{DEFAULT_ANIMATION_DELAY_MS, MIN_ANIMATION_DELAY_THRESHOLD_MS},
    loader::{AnimationFrame, DecodedImage, ImageData},
};
#[cfg(feature = "jpegxl")]
use std::sync::Arc;
#[cfg(feature = "jpegxl")]
use std::time::Duration;

#[cfg(feature = "jpegxl")]
struct JxlResizableRunnerPtr(*mut std::ffi::c_void);

#[cfg(feature = "jpegxl")]
impl JxlResizableRunnerPtr {
    fn try_new() -> Option<Self> {
        let ptr = unsafe { libjxl_sys::JxlResizableParallelRunnerCreate(std::ptr::null()) };
        if ptr.is_null() {
            None
        } else {
            Some(Self(ptr))
        }
    }

    fn as_ptr(&self) -> *mut std::ffi::c_void {
        self.0
    }
}

#[cfg(feature = "jpegxl")]
impl Drop for JxlResizableRunnerPtr {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { libjxl_sys::JxlResizableParallelRunnerDestroy(self.0) };
            self.0 = std::ptr::null_mut();
        }
    }
}

pub(crate) fn is_jxl_header(header: &[u8]) -> bool {
    header.starts_with(&[0xff, 0x0a])
        || header.starts_with(&[0x00, 0x00, 0x00, 0x0c, b'J', b'X', b'L', b' '])
}

// JPEG XL colour / container behaviour (normative references for this module):
//
// - **ISO/IEC 18181-1** — JPEG XL codestream (image data, colour description in bitstream).
// - **ISO/IEC 18181-2** — JPEG XL file format (BMFF boxes, optional ICC, orientation, etc.).
// - **ISO/IEC 18181-4** — Reference software; **libjxl** is the de-facto normative decoder API
//   used here (`jxl/decode.h`). Decoder colour queries are defined in that API, not guessed.
// - **`JxlColorProfileTarget`** (libjxl): `JXL_COLOR_PROFILE_TARGET_DATA` is the profile of the
//   **decoded pixels** written to the image out buffer; `JXL_COLOR_PROFILE_TARGET_ORIGINAL` is
//   the profile carried in metadata / codestream before decode. For `JXL_TYPE_FLOAT` output,
//   interpret samples against **TARGET_DATA ICC** when present, else `JxlColorEncoding` for DATA
//   (ICC wins over a generic encoded enum for XYB+ICC streams such as `bench_oriented_brg`).
// - **Associated alpha** (`JxlDecoderSetUnpremultiplyAlpha`): default decode is **premultiplied**
//   RGB when alpha is associated; we enable unpremultiply before decode so tone mapping sees
//   straight RGB (`jxl/decode.h`).
// - **XYB without ICC** (`JxlDecoderSetPreferredColorProfile`): when `TARGET_DATA` has **no** ICC,
//   steer XYB→float RGB with primaries inferred from any codestream ICC hint. If `TARGET_DATA`
//   already has an ICC, libjxl follows it for pixels — calling `SetPreferredColorProfile` then can
//   fight that path (washed highlights on conformance `bench_oriented_brg`).
// - **`JxlDecoderSetDesiredIntensityTarget`**: after `JXL_DEC_BASIC_INFO`, pass the codestream
//   `intensity_target` so float output luminance is scaled for that peak (e.g. 255 nits tests).
// - **ICC v4 `cicp` tag** (optional in profiles): carries ITU-T **H.273** codes; we map those
//   when present. Otherwise we derive primaries from ICC `rXYZ`/`gXYZ`/`bXYZ` per ICC.1.
//
// `JxlTransferFunction` discriminant values from libjxl `jxl/color_encoding.h`
/// (`JXL_TRANSFER_FUNCTION_*`). These are **not** ITU-T H.273 CICP
/// `transfer_characteristics` codes (numeric overlap is incidental).
pub(crate) const JXL_TRANSFER_FUNCTION_709: u16 = 1;
pub(crate) const JXL_TRANSFER_FUNCTION_GAMMA: u16 = 65535;
pub(crate) const JXL_TRANSFER_FUNCTION_LINEAR: u16 = 8;
pub(crate) const JXL_TRANSFER_FUNCTION_SRGB: u16 = 13;
pub(crate) const JXL_TRANSFER_FUNCTION_PQ: u16 = 16;
pub(crate) const JXL_TRANSFER_FUNCTION_HLG: u16 = 18;

#[allow(dead_code)]
pub(crate) fn jxl_color_encoding_to_metadata(
    color_primaries: u16,
    transfer_characteristics: u16,
    intensity_target_nits: Option<f32>,
) -> HdrImageMetadata {
    let transfer_function = match transfer_characteristics {
        JXL_TRANSFER_FUNCTION_LINEAR => HdrTransferFunction::Linear,
        JXL_TRANSFER_FUNCTION_SRGB => HdrTransferFunction::Srgb,
        JXL_TRANSFER_FUNCTION_PQ => HdrTransferFunction::Pq,
        JXL_TRANSFER_FUNCTION_HLG => HdrTransferFunction::Hlg,
        _ => HdrTransferFunction::Unknown,
    };
    let reference = match transfer_function {
        HdrTransferFunction::Pq => HdrReference::DisplayReferred,
        HdrTransferFunction::Hlg => HdrReference::SceneLinear,
        _ => HdrReference::Unknown,
    };

    HdrImageMetadata {
        transfer_function,
        reference,
        color_profile: HdrColorProfile::Cicp {
            color_primaries,
            transfer_characteristics,
            matrix_coefficients: 0,
            full_range: true,
        },
        luminance: HdrLuminanceMetadata {
            mastering_max_nits: intensity_target_nits,
            ..HdrLuminanceMetadata::default()
        },
        gain_map: None,
    }
}

#[cfg(feature = "jpegxl")]
#[allow(dead_code)]
pub(crate) fn load_jxl_hdr(path: &std::path::Path) -> Result<ImageData, String> {
    let bytes = std::fs::read(path).map_err(|err| format!("Failed to read JPEG XL: {err}"))?;
    decode_jxl_bytes_to_image_data(
        &bytes,
        crate::hdr::types::HdrToneMapSettings::default().target_hdr_capacity(),
        crate::hdr::types::HdrToneMapSettings::default(),
    )
}

#[cfg(feature = "jpegxl")]
#[allow(dead_code)]
pub(crate) fn decode_jxl_hdr(path: &std::path::Path) -> Result<HdrImageBuffer, String> {
    let bytes = std::fs::read(path).map_err(|err| format!("Failed to read JPEG XL: {err}"))?;
    decode_jxl_hdr_bytes(&bytes)
}

#[cfg(feature = "jpegxl")]
pub(crate) fn decode_jxl_hdr_bytes(bytes: &[u8]) -> Result<HdrImageBuffer, String> {
    decode_jxl_hdr_bytes_with_target_capacity(
        bytes,
        crate::hdr::types::HdrToneMapSettings::default().target_hdr_capacity(),
    )
}

#[cfg(feature = "jpegxl")]
pub(crate) fn load_jxl_hdr_with_target_capacity(
    path: &std::path::Path,
    target_hdr_capacity: f32,
    tone_map: HdrToneMapSettings,
) -> Result<ImageData, String> {
    let bytes = std::fs::read(path).map_err(|err| format!("Failed to read JPEG XL: {err}"))?;
    decode_jxl_bytes_to_image_data(&bytes, target_hdr_capacity, tone_map)
}

#[cfg(feature = "jpegxl")]
#[allow(dead_code)]
pub(crate) fn decode_jxl_hdr_with_target_capacity(
    path: &std::path::Path,
    target_hdr_capacity: f32,
) -> Result<HdrImageBuffer, String> {
    let bytes = std::fs::read(path).map_err(|err| format!("Failed to read JPEG XL: {err}"))?;
    decode_jxl_hdr_bytes_with_target_capacity(&bytes, target_hdr_capacity)
}

#[cfg(feature = "jpegxl")]
/// SDR-grade JPEG XL: when `intensity_target ≤ 255` and transfer is linear in our metadata
/// extraction, libjxl's float output for `bench_oriented_brg` (VarDCT + reconstructed JPEG +
/// embedded sRGB ICC) actually comes back **already sRGB OETF-encoded** in 0–1, not in
/// scene-linear despite the libjxl docstring. Pixel-level conformance against the reference
/// `ref.png`: an OETF-twice path (treating these as linear and re-applying sRGB OETF) drifts
/// ~+60 code values per channel uniformly — exactly the wash-out users see on SDR displays.
///
/// So for SDR-grade content we treat the float buffer as sRGB-encoded display values and quantize
/// straight to 8-bit. Primary conversion is still applied for non-sRGB primaries (BT.2020 /
/// Display P3) — that step operates on coded values, which is also fine when the source carries
/// linear-light samples in those primaries. HDR (peak > 255) keeps the Reinhard tone-map path.
///
/// # Conformance baseline (do **not** chase `ref.png` blindly)
///
/// This path is the official libjxl SDR display path. Verified against
/// `djxl.exe` v0.11.2 (vcpkg `libjxl[tools]:x64-windows-static`, built locally
/// from this manifest with the `tools` feature enabled and then disabled
/// again):
///
/// ```text
///   ours vs djxl --color_space=RGB_D65_SRG_Per_SRG : RMSE 0.495,  peak Δ = 1
///   ours vs djxl (default, no --color_space)        : RMSE ~40,    peak Δ ~62
/// ```
///
/// i.e. our SDR fallback equals `djxl --color_space=RGB_D65_SRG_Per_SRG`
/// to within 1 code per channel (rounding noise). The plain `djxl` default
/// is the well-known linear-sRGB bug from libjxl issue #2289.
///
/// Some `ref.png` files in the libjxl `conformance/testcases/` corpus are
/// **not** produced by `djxl`. For instance `progressive/ref.png` differs from
/// every `djxl` invocation (RMSE ≥ 12, peak Δ ≥ 17) and is uniformly darker by
/// 6–16 code values per channel — it is generated by an in-corpus
/// `numpy → PNG` tool that applies a different normalization than lcms2's
/// `decoded.icc → reference.icc → sRGB` path. Treat such `ref.png` files as
/// visual aids only, **not** as a normative target. The normative conformance
/// criterion is `reference_image.npy + reference.icc` compared via
/// `lcms2.convert_pixels` (see `libjxl/conformance/scripts/conformance.py`,
/// `CompareNPY`).
fn jxl_sdr_grade_fallback_rgba8(
    rgba_f32: &[f32],
    color_space: HdrColorSpace,
    metadata: &HdrImageMetadata,
) -> Option<Vec<u8>> {
    let peak = metadata.luminance.mastering_max_nits.unwrap_or(0.0);
    if !peak.is_finite() || peak <= 0.0 || peak > 255.0 {
        return None;
    }
    // The float buffer libjxl gave us is encoded according to
    // `metadata.transfer_function`. SDR grade has only two interesting cases:
    //   - `Linear`: truly linear-light values (e.g. conformance
    //     `patches/input.jxl`, TF=8 in the codestream). Apply the sRGB OETF
    //     before quantizing or shadows quantize to ~0 and the image looks
    //     ~22 codes too dark across the board.
    //   - `Srgb` / `Gamma` / `Unknown`: libjxl preserved the codestream's
    //     non-linear encoding (e.g. `bench_oriented_brg`, `blendmodes`,
    //     `bike`, `cmyk_layers` after the lcms2 transform). Direct quantize
    //     `value * 255`.
    // PQ / HLG → fall through to the HDR pipeline.
    let needs_srgb_oetf = match metadata.transfer_function {
        HdrTransferFunction::Linear => true,
        HdrTransferFunction::Srgb
        | HdrTransferFunction::Gamma
        | HdrTransferFunction::Unknown => false,
        HdrTransferFunction::Pq | HdrTransferFunction::Hlg => return None,
    };
    let mut out = Vec::with_capacity(rgba_f32.len());
    for px in rgba_f32.chunks_exact(4) {
        let mapped = crate::hdr::decode::linear_primary_to_linear_srgb(
            [px[0], px[1], px[2]],
            color_space,
            metadata,
        );
        if needs_srgb_oetf {
            out.push(linear_to_srgb_u8(mapped[0]));
            out.push(linear_to_srgb_u8(mapped[1]));
            out.push(linear_to_srgb_u8(mapped[2]));
        } else {
            out.push(srgb_unit_to_u8(mapped[0]));
            out.push(srgb_unit_to_u8(mapped[1]));
            out.push(srgb_unit_to_u8(mapped[2]));
        }
        let a = px[3];
        let a8 = if a.is_finite() {
            (a.clamp(0.0, 1.0) * 255.0).round() as u8
        } else {
            255
        };
        out.push(a8);
    }
    Some(out)
}

#[cfg(feature = "jpegxl")]
fn srgb_unit_to_u8(value: f32) -> u8 {
    if !value.is_finite() {
        return 0;
    }
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Locate the index of the first extra channel of type `JXL_CHANNEL_BLACK`
/// (the "K" in CMYK). Returns `None` when the source has no K plane or when
/// libjxl rejects the channel-info call.
#[cfg(feature = "jpegxl")]
fn jxl_find_black_extra_channel_index(
    decoder: *mut libjxl_sys::JxlDecoder,
    info: &libjxl_sys::JxlBasicInfo,
) -> Option<u32> {
    for idx in 0..info.num_extra_channels {
        let mut ec = std::mem::MaybeUninit::<libjxl_sys::JxlExtraChannelInfo>::zeroed();
        let st = unsafe {
            libjxl_sys::JxlDecoderGetExtraChannelInfo(
                decoder.cast_const(),
                idx as usize,
                ec.as_mut_ptr(),
            )
        };
        if st != libjxl_sys::JXL_DEC_SUCCESS {
            continue;
        }
        let ec = unsafe { ec.assume_init() };
        if ec.type_ == libjxl_sys::JXL_CHANNEL_BLACK {
            return Some(idx);
        }
    }
    None
}

/// Composite the K (black) ink plane into the RGBA float buffer in place.
///
/// libjxl returns the CMY-as-RGB visible channels for CMYK sources but drops K.
/// Empirical pixel-level diff against the conformance `cmyk_layers/ref.png`:
/// `R *= K, G *= K, B *= K` matches with mean signed diff (+3.6, +2.8, -2.0)
/// Composite a CMYK JPEG-XL frame into sRGB **in-place**, mirroring exactly what
/// `djxl` does (libjxl `enc_color_management.cc` LCMS path + `enc_image_bundle.cc`
/// `CopyToT` for CMYK). For sources with a `JXL_CHANNEL_BLACK` extra channel,
/// libjxl emits CMY in the RGB float slots (`0 = max ink, 1 = white`) and the
/// K plane separately; the proper rendering path is to apply the embedded CMYK
/// ICC profile via an external CMS to obtain visually-correct sRGB.
///
/// We use lcms2 (`TYPE_CMYK_FLT` → `TYPE_RGBA_FLT`, `INTENT_PERCEPTUAL`) which
/// matches what djxl falls back to when libjxl is built without skcms. The
/// PostScript-style 0..100 scaling on the input matches the comment
/// "LCMS does CMYK in a weird way: 0 = white, 100 = max ink" in libjxl. Output
/// values are sRGB-encoded floats in 0..1 with alpha passed through.
///
/// Returns `true` if the transform succeeded; on failure (e.g. unparseable ICC
/// or lcms internal error) leaves `rgba` untouched and logs a warning so the
/// caller can fall back to the existing native-HDR / SDR-grading path.
#[cfg(feature = "jpegxl")]
fn apply_cmyk_to_srgb_via_lcms(
    rgba: &mut [f32],
    k: &[f32],
    source_icc: &[u8],
) -> bool {
    let pixel_count = rgba.len() / 4;
    if pixel_count != k.len() {
        log::warn!(
            "[JXL] CMYK K plane length ({}) does not match RGBA pixel count ({pixel_count}); skipping CMS transform",
            k.len()
        );
        return false;
    }
    if source_icc.is_empty() {
        log::warn!("[JXL] CMYK source has no embedded ICC profile; skipping CMS transform");
        return false;
    }

    // Build interleaved CMYK input in lcms2's native PostScript units (0..100,
    // 0 = no ink, 100 = max ink). libjxl uses (0 = max ink, 1 = white), so the
    // scale is `100 - 100*v`.
    let mut cmyk = Vec::<f32>::with_capacity(pixel_count * 4);
    let mut alpha = Vec::<f32>::with_capacity(pixel_count);
    for (px, &k_val) in rgba.chunks_exact(4).zip(k.iter()) {
        cmyk.push(100.0 - 100.0 * px[0].clamp(0.0, 1.0));
        cmyk.push(100.0 - 100.0 * px[1].clamp(0.0, 1.0));
        cmyk.push(100.0 - 100.0 * px[2].clamp(0.0, 1.0));
        cmyk.push(100.0 - 100.0 * k_val.clamp(0.0, 1.0));
        alpha.push(px[3]);
    }

    let mut rgba_out = vec![0.0_f32; pixel_count * 4];
    unsafe {
        let in_profile = libjxl_sys::cmsOpenProfileFromMem(
            source_icc.as_ptr().cast(),
            source_icc.len() as u32,
        );
        if in_profile.is_null() {
            log::warn!("[JXL] lcms2 could not parse embedded CMYK ICC; skipping CMS transform");
            return false;
        }
        let out_profile = libjxl_sys::cmsCreate_sRGBProfile();
        if out_profile.is_null() {
            libjxl_sys::cmsCloseProfile(in_profile);
            log::warn!("[JXL] lcms2 could not build sRGB profile; skipping CMS transform");
            return false;
        }
        let transform = libjxl_sys::cmsCreateTransform(
            in_profile,
            libjxl_sys::LCMS_TYPE_CMYK_FLT,
            out_profile,
            libjxl_sys::LCMS_TYPE_RGBA_FLT,
            libjxl_sys::LCMS_INTENT_PERCEPTUAL,
            0,
        );
        if transform.is_null() {
            libjxl_sys::cmsCloseProfile(in_profile);
            libjxl_sys::cmsCloseProfile(out_profile);
            log::warn!(
                "[JXL] lcms2 could not build CMYK→sRGB transform from {}-byte ICC; skipping",
                source_icc.len()
            );
            return false;
        }
        libjxl_sys::cmsDoTransform(
            transform,
            cmyk.as_ptr().cast(),
            rgba_out.as_mut_ptr().cast(),
            pixel_count as u32,
        );
        libjxl_sys::cmsDeleteTransform(transform);
        libjxl_sys::cmsCloseProfile(in_profile);
        libjxl_sys::cmsCloseProfile(out_profile);
    }

    for (i, (dst, src)) in rgba
        .chunks_exact_mut(4)
        .zip(rgba_out.chunks_exact(4))
        .enumerate()
    {
        dst[0] = src[0];
        dst[1] = src[1];
        dst[2] = src[2];
        dst[3] = alpha[i];
    }
    true
}

#[cfg(feature = "jpegxl")]
fn apply_jxl_jhgm_gain_map_if_present(
    jhgm_box: Option<&[u8]>,
    target_hdr_capacity: f32,
    rgba_f32: &mut Vec<f32>,
    width: u32,
    height: u32,
    metadata: &mut HdrImageMetadata,
) {
    let Some(jhgm_box) = jhgm_box else {
        return;
    };
    let expected_len = width as usize * height as usize * 4;
    match decode_jxl_gain_map(
        jhgm_box,
        target_hdr_capacity,
        rgba_f32,
        width,
        height,
    ) {
        Ok((gain_metadata, gain_width, gain_height, gain_rgba)) => {
            let diagnostic = gain_map_metadata_diagnostic(gain_metadata, target_hdr_capacity);
            let mut composed = Vec::with_capacity(expected_len);
            for y in 0..height {
                for x in 0..width {
                    let index = (y as usize * width as usize + x as usize) * 4;
                    let sdr_rgba = [
                        (linear_to_srgb_u8(rgba_f32[index])),
                        (linear_to_srgb_u8(rgba_f32[index + 1])),
                        (linear_to_srgb_u8(rgba_f32[index + 2])),
                        (rgba_f32[index + 3] * 255.0).round().clamp(0.0, 255.0) as u8,
                    ];
                    let gain_value = sample_gain_map_rgb(
                        &gain_rgba,
                        gain_width,
                        gain_height,
                        x,
                        y,
                        width,
                        height,
                    );
                    append_hdr_pixel_from_sdr_and_gain(
                        &mut composed,
                        &sdr_rgba,
                        gain_value,
                        gain_metadata,
                        target_hdr_capacity,
                    );
                }
            }
            metadata.gain_map = Some(HdrGainMapMetadata {
                source: "JPEG XL",
                target_hdr_capacity: Some(target_hdr_capacity),
                diagnostic,
                capped_display_referred: false,
            });
            *rgba_f32 = composed;
        }
        Err(err) => {
            log::warn!("[HDR] JPEG XL jhgm gain-map fallback: {err}");
        }
    }
}

#[cfg(feature = "jpegxl")]
fn jxl_frame_ticks_to_delay_ms(basic_info: &libjxl_sys::JxlBasicInfo, ticks: u32) -> u64 {
    let raw_ms = if basic_info.have_animation == 0 {
        DEFAULT_ANIMATION_DELAY_MS as u64
    } else if ticks == 0 {
        0u64
    } else {
        let num = basic_info.animation.tps_numerator.max(1) as u128;
        let den = basic_info.animation.tps_denominator.max(1) as u128;
        ((ticks as u128).saturating_mul(1000).saturating_mul(den) / num)
            .min(u128::from(u64::MAX)) as u64
    };
    if raw_ms == 0 || raw_ms <= MIN_ANIMATION_DELAY_THRESHOLD_MS as u64 {
        DEFAULT_ANIMATION_DELAY_MS as u64
    } else {
        raw_ms
    }
}

#[cfg(feature = "jpegxl")]
pub(crate) fn decode_jxl_hdr_bytes_with_target_capacity(
    bytes: &[u8],
    target_hdr_capacity: f32,
) -> Result<HdrImageBuffer, String> {
    match decode_jxl_bytes_to_image_data(
        bytes,
        target_hdr_capacity,
        crate::hdr::types::HdrToneMapSettings::default(),
    )? {
        ImageData::Hdr { hdr, .. } => Ok(hdr),
        ImageData::Animated(_) => Err(
            "JPEG XL has multiple animation frames; use the image loader or decode_jxl_bytes_to_image_data"
                .to_string(),
        ),
        ImageData::Static(_) | ImageData::Tiled(_) | ImageData::HdrTiled { .. } => Err(
            "unexpected JPEG XL decode outcome (expected HDR buffer)".to_string(),
        ),
    }
}

/// Decode a full JPEG XL file into [`ImageData`]. Multi-frame animations become
/// [`ImageData::Animated`] (SDR RGBA8 per frame); a single displayed frame stays
/// [`ImageData::Hdr`] with float pixels and an SDR fallback.
#[cfg(feature = "jpegxl")]
pub(crate) fn decode_jxl_bytes_to_image_data(
    bytes: &[u8],
    target_hdr_capacity: f32,
    tone_map: HdrToneMapSettings,
) -> Result<ImageData, String> {
    let probe_len = bytes.len().min(16).max(2);
    if !is_jxl_header(&bytes[..probe_len]) {
        return Err(
            "Input is not a valid JPEG XL codestream or BMFF container (wrong signature). \
If this is a libjxl conformance path ending in `*_5` on Windows, Git may have materialized a symlink as a tiny text file—open the sibling testcase without `_5`, or clone with symlink support."
                .to_string(),
        );
    }

    struct JxlDecoder(*mut libjxl_sys::JxlDecoder);
    impl Drop for JxlDecoder {
        fn drop(&mut self) {
            unsafe { libjxl_sys::JxlDecoderDestroy(self.0) };
        }
    }

    let parallel_runner = JxlResizableRunnerPtr::try_new();
    let decoder = JxlDecoder(unsafe { libjxl_sys::JxlDecoderCreate(std::ptr::null()) });
    if decoder.0.is_null() {
        return Err("Failed to create libjxl decoder".to_string());
    }

    // libjxl default for associated alpha is premultiplied RGB; our HDR/tone-map path expects
    // straight (unpremultiplied) linear RGB + separate alpha (`jxl/decode.h` — must set before decode).
    let unpremul_st = unsafe {
        libjxl_sys::JxlDecoderSetUnpremultiplyAlpha(decoder.0, libjxl_sys::JXL_TRUE)
    };
    if unpremul_st != libjxl_sys::JXL_DEC_SUCCESS {
        log::warn!(
            "JxlDecoderSetUnpremultiplyAlpha failed (libjxl status {unpremul_st}); colors may be wrong for premultiplied alpha"
        );
    }

    if let Some(runner) = parallel_runner.as_ref() {
        let st = unsafe {
            libjxl_sys::JxlDecoderSetParallelRunner(
                decoder.0,
                Some(libjxl_sys::JxlResizableParallelRunner),
                runner.as_ptr(),
            )
        };
        if st != libjxl_sys::JXL_DEC_SUCCESS {
            log::warn!(
                "JxlDecoderSetParallelRunner failed (libjxl status {st}); continuing with libjxl default threading"
            );
        }
    }

    let subscribed = libjxl_sys::JXL_DEC_BASIC_INFO
        | libjxl_sys::JXL_DEC_COLOR_ENCODING
        | libjxl_sys::JXL_DEC_PREVIEW_IMAGE
        | libjxl_sys::JXL_DEC_FRAME
        | libjxl_sys::JXL_DEC_FULL_IMAGE
        | libjxl_sys::JXL_DEC_BOX
        | libjxl_sys::JXL_DEC_BOX_COMPLETE;
    ensure_jxl_success(
        unsafe { libjxl_sys::JxlDecoderSubscribeEvents(decoder.0, subscribed) },
        "subscribe JPEG XL decoder events",
    )?;
    ensure_jxl_success(
        unsafe { libjxl_sys::JxlDecoderSetInput(decoder.0, bytes.as_ptr(), bytes.len()) },
        "set JPEG XL input",
    )?;
    ensure_jxl_success(
        unsafe { libjxl_sys::JxlDecoderSetDecompressBoxes(decoder.0, 1) },
        "enable JPEG XL box decompression",
    )?;
    unsafe { libjxl_sys::JxlDecoderCloseInput(decoder.0) };

    let pixel_format = libjxl_sys::JxlPixelFormat {
        num_channels: 4,
        data_type: libjxl_sys::JXL_TYPE_FLOAT,
        endianness: libjxl_sys::JXL_NATIVE_ENDIAN,
        align: 0,
    };
    let extra_channel_format = libjxl_sys::JxlPixelFormat {
        num_channels: 1,
        ..pixel_format
    };

    let mut basic_info = None;
    let mut metadata = HdrImageMetadata::default();
    let mut rgba_f32 = Vec::<f32>::new();
    let mut current_box_type = [0_u8; 4];
    let mut current_box_buffer = Vec::<u8>::new();
    let mut current_box_pos = 0_usize;
    let mut jhgm_box = None::<Vec<u8>>;
    let mut pending_duration_ticks: u32 = 0;
    let mut captured_frames: Vec<(Vec<f32>, u32)> = Vec::new();
    let mut preview_scratch: Vec<u8> = Vec::new();
    // CMYK-style sources encode K (black ink) as an extra channel of type
    // `JXL_CHANNEL_BLACK`. Per libjxl PR #237 / `enc_color_management.cc`, the
    // visible-RGB output for these requires running the embedded CMYK ICC
    // through an external CMS (4-channel CMYK input → 3-channel sRGB output);
    // libjxl's bundled CMS does NOT auto-convert non-XYB CMYK output. We
    // capture the K plane plus the source CMYK ICC, then call lcms2 in
    // `apply_cmyk_to_srgb_via_lcms`. Without this, conformance file
    // `cmyk_layers/input.jxl` loses the black ink layer (missing "black" word)
    // and renders flat process colors (lime instead of teal background).
    let mut k_extra_channel_index: Option<u32> = None;
    let mut k_f32 = Vec::<f32>::new();
    let mut cmyk_source_icc: Vec<u8> = Vec::new();

    loop {
        match unsafe { libjxl_sys::JxlDecoderProcessInput(decoder.0) } {
            libjxl_sys::JXL_DEC_SUCCESS => {
                // libjxl: keep calling ProcessInput after each FULL_IMAGE until SUCCESS
                // (see libjxl examples/decode_oneshot.cc — animations decode multiple frames).
                let info: libjxl_sys::JxlBasicInfo =
                    basic_info.ok_or("libjxl finished without basic info")?;
                if captured_frames.is_empty() && rgba_f32.is_empty() {
                    return Err("libjxl decode completed without an image".to_string());
                }
                let expected_len = info.xsize as usize * info.ysize as usize * 4;
                if captured_frames.len() > 1 {
                    for (buf, _) in &captured_frames {
                        if buf.len() != expected_len {
                            return Err(format!(
                                "libjxl output buffer length mismatch: got {}, expected {}",
                                buf.len(),
                                expected_len
                            ));
                        }
                    }
                }

                if captured_frames.len() > 1 {
                    let meta_base = metadata.clone();
                    let mut animation = Vec::with_capacity(captured_frames.len());
                    for (mut buf, ticks) in captured_frames {
                        let mut frame_metadata = meta_base.clone();
                        apply_jxl_jhgm_gain_map_if_present(
                            jhgm_box.as_deref(),
                            target_hdr_capacity,
                            &mut buf,
                            info.xsize,
                            info.ysize,
                            &mut frame_metadata,
                        );
                        let color_space = frame_metadata.color_space_hint();
                        let pixels = if let Some(px) =
                            jxl_sdr_grade_fallback_rgba8(&buf, color_space, &frame_metadata)
                        {
                            px
                        } else {
                            let hdr = HdrImageBuffer {
                                width: info.xsize,
                                height: info.ysize,
                                format: HdrPixelFormat::Rgba32Float,
                                color_space,
                                metadata: frame_metadata,
                                rgba_f32: Arc::new(buf),
                            };
                            crate::hdr::decode::hdr_to_sdr_rgba8_with_tone_settings(
                                &hdr,
                                tone_map.exposure_ev,
                                &tone_map,
                            )?
                        };
                        let delay_ms = jxl_frame_ticks_to_delay_ms(&info, ticks);
                        animation.push(AnimationFrame::new(
                            info.xsize,
                            info.ysize,
                            pixels,
                            Duration::from_millis(delay_ms),
                        ));
                    }
                    return Ok(ImageData::Animated(animation));
                }

                let mut rgba = captured_frames
                    .pop()
                    .map(|(buf, _)| buf)
                    .unwrap_or(rgba_f32);
                if rgba.len() != expected_len {
                    return Err(format!(
                        "libjxl output buffer length mismatch: got {}, expected {}",
                        rgba.len(),
                        expected_len
                    ));
                }
                if k_extra_channel_index.is_some() && !k_f32.is_empty() {
                    let cmyk_converted =
                        apply_cmyk_to_srgb_via_lcms(&mut rgba, &k_f32, &cmyk_source_icc);
                    if cmyk_converted {
                        // After lcms2 CMYK→sRGB the float buffer holds sRGB-
                        // ENCODED values in 0..1 (PostScript-style 0..100 input
                        // mapped through the embedded CMYK ICC + sRGB output
                        // profile, intent=Perceptual). Tag as Srgb so the SDR
                        // grade fallback (`jxl_sdr_grade_fallback_rgba8`)
                        // direct-quantizes via `srgb_unit_to_u8` and does NOT
                        // re-apply the OETF.
                        metadata.transfer_function = HdrTransferFunction::Srgb;
                        metadata.color_profile = HdrColorProfile::Cicp {
                            color_primaries: 1,
                            transfer_characteristics: 13,
                            matrix_coefficients: 0,
                            full_range: true,
                        };
                        metadata.luminance.mastering_max_nits = Some(100.0);
                    }
                }
                apply_jxl_jhgm_gain_map_if_present(
                    jhgm_box.as_deref(),
                    target_hdr_capacity,
                    &mut rgba,
                    info.xsize,
                    info.ysize,
                    &mut metadata,
                );
                let color_space = metadata.color_space_hint();
                let sdr_grade_fallback =
                    jxl_sdr_grade_fallback_rgba8(&rgba, color_space, &metadata);
                let hdr = HdrImageBuffer {
                    width: info.xsize,
                    height: info.ysize,
                    format: HdrPixelFormat::Rgba32Float,
                    color_space,
                    metadata,
                    rgba_f32: Arc::new(rgba),
                };
                let fallback_pixels = match sdr_grade_fallback {
                    Some(px) => px,
                    None => crate::hdr::decode::hdr_to_sdr_rgba8_with_tone_settings(
                        &hdr,
                        tone_map.exposure_ev,
                        &tone_map,
                    )?,
                };
                let fallback = DecodedImage::new(hdr.width, hdr.height, fallback_pixels);
                return Ok(ImageData::Hdr { hdr, fallback });
            }
            libjxl_sys::JXL_DEC_ERROR => {
                return Err(
                    "libjxl decode failed (invalid codestream, unsupported feature, or internal decoder error)"
                        .to_string(),
                );
            }
            libjxl_sys::JXL_DEC_NEED_MORE_INPUT => {
                return Err("libjxl requested more input after full file was supplied".to_string());
            }
            libjxl_sys::JXL_DEC_BASIC_INFO => {
                let mut info = std::mem::MaybeUninit::<libjxl_sys::JxlBasicInfo>::zeroed();
                ensure_jxl_success(
                    unsafe { libjxl_sys::JxlDecoderGetBasicInfo(decoder.0, info.as_mut_ptr()) },
                    "read JPEG XL basic info",
                )?;
                let info = unsafe { info.assume_init() };
                if info.xsize == 0 || info.ysize == 0 {
                    return Err("libjxl decoded zero-sized image".to_string());
                }
                metadata.luminance.mastering_max_nits =
                    (info.intensity_target > 0.0).then_some(info.intensity_target);
                metadata.luminance.mastering_min_nits =
                    (info.min_nits > 0.0).then_some(info.min_nits);
                if info.intensity_target.is_finite() && info.intensity_target > 0.0 {
                    let st = unsafe {
                        libjxl_sys::JxlDecoderSetDesiredIntensityTarget(
                            decoder.0,
                            info.intensity_target,
                        )
                    };
                    if st != libjxl_sys::JXL_DEC_SUCCESS {
                        log::debug!(
                            "JxlDecoderSetDesiredIntensityTarget({}) returned {st}",
                            info.intensity_target
                        );
                    }
                }
                if let Some(runner) = parallel_runner.as_ref() {
                    unsafe {
                        let threads = libjxl_sys::JxlResizableParallelRunnerSuggestThreads(
                            info.xsize as u64,
                            info.ysize as u64,
                        )
                        .max(1) as usize;
                        libjxl_sys::JxlResizableParallelRunnerSetThreads(runner.as_ptr(), threads);
                    }
                }
                k_extra_channel_index = jxl_find_black_extra_channel_index(decoder.0, &info);
                if let Some(idx) = k_extra_channel_index {
                    log::debug!(
                        "[JXL] CMYK-style K (black) extra channel found at index {idx}"
                    );
                }
                basic_info = Some(info);
            }
            libjxl_sys::JXL_DEC_COLOR_ENCODING => {
                // Animations: `SetPreferredColorProfile` can break multi-frame decode on some libjxl
                // builds. Still images with TARGET_DATA ICC: let libjxl use that ICC for XYB→RGB;
                // adding a preferred enum profile on top can desaturate / blow highlights
                // (conformance `bench_oriented_brg`).
                let is_animation = basic_info.is_some_and(|info| info.have_animation != 0);
                let has_target_icc =
                    jxl_decoder_copy_target_data_icc(decoder.0.cast_const()).is_some();
                if !is_animation && !has_target_icc {
                    jxl_apply_preferred_profile_from_target_data_icc(decoder.0);
                }
                // Capture the source CMYK ICC for later lcms2 transform. The
                // ICC has to be read from `TARGET_ORIGINAL` since `TARGET_DATA`
                // can be overridden by libjxl's color management (and for non-
                // XYB CMYK sources both happen to be the same CMYK profile).
                if k_extra_channel_index.is_some() && cmyk_source_icc.is_empty() {
                    if let Some(icc) =
                        jxl_decoder_copy_target_original_icc(decoder.0.cast_const())
                    {
                        log::debug!(
                            "[JXL] captured {} byte CMYK source ICC for lcms2 transform",
                            icc.len()
                        );
                        cmyk_source_icc = icc;
                    }
                }
                metadata = read_jxl_metadata(decoder.0, metadata);
            }
            libjxl_sys::JXL_DEC_NEED_PREVIEW_OUT_BUFFER => {
                let mut size = 0_usize;
                ensure_jxl_success(
                    unsafe {
                        libjxl_sys::JxlDecoderPreviewOutBufferSize(
                            decoder.0,
                            &pixel_format,
                            &mut size,
                        )
                    },
                    "size JPEG XL preview output buffer",
                )?;
                if size % std::mem::size_of::<f32>() != 0 {
                    return Err("libjxl preview buffer size is not float-aligned".to_string());
                }
                preview_scratch.resize(size, 0);
                ensure_jxl_success(
                    unsafe {
                        libjxl_sys::JxlDecoderSetPreviewOutBuffer(
                            decoder.0,
                            &pixel_format,
                            preview_scratch.as_mut_ptr().cast(),
                            size,
                        )
                    },
                    "set JPEG XL preview output buffer",
                )?;
            }
            libjxl_sys::JXL_DEC_PREVIEW_IMAGE => {
                continue;
            }
            libjxl_sys::JXL_DEC_FRAME => {
                let mut fh = std::mem::MaybeUninit::<libjxl_sys::JxlFrameHeader>::zeroed();
                let st = unsafe {
                    libjxl_sys::JxlDecoderGetFrameHeader(
                        decoder.0 as *const libjxl_sys::JxlDecoder,
                        fh.as_mut_ptr(),
                    )
                };
                if st == libjxl_sys::JXL_DEC_SUCCESS {
                    pending_duration_ticks = unsafe { fh.assume_init() }.duration;
                }
            }
            libjxl_sys::JXL_DEC_NEED_IMAGE_OUT_BUFFER => {
                let mut size = 0_usize;
                ensure_jxl_success(
                    unsafe {
                        libjxl_sys::JxlDecoderImageOutBufferSize(
                            decoder.0,
                            &pixel_format,
                            &mut size,
                        )
                    },
                    "size JPEG XL output buffer",
                )?;
                if size % std::mem::size_of::<f32>() != 0 {
                    return Err("libjxl returned a misaligned float output size".to_string());
                }
                rgba_f32 = vec![0.0; size / std::mem::size_of::<f32>()];
                ensure_jxl_success(
                    unsafe {
                        libjxl_sys::JxlDecoderSetImageOutBuffer(
                            decoder.0,
                            &pixel_format,
                            rgba_f32.as_mut_ptr().cast(),
                            size,
                        )
                    },
                    "set JPEG XL output buffer",
                )?;
                if let Some(idx) = k_extra_channel_index {
                    let mut k_size = 0_usize;
                    let st = unsafe {
                        libjxl_sys::JxlDecoderExtraChannelBufferSize(
                            decoder.0,
                            &extra_channel_format,
                            &mut k_size,
                            idx,
                        )
                    };
                    if st == libjxl_sys::JXL_DEC_SUCCESS
                        && k_size % std::mem::size_of::<f32>() == 0
                    {
                        k_f32 = vec![0.0; k_size / std::mem::size_of::<f32>()];
                        let set_st = unsafe {
                            libjxl_sys::JxlDecoderSetExtraChannelBuffer(
                                decoder.0,
                                &extra_channel_format,
                                k_f32.as_mut_ptr().cast(),
                                k_size,
                                idx,
                            )
                        };
                        if set_st != libjxl_sys::JXL_DEC_SUCCESS {
                            log::warn!(
                                "JxlDecoderSetExtraChannelBuffer for K returned {set_st}; CMYK K plane will be ignored"
                            );
                            k_f32.clear();
                            k_extra_channel_index = None;
                        }
                    } else {
                        log::warn!(
                            "JxlDecoderExtraChannelBufferSize for K returned {st} size={k_size}; CMYK K plane will be ignored"
                        );
                        k_extra_channel_index = None;
                    }
                }
            }
            libjxl_sys::JXL_DEC_BOX => {
                if !current_box_buffer.is_empty() {
                    capture_jxl_box(
                        decoder.0,
                        current_box_type,
                        &mut current_box_buffer,
                        current_box_pos,
                        &mut jhgm_box,
                    );
                    current_box_buffer.clear();
                    current_box_pos = 0;
                }
                ensure_jxl_success(
                    unsafe {
                        libjxl_sys::JxlDecoderGetBoxType(
                            decoder.0,
                            current_box_type.as_mut_ptr(),
                            1,
                        )
                    },
                    "read JPEG XL box type",
                )?;
                if current_box_type == *b"jhgm" {
                    let mut box_size = 0_u64;
                    ensure_jxl_success(
                        unsafe {
                            libjxl_sys::JxlDecoderGetBoxSizeContents(decoder.0, &mut box_size)
                        },
                        "read JPEG XL jhgm box size",
                    )?;
                    if box_size > usize::MAX as u64 {
                        return Err("JPEG XL jhgm box too large".to_string());
                    }
                    current_box_buffer = vec![0_u8; box_size as usize];
                    current_box_pos = 0;
                    ensure_jxl_success(
                        unsafe {
                            libjxl_sys::JxlDecoderSetBoxBuffer(
                                decoder.0,
                                current_box_buffer.as_mut_ptr(),
                                current_box_buffer.len(),
                            )
                        },
                        "set JPEG XL jhgm box buffer",
                    )?;
                }
            }
            libjxl_sys::JXL_DEC_BOX_NEED_MORE_OUTPUT => {
                let remaining = unsafe { libjxl_sys::JxlDecoderReleaseBoxBuffer(decoder.0) };
                current_box_pos = current_box_buffer.len().saturating_sub(remaining);
                if current_box_type == *b"jhgm" && remaining > 0 {
                    ensure_jxl_success(
                        unsafe {
                            libjxl_sys::JxlDecoderSetBoxBuffer(
                                decoder.0,
                                current_box_buffer[current_box_pos..].as_mut_ptr(),
                                remaining,
                            )
                        },
                        "continue JPEG XL jhgm box buffer",
                    )?;
                }
            }
            libjxl_sys::JXL_DEC_BOX_COMPLETE => {
                capture_jxl_box(
                    decoder.0,
                    current_box_type,
                    &mut current_box_buffer,
                    current_box_pos,
                    &mut jhgm_box,
                );
                current_box_buffer.clear();
                current_box_pos = 0;
            }
            libjxl_sys::JXL_DEC_FULL_IMAGE => {
                let info = basic_info.ok_or("libjxl produced pixels before basic info")?;
                let expected_len = info.xsize as usize * info.ysize as usize * 4;
                if rgba_f32.len() != expected_len {
                    return Err(format!(
                        "libjxl output buffer length mismatch: got {}, expected {}",
                        rgba_f32.len(),
                        expected_len
                    ));
                }
                captured_frames.push((rgba_f32.clone(), pending_duration_ticks));
                // Animations emit multiple FULL_IMAGE events; keep calling ProcessInput until SUCCESS.
                continue;
            }
            libjxl_sys::JXL_DEC_DC_IMAGE | libjxl_sys::JXL_DEC_FRAME_PROGRESSION => {
                continue;
            }
            libjxl_sys::JXL_DEC_JPEG_RECONSTRUCTION | libjxl_sys::JXL_DEC_JPEG_NEED_MORE_OUTPUT => {
                return Err(
                    "JPEG XL JPEG reconstruction stream is not supported by this viewer".to_string(),
                );
            }
            status => {
                return Err(format!("unsupported libjxl decoder status {status}"));
            }
        }
    }
}

#[cfg(feature = "jpegxl")]
fn ensure_jxl_success(status: libjxl_sys::JxlDecoderStatus, action: &str) -> Result<(), String> {
    if status == libjxl_sys::JXL_DEC_SUCCESS {
        Ok(())
    } else {
        Err(format!("Failed to {action}: libjxl status {status}"))
    }
}

#[cfg(feature = "jpegxl")]
fn capture_jxl_box(
    decoder: *mut libjxl_sys::JxlDecoder,
    box_type: [u8; 4],
    buffer: &mut Vec<u8>,
    buffer_pos: usize,
    jhgm_box: &mut Option<Vec<u8>>,
) {
    if buffer.is_empty() || box_type != *b"jhgm" {
        return;
    }
    let remaining = unsafe { libjxl_sys::JxlDecoderReleaseBoxBuffer(decoder) };
    let written = if remaining > 0 {
        buffer.len().saturating_sub(remaining)
    } else {
        buffer.len()
    }
    .max(buffer_pos)
    .min(buffer.len());
    jhgm_box.replace(buffer[..written].to_vec());
}

#[cfg(feature = "jpegxl")]
fn decode_jxl_gain_map(
    jhgm_box: &[u8],
    target_hdr_capacity: f32,
    _base_rgba_f32: &[f32],
    _base_width: u32,
    _base_height: u32,
) -> Result<(GainMapMetadata, u32, u32, Vec<u8>), String> {
    let bundle = read_jxl_gain_map_bundle(jhgm_box)?;
    let metadata = parse_iso_gain_map_metadata(bundle.metadata)?;
    let gain_map = decode_jxl_hdr_bytes_with_target_capacity(bundle.gain_map, target_hdr_capacity)?;
    let gain_rgba = gain_map
        .rgba_f32
        .iter()
        .map(|value| (value * 255.0).round().clamp(0.0, 255.0) as u8)
        .collect();
    Ok((metadata, gain_map.width, gain_map.height, gain_rgba))
}

#[cfg(feature = "jpegxl")]
#[derive(Debug, Clone, Copy)]
pub(crate) struct JxlGainMapBundleRef<'a> {
    #[allow(dead_code)]
    pub(crate) version: u8,
    pub(crate) metadata: &'a [u8],
    pub(crate) gain_map: &'a [u8],
}

#[cfg(feature = "jpegxl")]
pub(crate) fn read_jxl_gain_map_bundle(jhgm_box: &[u8]) -> Result<JxlGainMapBundleRef<'_>, String> {
    let mut reader = JxlBundleReader::new(jhgm_box);
    let version = reader.read_u8()?;
    let metadata_size = reader.read_u16()? as usize;
    let metadata = reader.read_slice(metadata_size)?;
    let compressed_color_encoding_size = reader.read_u8()? as usize;
    let _compressed_color_encoding = reader.read_slice(compressed_color_encoding_size)?;
    let compressed_icc_size = reader.read_u32()? as usize;
    let _compressed_icc = reader.read_slice(compressed_icc_size)?;
    let gain_map = reader.remaining_slice();

    if metadata.is_empty() {
        return Err("JPEG XL jhgm bundle has no ISO gain-map metadata".to_string());
    }
    if gain_map.is_empty() {
        return Err("JPEG XL jhgm bundle has no gain-map codestream".to_string());
    }

    Ok(JxlGainMapBundleRef {
        version,
        metadata,
        gain_map,
    })
}

#[cfg(feature = "jpegxl")]
struct JxlBundleReader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

#[cfg(feature = "jpegxl")]
impl<'a> JxlBundleReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn read_u8(&mut self) -> Result<u8, String> {
        let slice = self.read_slice(1)?;
        Ok(slice[0])
    }

    fn read_u16(&mut self) -> Result<u16, String> {
        let slice = self.read_slice(2)?;
        Ok(u16::from_be_bytes([slice[0], slice[1]]))
    }

    fn read_u32(&mut self) -> Result<u32, String> {
        let slice = self.read_slice(4)?;
        Ok(u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]))
    }

    fn read_slice(&mut self, len: usize) -> Result<&'a [u8], String> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or_else(|| "JPEG XL jhgm bundle length overflow".to_string())?;
        if end > self.bytes.len() {
            return Err("truncated JPEG XL jhgm gain-map bundle".to_string());
        }
        let slice = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(slice)
    }

    fn remaining_slice(&mut self) -> &'a [u8] {
        let slice = &self.bytes[self.offset..];
        self.offset = self.bytes.len();
        slice
    }
}

#[cfg(feature = "jpegxl")]
fn linear_to_srgb_u8(value: f32) -> u8 {
    let value = value.max(0.0);
    let encoded = if value <= 0.0031308 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    };
    (encoded * 255.0).round().clamp(0.0, 255.0) as u8
}

#[cfg(feature = "jpegxl")]
fn icc_find_tag_element_offset(icc: &[u8], tag: &[u8; 4]) -> Option<usize> {
    const HEADER: usize = 128;
    if icc.len() < HEADER + 4 {
        return None;
    }
    let tag_count = u32::from_be_bytes(icc[128..132].try_into().ok()?) as usize;
    if tag_count > 4096 {
        return None;
    }
    let mut entry = 132usize;
    for _ in 0..tag_count {
        if entry + 12 > icc.len() {
            break;
        }
        if &icc[entry..entry + 4] == tag {
            let offset = u32::from_be_bytes(icc[entry + 4..entry + 8].try_into().ok()?) as usize;
            return Some(offset);
        }
        entry += 12;
    }
    None
}

#[cfg(feature = "jpegxl")]
fn icc_read_s15fixed16(bytes: &[u8], offset: usize) -> Option<f32> {
    let v = i32::from_be_bytes(bytes.get(offset..offset + 4)?.try_into().ok()?);
    Some(v as f32 / 65536.0)
}

/// Classify the `rTRC` (red Tone Reproduction Curve) tag of an ICC profile so
/// we can decide whether libjxl's float buffer for an embedded-ICC source is
/// already in encoded form (`Srgb` / `Gamma`) or truly linear (`Linear`). The
/// classification is a heuristic — it only inspects `rTRC` and assumes the
/// per-channel TRCs are uniform — but it's enough for the JXL conformance
/// corpus we care about (sRGB ICC, Display P3 linear ICC, etc.).
///
/// ICC v4 §10.5: `curveType` is `'curv'` followed by reserved (4) and a u32
/// `count`:
///   - count == 0 → identity (linear)
///   - count == 1 → single u8.8 fixed-point gamma value (`0x0100` = 1.0)
///   - count >= 2 → a `count`-entry u16 LUT (e.g. ICC v4 sRGB has count == 1024)
///
/// Returns `None` if the tag is missing or malformed (caller falls back).
#[cfg(feature = "jpegxl")]
fn icc_trc_kind(icc: &[u8]) -> Option<HdrTransferFunction> {
    let off = icc_find_tag_element_offset(icc, b"rTRC")?;
    if off + 12 > icc.len() {
        return None;
    }
    if &icc[off..off + 4] != b"curv" {
        // Could be `parametricCurveType` (`para`) — ICC v4 §10.18. We only
        // bother with the linear/non-linear distinction.
        if &icc[off..off + 4] == b"para" {
            // ICC v4 §10.18: function type at offset+8 (u16). Type 0 = simple
            // power gamma `Y = X^g`. Type 1+ are sRGB-style piecewise.
            let function_type = u16::from_be_bytes(icc[off + 8..off + 10].try_into().ok()?);
            if function_type == 0 {
                let gamma = icc_read_s15fixed16(icc, off + 12)?;
                if (gamma - 1.0).abs() < 1e-3 {
                    return Some(HdrTransferFunction::Linear);
                }
                return Some(HdrTransferFunction::Gamma);
            }
            return Some(HdrTransferFunction::Srgb);
        }
        return None;
    }
    let count = u32::from_be_bytes(icc[off + 8..off + 12].try_into().ok()?) as usize;
    if count == 0 {
        return Some(HdrTransferFunction::Linear);
    }
    if count == 1 {
        if off + 14 > icc.len() {
            return None;
        }
        let raw = u16::from_be_bytes(icc[off + 12..off + 14].try_into().ok()?);
        let gamma = raw as f32 / 256.0; // u8.8 fixed point
        if (gamma - 1.0).abs() < 1e-2 {
            return Some(HdrTransferFunction::Linear);
        }
        return Some(HdrTransferFunction::Gamma);
    }
    // Multi-entry LUT: assume sRGB-style encoding curve. We could detect a
    // pure-linear LUT here (identity ramp) but real-world ICCs that ship a
    // LUT are non-linear, and the SDR fallback's direct-quantize path is the
    // safe choice for any non-linear curve we encounter on the JXL conformance
    // corpus.
    Some(HdrTransferFunction::Srgb)
}

/// Read an `XYZType` payload (`XYZ ` + reserved + three s15Fixed16) and convert to CIE xy.
#[cfg(feature = "jpegxl")]
fn icc_xyz_type_to_xy(icc: &[u8], tag_element_offset: usize) -> Option<(f64, f64)> {
    if tag_element_offset + 20 > icc.len() {
        return None;
    }
    if &icc[tag_element_offset..tag_element_offset + 4] != b"XYZ " {
        return None;
    }
    let x = icc_read_s15fixed16(icc, tag_element_offset + 8)? as f64;
    let y = icc_read_s15fixed16(icc, tag_element_offset + 12)? as f64;
    let z = icc_read_s15fixed16(icc, tag_element_offset + 16)? as f64;
    let sum = x + y + z;
    if !sum.is_finite() || sum.abs() < 1e-20 {
        return None;
    }
    Some((x / sum, y / sum))
}

/// Derive CICP-style primaries from ICC `rXYZ`/`gXYZ`/`bXYZ` when no `cicp` tag is present
/// (common for libjxl-generated PQ profiles).
///
/// ICC tags are named after **file** channel order (e.g. JPEG XL `brg` / `bgr`), not necessarily
/// RGB semantics, so we match the multiset of three xy points to BT.2020 / Display P3 primaries.
///
/// ICC `rXYZ`/`gXYZ`/`bXYZ` often encodes **BT.709** primaries for PQ/HDR JPEG XL while libjxl
/// still outputs **linear light in that same narrow gamut** (see conformance `bench_oriented_brg`).
/// Do **not** assume Rec.2020 unless the chromaticities actually match BT.2020 / P3.
#[cfg(feature = "jpegxl")]
fn hdr_metadata_from_icc_rgb_xyz_primaries_for_jxl_float(icc: &[u8]) -> Option<HdrImageMetadata> {
    let r_off = icc_find_tag_element_offset(icc, b"rXYZ")?;
    let g_off = icc_find_tag_element_offset(icc, b"gXYZ")?;
    let b_off = icc_find_tag_element_offset(icc, b"bXYZ")?;
    let xy0 = icc_xyz_type_to_xy(icc, r_off)?;
    let xy1 = icc_xyz_type_to_xy(icc, g_off)?;
    let xy2 = icc_xyz_type_to_xy(icc, b_off)?;
    let observed = [xy0, xy1, xy2];

    const BT2020: [(f64, f64); 3] = [(0.708, 0.292), (0.17, 0.797), (0.131, 0.046)];
    const DISPLAY_P3: [(f64, f64); 3] = [(0.68, 0.32), (0.265, 0.69), (0.15, 0.06)];
    const BT709: [(f64, f64); 3] = [(0.64, 0.33), (0.3, 0.6), (0.15, 0.06)];
    const PERMS: [[usize; 3]; 6] = [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ];

    let multiset_close =
        |obs: [(f64, f64); 3], tgt: [(f64, f64); 3], eps: f64| {
            PERMS.iter().any(|perm| {
                (0..3).all(|i| {
                    let p = obs[perm[i]];
                    let t = tgt[i];
                    (p.0 - t.0).hypot(p.1 - t.1) <= eps
                })
            })
        };

    let color_primaries = if multiset_close(observed, BT2020, 0.08) {
        9u16
    } else if multiset_close(observed, DISPLAY_P3, 0.1) {
        11u16
    } else if multiset_close(observed, BT709, 0.06) {
        1u16
    } else {
        return None;
    };

    // The function only fires when libjxl can't expose the codestream encoding
    // as a JxlColorEncoding enum (empty `JxlDecoderGetColorAsEncodedProfile`),
    // i.e. the ICC profile is the only ground truth. Parse the rTRC tag to
    // distinguish actually-linear ICCs (e.g. conformance `patches_lossless`
    // with a Display P3 linear profile) from sRGB-curve ICCs (e.g.
    // `bench_oriented_brg` JPEG-recompressed sRGB) — the float buffer libjxl
    // emits matches the ICC's TRC, so the SDR-grade fallback needs to know.
    let trc = icc_trc_kind(icc).unwrap_or(HdrTransferFunction::Srgb);
    let (cicp_transfer, internal_tf, reference) = match trc {
        HdrTransferFunction::Linear => (8_u16, HdrTransferFunction::Linear, HdrReference::Unknown),
        HdrTransferFunction::Gamma => (4_u16, HdrTransferFunction::Gamma, HdrReference::Unknown),
        // Fallback: encoded-curve ICC — match `bench_oriented_brg` behavior.
        // (BT.2020 / Display P3 with non-linear curves still go through the
        // SDR-grade direct-quantize path, intensity_target gates HDR.)
        _ => (13_u16, HdrTransferFunction::Srgb, HdrReference::Unknown),
    };

    Some(HdrImageMetadata {
        transfer_function: internal_tf,
        reference,
        color_profile: HdrColorProfile::Cicp {
            color_primaries,
            transfer_characteristics: cicp_transfer,
            matrix_coefficients: 0,
            full_range: true,
        },
        luminance: HdrLuminanceMetadata::default(),
        gain_map: None,
    })
}

#[cfg(feature = "jpegxl")]
fn icc_scan_cicp_tag(icc: &[u8]) -> Option<(u16, u16, u16, bool)> {
    const HEADER: usize = 128;
    if icc.len() < HEADER + 4 {
        return None;
    }
    let tag_count = u32::from_be_bytes(icc[128..132].try_into().ok()?) as usize;
    if tag_count > 4096 {
        return None;
    }
    let mut entry = 132usize;
    for _ in 0..tag_count {
        if entry + 12 > icc.len() {
            break;
        }
        if icc[entry..entry + 4] == *b"cicp" {
            let offset = u32::from_be_bytes(icc[entry + 4..entry + 8].try_into().ok()?) as usize;
            let _size = u32::from_be_bytes(icc[entry + 8..entry + 12].try_into().ok()?) as usize;
            // Tag data: signature (4) + reserved (4) + payload
            if offset + 12 > icc.len() {
                return None;
            }
            let p = u16::from(icc[offset + 8]);
            let t = u16::from(icc[offset + 9]);
            let m = u16::from(icc[offset + 10]);
            let fr = icc[offset + 11] != 0;
            return Some((p, t, m, fr));
        }
        entry += 12;
    }
    None
}

#[cfg(feature = "jpegxl")]
fn hdr_metadata_from_h273_cicp_for_jxl_float_buffer(
    color_primaries: u16,
    transfer_characteristics: u16,
    matrix_coefficients: u16,
    full_range: bool,
) -> HdrImageMetadata {
    // CICP transfer characteristics carry ground-truth source TF when present
    // (ITU-T H.273 §8.2). Map them to our internal flag so the SDR-grade
    // fallback knows whether libjxl's float buffer is linear (needs OETF) or
    // already encoded (direct quantize). Previously this hard-coded `Linear`
    // and the rest of the pipeline papered over it — that broke true-linear
    // sources like conformance `patches/input.jxl`.
    let (internal_tf, reference) = match transfer_characteristics {
        8 => (HdrTransferFunction::Linear, HdrReference::Unknown),
        16 => (HdrTransferFunction::Pq, HdrReference::DisplayReferred),
        18 => (HdrTransferFunction::Hlg, HdrReference::SceneLinear),
        4 => (HdrTransferFunction::Gamma, HdrReference::Unknown),
        // 1 (BT.709), 6 / 14 / 15 (BT.601 / BT.2020 ish), 13 (sRGB IEC 61966-2-1):
        // all encoded with sRGB-equivalent OETF for SDR, the float buffer is
        // already in encoded form for libjxl's Modular mode output.
        _ => (HdrTransferFunction::Srgb, HdrReference::Unknown),
    };
    HdrImageMetadata {
        transfer_function: internal_tf,
        reference,
        color_profile: HdrColorProfile::Cicp {
            color_primaries,
            transfer_characteristics,
            matrix_coefficients,
            full_range,
        },
        luminance: HdrLuminanceMetadata::default(),
        gain_map: None,
    }
}

#[cfg(feature = "jpegxl")]
fn jxl_xy_dist(a: [f64; 2], b: [f64; 2]) -> f64 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

#[cfg(feature = "jpegxl")]
fn jxl_xy_close(a: [f64; 2], b: [f64; 2], eps: f64) -> bool {
    jxl_xy_dist(a, b) <= eps
}

/// Map `JxlColorEncoding` primaries to an H.273-style `color_primaries` code for our
/// `HdrColorProfile::Cicp` hint. `JXL_PRIMARIES_CUSTOM` is resolved from `primaries_*_xy`.
#[cfg(feature = "jpegxl")]
fn jxl_cicp_color_primaries_from_encoding(color: &libjxl_sys::JxlColorEncoding) -> u16 {
    if color.color_space != libjxl_sys::JXL_COLOR_SPACE_RGB {
        return 2;
    }
    if color.primaries == libjxl_sys::JXL_PRIMARIES_2100 {
        return 9;
    }
    if color.primaries == libjxl_sys::JXL_PRIMARIES_P3 {
        return 11;
    }
    if color.primaries == libjxl_sys::JXL_PRIMARIES_SRGB {
        return 1;
    }
    if color.primaries == libjxl_sys::JXL_PRIMARIES_CUSTOM {
        if chromaticities_close_to_bt2020(color) {
            return 9;
        }
        if chromaticities_close_to_display_p3(color) {
            return 11;
        }
        if chromaticities_close_to_bt709_srgb(color) {
            return 1;
        }
    }
    2
}

#[cfg(feature = "jpegxl")]
fn chromaticities_close_to_bt2020(color: &libjxl_sys::JxlColorEncoding) -> bool {
    const R: [f64; 2] = [0.708, 0.292];
    const G: [f64; 2] = [0.17, 0.797];
    const B: [f64; 2] = [0.131, 0.046];
    const EPS: f64 = 0.06;
    jxl_xy_close(color.primaries_red_xy, R, EPS)
        && jxl_xy_close(color.primaries_green_xy, G, EPS)
        && jxl_xy_close(color.primaries_blue_xy, B, EPS)
}

#[cfg(feature = "jpegxl")]
fn chromaticities_close_to_display_p3(color: &libjxl_sys::JxlColorEncoding) -> bool {
    const R: [f64; 2] = [0.68, 0.32];
    const G: [f64; 2] = [0.265, 0.69];
    const B: [f64; 2] = [0.15, 0.06];
    const EPS: f64 = 0.05;
    jxl_xy_close(color.primaries_red_xy, R, EPS)
        && jxl_xy_close(color.primaries_green_xy, G, EPS)
        && jxl_xy_close(color.primaries_blue_xy, B, EPS)
}

#[cfg(feature = "jpegxl")]
fn chromaticities_close_to_bt709_srgb(color: &libjxl_sys::JxlColorEncoding) -> bool {
    const R: [f64; 2] = [0.64, 0.33];
    const G: [f64; 2] = [0.3, 0.6];
    const B: [f64; 2] = [0.15, 0.06];
    const EPS: f64 = 0.04;
    jxl_xy_close(color.primaries_red_xy, R, EPS)
        && jxl_xy_close(color.primaries_green_xy, G, EPS)
        && jxl_xy_close(color.primaries_blue_xy, B, EPS)
}

/// Build metadata from `JxlColorEncoding` for **`JXL_COLOR_PROFILE_TARGET_DATA`** (decoded pixels).
///
/// With `JXL_TYPE_FLOAT` + default bit depth, libjxl returns **linear light** in the profile's
/// RGB primaries; the encoding's `transfer_function` describes the **coded** image, not raw
/// nonlinear samples in the float buffer (see libjxl decoder API / examples).
#[cfg(feature = "jpegxl")]
fn hdr_metadata_from_jxl_float_decode(color: &libjxl_sys::JxlColorEncoding) -> HdrImageMetadata {
    let cicp_primaries = jxl_cicp_color_primaries_from_encoding(color);
    // libjxl's `JxlTransferFunction` is a signed `c_int` enum but the values
    // we care about (1, 4, 8, 13, 16, 18, 65535=GAMMA) all fit unsigned u16.
    let jxl_tf_code = color.transfer_function as i64;
    let cicp_transfer = jxl_cicp_transfer_code_from_jxl(jxl_tf_code);
    let internal_tf = jxl_internal_transfer_for_jxl_float_buffer(jxl_tf_code);
    let reference = match internal_tf {
        HdrTransferFunction::Pq => HdrReference::DisplayReferred,
        HdrTransferFunction::Hlg => HdrReference::SceneLinear,
        _ => HdrReference::Unknown,
    };
    HdrImageMetadata {
        transfer_function: internal_tf,
        reference,
        color_profile: HdrColorProfile::Cicp {
            color_primaries: cicp_primaries,
            transfer_characteristics: cicp_transfer,
            matrix_coefficients: 0,
            full_range: true,
        },
        luminance: HdrLuminanceMetadata::default(),
        gain_map: None,
    }
}

/// Map libjxl's `JxlTransferFunction` enum (codestream value) to the
/// `HdrTransferFunction` we use internally to decide how to quantize the float
/// buffer for SDR fallback. Per empirical sampling of conformance files,
/// libjxl preserves the codestream's encoding in the float buffer for
/// Modular-mode files: TF=Linear → linear floats, TF=sRGB / 709 / Gamma →
/// already-encoded floats. PQ / HLG signal HDR.
#[cfg(feature = "jpegxl")]
fn jxl_internal_transfer_for_jxl_float_buffer(jxl_tf: i64) -> HdrTransferFunction {
    match jxl_tf {
        x if x == JXL_TRANSFER_FUNCTION_LINEAR as i64 => HdrTransferFunction::Linear,
        x if x == JXL_TRANSFER_FUNCTION_SRGB as i64 || x == JXL_TRANSFER_FUNCTION_709 as i64 => {
            HdrTransferFunction::Srgb
        }
        x if x == JXL_TRANSFER_FUNCTION_PQ as i64 => HdrTransferFunction::Pq,
        x if x == JXL_TRANSFER_FUNCTION_HLG as i64 => HdrTransferFunction::Hlg,
        x if x == JXL_TRANSFER_FUNCTION_GAMMA as i64 => HdrTransferFunction::Gamma,
        _ => HdrTransferFunction::Unknown,
    }
}

/// Convert libjxl's `JxlTransferFunction` enum into the matching CICP transfer
/// characteristics code (ITU-T H.273), so downstream components see the same
/// numeric values the JXL bitstream specified instead of always reporting
/// "linear" (which used to be the previous hard-coded fallback).
#[cfg(feature = "jpegxl")]
fn jxl_cicp_transfer_code_from_jxl(jxl_tf: i64) -> u16 {
    match jxl_tf {
        x if x == JXL_TRANSFER_FUNCTION_709 as i64 => 1,
        x if x == JXL_TRANSFER_FUNCTION_LINEAR as i64 => 8,
        x if x == JXL_TRANSFER_FUNCTION_SRGB as i64 => 13,
        x if x == JXL_TRANSFER_FUNCTION_PQ as i64 => 16,
        x if x == JXL_TRANSFER_FUNCTION_HLG as i64 => 18,
        x if x == JXL_TRANSFER_FUNCTION_GAMMA as i64 => 4,
        _ => 2,
    }
}

#[cfg(feature = "jpegxl")]
fn jxl_decoder_copy_target_data_icc(decoder: *const libjxl_sys::JxlDecoder) -> Option<Vec<u8>> {
    jxl_decoder_copy_icc_for_target(decoder, libjxl_sys::JXL_COLOR_PROFILE_TARGET_DATA)
}

/// Read the **original** color profile of the JXL bitstream (i.e. before any
/// CMS applied by libjxl). This is the "source" profile used by external
/// color management — for CMYK-style sources it's a CMYK ICC profile that we
/// feed into lcms2 to compose CMYK→sRGB.
#[cfg(feature = "jpegxl")]
fn jxl_decoder_copy_target_original_icc(
    decoder: *const libjxl_sys::JxlDecoder,
) -> Option<Vec<u8>> {
    jxl_decoder_copy_icc_for_target(decoder, libjxl_sys::JXL_COLOR_PROFILE_TARGET_ORIGINAL)
}

#[cfg(feature = "jpegxl")]
fn jxl_decoder_copy_icc_for_target(
    decoder: *const libjxl_sys::JxlDecoder,
    target: libjxl_sys::JxlColorProfileTarget,
) -> Option<Vec<u8>> {
    let mut icc_size = 0_usize;
    let st = unsafe {
        libjxl_sys::JxlDecoderGetICCProfileSize(decoder, target, &mut icc_size)
    };
    if st != libjxl_sys::JXL_DEC_SUCCESS || icc_size == 0 {
        return None;
    }
    let mut icc = vec![0_u8; icc_size];
    let st2 = unsafe {
        libjxl_sys::JxlDecoderGetColorAsICCProfile(
            decoder,
            target,
            icc.as_mut_ptr(),
            icc.len(),
        )
    };
    (st2 == libjxl_sys::JXL_DEC_SUCCESS).then_some(icc)
}

/// VarDCT (XYB) + ICC: steer libjxl's XYB→float-RGB path toward primaries inferred from the
/// embedded `TARGET_DATA` ICC (`rXYZ`/`gXYZ`/`bXYZ`), instead of relying on the decoder's generic
/// fallback that can disagree with narrow-gamut PQ ICCs (e.g. conformance `bench_oriented_brg`).
#[cfg(feature = "jpegxl")]
fn jxl_apply_preferred_profile_from_target_data_icc(decoder: *mut libjxl_sys::JxlDecoder) {
    let Some(icc) = jxl_decoder_copy_target_data_icc(decoder.cast_const()) else {
        return;
    };
    let Some(meta) = hdr_metadata_from_icc_rgb_xyz_primaries_for_jxl_float(&icc) else {
        return;
    };
    let HdrColorProfile::Cicp {
        color_primaries,
        ..
    } = meta.color_profile
    else {
        return;
    };
    let primaries = match color_primaries {
        1 => libjxl_sys::JXL_PRIMARIES_SRGB,
        9 => libjxl_sys::JXL_PRIMARIES_2100,
        11 => libjxl_sys::JXL_PRIMARIES_P3,
        _ => return,
    };

    let enc = libjxl_sys::JxlColorEncoding {
        color_space: libjxl_sys::JXL_COLOR_SPACE_RGB,
        white_point: libjxl_sys::JXL_WHITE_POINT_D65,
        white_point_xy: [0.0, 0.0],
        primaries,
        primaries_red_xy: [0.0, 0.0],
        primaries_green_xy: [0.0, 0.0],
        primaries_blue_xy: [0.0, 0.0],
        transfer_function: libjxl_sys::JXL_TRANSFER_FUNCTION_LINEAR,
        gamma: 0.0,
        rendering_intent: libjxl_sys::JXL_RENDERING_INTENT_RELATIVE,
    };

    let st = unsafe { libjxl_sys::JxlDecoderSetPreferredColorProfile(decoder, &enc) };
    if st != libjxl_sys::JXL_DEC_SUCCESS {
        log::debug!(
            "JxlDecoderSetPreferredColorProfile returned {st} (decoder may use its default XYB output)"
        );
    }
}

#[cfg(feature = "jpegxl")]
fn read_jxl_metadata(
    decoder: *const libjxl_sys::JxlDecoder,
    mut metadata: HdrImageMetadata,
) -> HdrImageMetadata {
    let saved_luminance = metadata.luminance;

    // 1) ENUM profile of **decoded pixels** (`JXL_COLOR_PROFILE_TARGET_DATA`) — when libjxl
    // can express the float buffer's encoding as a `JxlColorEncoding`, this is the most
    // accurate signal of what's actually in the buffer. For Modular-mode files libjxl
    // preserves the codestream encoding (TF=Linear → linear floats; TF=sRGB → already-
    // encoded floats), so trusting the enum here makes `jxl_sdr_grade_fallback_rgba8`
    // pick the right quantizer instead of always assuming "encoded floats" (which used to
    // break conformance `patches/input.jxl` to ~22 codes too dark across every pixel).
    let mut color_data = std::mem::MaybeUninit::<libjxl_sys::JxlColorEncoding>::zeroed();
    let encoded_data_status = unsafe {
        libjxl_sys::JxlDecoderGetColorAsEncodedProfile(
            decoder,
            libjxl_sys::JXL_COLOR_PROFILE_TARGET_DATA,
            color_data.as_mut_ptr(),
        )
    };
    if encoded_data_status == libjxl_sys::JXL_DEC_SUCCESS {
        let color = unsafe { color_data.assume_init() };
        let mut out = hdr_metadata_from_jxl_float_decode(&color);
        out.luminance = saved_luminance;
        return out;
    }

    // 2) ICC profile of decoded pixels (e.g. conformance `bench_oriented_brg` whose JPEG
    // reconstruction yields an sRGB ICC that libjxl can't express as enum, or
    // `patches_lossless` whose Display P3 linear ICC the same). Walk CICP first, then
    // RGB primary tags (parses `rTRC` to distinguish linear vs encoded ICCs), finally
    // fall back to a minimal "trust the ICC blob" path — that path itself parses `rTRC`
    // so the SDR-grade fallback applies (or skips) the sRGB OETF correctly.
    if let Some(icc) = jxl_decoder_copy_target_data_icc(decoder) {
        if let Some((p, t, m, fr)) = icc_scan_cicp_tag(&icc) {
            let mut out = hdr_metadata_from_h273_cicp_for_jxl_float_buffer(p, t, m, fr);
            out.luminance = saved_luminance;
            return out;
        }
        if let Some(mut out) = hdr_metadata_from_icc_rgb_xyz_primaries_for_jxl_float(&icc) {
            out.luminance = saved_luminance;
            return out;
        }
        let trc = icc_trc_kind(&icc).unwrap_or(HdrTransferFunction::Srgb);
        metadata.color_profile = HdrColorProfile::Icc(Arc::new(icc));
        metadata.transfer_function = trc;
        metadata.reference = HdrReference::Unknown;
        metadata.luminance = saved_luminance;
        return metadata;
    }

    // 3) ENUM profile of the **original** codestream — last resort when neither the
    // decoded enum nor a DATA ICC was exposed. Not strictly interchangeable with DATA but
    // libjxl's Modular path preserves the source encoding.
    let mut color_orig = std::mem::MaybeUninit::<libjxl_sys::JxlColorEncoding>::zeroed();
    let orig_status = unsafe {
        libjxl_sys::JxlDecoderGetColorAsEncodedProfile(
            decoder,
            libjxl_sys::JXL_COLOR_PROFILE_TARGET_ORIGINAL,
            color_orig.as_mut_ptr(),
        )
    };
    if orig_status == libjxl_sys::JXL_DEC_SUCCESS {
        let o = unsafe { color_orig.assume_init() };
        let mut out = hdr_metadata_from_jxl_float_decode(&o);
        out.luminance = saved_luminance;
        return out;
    }

    metadata.luminance = saved_luminance;
    metadata
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "jpegxl")]
    use crate::hdr::jpegxl::read_jxl_gain_map_bundle;
    use crate::hdr::jpegxl::{
        is_jxl_header, jxl_color_encoding_to_metadata, JXL_TRANSFER_FUNCTION_HLG,
        JXL_TRANSFER_FUNCTION_LINEAR, JXL_TRANSFER_FUNCTION_PQ, JXL_TRANSFER_FUNCTION_SRGB,
    };
    use crate::hdr::types::{HdrImageMetadata, HdrReference, HdrTransferFunction};
    #[cfg(feature = "jpegxl")]
    use crate::hdr::types::HdrColorSpace;

    #[test]
    fn jxl_header_detection_accepts_codestream_and_container() {
        assert!(is_jxl_header(&[0xff, 0x0a, 0x00, 0x00]));
        assert!(is_jxl_header(&[
            0x00, 0x00, 0x00, 0x0c, b'J', b'X', b'L', b' ', 0x0d, 0x0a, 0x87, 0x0a,
        ]));
        assert!(!is_jxl_header(b"\x89PNG"));
    }

    #[test]
    fn jxl_pq_metadata_is_display_referred_with_intensity_target() {
        let metadata = jxl_color_encoding_to_metadata(9, JXL_TRANSFER_FUNCTION_PQ, Some(4000.0));

        assert_eq!(metadata.transfer_function, HdrTransferFunction::Pq);
        assert_eq!(metadata.reference, HdrReference::DisplayReferred);
        assert_eq!(metadata.luminance.mastering_max_nits, Some(4000.0));
    }

    #[test]
    fn jxl_linear_transfer_maps_for_float_decoder_output() {
        let metadata = jxl_color_encoding_to_metadata(9, JXL_TRANSFER_FUNCTION_LINEAR, Some(1000.0));

        assert_eq!(metadata.transfer_function, HdrTransferFunction::Linear);
        assert_eq!(metadata.reference, HdrReference::Unknown);
    }

    #[cfg(feature = "jpegxl")]
    #[test]
    fn jxl_transfer_named_consts_match_libjxl_headers() {
        assert_eq!(
            JXL_TRANSFER_FUNCTION_LINEAR,
            libjxl_sys::JXL_TRANSFER_FUNCTION_LINEAR as u16
        );
        assert_eq!(
            JXL_TRANSFER_FUNCTION_SRGB,
            libjxl_sys::JXL_TRANSFER_FUNCTION_SRGB as u16
        );
        assert_eq!(
            JXL_TRANSFER_FUNCTION_PQ,
            libjxl_sys::JXL_TRANSFER_FUNCTION_PQ as u16
        );
        assert_eq!(
            JXL_TRANSFER_FUNCTION_HLG,
            libjxl_sys::JXL_TRANSFER_FUNCTION_HLG as u16
        );
    }

    #[cfg(feature = "jpegxl")]
    #[test]
    fn jxl_float_decode_metadata_maps_custom_bt2020_xy_to_rec2020_hint() {
        let mut color: libjxl_sys::JxlColorEncoding = unsafe { std::mem::zeroed() };
        color.color_space = libjxl_sys::JXL_COLOR_SPACE_RGB;
        color.primaries = libjxl_sys::JXL_PRIMARIES_CUSTOM;
        color.primaries_red_xy = [0.708, 0.292];
        color.primaries_green_xy = [0.17, 0.797];
        color.primaries_blue_xy = [0.131, 0.046];
        color.transfer_function = libjxl_sys::JXL_TRANSFER_FUNCTION_PQ;

        let m = super::hdr_metadata_from_jxl_float_decode(&color);

        assert_eq!(m.color_space_hint(), HdrColorSpace::Rec2020Linear);
        assert_eq!(m.transfer_function, HdrTransferFunction::Pq);
        assert_eq!(m.reference, HdrReference::DisplayReferred);
    }

    #[cfg(feature = "jpegxl")]
    #[test]
    fn jxl_float_decode_metadata_maps_p3_primaries_enum() {
        let mut color: libjxl_sys::JxlColorEncoding = unsafe { std::mem::zeroed() };
        color.color_space = libjxl_sys::JXL_COLOR_SPACE_RGB;
        color.primaries = libjxl_sys::JXL_PRIMARIES_P3;
        color.transfer_function = libjxl_sys::JXL_TRANSFER_FUNCTION_SRGB;

        let m = super::hdr_metadata_from_jxl_float_decode(&color);

        assert_eq!(m.color_space_hint(), HdrColorSpace::DisplayP3Linear);
        assert_eq!(m.transfer_function, HdrTransferFunction::Srgb);
    }

    #[cfg(feature = "jpegxl")]
    #[test]
    fn jxl_sdr_grade_fallback_direct_quantize_for_already_encoded_srgb() {
        // libjxl preserves codestream encoding in the float buffer for Modular
        // mode files: TF=sRGB → already-encoded floats. The fast path quantizes
        // them directly via `value * 255` (no second-pass OETF).
        let rgba = vec![1.0_f32, 0.5, 0.0, 1.0];
        let mut meta = HdrImageMetadata::default();
        meta.transfer_function = HdrTransferFunction::Srgb;
        meta.luminance.mastering_max_nits = Some(255.0);
        let px = super::jxl_sdr_grade_fallback_rgba8(&rgba, HdrColorSpace::LinearSrgb, &meta)
            .expect("sdr-grade content must use direct sRGB encode");
        assert_eq!(px[0], 255, "1.0 → 255, got {}", px[0]);
        assert!(
            (px[1] as i32 - 128).abs() <= 1,
            "0.5 → ~128 (direct quantize, no second-pass OETF), got {}",
            px[1]
        );
        assert_eq!(px[2], 0);
        assert_eq!(px[3], 255);
    }

    #[cfg(feature = "jpegxl")]
    #[test]
    fn jxl_sdr_grade_fallback_applies_srgb_oetf_for_truly_linear_floats() {
        // For TF=Linear codestream sources (e.g. conformance `patches/input.jxl`)
        // libjxl emits truly linear floats. The fast path must apply the sRGB
        // OETF before quantizing or shadows quantize ~22 codes too dark.
        // Linear 0.5 → encoded ~0.735 → ~187 in 8-bit (not 128).
        let rgba = vec![1.0_f32, 0.5, 0.0, 1.0];
        let mut meta = HdrImageMetadata::default();
        meta.transfer_function = HdrTransferFunction::Linear;
        meta.luminance.mastering_max_nits = Some(255.0);
        let px = super::jxl_sdr_grade_fallback_rgba8(&rgba, HdrColorSpace::LinearSrgb, &meta)
            .expect("sdr-grade content must use the OETF + quantize path");
        assert_eq!(px[0], 255, "1.0 → 255, got {}", px[0]);
        assert!(
            (px[1] as i32 - 188).abs() <= 1,
            "linear 0.5 → encoded ~188 (sRGB OETF), got {}",
            px[1]
        );
        assert_eq!(px[2], 0);
        assert_eq!(px[3], 255);
    }

    #[cfg(feature = "jpegxl")]
    #[test]
    fn jxl_sdr_grade_fallback_skipped_for_high_peak_hdr() {
        let rgba = vec![1.0_f32, 1.0, 1.0, 1.0];
        let mut meta = HdrImageMetadata::default();
        meta.transfer_function = HdrTransferFunction::Srgb;
        meta.luminance.mastering_max_nits = Some(1000.0);
        assert!(
            super::jxl_sdr_grade_fallback_rgba8(&rgba, HdrColorSpace::LinearSrgb, &meta).is_none(),
            "HDR (peak > 255 nits) must keep the tone-mapped path"
        );
    }

    #[cfg(feature = "jpegxl")]
    #[test]
    fn conformance_animation_icos4d_input_jxl_decodes_when_sample_present() {
        let path = std::path::Path::new(r"F:\HDR\conformance\testcases\animation_icos4d\input.jxl");
        if !path.is_file() {
            return;
        }
        let bytes = std::fs::read(path).expect("read conformance jxl");
        let tone = crate::hdr::types::HdrToneMapSettings::default();
        let got = super::decode_jxl_bytes_to_image_data(
            &bytes,
            tone.target_hdr_capacity(),
            tone,
        );
        assert!(
            got.is_ok(),
            "decode animation_icos4d: {:?}",
            got.as_ref().err()
        );
        match got.expect("decoded") {
            crate::loader::ImageData::Animated(frames) => {
                assert!(frames.len() > 1, "expected animation, got {} frames", frames.len());
            }
            crate::loader::ImageData::Static(_) => panic!("expected ImageData::Animated, got Static"),
            crate::loader::ImageData::Tiled(_) => panic!("expected ImageData::Animated, got Tiled"),
            crate::loader::ImageData::Hdr { .. } => panic!("expected ImageData::Animated, got Hdr"),
            crate::loader::ImageData::HdrTiled { .. } => {
                panic!("expected ImageData::Animated, got HdrTiled");
            }
        }
    }

    #[cfg(feature = "jpegxl")]
    #[test]
    fn conformance_bench_oriented_brg_input_jxl_color_space_when_sample_present() {
        // libjxl HDR conformance: `bench_oriented_brg/input.jxl` — decoded pixels described by
        // `JXL_COLOR_PROFILE_TARGET_DATA` ICC (BT.709 primaries); see read_jxl_metadata order.
        let path = std::path::Path::new(r"F:\HDR\conformance\testcases\bench_oriented_brg\input.jxl");
        if !path.is_file() {
            return;
        }
        let bytes = std::fs::read(path).expect("read conformance jxl");
        let hdr = super::decode_jxl_hdr_bytes(&bytes).expect("decode conformance jxl");
        assert_eq!(
            hdr.color_space,
            HdrColorSpace::LinearSrgb,
            "expected linear sRGB (BT.709 primaries) for bench_oriented_brg ICC; metadata={:#?}",
            hdr.metadata
        );
        assert_eq!(
            hdr.metadata.transfer_function,
            HdrTransferFunction::Srgb,
            "bench_oriented_brg's libjxl float buffer is sRGB-encoded (the JPEG \
             reconstruction path keeps codestream values intact); the metadata must \
             reflect this so the SDR-grade fallback direct-quantizes (no second-pass OETF)"
        );
    }

    /// Diagnostic: actual libjxl float output range for `bench_oriented_brg/input.jxl`.
    #[cfg(feature = "jpegxl")]
    #[test]
    fn conformance_bench_oriented_brg_float_pixel_range_when_sample_present() {
        let path = std::path::Path::new(r"F:\HDR\conformance\testcases\bench_oriented_brg\input.jxl");
        if !path.is_file() {
            return;
        }
        let bytes = std::fs::read(path).expect("read conformance jxl");
        let hdr = super::decode_jxl_hdr_bytes(&bytes).expect("decode conformance jxl");
        let mut mn = f32::INFINITY;
        let mut mx = f32::NEG_INFINITY;
        let mut sum = 0.0_f64;
        let mut count = 0_usize;
        for px in hdr.rgba_f32.chunks_exact(4) {
            for &c in &px[..3] {
                if c.is_finite() {
                    mn = mn.min(c);
                    mx = mx.max(c);
                    sum += c as f64;
                    count += 1;
                }
            }
        }
        let avg = sum / count.max(1) as f64;
        eprintln!(
            "bench_oriented_brg float RGB: min={mn:.4} max={mx:.4} avg={avg:.4} peak_nits={:?}",
            hdr.metadata.luminance.mastering_max_nits
        );
        assert!(mx.is_finite(), "max should be finite");
    }

    /// SDR fallback must not Reinhard-clamp almost everything to white (non-HDR monitor path).
    #[cfg(feature = "jpegxl")]
    #[test]
    fn conformance_bench_oriented_brg_sdr_fallback_mean_not_washed_when_sample_present() {
        use crate::hdr::types::HdrToneMapSettings;
        let path = std::path::Path::new(r"F:\HDR\conformance\testcases\bench_oriented_brg\input.jxl");
        if !path.is_file() {
            return;
        }
        let bytes = std::fs::read(path).expect("read conformance jxl");
        let tone = HdrToneMapSettings::default();
        let img = super::decode_jxl_bytes_to_image_data(
            &bytes,
            tone.target_hdr_capacity(),
            tone,
        )
        .expect("decode");
        let crate::loader::ImageData::Hdr { fallback, .. } = img else {
            panic!("expected ImageData::Hdr");
        };
        let px = fallback.rgba();
        let mut sum = 0_u64;
        for c in px.chunks_exact(4) {
            sum += u64::from(c[0]) + u64::from(c[1]) + u64::from(c[2]);
        }
        let n = (px.len() / 4) as u64;
        let avg = (sum / (n * 3)) as u32;
        let mut darks = 0_u64;
        for c in px.chunks_exact(4) {
            if u32::from(c[0]) + u32::from(c[1]) + u32::from(c[2]) < 60 {
                darks += 1;
            }
        }
        // Reinhard-on-SDR collapses everything into a 153–178 mid band: mean ~180 and zero darks.
        // A correct sRGB encode keeps the mean lower and preserves shadow detail.
        assert!(
            avg < 200,
            "mean RGB {avg}/255 too high on SDR fallback (Reinhard wash-out)"
        );
        assert!(
            darks > 0,
            "no shadow pixels in SDR fallback ⇒ contrast collapsed"
        );
    }

    /// Pixel-level comparison between our SDR fallback and the conformance `ref.png`. They MUST
    /// match closely (≤ a few code values mean diff, mostly identical channels) — `ref.png` is the
    /// libjxl conformance reference SDR rendering of `input.jxl`. Any larger drift means our
    /// `jxl_sdr_grade_fallback_rgba8` is NOT producing what the reference says.
    #[cfg(feature = "jpegxl")]
    #[test]
    fn conformance_bench_oriented_brg_sdr_fallback_matches_ref_png_when_sample_present() {
        use crate::hdr::types::HdrToneMapSettings;
        let jxl_path =
            std::path::Path::new(r"F:\HDR\conformance\testcases\bench_oriented_brg\input.jxl");
        let ref_path =
            std::path::Path::new(r"F:\HDR\conformance\testcases\bench_oriented_brg\ref.png");
        if !jxl_path.is_file() || !ref_path.is_file() {
            return;
        }
        let bytes = std::fs::read(jxl_path).expect("read conformance jxl");
        let tone = HdrToneMapSettings::default();
        let img = super::decode_jxl_bytes_to_image_data(
            &bytes,
            tone.target_hdr_capacity(),
            tone,
        )
        .expect("decode jxl");
        let crate::loader::ImageData::Hdr {
            fallback,
            hdr,
            ..
        } = img
        else {
            panic!("expected ImageData::Hdr");
        };
        let jxl_w = hdr.width as usize;
        let jxl_h = hdr.height as usize;
        let jxl_bytes = fallback.rgba().to_vec();

        let ref_img = image::open(ref_path).expect("decode ref.png").to_rgba8();
        let ref_w = ref_img.width() as usize;
        let ref_h = ref_img.height() as usize;
        assert_eq!(
            (jxl_w, jxl_h),
            (ref_w, ref_h),
            "ref.png dimensions {ref_w}×{ref_h} must match JXL fallback {jxl_w}×{jxl_h}"
        );
        let ref_bytes = ref_img.into_raw();
        assert_eq!(jxl_bytes.len(), ref_bytes.len());

        let n_pixels = (ref_bytes.len() / 4) as u64;
        let (mut sum_jxl_r, mut sum_jxl_g, mut sum_jxl_b) = (0_u64, 0_u64, 0_u64);
        let (mut sum_ref_r, mut sum_ref_g, mut sum_ref_b) = (0_u64, 0_u64, 0_u64);
        let (mut diff_r, mut diff_g, mut diff_b) = (0_i64, 0_i64, 0_i64);
        let (mut max_diff_r, mut max_diff_g, mut max_diff_b) = (0_u32, 0_u32, 0_u32);
        for (j, r) in jxl_bytes.chunks_exact(4).zip(ref_bytes.chunks_exact(4)) {
            sum_jxl_r += u64::from(j[0]);
            sum_jxl_g += u64::from(j[1]);
            sum_jxl_b += u64::from(j[2]);
            sum_ref_r += u64::from(r[0]);
            sum_ref_g += u64::from(r[1]);
            sum_ref_b += u64::from(r[2]);
            diff_r += i64::from(j[0]) - i64::from(r[0]);
            diff_g += i64::from(j[1]) - i64::from(r[1]);
            diff_b += i64::from(j[2]) - i64::from(r[2]);
            max_diff_r = max_diff_r.max((j[0] as i32 - r[0] as i32).unsigned_abs());
            max_diff_g = max_diff_g.max((j[1] as i32 - r[1] as i32).unsigned_abs());
            max_diff_b = max_diff_b.max((j[2] as i32 - r[2] as i32).unsigned_abs());
        }
        let avg_jxl_r = sum_jxl_r / n_pixels;
        let avg_jxl_g = sum_jxl_g / n_pixels;
        let avg_jxl_b = sum_jxl_b / n_pixels;
        let avg_ref_r = sum_ref_r / n_pixels;
        let avg_ref_g = sum_ref_g / n_pixels;
        let avg_ref_b = sum_ref_b / n_pixels;
        let bias_r = diff_r as f64 / n_pixels as f64;
        let bias_g = diff_g as f64 / n_pixels as f64;
        let bias_b = diff_b as f64 / n_pixels as f64;
        eprintln!(
            "bench_oriented_brg fallback vs ref.png:\n  \
             JXL avg RGB = ({avg_jxl_r}, {avg_jxl_g}, {avg_jxl_b})\n  \
             REF avg RGB = ({avg_ref_r}, {avg_ref_g}, {avg_ref_b})\n  \
             mean signed diff (jxl-ref) = ({bias_r:+.2}, {bias_g:+.2}, {bias_b:+.2})\n  \
             max abs diff = ({max_diff_r}, {max_diff_g}, {max_diff_b})"
        );
        // Tight: the conformance ref is the canonical libjxl decode; if our pipeline drifts more
        // than ~5 code values on average, it's a real bug (and the user sees washing on screen).
        assert!(
            bias_r.abs() < 5.0 && bias_g.abs() < 5.0 && bias_b.abs() < 5.0,
            "SDR fallback drifts from ref.png — bias=({bias_r:+.2}, {bias_g:+.2}, {bias_b:+.2}); \
             check linear/sRGB transfer handling and intensity_target scaling"
        );
    }

    /// Diagnostic: dump basic info + color encoding + extra channels for `cmyk_layers/input.jxl`
    /// so we can see how libjxl describes the source. Symptom: rendered image is missing the
    /// "black" word and shifts greens (lime instead of teal) compared to `ref.png`. Hypothesis:
    /// the source is CMYK (3 color channels + black extra channel) and we drop the K plane when
    /// requesting `JXL_TYPE_FLOAT` RGBA output.
    #[cfg(feature = "jpegxl")]
    #[test]
    fn conformance_cmyk_layers_basic_info_and_channels_when_sample_present() {
        let path = std::path::Path::new(r"F:\HDR\conformance\testcases\cmyk_layers\input.jxl");
        if !path.is_file() {
            return;
        }
        let bytes = std::fs::read(path).expect("read cmyk_layers/input.jxl");

        unsafe {
            let decoder = libjxl_sys::JxlDecoderCreate(std::ptr::null());
            assert!(!decoder.is_null());
            let subscribed = libjxl_sys::JXL_DEC_BASIC_INFO
                | libjxl_sys::JXL_DEC_COLOR_ENCODING
                | libjxl_sys::JXL_DEC_FRAME;
            assert_eq!(
                libjxl_sys::JxlDecoderSubscribeEvents(decoder, subscribed as i32),
                libjxl_sys::JXL_DEC_SUCCESS
            );
            assert_eq!(
                libjxl_sys::JxlDecoderSetInput(decoder, bytes.as_ptr(), bytes.len()),
                libjxl_sys::JXL_DEC_SUCCESS
            );
            libjxl_sys::JxlDecoderCloseInput(decoder);

            let mut info: libjxl_sys::JxlBasicInfo = std::mem::zeroed();
            let mut got_basic = false;
            loop {
                let st = libjxl_sys::JxlDecoderProcessInput(decoder);
                if st == libjxl_sys::JXL_DEC_BASIC_INFO {
                    assert_eq!(
                        libjxl_sys::JxlDecoderGetBasicInfo(decoder, &mut info),
                        libjxl_sys::JXL_DEC_SUCCESS
                    );
                    got_basic = true;
                } else if st == libjxl_sys::JXL_DEC_COLOR_ENCODING {
                    let mut color: libjxl_sys::JxlColorEncoding = std::mem::zeroed();
                    let cs = libjxl_sys::JxlDecoderGetColorAsEncodedProfile(
                        decoder.cast_const(),
                        libjxl_sys::JXL_COLOR_PROFILE_TARGET_ORIGINAL,
                        &mut color,
                    );
                    eprintln!(
                        "TARGET_ORIGINAL color: status={cs} color_space={} primaries={} transfer={} rendering_intent={}",
                        color.color_space, color.primaries, color.transfer_function, color.rendering_intent
                    );
                    let mut color_data: libjxl_sys::JxlColorEncoding = std::mem::zeroed();
                    let ds = libjxl_sys::JxlDecoderGetColorAsEncodedProfile(
                        decoder.cast_const(),
                        libjxl_sys::JXL_COLOR_PROFILE_TARGET_DATA,
                        &mut color_data,
                    );
                    eprintln!(
                        "TARGET_DATA color: status={ds} color_space={} primaries={} transfer={}",
                        color_data.color_space,
                        color_data.primaries,
                        color_data.transfer_function
                    );
                    break;
                } else if st == libjxl_sys::JXL_DEC_ERROR
                    || st == libjxl_sys::JXL_DEC_NEED_MORE_INPUT
                {
                    panic!("libjxl process error/need-more-input: {st}");
                }
            }
            assert!(got_basic);
            eprintln!(
                "BasicInfo: xsize={} ysize={} bits_per_sample={} num_color_channels={} num_extra_channels={} alpha_bits={} have_animation={} intensity_target={}",
                info.xsize,
                info.ysize,
                info.bits_per_sample,
                info.num_color_channels,
                info.num_extra_channels,
                info.alpha_bits,
                info.have_animation,
                info.intensity_target
            );
            for i in 0..info.num_extra_channels {
                let mut ec: libjxl_sys::JxlExtraChannelInfo = std::mem::zeroed();
                let st = libjxl_sys::JxlDecoderGetExtraChannelInfo(
                    decoder.cast_const(),
                    i as usize,
                    &mut ec,
                );
                if st != libjxl_sys::JXL_DEC_SUCCESS {
                    eprintln!("extra channel #{i}: GetExtraChannelInfo status={st}");
                    continue;
                }
                let mut name = vec![0u8; (ec.name_length as usize).max(1) + 1];
                let _ = libjxl_sys::JxlDecoderGetExtraChannelName(
                    decoder.cast_const(),
                    i as usize,
                    name.as_mut_ptr().cast(),
                    name.len(),
                );
                let name = std::ffi::CStr::from_ptr(name.as_ptr().cast())
                    .to_string_lossy()
                    .into_owned();
                eprintln!(
                    "extra channel #{i}: type={} bits_per_sample={} name=\"{}\"",
                    ec.type_, ec.bits_per_sample, name
                );
            }
            libjxl_sys::JxlDecoderDestroy(decoder);
        }
    }

    /// **Validate the lcms2-based CMYK→sRGB path** end-to-end on `cmyk_layers/input.jxl`.
    ///
    /// Per libjxl PR #237, JPEG-recompressed CMYK files require external color management
    /// (4-channel CMYK input → 3-channel sRGB output). libjxl's `JxlDecoderSetOutputColorProfile`
    /// is a no-op for non-XYB sources even with a CMS attached.
    ///
    /// Plumbing:
    ///   1. Decode RGBA float (CMY in RGB slots) + K extra channel (`JXL_CHANNEL_BLACK`).
    ///   2. Build an interleaved CMYK buffer, **inverting** values: libjxl uses
    ///      `0 = max ink, 1 = no ink` (per `cms_interface.h`); lcms2 `TYPE_CMYK_FLT` uses the
    ///      opposite (`0 = no ink, 1 = max ink`).
    ///   3. Apply the embedded CMYK ICC via `cmsCreateTransform(... LCMS_TYPE_CMYK_FLT, sRGB,
    ///      LCMS_TYPE_RGBA_FLT, INTENT_PERCEPTUAL, 0)`. Alpha rides as an "extra" channel.
    ///   4. Quantize to 8-bit and compare against `ref.png` — should match within ~±2 codes
    ///      per channel.
    #[cfg(feature = "jpegxl")]
    #[test]
    fn conformance_cmyk_layers_cms_srgb_output_matches_ref_png_when_sample_present() {
        let jxl_path = std::path::Path::new(r"F:\HDR\conformance\testcases\cmyk_layers\input.jxl");
        let ref_path = std::path::Path::new(r"F:\HDR\conformance\testcases\cmyk_layers\ref.png");
        if !jxl_path.is_file() || !ref_path.is_file() {
            return;
        }
        let bytes = std::fs::read(jxl_path).expect("read cmyk_layers/input.jxl");

        let mut composed: Vec<u8> = Vec::new();
        let mut width = 0_u32;
        let mut height = 0_u32;
        let mut rgba_f32: Vec<f32> = Vec::new();
        let mut k_f32: Vec<f32> = Vec::new();
        let mut source_icc: Vec<u8> = Vec::new();
        unsafe {
            let decoder = libjxl_sys::JxlDecoderCreate(std::ptr::null());
            assert!(!decoder.is_null());

            let subscribed = libjxl_sys::JXL_DEC_BASIC_INFO
                | libjxl_sys::JXL_DEC_COLOR_ENCODING
                | libjxl_sys::JXL_DEC_FRAME
                | libjxl_sys::JXL_DEC_FULL_IMAGE;
            assert_eq!(
                libjxl_sys::JxlDecoderSubscribeEvents(decoder, subscribed as i32),
                libjxl_sys::JXL_DEC_SUCCESS
            );
            assert_eq!(
                libjxl_sys::JxlDecoderSetInput(decoder, bytes.as_ptr(), bytes.len()),
                libjxl_sys::JXL_DEC_SUCCESS
            );
            libjxl_sys::JxlDecoderCloseInput(decoder);

            let pixel_format = libjxl_sys::JxlPixelFormat {
                num_channels: 4,
                data_type: libjxl_sys::JXL_TYPE_FLOAT,
                endianness: libjxl_sys::JXL_NATIVE_ENDIAN,
                align: 0,
            };
            let k_format = libjxl_sys::JxlPixelFormat {
                num_channels: 1,
                ..pixel_format
            };

            let mut info: libjxl_sys::JxlBasicInfo = std::mem::zeroed();
            let mut k_idx = None::<u32>;
            loop {
                let st = libjxl_sys::JxlDecoderProcessInput(decoder);
                if st == libjxl_sys::JXL_DEC_BASIC_INFO {
                    assert_eq!(
                        libjxl_sys::JxlDecoderGetBasicInfo(decoder, &mut info),
                        libjxl_sys::JXL_DEC_SUCCESS
                    );
                    width = info.xsize;
                    height = info.ysize;
                    k_idx = super::jxl_find_black_extra_channel_index(decoder, &info);
                    assert!(k_idx.is_some(), "expected a JXL_CHANNEL_BLACK extra channel");
                } else if st == libjxl_sys::JXL_DEC_COLOR_ENCODING {
                    let mut icc_size = 0_usize;
                    assert_eq!(
                        libjxl_sys::JxlDecoderGetICCProfileSize(
                            decoder.cast_const(),
                            libjxl_sys::JXL_COLOR_PROFILE_TARGET_ORIGINAL,
                            &mut icc_size,
                        ),
                        libjxl_sys::JXL_DEC_SUCCESS
                    );
                    source_icc = vec![0u8; icc_size];
                    assert_eq!(
                        libjxl_sys::JxlDecoderGetColorAsICCProfile(
                            decoder.cast_const(),
                            libjxl_sys::JXL_COLOR_PROFILE_TARGET_ORIGINAL,
                            source_icc.as_mut_ptr(),
                            icc_size,
                        ),
                        libjxl_sys::JXL_DEC_SUCCESS
                    );
                    eprintln!("source CMYK ICC: {} bytes", source_icc.len());
                } else if st == libjxl_sys::JXL_DEC_NEED_IMAGE_OUT_BUFFER {
                    let mut size = 0_usize;
                    assert_eq!(
                        libjxl_sys::JxlDecoderImageOutBufferSize(
                            decoder.cast_const(),
                            &pixel_format,
                            &mut size
                        ),
                        libjxl_sys::JXL_DEC_SUCCESS
                    );
                    rgba_f32.resize(size / std::mem::size_of::<f32>(), 0.0);
                    assert_eq!(
                        libjxl_sys::JxlDecoderSetImageOutBuffer(
                            decoder,
                            &pixel_format,
                            rgba_f32.as_mut_ptr().cast(),
                            size
                        ),
                        libjxl_sys::JXL_DEC_SUCCESS
                    );
                    let idx = k_idx.expect("k channel index");
                    let mut k_size = 0_usize;
                    assert_eq!(
                        libjxl_sys::JxlDecoderExtraChannelBufferSize(
                            decoder.cast_const(),
                            &k_format,
                            &mut k_size,
                            idx,
                        ),
                        libjxl_sys::JXL_DEC_SUCCESS
                    );
                    k_f32.resize(k_size / std::mem::size_of::<f32>(), 0.0);
                    assert_eq!(
                        libjxl_sys::JxlDecoderSetExtraChannelBuffer(
                            decoder,
                            &k_format,
                            k_f32.as_mut_ptr().cast(),
                            k_size,
                            idx,
                        ),
                        libjxl_sys::JXL_DEC_SUCCESS
                    );
                } else if st == libjxl_sys::JXL_DEC_FULL_IMAGE {
                    break;
                } else if st == libjxl_sys::JXL_DEC_ERROR
                    || st == libjxl_sys::JXL_DEC_NEED_MORE_INPUT
                {
                    panic!("libjxl process error/need-more-input: {st}");
                }
            }
            libjxl_sys::JxlDecoderDestroy(decoder);
        }

        // Build CMYK input following libjxl's `enc_color_management.cc` LCMS path
        // (the "0=white, 100=max ink" comment + `100 - 100 * v` line). lcms2's
        // `TYPE_CMYK_FLT` is encoded in **PostScript percent units** (0..100),
        // and libjxl's RGBA float output uses `0=max ink, 1=white` for CMYK
        // sources. Channel order is (C, M, Y) from RGB slots + K from the
        // BLACK extra channel (matching `CopyToT` in `enc_image_bundle.cc`).
        let n_pixels = (rgba_f32.len() / 4) as u32;
        assert_eq!(n_pixels as usize, k_f32.len());
        let mut cmyk_input = Vec::<f32>::with_capacity(n_pixels as usize * 4);
        let mut alpha = Vec::<f32>::with_capacity(n_pixels as usize);
        for (px, &k) in rgba_f32.chunks_exact(4).zip(k_f32.iter()) {
            cmyk_input.push(100.0 - 100.0 * px[0].clamp(0.0, 1.0));
            cmyk_input.push(100.0 - 100.0 * px[1].clamp(0.0, 1.0));
            cmyk_input.push(100.0 - 100.0 * px[2].clamp(0.0, 1.0));
            cmyk_input.push(100.0 - 100.0 * k.clamp(0.0, 1.0));
            alpha.push(px[3]);
        }

        let mut rgba_out = vec![0.0_f32; n_pixels as usize * 4];
        unsafe {
            let in_profile = libjxl_sys::cmsOpenProfileFromMem(
                source_icc.as_ptr().cast(),
                source_icc.len() as u32,
            );
            assert!(!in_profile.is_null(), "lcms could not parse CMYK ICC");
            let out_profile = libjxl_sys::cmsCreate_sRGBProfile();
            assert!(!out_profile.is_null(), "lcms could not build sRGB profile");
            // djxl converts CMYK→sRGB with the destination's rendering intent.
            // For its `ColorEncoding::SRGB(false)` target the default intent is
            // perceptual (matches `INTENT_PERCEPTUAL = 0`).
            let transform = libjxl_sys::cmsCreateTransform(
                in_profile,
                libjxl_sys::LCMS_TYPE_CMYK_FLT,
                out_profile,
                libjxl_sys::LCMS_TYPE_RGBA_FLT,
                libjxl_sys::LCMS_INTENT_PERCEPTUAL,
                0,
            );
            assert!(!transform.is_null(), "lcms could not build CMYK→sRGB transform");
            libjxl_sys::cmsDoTransform(
                transform,
                cmyk_input.as_ptr().cast(),
                rgba_out.as_mut_ptr().cast(),
                n_pixels,
            );
            libjxl_sys::cmsDeleteTransform(transform);
            libjxl_sys::cmsCloseProfile(in_profile);
            libjxl_sys::cmsCloseProfile(out_profile);
        }

        composed.reserve(n_pixels as usize * 4);
        for (i, px) in rgba_out.chunks_exact(4).enumerate() {
            composed.push(super::srgb_unit_to_u8(px[0]));
            composed.push(super::srgb_unit_to_u8(px[1]));
            composed.push(super::srgb_unit_to_u8(px[2]));
            composed.push(super::srgb_unit_to_u8(alpha[i]));
        }

        let ref_img = image::open(ref_path).expect("decode ref.png").to_rgba8();
        let ref_bytes_for_diag = ref_img.clone().into_raw();
        let pick = |bytes: &[u8], x: u32, y: u32| {
            let i = (y as usize * width as usize + x as usize) * 4;
            (bytes[i], bytes[i + 1], bytes[i + 2])
        };
        eprintln!(
            "lcms diagnostic samples (jxl vs ref.png):\n  black-area(135,14): jxl={:?} ref={:?}\n  bg(256,225):       jxl={:?} ref={:?}\n  bg(220,360):       jxl={:?} ref={:?}",
            pick(&composed, 135, 14),
            pick(&ref_bytes_for_diag, 135, 14),
            pick(&composed, 256, 225),
            pick(&ref_bytes_for_diag, 256, 225),
            pick(&composed, 220, 360),
            pick(&ref_bytes_for_diag, 220, 360),
        );

        assert_eq!((width, height), (ref_img.width(), ref_img.height()));
        let ref_bytes = ref_img.into_raw();
        assert_eq!(composed.len(), ref_bytes.len());
        let n = (composed.len() / 4) as i64;
        let (mut diff_r, mut diff_g, mut diff_b) = (0_i64, 0_i64, 0_i64);
        let (mut max_r, mut max_g, mut max_b) = (0_u32, 0_u32, 0_u32);
        for (j, r) in composed.chunks_exact(4).zip(ref_bytes.chunks_exact(4)) {
            diff_r += i64::from(j[0]) - i64::from(r[0]);
            diff_g += i64::from(j[1]) - i64::from(r[1]);
            diff_b += i64::from(j[2]) - i64::from(r[2]);
            max_r = max_r.max((j[0] as i32 - r[0] as i32).unsigned_abs());
            max_g = max_g.max((j[1] as i32 - r[1] as i32).unsigned_abs());
            max_b = max_b.max((j[2] as i32 - r[2] as i32).unsigned_abs());
        }
        let bias_r = diff_r as f64 / n as f64;
        let bias_g = diff_g as f64 / n as f64;
        let bias_b = diff_b as f64 / n as f64;
        eprintln!(
            "cmyk_layers (lcms2 CMYK→sRGB) vs ref.png:\n  mean signed diff = ({bias_r:+.2}, {bias_g:+.2}, {bias_b:+.2})\n  max abs diff = ({max_r}, {max_g}, {max_b})"
        );
        // ref.png was rendered by djxl with skcms; we use lcms2. Both should
        // produce the same colorimetric transform; small (<5 codes) bias is
        // tolerable due to differences in profile-internal LUT interpolation
        // and intent handling between the two CMSes.
        assert!(
            bias_r.abs() < 5.0 && bias_g.abs() < 5.0 && bias_b.abs() < 5.0,
            "lcms2 CMYK→sRGB drifts too far from ref.png: bias=({bias_r:+.2}, {bias_g:+.2}, {bias_b:+.2})"
        );
    }

    /// Historical diagnostic: dumps libjxl's CMYK output as raw RGB plus a few
    /// hand-rolled compositing models (`R*K`, `R*(1-K)`, `min(R,K)`, etc.) and
    /// reports the per-channel pixel diff against the conformance ref.png.
    /// All such models are wrong without proper ICC-managed CMYK→sRGB
    /// conversion (see PR #237 in libjxl). We retain the test as a debugging
    /// aid — it documents how the old "guess the formula" approach misbehaves
    /// across ink mixes — but the real fix lives in
    /// `apply_cmyk_to_srgb_via_lcms`.
    #[cfg(feature = "jpegxl")]
    #[test]
    fn conformance_cmyk_layers_naive_composition_models_are_all_wrong_when_sample_present() {
        let jxl_path = std::path::Path::new(r"F:\HDR\conformance\testcases\cmyk_layers\input.jxl");
        let ref_path = std::path::Path::new(r"F:\HDR\conformance\testcases\cmyk_layers\ref.png");
        if !jxl_path.is_file() || !ref_path.is_file() {
            return;
        }
        let bytes = std::fs::read(jxl_path).expect("read cmyk_layers/input.jxl");

        unsafe {
            let decoder = libjxl_sys::JxlDecoderCreate(std::ptr::null());
            assert!(!decoder.is_null());
            let subscribed = libjxl_sys::JXL_DEC_BASIC_INFO
                | libjxl_sys::JXL_DEC_COLOR_ENCODING
                | libjxl_sys::JXL_DEC_FRAME
                | libjxl_sys::JXL_DEC_FULL_IMAGE;
            assert_eq!(
                libjxl_sys::JxlDecoderSubscribeEvents(decoder, subscribed as i32),
                libjxl_sys::JXL_DEC_SUCCESS
            );
            assert_eq!(
                libjxl_sys::JxlDecoderSetInput(decoder, bytes.as_ptr(), bytes.len()),
                libjxl_sys::JXL_DEC_SUCCESS
            );
            libjxl_sys::JxlDecoderCloseInput(decoder);

            let pixel_format = libjxl_sys::JxlPixelFormat {
                num_channels: 4,
                data_type: libjxl_sys::JXL_TYPE_FLOAT,
                endianness: libjxl_sys::JXL_NATIVE_ENDIAN,
                align: 0,
            };
            let k_format = libjxl_sys::JxlPixelFormat {
                num_channels: 1,
                ..pixel_format
            };

            let mut info: libjxl_sys::JxlBasicInfo = std::mem::zeroed();
            let mut rgba_f32: Vec<f32> = Vec::new();
            let mut k_f32: Vec<f32> = Vec::new();
            loop {
                let st = libjxl_sys::JxlDecoderProcessInput(decoder);
                if st == libjxl_sys::JXL_DEC_BASIC_INFO {
                    assert_eq!(
                        libjxl_sys::JxlDecoderGetBasicInfo(decoder, &mut info),
                        libjxl_sys::JXL_DEC_SUCCESS
                    );
                } else if st == libjxl_sys::JXL_DEC_NEED_IMAGE_OUT_BUFFER {
                    let mut size = 0_usize;
                    assert_eq!(
                        libjxl_sys::JxlDecoderImageOutBufferSize(
                            decoder.cast_const(),
                            &pixel_format,
                            &mut size
                        ),
                        libjxl_sys::JXL_DEC_SUCCESS
                    );
                    rgba_f32.resize(size / std::mem::size_of::<f32>(), 0.0);
                    assert_eq!(
                        libjxl_sys::JxlDecoderSetImageOutBuffer(
                            decoder,
                            &pixel_format,
                            rgba_f32.as_mut_ptr().cast(),
                            size
                        ),
                        libjxl_sys::JXL_DEC_SUCCESS
                    );
                    // Channel 0 is type=BLACK (per the diagnostic above).
                    let mut k_size = 0_usize;
                    assert_eq!(
                        libjxl_sys::JxlDecoderExtraChannelBufferSize(
                            decoder.cast_const(),
                            &k_format,
                            &mut k_size,
                            0
                        ),
                        libjxl_sys::JXL_DEC_SUCCESS
                    );
                    k_f32.resize(k_size / std::mem::size_of::<f32>(), 0.0);
                    assert_eq!(
                        libjxl_sys::JxlDecoderSetExtraChannelBuffer(
                            decoder,
                            &k_format,
                            k_f32.as_mut_ptr().cast(),
                            k_size,
                            0
                        ),
                        libjxl_sys::JXL_DEC_SUCCESS
                    );
                } else if st == libjxl_sys::JXL_DEC_FULL_IMAGE {
                    break;
                } else if st == libjxl_sys::JXL_DEC_ERROR
                    || st == libjxl_sys::JXL_DEC_NEED_MORE_INPUT
                {
                    panic!("libjxl process error/need-more-input: {st}");
                }
            }
            libjxl_sys::JxlDecoderDestroy(decoder);

            let n = (rgba_f32.len() / 4) as u64;
            let denom = n.max(1) as f64;

            // K stats — is it "0=no ink, 1=full ink" or "0=black, 1=white" (visible intensity)?
            let (mut k_min, mut k_max, mut k_sum) = (1.0_f32, 0.0_f32, 0.0_f64);
            for &k in &k_f32 {
                k_min = k_min.min(k);
                k_max = k_max.max(k);
                k_sum += k as f64;
            }
            eprintln!(
                "K channel: min={k_min:.4} max={k_max:.4} mean={:.4}",
                k_sum / denom
            );

            // Sample a few specific pixels at known regions: top of image (the "black" word
            // region) and middle (the "Background" green text region).
            let pick = |x: u32, y: u32| {
                let idx = (y * info.xsize + x) as usize;
                (
                    rgba_f32[idx * 4],
                    rgba_f32[idx * 4 + 1],
                    rgba_f32[idx * 4 + 2],
                    rgba_f32[idx * 4 + 3],
                    k_f32[idx],
                )
            };
            // Approximate text positions on a 512×512 conformance test card.
            for (label, x, y) in [
                ("top-center  ", 256, 75),
                ("background  ", 256, 200),
                ("layer1      ", 256, 256),
                ("test-name   ", 256, 380),
                ("white-bg    ", 50, 50),
            ] {
                let (r, g, b, a, k) = pick(x, y);
                eprintln!(
                    "px ({label}) ({x:3}, {y:3}): R={r:.3} G={g:.3} B={b:.3} A={a:.3} K={k:.3}"
                );
            }

            // Try several compositing models and report mean diff to ref.png.
            let ref_img = image::open(ref_path).expect("decode ref.png").to_rgba8();
            assert_eq!((ref_img.width(), ref_img.height()), (info.xsize, info.ysize));
            let ref_bytes = ref_img.into_raw();

            let try_compose = |name: &str, compose: fn(f32, f32, f32, f32) -> [f32; 3]| {
                let mut diff_r = 0_i64;
                let mut diff_g = 0_i64;
                let mut diff_b = 0_i64;
                let (mut max_r, mut max_g, mut max_b) = (0_u32, 0_u32, 0_u32);
                for (i, (px, k)) in rgba_f32.chunks_exact(4).zip(k_f32.iter()).enumerate() {
                    let [r, g, b] = compose(px[0], px[1], px[2], *k);
                    let r_u = super::srgb_unit_to_u8(r) as i32;
                    let g_u = super::srgb_unit_to_u8(g) as i32;
                    let b_u = super::srgb_unit_to_u8(b) as i32;
                    let ref_r = ref_bytes[i * 4] as i32;
                    let ref_g = ref_bytes[i * 4 + 1] as i32;
                    let ref_b = ref_bytes[i * 4 + 2] as i32;
                    diff_r += (r_u - ref_r) as i64;
                    diff_g += (g_u - ref_g) as i64;
                    diff_b += (b_u - ref_b) as i64;
                    max_r = max_r.max((r_u - ref_r).unsigned_abs());
                    max_g = max_g.max((g_u - ref_g).unsigned_abs());
                    max_b = max_b.max((b_u - ref_b).unsigned_abs());
                }
                eprintln!(
                    "{name}: bias=({:+.2}, {:+.2}, {:+.2}) max=({max_r}, {max_g}, {max_b})",
                    diff_r as f64 / n as f64,
                    diff_g as f64 / n as f64,
                    diff_b as f64 / n as f64
                );
            };

            try_compose("RGB                    ", |r, g, b, _k| [r, g, b]);
            try_compose("RGB * (1 - K)          ", |r, g, b, k| {
                [r * (1.0 - k), g * (1.0 - k), b * (1.0 - k)]
            });
            try_compose("RGB * K                ", |r, g, b, k| [r * k, g * k, b * k]);
            try_compose("min(RGB, K)            ", |r, g, b, k| {
                [r.min(k), g.min(k), b.min(k)]
            });
            try_compose("RGB - (1 - K)          ", |r, g, b, k| {
                [(r - (1.0 - k)).max(0.0), (g - (1.0 - k)).max(0.0), (b - (1.0 - k)).max(0.0)]
            });

            // Find the 5 worst-mismatch pixels using the raw RGB output, dump (x, y, JXL, K, ref).
            let mut diffs: Vec<(u32, i64)> = (0..n as u32)
                .map(|i| {
                    let j = i as usize;
                    let dr = (super::srgb_unit_to_u8(rgba_f32[j * 4]) as i32
                        - ref_bytes[j * 4] as i32)
                        .abs();
                    let dg = (super::srgb_unit_to_u8(rgba_f32[j * 4 + 1]) as i32
                        - ref_bytes[j * 4 + 1] as i32)
                        .abs();
                    let db = (super::srgb_unit_to_u8(rgba_f32[j * 4 + 2]) as i32
                        - ref_bytes[j * 4 + 2] as i32)
                        .abs();
                    (i, (dr + dg + db) as i64)
                })
                .collect();
            diffs.sort_by_key(|(_, d)| std::cmp::Reverse(*d));
            eprintln!("--- top 8 worst-mismatch pixels (raw RGB vs ref.png) ---");
            for &(i, d) in diffs.iter().take(8) {
                let x = i % info.xsize;
                let y = i / info.xsize;
                let j = i as usize;
                let r = rgba_f32[j * 4];
                let g = rgba_f32[j * 4 + 1];
                let b = rgba_f32[j * 4 + 2];
                let a = rgba_f32[j * 4 + 3];
                let k = k_f32[j];
                let rr = ref_bytes[j * 4];
                let rg = ref_bytes[j * 4 + 1];
                let rb = ref_bytes[j * 4 + 2];
                let ra = ref_bytes[j * 4 + 3];
                eprintln!(
                    "({x:3},{y:3}) sum_diff={d:3}: \
                     JXL(R={r:.3} G={g:.3} B={b:.3} A={a:.3} K={k:.3}) \
                     ref(R={rr} G={rg} B={rb} A={ra})"
                );
            }
        }
    }

    /// End-to-end regression: the live decode pipeline now applies the embedded
    /// CMYK ICC profile through lcms2 (`apply_cmyk_to_srgb_via_lcms`) when a
    /// `JXL_CHANNEL_BLACK` extra channel is present. The resulting SDR fallback
    /// for `cmyk_layers/input.jxl` must reproduce the conformance `ref.png`
    /// (which djxl rendered with the same CMS pipeline) to within ~5 code
    /// values mean signed diff. Without ICC-managed conversion the K plane is
    /// dropped (missing "black" word) and process colors render flat (lime
    /// instead of teal background).
    #[cfg(feature = "jpegxl")]
    #[test]
    fn conformance_cmyk_layers_sdr_fallback_matches_ref_png_when_sample_present() {
        use crate::hdr::types::HdrToneMapSettings;
        let jxl_path = std::path::Path::new(r"F:\HDR\conformance\testcases\cmyk_layers\input.jxl");
        let ref_path = std::path::Path::new(r"F:\HDR\conformance\testcases\cmyk_layers\ref.png");
        if !jxl_path.is_file() || !ref_path.is_file() {
            return;
        }
        let bytes = std::fs::read(jxl_path).expect("read cmyk_layers/input.jxl");
        let tone = HdrToneMapSettings::default();
        let img =
            super::decode_jxl_bytes_to_image_data(&bytes, tone.target_hdr_capacity(), tone)
                .expect("decode cmyk_layers");
        let crate::loader::ImageData::Hdr { fallback, hdr, .. } = img else {
            panic!("expected ImageData::Hdr");
        };
        let jxl_bytes = fallback.rgba().to_vec();
        let ref_img = image::open(ref_path).expect("decode ref.png").to_rgba8();
        assert_eq!(
            (hdr.width, hdr.height),
            (ref_img.width(), ref_img.height()),
            "ref.png dimensions must match cmyk_layers JXL"
        );
        let ref_bytes = ref_img.into_raw();
        assert_eq!(jxl_bytes.len(), ref_bytes.len());
        let n = (jxl_bytes.len() / 4) as i64;
        let (mut diff_r, mut diff_g, mut diff_b) = (0_i64, 0_i64, 0_i64);
        let (mut max_r, mut max_g, mut max_b) = (0_u32, 0_u32, 0_u32);
        for (j, r) in jxl_bytes.chunks_exact(4).zip(ref_bytes.chunks_exact(4)) {
            diff_r += i64::from(j[0]) - i64::from(r[0]);
            diff_g += i64::from(j[1]) - i64::from(r[1]);
            diff_b += i64::from(j[2]) - i64::from(r[2]);
            max_r = max_r.max((j[0] as i32 - r[0] as i32).unsigned_abs());
            max_g = max_g.max((j[1] as i32 - r[1] as i32).unsigned_abs());
            max_b = max_b.max((j[2] as i32 - r[2] as i32).unsigned_abs());
        }
        let bias_r = diff_r as f64 / n as f64;
        let bias_g = diff_g as f64 / n as f64;
        let bias_b = diff_b as f64 / n as f64;
        eprintln!(
            "cmyk_layers fallback vs ref.png:\n  mean signed diff = ({bias_r:+.2}, {bias_g:+.2}, {bias_b:+.2})\n  max abs diff = ({max_r}, {max_g}, {max_b})"
        );
        assert!(
            bias_r.abs() < 5.0 && bias_g.abs() < 5.0 && bias_b.abs() < 5.0,
            "lcms2 CMYK→sRGB SDR fallback bias too large: ({bias_r:+.2}, {bias_g:+.2}, {bias_b:+.2}) — \
             check JxlDecoderSetExtraChannelBuffer wiring + jxl_decoder_copy_target_original_icc + \
             apply_cmyk_to_srgb_via_lcms (libjxl CMYK convention 0=max ink, lcms2 0=no ink in 0..100)"
        );
    }

    #[cfg(feature = "jpegxl")]
    #[test]
    fn jxl_gain_map_bundle_rejects_malformed_payload() {
        let err = read_jxl_gain_map_bundle(&[0, 0, 1, 0]).expect_err("reject malformed jhgm");

        assert!(err.contains("jhgm"));
    }

    #[cfg(feature = "jpegxl")]
    #[test]
    fn jxl_gain_map_bundle_parses_metadata_and_embedded_codestream() {
        let metadata = [1_u8, 2, 3];
        let gain_map = [0xff_u8, 0x0a, 0x55];
        let mut bundle = Vec::new();
        bundle.push(0);
        bundle.extend_from_slice(&(metadata.len() as u16).to_be_bytes());
        bundle.extend_from_slice(&metadata);
        bundle.push(0); // no compressed color encoding
        bundle.extend_from_slice(&0_u32.to_be_bytes()); // no compressed ICC
        bundle.extend_from_slice(&gain_map);

        let parsed = read_jxl_gain_map_bundle(&bundle).expect("parse jhgm");

        assert_eq!(parsed.version, 0);
        assert_eq!(parsed.metadata, metadata);
        assert_eq!(parsed.gain_map, gain_map);
    }

    /// Conformance regression: `patches/input.jxl` ships TF=Linear in the
    /// codestream (libjxl emits truly linear floats). Before fixing the SDR-
    /// grade fallback to honor the actual TF reported by
    /// `JxlDecoderGetColorAsEncodedProfile`, our renderer treated those linear
    /// floats as if they were sRGB-encoded and quantized them directly,
    /// producing a uniformly ~22-code darker image (mean signed diff
    /// (-19.76, -22.22, -25.93), max diff 75) and effectively losing the gray
    /// table grid lines that should be visible. After the fix every pixel
    /// matches `ref.png` to within ≤3 codes.
    #[cfg(feature = "jpegxl")]
    #[test]
    fn conformance_patches_sdr_fallback_matches_ref_png_when_sample_present() {
        use crate::hdr::types::HdrToneMapSettings;
        let jxl_path = std::path::Path::new(r"F:\HDR\conformance\testcases\patches\input.jxl");
        let ref_path = std::path::Path::new(r"F:\HDR\conformance\testcases\patches\ref.png");
        if !jxl_path.is_file() || !ref_path.is_file() {
            return;
        }
        let bytes = std::fs::read(jxl_path).expect("read jxl");
        let img = super::decode_jxl_bytes_to_image_data(
            &bytes,
            HdrToneMapSettings::default().target_hdr_capacity(),
            HdrToneMapSettings::default(),
        )
        .expect("decode jxl");
        let crate::loader::ImageData::Hdr { fallback, hdr } = img else {
            panic!("unexpected ImageData variant");
        };
        assert_eq!(
            hdr.metadata.transfer_function,
            HdrTransferFunction::Linear,
            "patches.jxl ships TF=Linear in the codestream — read_jxl_metadata must surface that"
        );
        let ours = fallback.rgba();
        let ref_img = image::open(ref_path).expect("decode ref.png").to_rgba8();
        assert_eq!(
            (hdr.width, hdr.height),
            (ref_img.width(), ref_img.height())
        );
        let r = ref_img.into_raw();
        let n = (ours.len() / 4) as i64;
        let (mut sr, mut sg, mut sb) = (0_i64, 0_i64, 0_i64);
        let (mut mr, mut mg, mut mb) = (0_u32, 0_u32, 0_u32);
        for (j, p) in ours.chunks_exact(4).zip(r.chunks_exact(4)) {
            let dr = j[0] as i32 - p[0] as i32;
            let dg = j[1] as i32 - p[1] as i32;
            let db = j[2] as i32 - p[2] as i32;
            sr += dr as i64;
            sg += dg as i64;
            sb += db as i64;
            mr = mr.max(dr.unsigned_abs());
            mg = mg.max(dg.unsigned_abs());
            mb = mb.max(db.unsigned_abs());
        }
        let bias = (
            sr as f64 / n as f64,
            sg as f64 / n as f64,
            sb as f64 / n as f64,
        );
        assert!(
            bias.0.abs() < 2.0 && bias.1.abs() < 2.0 && bias.2.abs() < 2.0,
            "mean signed diff vs ref.png must stay within ±2 codes (was -19.76 / -22.22 / -25.93 \
             before treating TF=Linear codestream as truly linear); got {bias:?}"
        );
        assert!(
            mr <= 5 && mg <= 5 && mb <= 5,
            "max abs diff vs ref.png must stay within 5 codes (was 75 / 74 / 73 before the fix); \
             got ({mr}, {mg}, {mb})"
        );
    }

    /// Conformance regression: `patches_lossless/input.jxl`. Distinct from
    /// `patches/input.jxl` in that the lossless variant ships a 2924-byte
    /// **embedded ICC profile** (Display P3 primaries with a *linear* `rTRC`)
    /// that libjxl can't represent as a `JxlColorEncoding` enum
    /// (`JxlDecoderGetColorAsEncodedProfile` returns `JXL_DEC_ERROR` here).
    /// libjxl emits truly linear floats per the codestream, so the SDR-grade
    /// fallback must apply the sRGB OETF before quantizing. Before parsing
    /// `rTRC`, we'd guess "non-sRGB primaries → PQ" for any non-sRGB ICC and
    /// route the linear floats through the HDR tone-mapping pipeline, which
    /// produced a uniformly ~16-code darker image (mean diff
    /// (-14.26, -16.16, -19.21), max 51) and washed out the gray table grid
    /// lines plus cell background grays.
    #[cfg(feature = "jpegxl")]
    #[test]
    fn conformance_patches_lossless_sdr_fallback_matches_ref_png_when_sample_present() {
        use crate::hdr::types::HdrToneMapSettings;
        let jxl_path = std::path::Path::new(r"F:\HDR\conformance\testcases\patches_lossless\input.jxl");
        let ref_path = std::path::Path::new(r"F:\HDR\conformance\testcases\patches_lossless\ref.png");
        if !jxl_path.is_file() || !ref_path.is_file() {
            return;
        }
        let bytes = std::fs::read(jxl_path).expect("read jxl");
        let img = super::decode_jxl_bytes_to_image_data(
            &bytes,
            HdrToneMapSettings::default().target_hdr_capacity(),
            HdrToneMapSettings::default(),
        )
        .expect("decode jxl");
        let crate::loader::ImageData::Hdr { fallback, hdr } = img else {
            panic!("unexpected ImageData variant");
        };
        // The metadata TF is whatever the rTRC parser decides — it can be
        // `Srgb` (piecewise / parametric / LUT curve) or `Linear` (count=0 or
        // count=1@1.0). Either way the SDR-grade fallback must produce bytes
        // that match ref.png; the previous bug was routing through the HDR
        // tone-mapping pipeline (because the old code guessed PQ for any non-
        // sRGB primary ICC).
        assert!(
            !matches!(
                hdr.metadata.transfer_function,
                HdrTransferFunction::Pq | HdrTransferFunction::Hlg
            ),
            "patches_lossless is SDR — must not route through the HDR pipeline; \
             got transfer_function={:?}",
            hdr.metadata.transfer_function
        );
        let ours = fallback.rgba();
        let ref_img = image::open(ref_path).expect("decode ref.png").to_rgba8();
        assert_eq!(
            (hdr.width, hdr.height),
            (ref_img.width(), ref_img.height())
        );
        let r = ref_img.into_raw();
        let n = (ours.len() / 4) as i64;
        let (mut sr, mut sg, mut sb) = (0_i64, 0_i64, 0_i64);
        let (mut mr, mut mg, mut mb) = (0_u32, 0_u32, 0_u32);
        for (j, p) in ours.chunks_exact(4).zip(r.chunks_exact(4)) {
            let dr = j[0] as i32 - p[0] as i32;
            let dg = j[1] as i32 - p[1] as i32;
            let db = j[2] as i32 - p[2] as i32;
            sr += dr as i64;
            sg += dg as i64;
            sb += db as i64;
            mr = mr.max(dr.unsigned_abs());
            mg = mg.max(dg.unsigned_abs());
            mb = mb.max(db.unsigned_abs());
        }
        let bias = (
            sr as f64 / n as f64,
            sg as f64 / n as f64,
            sb as f64 / n as f64,
        );
        assert!(
            bias.0.abs() < 2.0 && bias.1.abs() < 2.0 && bias.2.abs() < 2.0,
            "mean signed diff vs ref.png must stay within ±2 codes (was -14.26 / -16.16 / -19.21 \
             before parsing rTRC); got {bias:?}"
        );
        assert!(
            mr <= 5 && mg <= 5 && mb <= 5,
            "max abs diff vs ref.png must stay within 5 codes (was 51 / 49 / 49 before the fix); \
             got ({mr}, {mg}, {mb})"
        );
    }

    /// Unit coverage for the ICC `rTRC` classifier: synthetic `curveType`
    /// payloads exercise the linear (count=0, count=1@1.0), gamma
    /// (count=1@2.2), and LUT (count>=2) branches.
    #[cfg(feature = "jpegxl")]
    #[test]
    fn icc_trc_kind_classifies_linear_gamma_and_lut() {
        // Build a minimal 4096-byte ICC profile with a single rTRC tag at a
        // known offset. We don't need a valid header — `icc_find_tag_element_offset`
        // only reads the tag-table at offset 128.
        fn make(count: u32, payload_after_count: &[u8]) -> Vec<u8> {
            let trc_offset = 256_u32;
            let mut icc = vec![0u8; 4096];
            icc[128..132].copy_from_slice(&1_u32.to_be_bytes()); // tag_count
            icc[132..136].copy_from_slice(b"rTRC");
            icc[136..140].copy_from_slice(&trc_offset.to_be_bytes());
            // size (unused by the parser but spec-correct):
            let size = (12 + payload_after_count.len()) as u32;
            icc[140..144].copy_from_slice(&size.to_be_bytes());
            let off = trc_offset as usize;
            icc[off..off + 4].copy_from_slice(b"curv");
            icc[off + 4..off + 8].fill(0); // reserved
            icc[off + 8..off + 12].copy_from_slice(&count.to_be_bytes());
            icc[off + 12..off + 12 + payload_after_count.len()]
                .copy_from_slice(payload_after_count);
            icc
        }

        let linear_zero = make(0, &[]);
        assert_eq!(super::icc_trc_kind(&linear_zero), Some(HdrTransferFunction::Linear));

        let linear_one = make(1, &[0x01, 0x00]); // u8.8 = 1.0
        assert_eq!(super::icc_trc_kind(&linear_one), Some(HdrTransferFunction::Linear));

        let gamma_22 = make(1, &[0x02, 0x33]); // u8.8 ≈ 2.2
        assert_eq!(super::icc_trc_kind(&gamma_22), Some(HdrTransferFunction::Gamma));

        let lut = make(1024, &[0u8; 2048]);
        assert_eq!(super::icc_trc_kind(&lut), Some(HdrTransferFunction::Srgb));
    }

    /// Conformance regression: `blendmodes/input.jxl` (12-bit Modular, multiple
    /// blend modes, TF=sRGB codestream). The float buffer libjxl gives us is
    /// already sRGB-encoded; the SDR-grade fallback must direct-quantize
    /// (`value * 255`) without re-applying the OETF. The blend-mode arithmetic
    /// libjxl uses for partially-transparent / HDR-alpha (>1.0) pixels can
    /// disagree with the reference compositor by up to ~90 codes on the
    /// diagonal-stripe regions, so we lock the global statistics rather than
    /// pixel-exact equality. Any future regression that accidentally re-applies
    /// the OETF or routes through the HDR pipeline will blow these bounds.
    #[cfg(feature = "jpegxl")]
    #[test]
    fn conformance_blendmodes_sdr_fallback_close_to_ref_png_when_sample_present() {
        use crate::hdr::types::HdrToneMapSettings;
        let jxl_path = std::path::Path::new(r"F:\HDR\conformance\testcases\blendmodes\input.jxl");
        let ref_path = std::path::Path::new(r"F:\HDR\conformance\testcases\blendmodes\ref.png");
        if !jxl_path.is_file() || !ref_path.is_file() {
            return;
        }
        let bytes = std::fs::read(jxl_path).expect("read jxl");
        let img = super::decode_jxl_bytes_to_image_data(
            &bytes,
            HdrToneMapSettings::default().target_hdr_capacity(),
            HdrToneMapSettings::default(),
        )
        .expect("decode jxl");
        let crate::loader::ImageData::Hdr { fallback, hdr } = img else {
            panic!("unexpected ImageData variant");
        };
        assert_eq!(
            hdr.metadata.transfer_function,
            HdrTransferFunction::Srgb,
            "blendmodes.jxl ships TF=sRGB in the codestream — read_jxl_metadata must surface \
             that so the SDR-grade fallback direct-quantizes without re-applying the OETF"
        );
        let ours = fallback.rgba();
        let ref_img = image::open(ref_path).expect("decode ref.png").to_rgba8();
        assert_eq!(
            (hdr.width, hdr.height),
            (ref_img.width(), ref_img.height())
        );
        let r = ref_img.into_raw();
        let total = (ours.len() / 4) as f64;
        let (mut sr, mut sg, mut sb) = (0_i64, 0_i64, 0_i64);
        let mut exact = 0_u32;
        let mut close_15 = 0_u32;
        for (j, p) in ours.chunks_exact(4).zip(r.chunks_exact(4)) {
            let dr = j[0] as i32 - p[0] as i32;
            let dg = j[1] as i32 - p[1] as i32;
            let db = j[2] as i32 - p[2] as i32;
            sr += dr as i64;
            sg += dg as i64;
            sb += db as i64;
            let m = dr.unsigned_abs().max(dg.unsigned_abs()).max(db.unsigned_abs());
            if m == 0 {
                exact += 1;
            }
            if m <= 15 {
                close_15 += 1;
            }
        }
        let bias = (sr as f64 / total, sg as f64 / total, sb as f64 / total);
        assert!(
            bias.0.abs() < 5.0 && bias.1.abs() < 5.0 && bias.2.abs() < 5.0,
            "global mean RGB bias vs ref.png must stay within ±5 codes (we currently sit at \
             ~+1.55, -2.76, -0.75); got {bias:?}"
        );
        let exact_pct = exact as f64 / total;
        let close_15_pct = close_15 as f64 / total;
        assert!(
            exact_pct >= 0.30,
            "≥30% of pixels must match ref.png exactly (currently ~37%); got {:.1}%",
            exact_pct * 100.0
        );
        assert!(
            close_15_pct >= 0.55,
            "≥55% of pixels must be within 15 codes of ref.png; got {:.1}%",
            close_15_pct * 100.0
        );
    }

    /// Diagnostic harness for investigating SDR-fallback regressions on JXL
    /// conformance files. For a given (jxl, ref_png) pair: dumps `JxlBasicInfo`
    /// + extra-channel info, runs the live decode pipeline, and reports per-
    /// channel bias / max-abs / pixel histogram of differences vs ref.png so
    /// we can localize where rendering goes wrong (gamma, primaries, blend
    /// modes, patches, ...). Skipped silently when the conformance corpus is
    /// not present on the host.
    #[cfg(feature = "jpegxl")]
    fn diagnose_conformance_pair(name: &str, jxl_path: &std::path::Path, ref_path: &std::path::Path) {
        use crate::hdr::types::HdrToneMapSettings;
        if !jxl_path.is_file() || !ref_path.is_file() {
            eprintln!("[{name}] skipped — conformance file not present");
            return;
        }
        let bytes = std::fs::read(jxl_path).expect("read jxl");
        unsafe {
            let decoder = libjxl_sys::JxlDecoderCreate(std::ptr::null());
            assert!(!decoder.is_null());
            let subscribed = libjxl_sys::JXL_DEC_BASIC_INFO | libjxl_sys::JXL_DEC_COLOR_ENCODING;
            libjxl_sys::JxlDecoderSubscribeEvents(decoder, subscribed as i32);
            libjxl_sys::JxlDecoderSetInput(decoder, bytes.as_ptr(), bytes.len());
            libjxl_sys::JxlDecoderCloseInput(decoder);
            let mut info: libjxl_sys::JxlBasicInfo = std::mem::zeroed();
            loop {
                let st = libjxl_sys::JxlDecoderProcessInput(decoder);
                if st == libjxl_sys::JXL_DEC_BASIC_INFO {
                    libjxl_sys::JxlDecoderGetBasicInfo(decoder, &mut info);
                } else if st == libjxl_sys::JXL_DEC_COLOR_ENCODING {
                    eprintln!(
                        "[{name}] dims={}x{} bits={} float={} num_color={} num_extra={} have_anim={} intensity_target={} min_nits={} alpha_bits={}",
                        info.xsize, info.ysize, info.bits_per_sample,
                        info.exponent_bits_per_sample,
                        info.num_color_channels, info.num_extra_channels,
                        info.have_animation, info.intensity_target, info.min_nits,
                        info.alpha_bits,
                    );
                    for i in 0..info.num_extra_channels {
                        let mut ec = std::mem::MaybeUninit::<libjxl_sys::JxlExtraChannelInfo>::zeroed();
                        if libjxl_sys::JxlDecoderGetExtraChannelInfo(decoder.cast_const(), i as usize, ec.as_mut_ptr()) == libjxl_sys::JXL_DEC_SUCCESS {
                            let ec = ec.assume_init();
                            eprintln!(
                                "[{name}]   extra channel #{i}: type={} bits={}",
                                ec.type_, ec.bits_per_sample,
                            );
                        }
                    }
                    let mut orig_size = 0_usize;
                    libjxl_sys::JxlDecoderGetICCProfileSize(decoder.cast_const(), libjxl_sys::JXL_COLOR_PROFILE_TARGET_ORIGINAL, &mut orig_size);
                    let mut data_size = 0_usize;
                    libjxl_sys::JxlDecoderGetICCProfileSize(decoder.cast_const(), libjxl_sys::JXL_COLOR_PROFILE_TARGET_DATA, &mut data_size);
                    let mut enc: libjxl_sys::JxlColorEncoding = std::mem::zeroed();
                    let enc_st = libjxl_sys::JxlDecoderGetColorAsEncodedProfile(decoder.cast_const(), libjxl_sys::JXL_COLOR_PROFILE_TARGET_DATA, &mut enc);
                    eprintln!(
                        "[{name}] icc_orig={} icc_data={} encoded_st={} (cs={} wp={} prim={} tf={} ri={})",
                        orig_size, data_size, enc_st,
                        enc.color_space, enc.white_point, enc.primaries,
                        enc.transfer_function, enc.rendering_intent
                    );
                    break;
                } else if st == libjxl_sys::JXL_DEC_ERROR || st == libjxl_sys::JXL_DEC_NEED_MORE_INPUT {
                    break;
                }
            }
            libjxl_sys::JxlDecoderDestroy(decoder);
        }

        let tone = HdrToneMapSettings::default();
        let img = match super::decode_jxl_bytes_to_image_data(&bytes, tone.target_hdr_capacity(), tone) {
            Ok(img) => img,
            Err(e) => { eprintln!("[{name}] decode failed: {e}"); return; }
        };
        let crate::loader::ImageData::Hdr { fallback, hdr, .. } = img else {
            eprintln!("[{name}] unexpected ImageData variant");
            return;
        };
        let jxl_bytes = fallback.rgba().to_vec();
        let ref_img = image::open(ref_path).expect("decode ref.png").to_rgba8();
        if (hdr.width, hdr.height) != (ref_img.width(), ref_img.height()) {
            eprintln!(
                "[{name}] dim mismatch jxl={}x{} ref={}x{}",
                hdr.width, hdr.height, ref_img.width(), ref_img.height()
            );
            return;
        }
        let ref_bytes = ref_img.into_raw();
        let n = (jxl_bytes.len() / 4) as i64;
        let (mut dr, mut dg, mut db, mut da) = (0_i64, 0_i64, 0_i64, 0_i64);
        let (mut mr, mut mg, mut mb, mut ma) = (0_u32, 0_u32, 0_u32, 0_u32);
        let mut buckets = [0_u32; 8]; // 0,1-3,4-7,8-15,16-31,32-63,64-127,128+
        for (j, r) in jxl_bytes.chunks_exact(4).zip(ref_bytes.chunks_exact(4)) {
            let cr = j[0] as i32 - r[0] as i32;
            let cg = j[1] as i32 - r[1] as i32;
            let cb = j[2] as i32 - r[2] as i32;
            let ca = j[3] as i32 - r[3] as i32;
            dr += cr as i64; dg += cg as i64; db += cb as i64; da += ca as i64;
            mr = mr.max(cr.unsigned_abs());
            mg = mg.max(cg.unsigned_abs());
            mb = mb.max(cb.unsigned_abs());
            ma = ma.max(ca.unsigned_abs());
            let max_abs = cr.unsigned_abs().max(cg.unsigned_abs()).max(cb.unsigned_abs());
            let bin = match max_abs { 0=>0, 1..=3=>1, 4..=7=>2, 8..=15=>3, 16..=31=>4, 32..=63=>5, 64..=127=>6, _=>7 };
            buckets[bin] += 1;
        }
        eprintln!(
            "[{name}] vs ref.png: bias=({:+.2},{:+.2},{:+.2},a:{:+.2}) max=({},{},{},a:{}) hist[==,1-3,4-7,8-15,16-31,32-63,64-127,>=128]={:?}",
            dr as f64 / n as f64,
            dg as f64 / n as f64,
            db as f64 / n as f64,
            da as f64 / n as f64,
            mr, mg, mb, ma,
            buckets,
        );
    }

    #[cfg(feature = "jpegxl")]
    #[test]
    fn diagnose_conformance_blendmodes_and_patches_when_sample_present() {
        // Kept as a hand-runnable diagnostic: prints per-channel bias / max-abs
        // / histogram for a handful of conformance pairs. Drop by name into
        // `cargo test -- --nocapture diagnose_conformance` when investigating
        // a new SDR-fallback regression.
        for case in ["bench_oriented_brg", "blendmodes", "patches", "cmyk_layers", "bike"] {
            let jxl = std::path::Path::new(r"F:\HDR\conformance\testcases").join(case).join("input.jxl");
            let png = std::path::Path::new(r"F:\HDR\conformance\testcases").join(case).join("ref.png");
            diagnose_conformance_pair(case, &jxl, &png);
        }
    }

    /// For each conformance file, sample a few specific pixels that are NOT
    /// pure black/white in `ref.png` and report:
    ///   - libjxl's raw float values out of `JxlDecoderProcessInput`
    ///   - what `srgb_unit_to_u8(v*255)` produces (direct quantize)
    ///   - what `linear_to_srgb_u8(v)` produces (apply sRGB OETF first)
    ///   - what `ref.png` actually has at that location
    /// The encoding that matches ref.png tells us how libjxl emitted floats
    /// for that bitstream — Modular-mode files with TF=Linear preserve linear
    /// values, while sRGB-tagged Modular-mode files preserve sRGB-encoded
    /// values, etc.
    /// Count `JXL_DEC_FRAME` events fired by libjxl for `blendmodes/input.jxl`
    /// — if it's >1 with `have_animation=0` we know the file ships multiple
    /// blend-mode layers that libjxl coalesces; if our pipeline is somehow
    /// giving back the un-coalesced last layer that explains why our SDR
    /// fallback differs from `ref.png` on partially-transparent pixels.
    #[cfg(feature = "jpegxl")]
    #[test]
    fn diagnose_blendmodes_frame_count_when_sample_present() {
        let path = std::path::Path::new(r"F:\HDR\conformance\testcases\blendmodes\input.jxl");
        if !path.is_file() { return; }
        let bytes = std::fs::read(path).expect("read");
        let mut frame_count = 0_u32;
        let mut full_image_count = 0_u32;
        unsafe {
            let decoder = libjxl_sys::JxlDecoderCreate(std::ptr::null());
            let subscribed = libjxl_sys::JXL_DEC_BASIC_INFO
                | libjxl_sys::JXL_DEC_COLOR_ENCODING
                | libjxl_sys::JXL_DEC_FRAME
                | libjxl_sys::JXL_DEC_FULL_IMAGE;
            libjxl_sys::JxlDecoderSubscribeEvents(decoder, subscribed as i32);
            libjxl_sys::JxlDecoderSetInput(decoder, bytes.as_ptr(), bytes.len());
            libjxl_sys::JxlDecoderCloseInput(decoder);
            let pf = libjxl_sys::JxlPixelFormat {
                num_channels: 4,
                data_type: libjxl_sys::JXL_TYPE_FLOAT,
                endianness: libjxl_sys::JXL_NATIVE_ENDIAN,
                align: 0,
            };
            let mut buf: Vec<f32> = Vec::new();
            loop {
                let st = libjxl_sys::JxlDecoderProcessInput(decoder);
                if st == libjxl_sys::JXL_DEC_FRAME { frame_count += 1; }
                else if st == libjxl_sys::JXL_DEC_FULL_IMAGE { full_image_count += 1; }
                else if st == libjxl_sys::JXL_DEC_NEED_IMAGE_OUT_BUFFER {
                    let mut size = 0_usize;
                    libjxl_sys::JxlDecoderImageOutBufferSize(decoder.cast_const(), &pf, &mut size);
                    buf.resize(size / 4, 0.0);
                    libjxl_sys::JxlDecoderSetImageOutBuffer(decoder, &pf, buf.as_mut_ptr().cast(), size);
                }
                else if st == libjxl_sys::JXL_DEC_SUCCESS || st == libjxl_sys::JXL_DEC_ERROR { break; }
                else if st == libjxl_sys::JXL_DEC_NEED_MORE_INPUT { break; }
            }
            libjxl_sys::JxlDecoderDestroy(decoder);
        }
        eprintln!("[blendmodes] JXL_DEC_FRAME fired {frame_count}× JXL_DEC_FULL_IMAGE fired {full_image_count}×");
    }

    /// Hunt for pixels with the largest channel diff between our SDR fallback
    /// bytes and `ref.png` for `blendmodes/input.jxl` and dump float + alpha
    /// + neighbours so we can identify whether the discrepancy is a clamp,
    /// alpha-compositing, or layer blend bug.
    #[cfg(feature = "jpegxl")]
    #[test]
    fn diagnose_blendmodes_worst_pixels_when_sample_present() {
        use crate::hdr::types::HdrToneMapSettings;
        let jxl_path = std::path::Path::new(r"F:\HDR\conformance\testcases\blendmodes\input.jxl");
        let ref_path = std::path::Path::new(r"F:\HDR\conformance\testcases\blendmodes\ref.png");
        if !jxl_path.is_file() || !ref_path.is_file() { return; }
        let bytes = std::fs::read(jxl_path).expect("read jxl");
        let img = super::decode_jxl_bytes_to_image_data(
            &bytes,
            HdrToneMapSettings::default().target_hdr_capacity(),
            HdrToneMapSettings::default(),
        ).expect("decode");
        let crate::loader::ImageData::Hdr { fallback, hdr, .. } = img else { return };
        let our = fallback.rgba().to_vec();
        let r = image::open(ref_path).expect("ref").to_rgba8().into_raw();
        let w = hdr.width as usize;
        let mut worst = Vec::<(i32, usize, usize)>::new();
        for y in (0..hdr.height as usize).step_by(8) {
            for x in (0..w).step_by(8) {
                let i = (y * w + x) * 4;
                let dr = (our[i] as i32 - r[i] as i32).abs();
                let dg = (our[i + 1] as i32 - r[i + 1] as i32).abs();
                let db = (our[i + 2] as i32 - r[i + 2] as i32).abs();
                let m = dr.max(dg).max(db);
                if m >= 30 {
                    worst.push((m, x, y));
                }
            }
        }
        worst.sort_by(|a, b| b.0.cmp(&a.0));
        worst.truncate(10);
        for &(diff, x, y) in &worst {
            let i = (y * w + x) * 4;
            let f_i = (y * hdr.width as usize + x) * 4;
            let f = &hdr.rgba_f32[f_i..f_i + 4];
            eprintln!(
                "[worst] ({x:4},{y:4}) diff={diff} ours=({},{},{},a:{}) ref=({},{},{},a:{}) f32=({:.3},{:.3},{:.3},a:{:.3})",
                our[i], our[i + 1], our[i + 2], our[i + 3],
                r[i], r[i + 1], r[i + 2], r[i + 3],
                f[0], f[1], f[2], f[3]
            );
        }
    }

    #[cfg(feature = "jpegxl")]
    #[test]
    fn diagnose_jxl_float_buffer_encoding_when_samples_present() {
        for case in ["blendmodes", "patches", "bench_oriented_brg", "bike"] {
            let jxl_path = std::path::Path::new(r"F:\HDR\conformance\testcases").join(case).join("input.jxl");
            let ref_path = std::path::Path::new(r"F:\HDR\conformance\testcases").join(case).join("ref.png");
            if !jxl_path.is_file() || !ref_path.is_file() { continue; }
            let bytes = std::fs::read(&jxl_path).expect("read jxl");
            let ref_img = image::open(&ref_path).expect("decode ref.png").to_rgba8();
            let (rw, rh) = (ref_img.width(), ref_img.height());
            let ref_bytes = ref_img.into_raw();

            let mut rgba_f32: Vec<f32> = Vec::new();
            let mut width: u32 = 0;
            unsafe {
                let decoder = libjxl_sys::JxlDecoderCreate(std::ptr::null());
                let subscribed = libjxl_sys::JXL_DEC_BASIC_INFO
                    | libjxl_sys::JXL_DEC_COLOR_ENCODING
                    | libjxl_sys::JXL_DEC_FRAME
                    | libjxl_sys::JXL_DEC_FULL_IMAGE;
                libjxl_sys::JxlDecoderSubscribeEvents(decoder, subscribed as i32);
                libjxl_sys::JxlDecoderSetInput(decoder, bytes.as_ptr(), bytes.len());
                libjxl_sys::JxlDecoderCloseInput(decoder);
                let pixel_format = libjxl_sys::JxlPixelFormat {
                    num_channels: 4,
                    data_type: libjxl_sys::JXL_TYPE_FLOAT,
                    endianness: libjxl_sys::JXL_NATIVE_ENDIAN,
                    align: 0,
                };
                let mut info: libjxl_sys::JxlBasicInfo = std::mem::zeroed();
                loop {
                    let st = libjxl_sys::JxlDecoderProcessInput(decoder);
                    if st == libjxl_sys::JXL_DEC_BASIC_INFO {
                        libjxl_sys::JxlDecoderGetBasicInfo(decoder, &mut info);
                        width = info.xsize;
                    } else if st == libjxl_sys::JXL_DEC_NEED_IMAGE_OUT_BUFFER {
                        let mut size = 0_usize;
                        libjxl_sys::JxlDecoderImageOutBufferSize(decoder.cast_const(), &pixel_format, &mut size);
                        rgba_f32.resize(size / std::mem::size_of::<f32>(), 0.0);
                        libjxl_sys::JxlDecoderSetImageOutBuffer(decoder, &pixel_format, rgba_f32.as_mut_ptr().cast(), size);
                    } else if st == libjxl_sys::JXL_DEC_FULL_IMAGE { break; }
                    else if st == libjxl_sys::JXL_DEC_ERROR || st == libjxl_sys::JXL_DEC_NEED_MORE_INPUT { break; }
                }
                libjxl_sys::JxlDecoderDestroy(decoder);
            }
            if width == 0 || rgba_f32.is_empty() { continue; }
            // Pick 6 sample pixels evenly spaced
            let samples: [(u32, u32); 6] = [
                (rw / 8,     rh / 8    ),
                (rw / 4,     rh / 4    ),
                (rw / 2,     rh / 4    ),
                (rw / 2,     rh / 2    ),
                (rw * 3 / 4, rh / 2    ),
                (rw * 3 / 4, rh * 3 / 4),
            ];
            eprintln!("\n--- {case} ({rw}x{rh}) — float vs ref.png ---");
            for (x, y) in samples {
                let i = (y as usize * width as usize + x as usize) * 4;
                if i + 2 >= rgba_f32.len() { continue; }
                let r = rgba_f32[i];
                let g = rgba_f32[i + 1];
                let b = rgba_f32[i + 2];
                let direct = (
                    super::srgb_unit_to_u8(r),
                    super::srgb_unit_to_u8(g),
                    super::srgb_unit_to_u8(b),
                );
                let linear_to_srgb = (
                    super::linear_to_srgb_u8(r),
                    super::linear_to_srgb_u8(g),
                    super::linear_to_srgb_u8(b),
                );
                let ref_pix = (
                    ref_bytes[i],
                    ref_bytes[i + 1],
                    ref_bytes[i + 2],
                );
                eprintln!(
                    "  ({x:4},{y:4}) f32=({r:.3},{g:.3},{b:.3}) direct={direct:?} linear->srgb={linear_to_srgb:?} ref={ref_pix:?}"
                );
            }
        }
    }
}
