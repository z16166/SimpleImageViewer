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

//! AVIF ISO gain-map decode without eager CPU composition (GPU deferred planes).

use crate::hdr::decode::{
    decode_transfer_to_display_linear, linear_primary_to_linear_srgb, linear_srgb_linear_to_srgb_u8,
};
use crate::hdr::gain_map::{GainMapMetadata, gain_map_metadata_diagnostic};
use crate::hdr::jpeg_gain_map_gpu::attach_iso_gain_map_gpu_deferred;
use crate::hdr::types::{
    DEFAULT_SDR_WHITE_NITS, HdrColorSpace, HdrImageBuffer, HdrImageMetadata, HdrLuminanceMetadata,
};

/// Build ISO forward gain-map baseline sRGB u8 samples from libavif RGBA16 output.
pub(crate) fn avif_build_iso_sdr_baseline_rgba8(
    rgba_u16: &[u16],
    rgb_out_depth: u32,
    width: u32,
    height: u32,
    metadata: &HdrImageMetadata,
    color_space: HdrColorSpace,
) -> Vec<u8> {
    let scale = rgb_channel_max_f(rgb_out_depth);
    let sdr_white = DEFAULT_SDR_WHITE_NITS;
    let mut sdr_rgba = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for x in 0..width {
            let index = (y as usize * width as usize + x as usize) * 4;
            let r = rgba_u16[index] as f32 / scale;
            let g = rgba_u16[index + 1] as f32 / scale;
            let b = rgba_u16[index + 2] as f32 / scale;
            let rgb_display_linear =
                decode_transfer_to_display_linear([r, g, b], metadata.transfer_function, sdr_white);
            let rgb_linear_srgb =
                linear_primary_to_linear_srgb(rgb_display_linear, color_space, metadata);
            sdr_rgba.push(linear_srgb_linear_to_srgb_u8(rgb_linear_srgb[0]));
            sdr_rgba.push(linear_srgb_linear_to_srgb_u8(rgb_linear_srgb[1]));
            sdr_rgba.push(linear_srgb_linear_to_srgb_u8(rgb_linear_srgb[2]));
            let a = rgba_u16[index + 3] as f32 / scale;
            sdr_rgba.push((a * 255.0_f32).round().clamp(0.0, 255.0) as u8);
        }
    }
    sdr_rgba
}

pub(crate) fn attach_avif_gain_map_gpu_deferred(
    width: u32,
    height: u32,
    sdr_rgba: Vec<u8>,
    gain_width: u32,
    gain_height: u32,
    gain_rgba: Vec<u8>,
    gain_metadata: GainMapMetadata,
    container_luminance: HdrLuminanceMetadata,
    target_hdr_capacity: f32,
) -> Result<HdrImageBuffer, String> {
    if gain_metadata.backward_direction {
        return Err(
            "AVIF ISO gain map has backward direction; deferred forward compose is invalid"
                .to_string(),
        );
    }
    log::debug!(
        "[HDR][AVIF] ISO gain map deferred metadata: {}",
        gain_map_metadata_diagnostic(gain_metadata, target_hdr_capacity)
    );
    let mut buffer = attach_iso_gain_map_gpu_deferred(
        "AVIF",
        width,
        height,
        sdr_rgba,
        gain_width,
        gain_height,
        gain_rgba,
        gain_metadata,
        target_hdr_capacity,
    )?;
    merge_avif_container_luminance(&mut buffer, container_luminance);
    Ok(buffer)
}

fn merge_avif_container_luminance(buffer: &mut HdrImageBuffer, container: HdrLuminanceMetadata) {
    if container.max_cll_nits.is_some() {
        buffer.metadata.luminance.max_cll_nits = container.max_cll_nits;
    }
    if container.max_fall_nits.is_some() {
        buffer.metadata.luminance.max_fall_nits = container.max_fall_nits;
    }
    if container.mastering_min_nits.is_some() {
        buffer.metadata.luminance.mastering_min_nits = container.mastering_min_nits;
    }
    if container.mastering_max_nits.is_some() {
        buffer.metadata.luminance.mastering_max_nits = container.mastering_max_nits;
    }
    if container.sdr_white_nits.is_some() {
        buffer.metadata.luminance.sdr_white_nits = container.sdr_white_nits;
    }
}

fn rgb_channel_max_f(depth: u32) -> f32 {
    match depth {
        8 => u8::MAX as f32,
        10 => 1023.0,
        12 => 4095.0,
        16 => u16::MAX as f32,
        other => (1_u32 << other.min(16)).saturating_sub(1) as f32,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::hdr::gain_map::{append_hdr_pixel_from_sdr_and_gain, sample_gain_map_rgb};
    use crate::hdr::types::{HdrGainMapMetadata, HdrPixelFormat, HdrTransferFunction};

    fn avif_compose_gain_map_cpu_reference(
        width: u32,
        height: u32,
        sdr_rgba: &[u8],
        gain_rgba: &[u8],
        gain_width: u32,
        gain_height: u32,
        gain_metadata: GainMapMetadata,
        container_luminance: HdrLuminanceMetadata,
        target_hdr_capacity: f32,
    ) -> HdrImageBuffer {
        let mut rgba_f32 = Vec::with_capacity(width as usize * height as usize * 4);
        for y in 0..height {
            for x in 0..width {
                let sdr_index = (y as usize * width as usize + x as usize) * 4;
                let gain_value =
                    sample_gain_map_rgb(gain_rgba, gain_width, gain_height, x, y, width, height);
                append_hdr_pixel_from_sdr_and_gain(
                    &mut rgba_f32,
                    &sdr_rgba[sdr_index..sdr_index + 4],
                    gain_value,
                    gain_metadata,
                    target_hdr_capacity,
                );
            }
        }
        let mut metadata = HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb);
        metadata.luminance = container_luminance;
        metadata.gain_map = Some(HdrGainMapMetadata {
            source: "AVIF",
            target_hdr_capacity: Some(target_hdr_capacity),
            diagnostic: gain_map_metadata_diagnostic(gain_metadata, target_hdr_capacity),
            capped_display_referred: false,
            apple_heic_deferred: None,
            iso_deferred: None,
        });
        HdrImageBuffer {
            width,
            height,
            format: HdrPixelFormat::Rgba32Float,
            color_space: HdrColorSpace::LinearSrgb,
            metadata,
            rgba_f32: Arc::new(rgba_f32),
        }
    }

    #[test]
    fn avif_baseline_builder_applies_oetf_for_linear_primary() {
        let mut meta = HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb);
        meta.transfer_function = HdrTransferFunction::Linear;
        let rgba_u16 = vec![32768_u16, 32768, 32768, 65535];
        let sdr = avif_build_iso_sdr_baseline_rgba8(
            &rgba_u16,
            16,
            1,
            1,
            &meta,
            HdrColorSpace::LinearSrgb,
        );
        assert_eq!(sdr.len(), 4);
        assert_eq!(sdr[3], 255);
        assert!(
            (sdr[0] as i32 - 188).abs() <= 1,
            "linear 0.5 → encoded ~188 (sRGB OETF), got {}",
            sdr[0]
        );
    }

    #[test]
    fn avif_deferred_baseline_matches_cpu_compose_input() {
        let gain_metadata = GainMapMetadata {
            gain_map_min: [0.0; 3],
            gain_map_max: [1.0; 3],
            gamma: [1.0; 3],
            offset_sdr: [1.0 / 64.0; 3],
            offset_hdr: [1.0 / 64.0; 3],
            hdr_capacity_min: 1.0,
            hdr_capacity_max: 4.0,
            backward_direction: false,
        };
        let sdr_rgba = vec![128_u8, 64, 32, 255, 200, 100, 50, 255];
        let gain_rgba = vec![128_u8, 128, 128, 255, 64, 64, 64, 255];
        let capacity = 4.0_f32;
        let deferred = attach_avif_gain_map_gpu_deferred(
            1,
            2,
            sdr_rgba.clone(),
            1,
            2,
            gain_rgba.clone(),
            gain_metadata,
            HdrLuminanceMetadata::default(),
            capacity,
        )
        .expect("attach");
        assert!(deferred.rgba_f32.is_empty());
        let iso_deferred = deferred
            .metadata
            .gain_map
            .as_ref()
            .and_then(|gm| gm.iso_deferred.as_ref())
            .expect("iso deferred");
        assert_eq!(iso_deferred.sdr_rgba.as_slice(), sdr_rgba.as_slice());
        assert_eq!(iso_deferred.gain_rgba.as_slice(), gain_rgba.as_slice());

        let cpu = avif_compose_gain_map_cpu_reference(
            1,
            2,
            &sdr_rgba,
            &gain_rgba,
            1,
            2,
            gain_metadata,
            HdrLuminanceMetadata::default(),
            capacity,
        );
        assert_eq!(cpu.rgba_f32.len(), 8);
        assert!(cpu.rgba_f32[0].is_finite());
    }

    #[test]
    fn avif_deferred_rejects_backward_direction() {
        let gain_metadata = GainMapMetadata {
            gain_map_min: [0.0; 3],
            gain_map_max: [1.0; 3],
            gamma: [1.0; 3],
            offset_sdr: [1.0 / 64.0; 3],
            offset_hdr: [1.0 / 64.0; 3],
            hdr_capacity_min: 1.0,
            hdr_capacity_max: 4.0,
            backward_direction: true,
        };
        let err = attach_avif_gain_map_gpu_deferred(
            1,
            1,
            vec![128; 4],
            1,
            1,
            vec![128; 4],
            gain_metadata,
            HdrLuminanceMetadata::default(),
            4.0,
        )
        .expect_err("backward");
        assert!(err.contains("backward"));
    }
}
