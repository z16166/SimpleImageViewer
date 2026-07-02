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
use super::decode::decode_primary_heif_to_hdr;
use super::gain_map::{decode_heif_gain_map, heif_has_apple_hdr_gain_map_auxiliary};
use super::metadata::{
    inspect_heif_gain_map_auxiliaries, read_heif_metadata,
    refine_heif_transfer_for_primary_bit_depth,
};
use super::orientation::allocate_decode_options_for_heif_manual_geometry_fixup;
use super::session::open_heif_primary_from_bytes;

use crate::hdr::types::HdrColorProfile;
#[cfg(feature = "heif-native")]
use crate::hdr::types::{HdrImageBuffer, HdrToneMapSettings};
#[cfg(feature = "heif-native")]
use std::path::Path;

#[cfg(feature = "heif-native")]
use super::HeifHdrDecodeDiag;

#[cfg(feature = "heif-native")]
pub(crate) fn load_heif_embedded_sdr_primary(
    path: &Path,
    diag: HeifHdrDecodeDiag<'_>,
) -> Result<crate::loader::ImageData, String> {
    use super::embedded_sdr::build_heif_embedded_sdr_master_hdr;
    use super::thumbnail::decode_heif_primary_sdr_from_bytes;
    use crate::loader::apply_exif_orientation_to_hdr_pair;

    #[cfg(feature = "preload-debug")]
    let total_start = std::time::Instant::now();
    #[cfg(not(feature = "preload-debug"))]
    let _diag = diag;

    let mmap =
        crate::mmap_util::map_file(path).map_err(|err| format!("Failed to read HEIF: {err}"))?;
    let (decoded, logical) = decode_heif_primary_sdr_from_bytes(&mmap[..])?;
    let hdr = build_heif_embedded_sdr_master_hdr(&mmap[..], logical)?;
    let (hdr, fallback) = apply_exif_orientation_to_hdr_pair(path, hdr, decoded);

    #[cfg(feature = "preload-debug")]
    {
        let total_ms = total_start.elapsed().as_millis();
        let idx = diag
            .idx
            .map(|i| i.to_string())
            .unwrap_or_else(|| "-".to_string());
        let path_label = diag
            .path
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| path.display().to_string());
        crate::preload_debug!(
            "[PreloadDebug][HEIF] embedded_sdr_primary total_ms={total_ms} idx={idx} path={path_label} size={}x{}",
            hdr.width,
            hdr.height
        );
    }

    Ok(crate::loader::ImageData::Hdr {
        hdr: Box::new(hdr),
        fallback,
    })
}

#[cfg(feature = "heif-native")]
pub(crate) fn heif_should_use_embedded_sdr_primary_load(
    prefer_embedded_sdr_master: bool,
    hdr_target_capacity: f32,
) -> bool {
    prefer_embedded_sdr_master && crate::loader::hdr_display_requests_sdr_preview(hdr_target_capacity)
}

#[cfg(feature = "heif-native")]
pub(crate) fn load_heif_hdr(
    path: &Path,
    hdr_target_capacity: f32,
    tone_map: HdrToneMapSettings,
    diag: HeifHdrDecodeDiag<'_>,
) -> Result<crate::loader::ImageData, String> {
    let hdr = decode_heif_hdr(path, hdr_target_capacity, diag)?;
    let fallback = if crate::loader::hdr_display_requests_sdr_preview(hdr_target_capacity) {
        crate::loader::DecodedImage::new(
            hdr.width,
            hdr.height,
            crate::hdr::decode::hdr_to_sdr_rgba8_with_tone_settings(
                &hdr,
                tone_map.exposure_ev,
                &tone_map,
            )?,
        )
    } else {
        crate::loader::DecodedImage::new_sdr_deferred_placeholder(
            hdr.width,
            hdr.height,
            crate::loader::cheap_hdr_sdr_placeholder_rgba8(hdr.width, hdr.height)?,
        )
    };

    Ok(crate::loader::ImageData::Hdr {
        hdr: Box::new(hdr),
        fallback,
    })
}

#[cfg(feature = "heif-native")]
pub(crate) fn decode_heif_hdr(
    path: &Path,
    hdr_target_capacity: f32,
    diag: HeifHdrDecodeDiag<'_>,
) -> Result<HdrImageBuffer, String> {
    let mmap =
        crate::mmap_util::map_file(path).map_err(|err| format!("Failed to read HEIF: {err}"))?;
    let label = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("(unknown)");
    decode_heif_hdr_bytes(&mmap[..], hdr_target_capacity, label, diag)
}

#[cfg(feature = "heif-native")]
pub(crate) fn decode_heif_hdr_bytes(
    bytes: &[u8],
    hdr_target_capacity: f32,
    source_label: &str,
    #[cfg_attr(not(feature = "preload-debug"), allow(unused_variables))] diag: HeifHdrDecodeDiag<
        '_,
    >,
) -> Result<HdrImageBuffer, String> {
    #[cfg(feature = "preload-debug")]
    let total_start = std::time::Instant::now();
    #[cfg(feature = "preload-debug")]
    let mut phase_start = std::time::Instant::now();

    let (_ctx, handle) = open_heif_primary_from_bytes(bytes)?;

    let mut metadata = read_heif_metadata(handle.as_ptr());
    if let Some(diagnostic) = inspect_heif_gain_map_auxiliaries(handle.as_ptr()) {
        metadata.gain_map = Some(diagnostic);
    }
    refine_heif_transfer_for_primary_bit_depth(handle.as_ptr(), &mut metadata);
    crate::hdr::types::log_unrecognized_embedded_icc_after_decode(&metadata);

    let decode_geo_holder = allocate_decode_options_for_heif_manual_geometry_fixup(bytes);
    let decode_opts_ptr = decode_geo_holder
        .as_ref()
        .map(|g| g.as_ptr())
        .unwrap_or(std::ptr::null());

    #[cfg(feature = "preload-debug")]
    let open_ms = crate::preload_debug::elapsed_ms(phase_start);
    #[cfg(feature = "preload-debug")]
    {
        phase_start = std::time::Instant::now();
    }

    let mut hdr = decode_primary_heif_to_hdr(handle.as_ptr(), metadata, decode_opts_ptr)?;

    #[cfg(feature = "preload-debug")]
    let primary_decode_ms = crate::preload_debug::elapsed_ms(phase_start);
    #[cfg(feature = "preload-debug")]
    {
        phase_start = std::time::Instant::now();
    }

    // Apple HDR gain map: only decode the auxiliary plane when display headroom weight > 0
    // (SDR tone-mapped output keeps the primary plane; skip redundant libheif + CPU work).
    if heif_has_apple_hdr_gain_map_auxiliary(handle.as_ptr()) {
        let headroom = crate::hdr::heif_apple_gain_map::resolve_apple_hdr_headroom_from_exif(
            crate::hdr::heif_apple_gain_map::read_heif_exif_block(handle.as_ptr()).as_deref(),
        );
        if crate::hdr::heif_apple_gain_map::should_apply_apple_heic_gain_map(
            hdr_target_capacity,
            &headroom,
        ) {
            log::debug!(
                "[HDR] Apple HDR Gain Map: headroom={:.3}, gain={:.3}, stops={:.3}, target_capacity={:.3}",
                headroom.hdr_headroom,
                headroom.hdr_gain,
                headroom.stops,
                hdr_target_capacity,
            );
            if let Some((gain_w, gain_h, gain_rgba)) =
                decode_heif_gain_map(handle.as_ptr(), decode_opts_ptr)
            {
                let headroom_span = headroom.linear_headroom - 1.0;
                match crate::hdr::heif_apple_gain_map_gpu::attach_apple_heic_gpu_deferred(
                    hdr.clone(),
                    gain_w,
                    gain_h,
                    gain_rgba,
                    headroom_span,
                    headroom.stops,
                    hdr_target_capacity,
                ) {
                    Ok(new_hdr) => {
                        hdr = new_hdr;
                    }
                    Err(err) => {
                        log::warn!("[HDR] Apple HDR Gain Map GPU deferred attach failed: {err}");
                    }
                }
            }
        } else {
            log::debug!(
                "[HDR] Apple HDR Gain Map skipped (display weight is zero at capacity={:.3}, stops={:.3})",
                hdr_target_capacity,
                headroom.stops,
            );
        }
    }

    let cicp_px_tc = match &hdr.metadata.color_profile {
        HdrColorProfile::Cicp {
            color_primaries,
            transfer_characteristics,
            ..
        } => Some((*color_primaries, *transfer_characteristics)),
        _ => None,
    };
    let profile_tag = match &hdr.metadata.color_profile {
        HdrColorProfile::LinearSrgb => "LinearSrgb",
        HdrColorProfile::ColorSpace(_) => "ColorSpace",
        HdrColorProfile::Cicp { .. } => "Cicp",
        HdrColorProfile::Icc(_) => "Icc",
        HdrColorProfile::Unknown => "Unknown",
    };
    log::debug!(
        "[HEIF] {source_label}: {}×{} color_hint={:?} transfer={:?} profile={} cicp(primaries,transfer)={:?} mastering_max_nits={:?} gain_map_aux_seen={}",
        hdr.width,
        hdr.height,
        hdr.color_space,
        hdr.metadata.transfer_function,
        profile_tag,
        cicp_px_tc,
        hdr.metadata.luminance.mastering_max_nits,
        hdr.metadata.gain_map.is_some(),
    );

    #[cfg(feature = "preload-debug")]
    {
        let tone_map_ms = crate::preload_debug::elapsed_ms(phase_start);
        let total_ms = total_start.elapsed().as_millis();
        let idx = diag
            .idx
            .map(|i| i.to_string())
            .unwrap_or_else(|| "-".to_string());
        let path = diag
            .path
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| source_label.to_string());
        crate::preload_debug!(
            "[PreloadDebug][HEIF] decode_heif_hdr_bytes open_ms={open_ms} primary_decode_ms={primary_decode_ms} tone_map_ms={tone_map_ms} total_ms={total_ms} idx={idx} path={path}"
        );
    }

    Ok(hdr)
}
