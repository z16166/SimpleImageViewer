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

//! JPEG XL `jhgm` box: defer ISO gain-map compose to the shared GPU path.

use std::sync::Arc;

use crate::hdr::decode::{linear_primary_to_linear_srgb, linear_srgb_linear_to_srgb_u8};
use crate::hdr::gain_map::{
    GainMapMetadata, append_hdr_pixel_from_sdr_and_gain, gain_map_metadata_diagnostic,
    iso_gain_map_skips_forward_compose, parse_iso_gain_map_metadata, sample_gain_map_rgb,
};
use crate::hdr::jpeg_gain_map_gpu::attach_iso_gain_map_gpu_deferred;
use crate::hdr::jpegxl::{
    JxlGainMapBundleRef, decode_jxl_gain_map_from_bundle, read_jxl_gain_map_bundle, srgb_unit_to_u8,
};
use crate::hdr::types::{
    HdrColorSpace, HdrGainMapMetadata, HdrImageBuffer, HdrImageMetadata, HdrPixelFormat,
    HdrTransferFunction,
};

/// Parsed `jhgm` box contents (metadata + bundle slices) for reuse across GPU/CPU paths.
pub(crate) struct JxlJhgmParsed<'a> {
    bundle: JxlGainMapBundleRef<'a>,
    metadata: GainMapMetadata,
    skips_forward_compose: bool,
}

pub(crate) fn parse_jxl_jhgm_box(jhgm_box: &[u8]) -> Result<JxlJhgmParsed<'_>, String> {
    let bundle = read_jxl_gain_map_bundle(jhgm_box)?;
    let metadata = parse_iso_gain_map_metadata(bundle.metadata)?;
    let skips_forward_compose = iso_gain_map_skips_forward_compose(metadata);
    Ok(JxlJhgmParsed {
        bundle,
        metadata,
        skips_forward_compose,
    })
}

/// Result of applying a JPEG XL `jhgm` box to one decoded primary frame.
pub(crate) enum JxlJhgmFrameOutcome {
    /// No `jhgm` box, malformed metadata, or CPU compose failure — caller builds HDR normally.
    Unprocessed,
    /// Primary codestream is already the HDR base rendition (Adobe `*_base_hdr.jxl`).
    PrecomposedHdr(HdrImageBuffer),
    /// ISO gain-map planes deferred to the shared GPU compose path.
    GpuDeferred(HdrImageBuffer),
    /// Forward gain map composed on the CPU.
    CpuComposed(HdrImageBuffer),
}

/// When ISO headroom indicates the primary codestream is already the HDR base rendition.
pub(crate) fn jxl_jhgm_primary_is_precomposed_hdr(jhgm_box: &[u8]) -> Result<bool, String> {
    Ok(parse_jxl_jhgm_box(jhgm_box)?.skips_forward_compose)
}

/// Quantize libjxl primary floats into ISO gain-map baseline sRGB u8 samples.
pub(crate) fn jxl_rgba_f32_to_iso_sdr_baseline(
    rgba_f32: &[f32],
    color_space: HdrColorSpace,
    metadata: &HdrImageMetadata,
) -> Vec<u8> {
    let needs_srgb_oetf = match metadata.transfer_function {
        HdrTransferFunction::Linear => true,
        HdrTransferFunction::Srgb
        | HdrTransferFunction::Gamma
        | HdrTransferFunction::Bt709
        | HdrTransferFunction::Unknown => false,
        HdrTransferFunction::Pq | HdrTransferFunction::Hlg => {
            debug_assert!(
                false,
                "ISO forward gain-map baseline is expected to be SDR-linear or sRGB-encoded"
            );
            true
        }
    };

    let mut sdr_rgba = Vec::with_capacity(rgba_f32.len());
    for px in rgba_f32.chunks_exact(4) {
        let mapped = linear_primary_to_linear_srgb([px[0], px[1], px[2]], color_space, metadata);
        if needs_srgb_oetf {
            sdr_rgba.push(linear_srgb_linear_to_srgb_u8(mapped[0]));
            sdr_rgba.push(linear_srgb_linear_to_srgb_u8(mapped[1]));
            sdr_rgba.push(linear_srgb_linear_to_srgb_u8(mapped[2]));
        } else {
            sdr_rgba.push(srgb_unit_to_u8(mapped[0]));
            sdr_rgba.push(srgb_unit_to_u8(mapped[1]));
            sdr_rgba.push(srgb_unit_to_u8(mapped[2]));
        }
        let alpha = if px[3].is_finite() {
            (px[3].clamp(0.0, 1.0) * 255.0).round() as u8
        } else {
            255
        };
        sdr_rgba.push(alpha);
    }
    sdr_rgba
}

fn apply_jxl_jhgm_gain_map_gpu_deferred(
    parsed: &JxlJhgmParsed<'_>,
    target_hdr_capacity: f32,
    base_rgba_f32: &[f32],
    width: u32,
    height: u32,
    color_space: HdrColorSpace,
    metadata: &HdrImageMetadata,
) -> Result<HdrImageBuffer, String> {
    if parsed.skips_forward_compose {
        log::debug!(
            "[HDR] JPEG XL jhgm: primary codestream is precomposed HDR base; skipping forward gain-map compose"
        );
        return Err("jhgm primary is precomposed HDR base".to_string());
    }

    let expected_len = width as usize * height as usize * 4;
    if base_rgba_f32.len() != expected_len {
        return Err(format!(
            "JPEG XL jhgm base buffer length mismatch: got {}, expected {}",
            base_rgba_f32.len(),
            expected_len
        ));
    }

    let (gain_metadata, gain_width, gain_height, gain_rgba) =
        decode_jxl_gain_map_from_bundle(&parsed.bundle, parsed.metadata, target_hdr_capacity)?;
    let sdr_rgba = jxl_rgba_f32_to_iso_sdr_baseline(base_rgba_f32, color_space, metadata);
    attach_iso_gain_map_gpu_deferred(
        "JPEG XL",
        width,
        height,
        sdr_rgba,
        gain_width,
        gain_height,
        gain_rgba,
        gain_metadata,
        target_hdr_capacity,
    )
}

/// When a `jhgm` box is present, build GPU-deferred planes instead of CPU-composing `rgba_f32`.
pub(crate) fn apply_jxl_jhgm_gain_map_gpu_deferred_if_present(
    jhgm_box: Option<&[u8]>,
    target_hdr_capacity: f32,
    base_rgba_f32: &[f32],
    width: u32,
    height: u32,
    color_space: HdrColorSpace,
    metadata: &HdrImageMetadata,
) -> Result<Option<HdrImageBuffer>, String> {
    let Some(jhgm_box) = jhgm_box else {
        return Ok(None);
    };
    let parsed = parse_jxl_jhgm_box(jhgm_box)?;
    match apply_jxl_jhgm_gain_map_gpu_deferred(
        &parsed,
        target_hdr_capacity,
        base_rgba_f32,
        width,
        height,
        color_space,
        metadata,
    ) {
        Ok(deferred) => Ok(Some(deferred)),
        Err(err) if err.contains("precomposed HDR base") => Ok(None),
        Err(err) => Err(err),
    }
}

fn jxl_hdr_buffer_from_rgba(
    rgba: Vec<f32>,
    width: u32,
    height: u32,
    metadata: HdrImageMetadata,
) -> HdrImageBuffer {
    HdrImageBuffer {
        width,
        height,
        format: HdrPixelFormat::Rgba32Float,
        color_space: metadata.color_space_hint(),
        metadata,
        rgba_f32: Arc::new(rgba),
    }
}

fn apply_jxl_jhgm_cpu_compose(
    parsed: &JxlJhgmParsed<'_>,
    target_hdr_capacity: f32,
    rgba_f32: &[f32],
    width: u32,
    height: u32,
    metadata: &HdrImageMetadata,
) -> Result<HdrImageBuffer, String> {
    let expected_len = width as usize * height as usize * 4;
    let color_space = metadata.color_space_hint();
    let (gain_metadata, gain_width, gain_height, gain_rgba) =
        decode_jxl_gain_map_from_bundle(&parsed.bundle, parsed.metadata, target_hdr_capacity)?;
    let diagnostic = gain_map_metadata_diagnostic(gain_metadata, target_hdr_capacity);
    let sdr_baseline = jxl_rgba_f32_to_iso_sdr_baseline(rgba_f32, color_space, metadata);
    let mut composed = Vec::with_capacity(expected_len);
    for y in 0..height {
        for x in 0..width {
            let index = (y as usize * width as usize + x as usize) * 4;
            let sdr_rgba = [
                sdr_baseline[index],
                sdr_baseline[index + 1],
                sdr_baseline[index + 2],
                sdr_baseline[index + 3],
            ];
            let gain_value =
                sample_gain_map_rgb(&gain_rgba, gain_width, gain_height, x, y, width, height);
            append_hdr_pixel_from_sdr_and_gain(
                &mut composed,
                &sdr_rgba,
                gain_value,
                gain_metadata,
                target_hdr_capacity,
            );
        }
    }
    let mut frame_metadata = metadata.clone();
    frame_metadata.gain_map = Some(HdrGainMapMetadata {
        source: "JPEG XL",
        target_hdr_capacity: Some(target_hdr_capacity),
        diagnostic,
        capped_display_referred: false,
        apple_heic_deferred: None,
        iso_deferred: None,
    });
    Ok(jxl_hdr_buffer_from_rgba(
        composed,
        width,
        height,
        frame_metadata,
    ))
}

/// Shared static/animation path: GPU deferred, precomposed skip, or CPU ISO compose.
pub(crate) fn finish_jxl_jhgm_frame(
    jhgm_box: Option<&[u8]>,
    target_hdr_capacity: f32,
    rgba: &[f32],
    width: u32,
    height: u32,
    metadata: &HdrImageMetadata,
) -> JxlJhgmFrameOutcome {
    let Some(jhgm_box) = jhgm_box else {
        return JxlJhgmFrameOutcome::Unprocessed;
    };

    let parsed = match parse_jxl_jhgm_box(jhgm_box) {
        Ok(parsed) => parsed,
        Err(err) => {
            log::warn!("[HDR] JPEG XL jhgm metadata: {err}");
            return JxlJhgmFrameOutcome::Unprocessed;
        }
    };

    if parsed.skips_forward_compose {
        log::debug!(
            "[HDR] JPEG XL jhgm: primary codestream is precomposed HDR base; skipping forward gain-map compose"
        );
        return JxlJhgmFrameOutcome::PrecomposedHdr(jxl_hdr_buffer_from_rgba(
            rgba.to_vec(),
            width,
            height,
            metadata.clone(),
        ));
    }

    let color_space = metadata.color_space_hint();
    match apply_jxl_jhgm_gain_map_gpu_deferred(
        &parsed,
        target_hdr_capacity,
        rgba,
        width,
        height,
        color_space,
        metadata,
    ) {
        Ok(deferred) => return JxlJhgmFrameOutcome::GpuDeferred(deferred),
        Err(err) => {
            log::warn!("[HDR] JPEG XL jhgm GPU deferred setup failed: {err}; using CPU compose");
        }
    }

    match apply_jxl_jhgm_cpu_compose(&parsed, target_hdr_capacity, rgba, width, height, metadata) {
        Ok(hdr) => JxlJhgmFrameOutcome::CpuComposed(hdr),
        Err(err) => {
            log::warn!("[HDR] JPEG XL jhgm gain-map fallback: {err}");
            JxlJhgmFrameOutcome::Unprocessed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hdr::types::HdrColorProfile;

    #[test]
    fn jxl_baseline_extract_applies_oetf_for_linear_primary() {
        let rgba = vec![1.0_f32, 0.5, 0.0, 1.0];
        let meta = HdrImageMetadata {
            transfer_function: HdrTransferFunction::Linear,
            color_profile: HdrColorProfile::LinearSrgb,
            ..HdrImageMetadata::default()
        };
        let baseline = jxl_rgba_f32_to_iso_sdr_baseline(&rgba, HdrColorSpace::LinearSrgb, &meta);
        assert_eq!(baseline.len(), 4);
        assert!(baseline[0] > baseline[1]);
        assert!(baseline[1] > baseline[2]);
        assert_eq!(baseline[3], 255);
    }

    #[test]
    fn jxl_baseline_extract_direct_quantizes_srgb_encoded_primary() {
        let rgba = vec![1.0_f32, 0.5, 0.25, 1.0];
        let meta = HdrImageMetadata {
            transfer_function: HdrTransferFunction::Srgb,
            color_profile: HdrColorProfile::LinearSrgb,
            ..HdrImageMetadata::default()
        };
        let baseline = jxl_rgba_f32_to_iso_sdr_baseline(&rgba, HdrColorSpace::LinearSrgb, &meta);
        assert_eq!(baseline, vec![255, 128, 64, 255]);
    }

    #[test]
    fn jxl_gpu_deferred_without_jhgm_box_returns_none() {
        let rgba = vec![1.0_f32, 0.5, 0.25, 1.0];
        let meta = HdrImageMetadata::default();
        let out = apply_jxl_jhgm_gain_map_gpu_deferred_if_present(
            None,
            4.0,
            &rgba,
            1,
            1,
            HdrColorSpace::LinearSrgb,
            &meta,
        )
        .expect("query");
        assert!(out.is_none());
    }

    #[test]
    fn probe_adobe_jxl_gain_map_sample_when_present() {
        use std::path::PathBuf;

        use crate::hdr::jpegxl::decode_jxl_bytes_to_image_data;
        use crate::hdr::types::HdrToneMapSettings;
        use crate::loader::ImageData;

        let path = PathBuf::from(
            r"F:\HDR\Gain_Map_Sample_Photos\Gain_Map_Sample_Photos\samples_jxl_base_hdr\03_base_hdr.jxl",
        );
        if !path.is_file() {
            eprintln!("skip probe; sample missing: {}", path.display());
            return;
        }

        let bytes = std::fs::read(&path).expect("read jxl");
        let tone = HdrToneMapSettings::default();
        let capacity = tone.target_hdr_capacity();
        let image =
            decode_jxl_bytes_to_image_data(&bytes, capacity, capacity, tone).expect("decode");
        let ImageData::Hdr { hdr, .. } = image else {
            panic!("expected static HDR");
        };

        eprintln!(
            "transfer={:?} peak_nits={:?} rgba_f32 len={} gain_map={:?}",
            hdr.metadata.transfer_function,
            hdr.metadata.luminance.mastering_max_nits,
            hdr.rgba_f32.len(),
            hdr.metadata
                .gain_map
                .as_ref()
                .map(|gm| gm.diagnostic.as_str()),
        );

        assert!(
            !hdr.rgba_f32.is_empty(),
            "Adobe base_hdr primary should display libjxl floats directly"
        );
        assert!(
            hdr.metadata.gain_map.is_none(),
            "precomposed HDR base must not attach forward gain-map compose"
        );
    }
}
