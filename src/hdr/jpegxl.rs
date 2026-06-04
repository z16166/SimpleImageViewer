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
use crate::hdr::gain_map::GainMapMetadata;
use crate::hdr::types::{
    HdrColorProfile, HdrImageMetadata, HdrLuminanceMetadata, HdrReference, HdrTransferFunction,
};
#[cfg(feature = "jpegxl")]
use crate::hdr::types::{HdrColorSpace, HdrImageBuffer, HdrPixelFormat, HdrToneMapSettings};
#[cfg(feature = "jpegxl")]
use crate::{
    constants::{
        DEFAULT_ANIMATION_DELAY_MS, JXL_PROBE_ITERATION_CAP, MAX_ICC_TAG_COUNT,
        MIN_ANIMATION_DELAY_THRESHOLD_MS,
    },
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
        if ptr.is_null() { None } else { Some(Self(ptr)) }
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

/// Peek [`JxlBasicInfo.orientation`] with [`libjxl_sys::JxlDecoderSetKeepOrientation`] enabled so
/// libjxl reports the codestream value (defaults would fold it to [`libjxl_sys::JXL_ORIENT_IDENTITY`]
/// once re-orientation is applied). Values match EXIF Orientation 1–8 (`jxl/codestream_header.h`).
#[cfg(feature = "jpegxl")]
pub(crate) fn libjxl_probe_orientation_from_bytes(bytes: &[u8]) -> Option<u16> {
    let probe_len = bytes.len().min(16).max(2);
    if bytes.len() < 2 || !is_jxl_header(&bytes[..probe_len]) {
        return None;
    }
    struct DecoderPtr(*mut libjxl_sys::JxlDecoder);
    impl Drop for DecoderPtr {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { libjxl_sys::JxlDecoderDestroy(self.0) };
                self.0 = std::ptr::null_mut();
            }
        }
    }
    unsafe {
        let raw = libjxl_sys::JxlDecoderCreate(std::ptr::null());
        if raw.is_null() {
            return None;
        }
        let decoder = DecoderPtr(raw);
        if libjxl_sys::JxlDecoderSetKeepOrientation(decoder.0, libjxl_sys::JXL_TRUE)
            != libjxl_sys::JXL_DEC_SUCCESS
        {
            return None;
        }
        if libjxl_sys::JxlDecoderSubscribeEvents(
            decoder.0,
            libjxl_sys::JXL_DEC_BASIC_INFO as std::os::raw::c_int,
        ) != libjxl_sys::JXL_DEC_SUCCESS
        {
            return None;
        }
        if libjxl_sys::JxlDecoderSetInput(decoder.0, bytes.as_ptr(), bytes.len())
            != libjxl_sys::JXL_DEC_SUCCESS
        {
            return None;
        }
        libjxl_sys::JxlDecoderCloseInput(decoder.0);

        // Subscribed-only basic-info probes should terminate quickly; cap iterations on bad input.
        for _ in 0..JXL_PROBE_ITERATION_CAP {
            match libjxl_sys::JxlDecoderProcessInput(decoder.0) {
                libjxl_sys::JXL_DEC_BASIC_INFO => {
                    let mut info = std::mem::MaybeUninit::<libjxl_sys::JxlBasicInfo>::uninit();
                    if libjxl_sys::JxlDecoderGetBasicInfo(decoder.0.cast_const(), info.as_mut_ptr())
                        != libjxl_sys::JXL_DEC_SUCCESS
                    {
                        return None;
                    }
                    let info = info.assume_init();
                    let o_ok = info.orientation as i32;
                    return ((1..=8).contains(&o_ok)).then_some(o_ok as u16);
                }
                libjxl_sys::JXL_DEC_SUCCESS
                | libjxl_sys::JXL_DEC_ERROR
                | libjxl_sys::JXL_DEC_NEED_MORE_INPUT => {
                    return None;
                }
                _ => {}
            }
        }
        None
    }
}

#[cfg(feature = "jpegxl")]
pub(crate) fn libjxl_probe_orientation_from_path(path: &std::path::Path) -> Option<u16> {
    let mmap = crate::mmap_util::map_file(path).ok()?;
    libjxl_probe_orientation_from_bytes(&mmap[..])
}

// JPEG XL colour / container behaviour (normative references for this module):
//
// - **ISO/IEC 18181-1** — JPEG XL codestream (image data, colour description in bitstream).
// - **ISO/IEC 18181-2** — JPEG XL file format (BMFF boxes, optional ICC, orientation, etc.).
// - **ISO/IEC 18181-4** — Reference software; **libjxl** is the de-facto normative decoder API
//   used here (`jxl/decode.h`). Decoder colour queries are defined in that API, not guessed.
// - **JPEG XL orientation** (`JxlDecoderSetKeepOrientation`, `JxlBasicInfo`): default libjxl applies
//   codestream orientation during decode **and folds** [`JxlBasicInfo.orientation`] back to identity
//   (`jxl/decode.h`). We enable **keep coded orientation** on the main decoder so pixels stay in codestream
//   layout while [`crate::metadata_utils::get_exif_orientation`] reads container EXIF when present or
//   else [`libjxl_probe_orientation_from_bytes`]/`path` parity for [`crate::loader::orientation`].
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
// libjxl `JxlTransferFunction` values (`jxl/color_encoding.h`). **Linear / sRGB / PQ / HLG**
// discriminants intentionally match ITU-T H.273 `transfer_characteristics` — reuse `hdr::cicp`.
/// BT.709 / BT.601 OETF family (see `JXL_TRANSFER_FUNCTION_*` in libjxl headers).
pub(crate) const JXL_TRANSFER_FUNCTION_709: u16 = 1;
/// LibjXL “gamma”; not a fixed H.273 code.
pub(crate) const JXL_TRANSFER_FUNCTION_GAMMA: u16 = 65535;
pub(crate) const JXL_TRANSFER_FUNCTION_LINEAR: u16 = crate::hdr::cicp::H273_TRANSFER_LINEAR;
pub(crate) const JXL_TRANSFER_FUNCTION_SRGB: u16 =
    crate::hdr::cicp::H273_TRANSFER_IEC61966_2_1_SRGB;
pub(crate) const JXL_TRANSFER_FUNCTION_PQ: u16 =
    crate::hdr::cicp::H273_TRANSFER_SMPTE_ST2084_FOR_PQ;
pub(crate) const JXL_TRANSFER_FUNCTION_HLG: u16 =
    crate::hdr::cicp::H273_TRANSFER_ARIB_STD_B67_FOR_HLG;

#[allow(dead_code)]
pub(crate) fn jxl_color_encoding_to_metadata(
    color_primaries: u16,
    transfer_characteristics: u16,
    intensity_target_nits: Option<f32>,
) -> HdrImageMetadata {
    crate::hdr::cicp::cicp_to_metadata(
        color_primaries,
        transfer_characteristics,
        0,
        true,
        intensity_target_nits,
    )
}

#[cfg(feature = "jpegxl")]
#[allow(dead_code)]
pub(crate) fn load_jxl_hdr(path: &std::path::Path) -> Result<ImageData, String> {
    let bytes = std::fs::read(path).map_err(|err| format!("Failed to read JPEG XL: {err}"))?;
    decode_jxl_bytes_to_image_data(
        &bytes,
        crate::hdr::types::HdrToneMapSettings::default().target_hdr_capacity(),
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
    decode_target_hdr_capacity: f32,
    display_hdr_target_capacity: f32,
    tone_map: HdrToneMapSettings,
) -> Result<ImageData, String> {
    let bytes = std::fs::read(path).map_err(|err| format!("Failed to read JPEG XL: {err}"))?;
    decode_jxl_bytes_to_image_data(
        &bytes,
        decode_target_hdr_capacity,
        display_hdr_target_capacity,
        tone_map,
    )
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
        | HdrTransferFunction::Bt709
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
pub(crate) fn srgb_unit_to_u8(value: f32) -> u8 {
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
fn apply_cmyk_to_srgb_via_lcms(rgba: &mut [f32], k: &[f32], source_icc: &[u8]) -> bool {
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
    let Some(in_profile) = libjxl_sys::CmsProfile::open_from_mem(source_icc) else {
        log::warn!("[JXL] lcms2 could not parse embedded CMYK ICC; skipping CMS transform");
        return false;
    };
    let Some(out_profile) = libjxl_sys::CmsProfile::new_srgb() else {
        log::warn!("[JXL] lcms2 could not build sRGB profile; skipping CMS transform");
        return false;
    };
    let Some(transform) = libjxl_sys::CmsTransform::new(
        &in_profile,
        libjxl_sys::LCMS_TYPE_CMYK_FLT,
        &out_profile,
        libjxl_sys::LCMS_TYPE_RGBA_FLT,
        libjxl_sys::LCMS_INTENT_PERCEPTUAL,
        0,
    ) else {
        log::warn!(
            "[JXL] lcms2 could not build CMYK→sRGB transform from {}-byte ICC; skipping",
            source_icc.len()
        );
        return false;
    };
    transform.do_transform(
        cmyk.as_ptr().cast(),
        rgba_out.as_mut_ptr().cast(),
        pixel_count as u32,
    );

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
fn jxl_build_hdr_fallback(
    hdr: &HdrImageBuffer,
    display_hdr_target_capacity: f32,
    tone_map: &HdrToneMapSettings,
) -> Result<DecodedImage, String> {
    let color_space = hdr.color_space;
    let sdr_grade_fallback =
        jxl_sdr_grade_fallback_rgba8(hdr.rgba_f32.as_ref(), color_space, &hdr.metadata);
    let fallback_pixels = match sdr_grade_fallback {
        Some(px) => px,
        None => {
            if crate::loader::hdr_display_requests_sdr_preview(display_hdr_target_capacity) {
                crate::hdr::decode::hdr_to_sdr_rgba8_with_tone_settings(
                    hdr,
                    tone_map.exposure_ev,
                    tone_map,
                )?
            } else {
                crate::loader::cheap_hdr_sdr_placeholder_rgba8(hdr.width, hdr.height)?
            }
        }
    };
    Ok(DecodedImage::new(hdr.width, hdr.height, fallback_pixels))
}

#[cfg(feature = "jpegxl")]
fn jxl_finish_static_frame(
    rgba: Vec<f32>,
    metadata: HdrImageMetadata,
    width: u32,
    height: u32,
    jhgm_box: Option<&[u8]>,
    decode_target_hdr_capacity: f32,
    display_hdr_target_capacity: f32,
    tone_map: &HdrToneMapSettings,
) -> Result<ImageData, String> {
    use crate::hdr::jxl_gain_map_deferred::{JxlJhgmFrameOutcome, finish_jxl_jhgm_frame};

    let hdr = match finish_jxl_jhgm_frame(
        jhgm_box,
        decode_target_hdr_capacity,
        &rgba,
        width,
        height,
        &metadata,
    ) {
        JxlJhgmFrameOutcome::PrecomposedHdr(hdr)
        | JxlJhgmFrameOutcome::GpuDeferred(hdr)
        | JxlJhgmFrameOutcome::CpuComposed(hdr) => hdr,
        JxlJhgmFrameOutcome::Unprocessed => {
            let color_space = metadata.color_space_hint();
            HdrImageBuffer {
                width,
                height,
                format: HdrPixelFormat::Rgba32Float,
                color_space,
                metadata,
                rgba_f32: Arc::new(rgba),
            }
        }
    };
    let fallback = jxl_build_hdr_fallback(&hdr, display_hdr_target_capacity, tone_map)?;
    Ok(ImageData::Hdr { hdr, fallback })
}

/// SDR-grade JXL float buffers hold **display-referred sRGB codes** (0–1), not scene-linear.
/// Tag [`HdrReference::DisplayReferred`] so the HDR plane applies EV like unmanaged sRGB stills.
#[cfg(feature = "jpegxl")]
fn jxl_tag_display_referred_when_sdr_grade(metadata: &mut HdrImageMetadata) {
    let peak = metadata.luminance.mastering_max_nits.unwrap_or(0.0);
    if !peak.is_finite() || peak <= 0.0 || peak > 255.0 {
        return;
    }
    match metadata.transfer_function {
        HdrTransferFunction::Srgb
        | HdrTransferFunction::Gamma
        | HdrTransferFunction::Bt709
        | HdrTransferFunction::Unknown => {
            metadata.reference = HdrReference::DisplayReferred;
        }
        HdrTransferFunction::Linear | HdrTransferFunction::Pq | HdrTransferFunction::Hlg => {}
    }
}

#[cfg(feature = "jpegxl")]
fn jxl_sanitize_straight_alpha(rgba: &mut [f32]) {
    for px in rgba.chunks_exact_mut(4) {
        if px[3] <= 0.0 {
            px[0] = 0.0;
            px[1] = 0.0;
            px[2] = 0.0;
        }
    }
}

#[cfg(feature = "jpegxl")]
fn jxl_animation_frames_need_hdr_plane(
    captured_frames: &[(Vec<f32>, u32)],
    metadata: &HdrImageMetadata,
) -> bool {
    let color_space = metadata.color_space_hint();
    captured_frames
        .iter()
        .any(|(buf, _)| jxl_sdr_grade_fallback_rgba8(buf, color_space, metadata).is_none())
}

#[cfg(feature = "jpegxl")]
fn jxl_build_hdr_animated_image_data(
    captured_frames: Vec<(Vec<f32>, u32)>,
    info: &libjxl_sys::JxlBasicInfo,
    meta_base: HdrImageMetadata,
    jhgm_box: Option<&[u8]>,
    decode_target_hdr_capacity: f32,
    display_hdr_target_capacity: f32,
    tone_map: &HdrToneMapSettings,
) -> Result<ImageData, String> {
    use crate::hdr::jxl_gain_map_deferred::{JxlJhgmFrameOutcome, finish_jxl_jhgm_frame};
    use crate::loader::HdrAnimationFrame;

    let require_jhgm_processing = jhgm_box.is_some();
    let mut frames = Vec::with_capacity(captured_frames.len());
    for (rgba, ticks) in captured_frames {
        let frame_metadata = meta_base.clone();
        let hdr = match finish_jxl_jhgm_frame(
            jhgm_box,
            decode_target_hdr_capacity,
            &rgba,
            info.xsize,
            info.ysize,
            &frame_metadata,
        ) {
            JxlJhgmFrameOutcome::PrecomposedHdr(hdr)
            | JxlJhgmFrameOutcome::GpuDeferred(hdr)
            | JxlJhgmFrameOutcome::CpuComposed(hdr) => hdr,
            JxlJhgmFrameOutcome::Unprocessed => {
                if require_jhgm_processing {
                    return Err(
                        "JPEG XL animated jhgm frame could not be processed (metadata or compose failure)"
                            .to_string(),
                    );
                }
                let color_space = frame_metadata.color_space_hint();
                HdrImageBuffer {
                    width: info.xsize,
                    height: info.ysize,
                    format: HdrPixelFormat::Rgba32Float,
                    color_space,
                    metadata: frame_metadata,
                    rgba_f32: Arc::new(rgba),
                }
            }
        };
        let fallback = jxl_build_hdr_fallback(&hdr, display_hdr_target_capacity, tone_map)?;
        let delay_ms = jxl_frame_ticks_to_delay_ms(info, ticks);
        frames.push(HdrAnimationFrame::new(
            hdr,
            fallback,
            Duration::from_millis(delay_ms),
        ));
    }
    Ok(ImageData::HdrAnimated(frames))
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
        ((ticks as u128).saturating_mul(1000).saturating_mul(den) / num).min(u128::from(u64::MAX))
            as u64
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
        target_hdr_capacity,
        crate::hdr::types::HdrToneMapSettings::default(),
    )? {
        ImageData::Hdr { hdr, .. } => Ok(hdr),
        ImageData::HdrAnimated(_) | ImageData::Animated(_) => Err(
            "JPEG XL has multiple animation frames; use the image loader or decode_jxl_bytes_to_image_data"
                .to_string(),
        ),
        ImageData::Static(_) | ImageData::Tiled(_) | ImageData::HdrTiled { .. } => Err(
            "unexpected JPEG XL decode outcome (expected HDR buffer)".to_string(),
        ),
    }
}

/// Decode a full JPEG XL file into [`ImageData`]. Multi-frame animations become
/// [`ImageData::Animated`] or [`ImageData::HdrAnimated`] when frames need the HDR plane
/// (ISO `jhgm` gain map and/or scene-referred float with peak above SDR grade);
/// a single displayed frame stays [`ImageData::Hdr`] with float pixels and an SDR fallback.
#[cfg(feature = "jpegxl")]
pub(crate) fn decode_jxl_bytes_to_image_data(
    bytes: &[u8],
    decode_target_hdr_capacity: f32,
    display_hdr_target_capacity: f32,
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
    let unpremul_st =
        unsafe { libjxl_sys::JxlDecoderSetUnpremultiplyAlpha(decoder.0, libjxl_sys::JXL_TRUE) };
    if unpremul_st != libjxl_sys::JXL_DEC_SUCCESS {
        log::warn!(
            "JxlDecoderSetUnpremultiplyAlpha failed (libjxl status {unpremul_st}); colors may be wrong for premultiplied alpha"
        );
    }

    let keep_ori_st =
        unsafe { libjxl_sys::JxlDecoderSetKeepOrientation(decoder.0, libjxl_sys::JXL_TRUE) };
    if keep_ori_st != libjxl_sys::JXL_DEC_SUCCESS {
        return Err(format!(
            "JxlDecoderSetKeepOrientation failed (libjxl status {keep_ori_st})"
        ));
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
                    let mut meta_base = metadata.clone();
                    jxl_tag_display_referred_when_sdr_grade(&mut meta_base);
                    let use_hdr_animated = jhgm_box.is_some()
                        || jxl_animation_frames_need_hdr_plane(&captured_frames, &meta_base)
                        || !crate::loader::hdr_display_requests_sdr_preview(
                            display_hdr_target_capacity,
                        );
                    if use_hdr_animated {
                        return jxl_build_hdr_animated_image_data(
                            captured_frames,
                            &info,
                            meta_base,
                            jhgm_box.as_deref(),
                            decode_target_hdr_capacity,
                            display_hdr_target_capacity,
                            &tone_map,
                        );
                    }
                    let mut animation = Vec::with_capacity(captured_frames.len());
                    for (buf, ticks) in captured_frames {
                        let frame_metadata = meta_base.clone();
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
                            if crate::loader::hdr_display_requests_sdr_preview(
                                display_hdr_target_capacity,
                            ) {
                                crate::hdr::decode::hdr_to_sdr_rgba8_with_tone_settings(
                                    &hdr,
                                    tone_map.exposure_ev,
                                    &tone_map,
                                )?
                            } else {
                                crate::loader::cheap_hdr_sdr_placeholder_rgba8(
                                    hdr.width, hdr.height,
                                )?
                            }
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
                jxl_sanitize_straight_alpha(&mut rgba);
                jxl_tag_display_referred_when_sdr_grade(&mut metadata);
                return jxl_finish_static_frame(
                    rgba,
                    metadata,
                    info.xsize,
                    info.ysize,
                    jhgm_box.as_deref(),
                    decode_target_hdr_capacity,
                    display_hdr_target_capacity,
                    &tone_map,
                );
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
                    log::debug!("[JXL] CMYK-style K (black) extra channel found at index {idx}");
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
                    if let Some(icc) = jxl_decoder_copy_target_original_icc(decoder.0.cast_const())
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
                    if st == libjxl_sys::JXL_DEC_SUCCESS && k_size % std::mem::size_of::<f32>() == 0
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
                jxl_sanitize_straight_alpha(&mut rgba_f32);
                captured_frames.push((rgba_f32.clone(), pending_duration_ticks));
                // Animations emit multiple FULL_IMAGE events; keep calling ProcessInput until SUCCESS.
                continue;
            }
            libjxl_sys::JXL_DEC_DC_IMAGE | libjxl_sys::JXL_DEC_FRAME_PROGRESSION => {
                continue;
            }
            libjxl_sys::JXL_DEC_JPEG_RECONSTRUCTION | libjxl_sys::JXL_DEC_JPEG_NEED_MORE_OUTPUT => {
                return Err(
                    "JPEG XL JPEG reconstruction stream is not supported by this viewer"
                        .to_string(),
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
pub(crate) fn decode_jxl_gain_map_from_bundle(
    bundle: &JxlGainMapBundleRef<'_>,
    metadata: GainMapMetadata,
    target_hdr_capacity: f32,
) -> Result<(GainMapMetadata, u32, u32, Vec<u8>), String> {
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
    for (sig, offset, _size) in icc_tag_entries(icc)? {
        if sig == *tag {
            return Some(offset as usize);
        }
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

    let multiset_close = |obs: [(f64, f64); 3], tgt: [(f64, f64); 3], eps: f64| {
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
    for (sig, offset, _size) in icc_tag_entries(icc)? {
        if sig == *b"cicp" {
            let offset = offset as usize;
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
    }
    None
}

#[cfg(feature = "jpegxl")]
fn icc_tag_entries(icc: &[u8]) -> Option<Vec<([u8; 4], u32, u32)>> {
    const HEADER: usize = 128;
    if icc.len() < HEADER + 4 {
        return None;
    }
    let tag_count = u32::from_be_bytes(icc[128..132].try_into().ok()?) as usize;
    if tag_count > MAX_ICC_TAG_COUNT {
        return None;
    }
    let mut out = Vec::with_capacity(tag_count.min(128));
    let mut entry = 132usize;
    for _ in 0..tag_count {
        if entry + 12 > icc.len() {
            break;
        }
        let sig = icc[entry..entry + 4].try_into().ok()?;
        let offset = u32::from_be_bytes(icc[entry + 4..entry + 8].try_into().ok()?);
        let size = u32::from_be_bytes(icc[entry + 8..entry + 12].try_into().ok()?);
        out.push((sig, offset, size));
        entry += 12;
    }
    Some(out)
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
/// Modular-mode files: TF=Linear → linear floats,
/// TF=IEC sRGB (**13**) / BT.709 codestream (**1**, [`HdrTransferFunction::Bt709`]) / Gamma (**4**) / Unknown →
/// preserve libjxl’s nonlinear floats; PQ / HLG (**16** / **18**) signal HDR.
#[cfg(feature = "jpegxl")]
fn jxl_internal_transfer_for_jxl_float_buffer(jxl_tf: i64) -> HdrTransferFunction {
    match jxl_tf {
        x if x == JXL_TRANSFER_FUNCTION_LINEAR as i64 => HdrTransferFunction::Linear,
        x if x == JXL_TRANSFER_FUNCTION_SRGB as i64 => HdrTransferFunction::Srgb,
        x if x == JXL_TRANSFER_FUNCTION_709 as i64 => HdrTransferFunction::Bt709,
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
fn jxl_decoder_copy_target_original_icc(decoder: *const libjxl_sys::JxlDecoder) -> Option<Vec<u8>> {
    jxl_decoder_copy_icc_for_target(decoder, libjxl_sys::JXL_COLOR_PROFILE_TARGET_ORIGINAL)
}

#[cfg(feature = "jpegxl")]
fn jxl_decoder_copy_icc_for_target(
    decoder: *const libjxl_sys::JxlDecoder,
    target: libjxl_sys::JxlColorProfileTarget,
) -> Option<Vec<u8>> {
    let mut icc_size = 0_usize;
    let st = unsafe { libjxl_sys::JxlDecoderGetICCProfileSize(decoder, target, &mut icc_size) };
    if st != libjxl_sys::JXL_DEC_SUCCESS || icc_size == 0 {
        return None;
    }
    let mut icc = vec![0_u8; icc_size];
    let st2 = unsafe {
        libjxl_sys::JxlDecoderGetColorAsICCProfile(decoder, target, icc.as_mut_ptr(), icc.len())
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
        color_primaries, ..
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
        jxl_tag_display_referred_when_sdr_grade(&mut out);
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
            jxl_tag_display_referred_when_sdr_grade(&mut out);
            return out;
        }
        if let Some(mut out) = hdr_metadata_from_icc_rgb_xyz_primaries_for_jxl_float(&icc) {
            out.luminance = saved_luminance;
            jxl_tag_display_referred_when_sdr_grade(&mut out);
            return out;
        }
        let trc = icc_trc_kind(&icc).unwrap_or(HdrTransferFunction::Srgb);
        metadata.color_profile = HdrColorProfile::Icc(Arc::new(icc));
        metadata.transfer_function = trc;
        metadata.reference = HdrReference::Unknown;
        metadata.luminance = saved_luminance;
        crate::hdr::types::log_unrecognized_embedded_icc_after_decode(&metadata);
        jxl_tag_display_referred_when_sdr_grade(&mut metadata);
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
        jxl_tag_display_referred_when_sdr_grade(&mut out);
        return out;
    }

    metadata.luminance = saved_luminance;
    jxl_tag_display_referred_when_sdr_grade(&mut metadata);
    metadata
}

#[cfg(test)]
mod tests;
