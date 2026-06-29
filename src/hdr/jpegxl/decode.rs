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

use super::probe::is_jxl_header;

use super::metadata::{
    capture_jxl_box, ensure_jxl_success, jxl_apply_preferred_profile_from_target_data_icc,
    jxl_decoder_copy_target_data_icc, jxl_decoder_copy_target_original_icc, linear_to_srgb_u8,
    read_jxl_metadata,
};
use super::runner::JxlResizableRunnerPtr;

use crate::hdr::types::{HdrColorProfile, HdrImageMetadata, HdrReference, HdrTransferFunction};
#[cfg(feature = "jpegxl")]
use crate::hdr::types::{HdrColorSpace, HdrImageBuffer, HdrPixelFormat, HdrToneMapSettings};
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
#[allow(dead_code)]
pub(crate) fn load_jxl_hdr(path: &std::path::Path) -> Result<ImageData, String> {
    let bytes =
        crate::mmap_util::map_file(path).map_err(|err| format!("Failed to mmap JPEG XL: {err}"))?;
    decode_jxl_bytes_to_image_data(
        bytes.as_ref(),
        crate::hdr::types::HdrToneMapSettings::default().target_hdr_capacity(),
        crate::hdr::types::HdrToneMapSettings::default().target_hdr_capacity(),
        crate::hdr::types::HdrToneMapSettings::default(),
    )
}

#[cfg(feature = "jpegxl")]
#[allow(dead_code)]
pub(crate) fn decode_jxl_hdr(path: &std::path::Path) -> Result<HdrImageBuffer, String> {
    let bytes =
        crate::mmap_util::map_file(path).map_err(|err| format!("Failed to mmap JPEG XL: {err}"))?;
    decode_jxl_hdr_bytes(bytes.as_ref())
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
    let bytes =
        crate::mmap_util::map_file(path).map_err(|err| format!("Failed to mmap JPEG XL: {err}"))?;
    decode_jxl_bytes_to_image_data(
        bytes.as_ref(),
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
    let bytes =
        crate::mmap_util::map_file(path).map_err(|err| format!("Failed to mmap JPEG XL: {err}"))?;
    decode_jxl_hdr_bytes_with_target_capacity(bytes.as_ref(), target_hdr_capacity)
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
pub(crate) fn jxl_sdr_grade_fallback_rgba8(
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
pub(crate) fn jxl_find_black_extra_channel_index(
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
    if hdr.rgba_f32.is_empty() {
        return Ok(DecodedImage::from_hdr_sdr_fallback(
            hdr.width,
            hdr.height,
            crate::loader::hdr_sdr_fallback_rgba8_eager_or_placeholder(
                hdr,
                display_hdr_target_capacity,
                tone_map,
            )?,
        ));
    }
    if let Some(px) =
        jxl_sdr_grade_fallback_rgba8(hdr.rgba_f32.as_ref(), hdr.color_space, &hdr.metadata)
    {
        return Ok(DecodedImage::new(hdr.width, hdr.height, px));
    }
    Ok(DecodedImage::from_hdr_sdr_fallback(
        hdr.width,
        hdr.height,
        crate::loader::hdr_sdr_fallback_rgba8_eager_or_placeholder(
            hdr,
            display_hdr_target_capacity,
            tone_map,
        )?,
    ))
}

#[cfg(feature = "jpegxl")]
struct JxlStaticFrameFinish<'a> {
    rgba: Vec<f32>,
    metadata: HdrImageMetadata,
    width: u32,
    height: u32,
    jhgm_box: Option<&'a [u8]>,
    decode_target_hdr_capacity: f32,
    display_hdr_target_capacity: f32,
    tone_map: &'a HdrToneMapSettings,
    strip_baseline_only: bool,
}

#[cfg(feature = "jpegxl")]
fn jxl_finish_static_frame(input: JxlStaticFrameFinish<'_>) -> Result<ImageData, String> {
    let JxlStaticFrameFinish {
        rgba,
        metadata,
        width,
        height,
        jhgm_box,
        decode_target_hdr_capacity,
        display_hdr_target_capacity,
        tone_map,
        strip_baseline_only,
    } = input;
    use crate::hdr::jxl_gain_map_deferred::{JxlJhgmFrameOutcome, finish_jxl_jhgm_frame};

    let hdr = match finish_jxl_jhgm_frame(
        jhgm_box,
        decode_target_hdr_capacity,
        &rgba,
        width,
        height,
        &metadata,
        strip_baseline_only,
    ) {
        JxlJhgmFrameOutcome::IsoGainMapBaseline(baseline) => {
            return Ok(ImageData::Static(DecodedImage::new(
                width, height, baseline,
            )));
        }
        JxlJhgmFrameOutcome::PrecomposedHdr(hdr)
        | JxlJhgmFrameOutcome::GpuDeferred(hdr)
        | JxlJhgmFrameOutcome::CpuComposed(hdr) => hdr,
        JxlJhgmFrameOutcome::Unprocessed => {
            if strip_baseline_only {
                return Err("JPEG XL strip baseline path requires ISO gain map".to_string());
            }
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
    Ok(ImageData::Hdr {
        hdr: Box::new(hdr),
        fallback,
    })
}

/// SDR-grade JXL float buffers hold **display-referred sRGB codes** (0–1), not scene-linear.
/// Tag [`HdrReference::DisplayReferred`] so the HDR plane applies EV like unmanaged sRGB stills.
#[cfg(feature = "jpegxl")]
pub(crate) fn jxl_tag_display_referred_when_sdr_grade(metadata: &mut HdrImageMetadata) {
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
pub(crate) fn jxl_sanitize_straight_alpha(rgba: &mut [f32]) {
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
            false,
        ) {
            JxlJhgmFrameOutcome::IsoGainMapBaseline(_) => {
                return Err("JPEG XL animated jhgm strip baseline is not supported".to_string());
            }
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
        ImageData::Hdr { hdr, .. } => Ok(*hdr),
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
    decode_jxl_bytes_to_image_data_impl(
        bytes,
        decode_target_hdr_capacity,
        display_hdr_target_capacity,
        tone_map,
        false,
    )
}

/// ISO `jhgm` directory-tree strip: decode primary only, skip gain-map codestream decode.
#[cfg(feature = "jpegxl")]
pub(crate) fn decode_jxl_strip_iso_gain_map_baseline(
    bytes: &[u8],
) -> Result<(Vec<u8>, u32, u32), String> {
    let tone_map = HdrToneMapSettings::default();
    match decode_jxl_bytes_to_image_data_impl(bytes, 1.0, 1.0, tone_map, true)? {
        ImageData::Static(mut decoded) => {
            Ok((decoded.take_rgba_owned(), decoded.width, decoded.height))
        }
        ImageData::Hdr { .. } | ImageData::HdrTiled { .. } | ImageData::HdrAnimated(_) => {
            Err("JPEG XL strip baseline expected Static image data".to_string())
        }
        ImageData::Animated(_) | ImageData::Tiled(_) => {
            Err("JPEG XL strip baseline does not support animation or tiling".to_string())
        }
    }
}

#[cfg(feature = "jpegxl")]
fn decode_jxl_bytes_to_image_data_impl(
    bytes: &[u8],
    decode_target_hdr_capacity: f32,
    display_hdr_target_capacity: f32,
    tone_map: HdrToneMapSettings,
    strip_baseline_only: bool,
) -> Result<ImageData, String> {
    let probe_len = bytes.len().clamp(2, 16);
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
                return jxl_finish_static_frame(JxlStaticFrameFinish {
                    rgba,
                    metadata,
                    width: info.xsize,
                    height: info.ysize,
                    jhgm_box: jhgm_box.as_deref(),
                    decode_target_hdr_capacity,
                    display_hdr_target_capacity,
                    tone_map: &tone_map,
                    strip_baseline_only,
                });
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
                if k_extra_channel_index.is_some()
                    && cmyk_source_icc.is_empty()
                    && let Some(icc) = jxl_decoder_copy_target_original_icc(decoder.0.cast_const())
                {
                    log::debug!(
                        "[JXL] captured {} byte CMYK source ICC for lcms2 transform",
                        icc.len()
                    );
                    cmyk_source_icc = icc;
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
                if !size.is_multiple_of(std::mem::size_of::<f32>()) {
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
                if !size.is_multiple_of(std::mem::size_of::<f32>()) {
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
                        && k_size.is_multiple_of(std::mem::size_of::<f32>())
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
                captured_frames.push((std::mem::take(&mut rgba_f32), pending_duration_ticks));
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
