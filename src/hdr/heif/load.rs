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
use super::embedded_sdr::heif_embedded_sdr_master_hdr_from_metadata;
use super::gain_map::{decode_heif_gain_map, heif_has_apple_hdr_gain_map_auxiliary};
use super::metadata::read_heif_opened_primary_metadata;
use super::orientation::heif_manual_geometry_decode_options;
use super::session::{HeifPrimaryGuard, open_heif_primary_from_bytes};
use super::thumbnail::{
    decode_heif_handle_to_rgba8, decode_heif_primary_sdr_from_handle, primary_logical_size,
};

use crate::hdr::types::{HdrColorProfile, HdrImageMetadata};
#[cfg(feature = "heif-native")]
use crate::hdr::types::{HdrImageBuffer, HdrToneMapSettings};
use crate::loader::preview_aspect_matches_logical;
#[cfg(feature = "heif-native")]
use std::path::Path;

#[cfg(feature = "heif-native")]
use super::HeifHdrDecodeDiag;

#[cfg(feature = "heif-native")]
struct HeifEmbeddedSdrFailure {
    err: String,
    /// Valid-aspect 8-bit primary from embedded-SDR attempt; never set on aspect mismatch.
    recovered_sdr: Option<crate::loader::DecodedImage>,
}

#[cfg(feature = "heif-native")]
fn heif_recovered_sdr_usable_for_logical(
    decoded: &crate::loader::DecodedImage,
    logical: (u32, u32),
) -> bool {
    preview_aspect_matches_logical(decoded.width, decoded.height, logical.0, logical.1)
}

#[cfg(feature = "heif-native")]
#[cfg_attr(not(test), allow(dead_code))]
fn heif_load_skips_primary_hdr_decode_at_capacity(hdr_target_capacity: f32) -> bool {
    crate::loader::hdr_display_requests_sdr_preview(hdr_target_capacity)
}

#[cfg(feature = "heif-native")]
fn try_heif_embedded_sdr_primary_from_opened(
    handle: *const libheif_sys::heif_image_handle,
    decode_opts_ptr: *const libheif_sys::heif_decoding_options,
    primary_metadata: &HdrImageMetadata,
    #[cfg_attr(not(feature = "preload-debug"), allow(unused_variables))] diag: HeifHdrDecodeDiag<
        '_,
    >,
) -> Result<(HdrImageBuffer, crate::loader::DecodedImage), HeifEmbeddedSdrFailure> {
    #[cfg(feature = "preload-debug")]
    let total_start = std::time::Instant::now();

    let logical = primary_logical_size(handle);
    if logical.0 == 0 || logical.1 == 0 {
        return Err(HeifEmbeddedSdrFailure {
            err: "HEIF primary has zero logical size".to_string(),
            recovered_sdr: None,
        });
    }

    let decoded = match decode_heif_handle_to_rgba8(handle, decode_opts_ptr) {
        Ok(decoded) => decoded,
        Err(err) => {
            return Err(HeifEmbeddedSdrFailure {
                err,
                recovered_sdr: None,
            });
        }
    };

    if !heif_recovered_sdr_usable_for_logical(&decoded, logical) {
        return Err(HeifEmbeddedSdrFailure {
            err: format!(
                "HEIF primary SDR aspect mismatch: {}x{} vs logical {}x{}",
                decoded.width, decoded.height, logical.0, logical.1
            ),
            recovered_sdr: None,
        });
    }

    let hdr = heif_embedded_sdr_master_hdr_from_metadata(primary_metadata.clone(), logical);

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
            .unwrap_or_else(|| "(unknown)".to_string());
        crate::preload_debug!(
            "[PreloadDebug][HEIF] embedded_sdr_primary total_ms={total_ms} idx={idx} path={path_label} size={}x{}",
            hdr.width,
            hdr.height
        );
    }

    Ok((hdr, decoded))
}

/// SDR monitor load: 8-bit primary + embedded HDR shell only (no 16-bit float primary decode).
#[cfg(feature = "heif-native")]
fn finish_heif_sdr_capacity_from_opened_primary(
    handle: *const libheif_sys::heif_image_handle,
    decode_opts_ptr: *const libheif_sys::heif_decoding_options,
    recovered_sdr: Option<crate::loader::DecodedImage>,
    primary_metadata: &HdrImageMetadata,
) -> Result<(HdrImageBuffer, crate::loader::DecodedImage), String> {
    let logical = primary_logical_size(handle);
    if logical.0 == 0 || logical.1 == 0 {
        return Err("HEIF primary has zero logical size".to_string());
    }

    if let Some(decoded) = recovered_sdr
        && heif_recovered_sdr_usable_for_logical(&decoded, logical)
    {
        let hdr = heif_embedded_sdr_master_hdr_from_metadata(primary_metadata.clone(), logical);
        return Ok((hdr, decoded));
    }

    let (decoded, _) = decode_heif_primary_sdr_from_handle(handle, decode_opts_ptr)?;
    let hdr = heif_embedded_sdr_master_hdr_from_metadata(primary_metadata.clone(), logical);
    Ok((hdr, decoded))
}

#[cfg(feature = "heif-native")]
fn heif_image_data_fallback(
    hdr: &HdrImageBuffer,
    recovered_sdr: Option<crate::loader::DecodedImage>,
) -> Result<crate::loader::DecodedImage, String> {
    if let Some(decoded) = recovered_sdr
        && heif_recovered_sdr_usable_for_logical(&decoded, (hdr.width, hdr.height))
    {
        return Ok(decoded);
    }
    let fb = crate::loader::hdr_sdr_fallback_rgba8_or_placeholder(hdr)?;
    Ok(crate::loader::DecodedImage::from_hdr_sdr_fallback(
        hdr.width, hdr.height, fb,
    ))
}

/// Single libheif open: optional embedded-SDR fast path, then full HDR decode on fallback.
#[cfg(feature = "heif-native")]
pub(crate) fn load_heif_with_optional_embedded_sdr_from_bytes(
    bytes: &[u8],
    path: &Path,
    hdr_target_capacity: f32,
    diag: HeifHdrDecodeDiag<'_>,
    try_embedded_sdr_master: bool,
) -> Result<crate::loader::ImageData, String> {
    let (ctx, primary) = open_heif_primary_from_bytes(bytes)?;
    let (_decode_geo_holder, decode_opts_ptr) = heif_manual_geometry_decode_options(bytes);
    let handle = primary.as_ptr();
    let primary_metadata = read_heif_opened_primary_metadata(handle);

    let mut recovered_sdr = None;
    if try_embedded_sdr_master {
        match try_heif_embedded_sdr_primary_from_opened(
            handle,
            decode_opts_ptr,
            &primary_metadata,
            diag,
        ) {
            Ok((hdr, decoded)) => {
                return Ok(crate::loader::ImageData::Hdr {
                    hdr: Box::new(hdr),
                    fallback: decoded,
                });
            }
            Err(failure) => {
                recovered_sdr = failure.recovered_sdr;
                crate::loader::embedded_sdr_fallback::log_embedded_sdr_master_fallback(
                    "HEIF",
                    path,
                    &failure.err,
                );
            }
        }
    }

    let _ctx = ctx;
    if crate::loader::should_use_embedded_sdr_master_load(
        try_embedded_sdr_master,
        hdr_target_capacity,
    ) {
        // Embedded-SDR master on SDR output: 8-bit primary + HDR shell only (no float plane).
        #[cfg(feature = "preload-debug")]
        crate::preload_debug!(
            "[PreloadDebug][HEIF] sdr_capacity_load skip_primary_hdr_decode path={}",
            path.display()
        );
        let (hdr, fallback) = finish_heif_sdr_capacity_from_opened_primary(
            primary.as_ptr(),
            decode_opts_ptr,
            recovered_sdr,
            &primary_metadata,
        )?;
        return Ok(crate::loader::ImageData::Hdr {
            hdr: Box::new(hdr),
            fallback,
        });
    }

    let label = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("(unknown)");
    let hdr = decode_heif_hdr_from_opened_primary(
        &primary,
        hdr_target_capacity,
        label,
        diag,
        decode_opts_ptr,
        primary_metadata,
    )?;
    let fallback = heif_image_data_fallback(&hdr, recovered_sdr)?;

    Ok(crate::loader::ImageData::Hdr {
        hdr: Box::new(hdr),
        fallback,
    })
}

#[cfg(feature = "heif-native")]
pub(crate) fn load_heif_embedded_sdr_primary_from_bytes(
    bytes: &[u8],
    diag: HeifHdrDecodeDiag<'_>,
) -> Result<crate::loader::ImageData, String> {
    let (ctx, primary) = open_heif_primary_from_bytes(bytes)?;
    let (_decode_geo_holder, decode_opts_ptr) = heif_manual_geometry_decode_options(bytes);
    let primary_metadata = read_heif_opened_primary_metadata(primary.as_ptr());
    let (hdr, decoded) = match try_heif_embedded_sdr_primary_from_opened(
        primary.as_ptr(),
        decode_opts_ptr,
        &primary_metadata,
        diag,
    ) {
        Ok(pair) => pair,
        Err(failure) => return Err(failure.err),
    };
    let _ctx = ctx;
    Ok(crate::loader::ImageData::Hdr {
        hdr: Box::new(hdr),
        fallback: decoded,
    })
}

#[cfg(feature = "heif-native")]
#[allow(dead_code)] // Path-based wrapper; production uses `load_heif_embedded_sdr_primary_from_bytes`.
pub(crate) fn load_heif_embedded_sdr_primary(
    path: &Path,
    diag: HeifHdrDecodeDiag<'_>,
) -> Result<crate::loader::ImageData, String> {
    let mmap =
        crate::mmap_util::map_file(path).map_err(|err| format!("Failed to read HEIF: {err}"))?;
    load_heif_embedded_sdr_primary_from_bytes(&mmap[..], diag)
}

#[cfg(feature = "heif-native")]
pub(crate) fn load_heif_hdr_from_bytes(
    bytes: &[u8],
    path: &Path,
    hdr_target_capacity: f32,
    _tone_map: HdrToneMapSettings,
    diag: HeifHdrDecodeDiag<'_>,
) -> Result<crate::loader::ImageData, String> {
    load_heif_with_optional_embedded_sdr_from_bytes(bytes, path, hdr_target_capacity, diag, false)
}

#[cfg(feature = "heif-native")]
pub(crate) fn heif_should_use_embedded_sdr_primary_load(
    prefer_embedded_sdr_master: bool,
    hdr_target_capacity: f32,
) -> bool {
    crate::loader::should_use_embedded_sdr_master_load(
        prefer_embedded_sdr_master,
        hdr_target_capacity,
    )
}

#[cfg(feature = "heif-native")]
#[allow(dead_code)] // Path-based wrapper; production uses `load_heif_hdr_from_bytes`.
pub(crate) fn load_heif_hdr(
    path: &Path,
    hdr_target_capacity: f32,
    tone_map: HdrToneMapSettings,
    diag: HeifHdrDecodeDiag<'_>,
) -> Result<crate::loader::ImageData, String> {
    let mmap =
        crate::mmap_util::map_file(path).map_err(|err| format!("Failed to read HEIF: {err}"))?;
    load_heif_hdr_from_bytes(&mmap[..], path, hdr_target_capacity, tone_map, diag)
}

#[cfg(feature = "heif-native")]
#[allow(dead_code)] // Used by tests and path-based `load_heif_hdr`.
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
    diag: HeifHdrDecodeDiag<'_>,
) -> Result<HdrImageBuffer, String> {
    let (_ctx, primary) = open_heif_primary_from_bytes(bytes)?;
    let (_decode_geo_holder, decode_opts_ptr) = heif_manual_geometry_decode_options(bytes);
    let primary_metadata = read_heif_opened_primary_metadata(primary.as_ptr());
    decode_heif_hdr_from_opened_primary(
        &primary,
        hdr_target_capacity,
        source_label,
        diag,
        decode_opts_ptr,
        primary_metadata,
    )
}

#[cfg(feature = "heif-native")]
fn decode_heif_hdr_from_opened_primary(
    primary: &HeifPrimaryGuard,
    hdr_target_capacity: f32,
    source_label: &str,
    #[cfg_attr(not(feature = "preload-debug"), allow(unused_variables))] diag: HeifHdrDecodeDiag<
        '_,
    >,
    decode_opts_ptr: *const libheif_sys::heif_decoding_options,
    metadata: HdrImageMetadata,
) -> Result<HdrImageBuffer, String> {
    #[cfg(feature = "preload-debug")]
    let total_start = std::time::Instant::now();
    #[cfg(feature = "preload-debug")]
    let mut phase_start = std::time::Instant::now();

    let handle = primary.as_ptr();
    crate::hdr::types::log_unrecognized_embedded_icc_after_decode(&metadata);

    #[cfg(feature = "preload-debug")]
    let open_ms = crate::preload_debug::elapsed_ms(phase_start);
    #[cfg(feature = "preload-debug")]
    {
        phase_start = std::time::Instant::now();
    }

    let mut hdr = decode_primary_heif_to_hdr(handle, metadata, decode_opts_ptr)?;

    #[cfg(feature = "preload-debug")]
    let primary_decode_ms = crate::preload_debug::elapsed_ms(phase_start);
    #[cfg(feature = "preload-debug")]
    {
        phase_start = std::time::Instant::now();
    }

    // Apple HDR gain map: only decode the auxiliary plane when display headroom weight > 0
    // (SDR tone-mapped output keeps the primary plane; skip redundant libheif + CPU work).
    if heif_has_apple_hdr_gain_map_auxiliary(handle) {
        let headroom = crate::hdr::heif_apple_gain_map::resolve_apple_hdr_headroom_from_exif(
            crate::hdr::heif_apple_gain_map::read_heif_exif_block(handle).as_deref(),
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
            if let Some((gain_w, gain_h, gain_rgba)) = decode_heif_gain_map(handle, decode_opts_ptr)
            {
                let headroom_span = headroom.linear_headroom - 1.0;
                match crate::hdr::heif_apple_gain_map_gpu::attach_apple_heic_gpu_deferred(
                    &hdr,
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

#[cfg(all(feature = "heif-native", test))]
mod tests {
    use super::*;
    use crate::hdr::types::{HdrImageBuffer, HdrImageMetadata, HdrPixelFormat};
    use crate::loader::DecodedImage;
    use std::sync::Arc;

    #[test]
    fn heif_load_skips_primary_hdr_decode_only_at_sdr_capacity() {
        assert!(heif_load_skips_primary_hdr_decode_at_capacity(1.0));
        assert!(heif_load_skips_primary_hdr_decode_at_capacity(1.001));
        assert!(!heif_load_skips_primary_hdr_decode_at_capacity(1.01));
        assert!(!heif_load_skips_primary_hdr_decode_at_capacity(4.0));
    }

    #[test]
    fn heif_sdr_capacity_still_decodes_float_plane_for_hdr_tone_map_mode() {
        assert!(!crate::loader::should_use_embedded_sdr_master_load(
            false, 1.0
        ));
        assert!(crate::loader::should_use_embedded_sdr_master_load(
            true, 1.0
        ));
    }

    #[test]
    fn heif_fallback_rejects_recovered_sdr_with_aspect_mismatch() {
        let recovered = DecodedImage::new(4, 2, vec![0; 4 * 2 * 4]);
        let hdr = HdrImageBuffer {
            width: 2,
            height: 2,
            format: HdrPixelFormat::Rgba32Float,
            color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
            metadata: HdrImageMetadata::default(),
            rgba_f32: Arc::new(vec![1.0; 16]),
        };
        let fallback = heif_image_data_fallback(&hdr, Some(recovered)).expect("fallback");
        assert!(fallback.is_sdr_deferred_placeholder());
    }

    #[test]
    fn heif_fallback_reuses_recovered_embedded_sdr_primary() {
        let recovered = DecodedImage::new(2, 2, [200_u8, 100, 50, 255].repeat(4));
        let hdr = HdrImageBuffer {
            width: 2,
            height: 2,
            format: HdrPixelFormat::Rgba32Float,
            color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
            metadata: HdrImageMetadata::default(),
            rgba_f32: Arc::new(vec![1.0; 16]),
        };
        let fallback = heif_image_data_fallback(&hdr, Some(recovered.clone())).expect("fallback");
        assert_eq!(fallback.rgba()[0], 200);
        assert!(!fallback.is_sdr_deferred_placeholder());
    }

    #[test]
    fn heif_fallback_without_recovered_sdr_uses_placeholder_for_apple_deferred() {
        use crate::hdr::types::{AppleHeicGainMapGpuSource, HdrGainMapMetadata};

        let metadata = HdrImageMetadata {
            gain_map: Some(HdrGainMapMetadata {
                source: "HEIF",
                target_hdr_capacity: None,
                diagnostic: String::new(),
                capped_display_referred: false,
                apple_heic_deferred: Some(AppleHeicGainMapGpuSource {
                    gain_rgba: Arc::new(vec![0; 4]),
                    gain_width: 1,
                    gain_height: 1,
                    headroom_span: 1.0,
                    stops: 2.0,
                }),
                iso_deferred: None,
            }),
            ..Default::default()
        };
        let hdr = HdrImageBuffer {
            width: 1,
            height: 1,
            format: HdrPixelFormat::Rgba32Float,
            color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
            metadata,
            rgba_f32: Arc::new(vec![0.5; 4]),
        };
        let fallback = heif_image_data_fallback(&hdr, None).expect("fallback");
        assert!(fallback.is_sdr_deferred_placeholder());
    }
}
