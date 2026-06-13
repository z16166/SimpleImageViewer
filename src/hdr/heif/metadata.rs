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
use super::brand::heif_nclx_to_metadata;
use super::gain_map::HeifAuxiliaryImageHandle;

#[cfg(feature = "heif-native")]
use crate::hdr::types::HdrGainMapMetadata;
use crate::hdr::types::{
    HdrColorProfile, HdrImageMetadata, HdrLuminanceMetadata, HdrReference, HdrTransferFunction,
};
#[cfg(feature = "heif-native")]
use std::ffi::CStr;
#[cfg(feature = "heif-native")]
use std::sync::Arc;

pub(crate) fn read_heif_metadata(
    handle: *const libheif_sys::heif_image_handle,
) -> HdrImageMetadata {
    let mut nclx_ptr = std::ptr::null_mut();
    let nclx_status =
        unsafe { libheif_sys::heif_image_handle_get_nclx_color_profile(handle, &mut nclx_ptr) };
    if nclx_status.code == libheif_sys::heif_error_Ok && !nclx_ptr.is_null() {
        let nclx = unsafe { *nclx_ptr };
        unsafe { libheif_sys::heif_nclx_color_profile_free(nclx_ptr) };
        return heif_nclx_to_metadata(
            nclx.color_primaries as u16,
            nclx.transfer_characteristics as u16,
            nclx.matrix_coefficients as u16,
            nclx.full_range_flag != 0,
        );
    }

    let icc_size = unsafe { libheif_sys::heif_image_handle_get_raw_color_profile_size(handle) };
    if icc_size > 0 {
        let mut icc = vec![0_u8; icc_size];
        let icc_status = unsafe {
            libheif_sys::heif_image_handle_get_raw_color_profile(handle, icc.as_mut_ptr().cast())
        };
        if icc_status.code == libheif_sys::heif_error_Ok {
            log::debug!(
                "[HEIF] using embedded ICC profile ({} bytes); no NCLX colour_property box",
                icc_size
            );
            return HdrImageMetadata {
                color_profile: HdrColorProfile::Icc(Arc::new(icc)),
                // Embedded ICC camera stills are almost always display-referred gamma; `Unknown` skips
                // WGSL sRGB decode and looks too bright on SDR / inconsistent on HDR when tagged PQ+8-bit.
                transfer_function: HdrTransferFunction::Srgb,
                reference: HdrReference::Unknown,
                luminance: HdrLuminanceMetadata::default(),
                gain_map: None,
                raw_gpu_source: None,
            };
        }
    }

    // No NCLX and no embedded ICC (or raw ICC read failed). Libheif still returns **display codes**
    // normalized to 0–1 floats for 8/10/12-bit primaries — *not* scene-linear HDR.
    //
    // Do **not** use [`HdrImageMetadata::default`] (`Linear` skips EOTFs). **Bt709 + Reinhard** on SDR
    // washes Nokia / `old_bridge_*` style stills vs Chrome — keep **sRGB-like** decode for this orphan path.
    heif_metadata_without_embedded_colour_info()
}

/// Metadata when HEIF exposes **no** NCLX and **no** readable embedded ICC blob.
#[cfg(feature = "heif-native")]
pub(crate) fn heif_metadata_without_embedded_colour_info() -> HdrImageMetadata {
    HdrImageMetadata {
        transfer_function: HdrTransferFunction::Srgb,
        reference: HdrReference::Unknown,
        color_profile: HdrColorProfile::LinearSrgb,
        luminance: HdrLuminanceMetadata::default(),
        gain_map: None,
        raw_gpu_source: None,
    }
}

/// Apple-style **composite HDR HEIC**: NCLX may mark **PQ** while the **primary** decoded surface is
/// an **8-bit SDR** compatible base; decoding that through PQ in WGSL crushes luminance (HDR too dark).
/// **Unknown** skips `srgb_to_linear` and often reads as linear (SDR too bright). Heuristic: ≤8-bit
/// luma on the **handle** ⇒ treat transfer as sRGB-like for the GPU decode path.
#[cfg(feature = "heif-native")]
pub(crate) fn refine_heif_transfer_for_primary_bit_depth(
    handle: *const libheif_sys::heif_image_handle,
    metadata: &mut HdrImageMetadata,
) {
    let luma = unsafe { libheif_sys::heif_image_handle_get_luma_bits_per_pixel(handle) }.max(0);
    apply_heif_transfer_depth_heuristics(luma, metadata);
    apply_heif_unknown_transfer_bt709_primaries_fallback(metadata);
}

/// **`Unknown` transfer + primaries 1**: treat as unmanaged **IEC sRGB-like** PQ codes — same rationale
/// as [`heif_nclx_to_metadata`] for transfer 1/6 (browser parity on SDR, avoids Reinhard “gray veil”).
#[cfg(feature = "heif-native")]
pub(crate) fn apply_heif_unknown_transfer_bt709_primaries_fallback(
    metadata: &mut HdrImageMetadata,
) {
    if metadata.transfer_function != HdrTransferFunction::Unknown {
        return;
    }

    let uses_bt709_primaries = matches!(
        &metadata.color_profile,
        HdrColorProfile::Cicp {
            color_primaries: 1,
            ..
        }
    );
    if !uses_bt709_primaries {
        return;
    }

    log::debug!(
        "[HEIF] unknown CICP transfer with BT.709 chromaticities (primaries=1) — assuming sRGB-like display codes \
         for HDR decode + SDR IEC path (Chrome-style unmanaged stills)."
    );
    metadata.transfer_function = HdrTransferFunction::Srgb;
    metadata.reference = HdrReference::Unknown;
}

#[cfg(feature = "heif-native")]
pub(crate) fn apply_heif_transfer_depth_heuristics(
    luma_bits: i32,
    metadata: &mut HdrImageMetadata,
) {
    let luma = luma_bits.max(0) as u32;
    if luma == 0 || luma > 8 {
        return;
    }

    if metadata.transfer_function == HdrTransferFunction::Pq {
        log::debug!(
            "[HEIF] PQ transfer with {luma}-bit primary handle — using sRGB-like decode (likely SDR base / tagging mismatch)"
        );
        metadata.transfer_function = HdrTransferFunction::Srgb;
        metadata.reference = HdrReference::Unknown;
        return;
    }

    if metadata.transfer_function == HdrTransferFunction::Unknown {
        log::debug!(
            "[HEIF] unknown transfer with {luma}-bit luma — assuming sRGB-like display gamma for decode"
        );
        metadata.transfer_function = HdrTransferFunction::Srgb;
    }
}

#[cfg(feature = "heif-native")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HeifAuxiliaryEvidence {
    pub(crate) item_id: u32,
    pub(crate) aux_type: String,
    pub(crate) classification: HeifAuxiliaryClassification,
}

#[cfg(feature = "heif-native")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeifAuxiliaryClassification {
    IsoGainMap,
    AppleHdrGainMap,
    AppleTmap,
    Unknown,
}

#[cfg(feature = "heif-native")]
pub(crate) fn classify_heif_auxiliary_type(aux_type: &str) -> HeifAuxiliaryClassification {
    let lower = aux_type.to_ascii_lowercase();
    if lower.contains("hdrgainmap") || lower.contains("hdr_gain_map") || lower.contains("gainmap") {
        return if lower.contains("apple") {
            HeifAuxiliaryClassification::AppleHdrGainMap
        } else {
            HeifAuxiliaryClassification::IsoGainMap
        };
    }
    if lower.contains("tmap") || lower.contains("tone") {
        return HeifAuxiliaryClassification::AppleTmap;
    }
    HeifAuxiliaryClassification::Unknown
}

#[cfg(feature = "heif-native")]
pub(crate) fn inspect_heif_gain_map_auxiliaries(
    handle: *const libheif_sys::heif_image_handle,
) -> Option<HdrGainMapMetadata> {
    let evidence = list_heif_auxiliary_evidence(handle);
    let relevant = evidence
        .iter()
        .filter(|item| item.classification != HeifAuxiliaryClassification::Unknown)
        .collect::<Vec<_>>();
    if relevant.is_empty() {
        return None;
    }
    let diagnostic = relevant
        .iter()
        .map(|item| {
            format!(
                "#{} {} ({:?})",
                item.item_id, item.aux_type, item.classification
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    log::warn!(
        "[HDR] HEIF auxiliary gain-map/tmap evidence found but no stable ISO metadata parser is exposed yet: {diagnostic}"
    );
    Some(HdrGainMapMetadata {
        source: "HEIF",
        target_hdr_capacity: None,
        diagnostic,
        capped_display_referred: false,
        apple_heic_deferred: None,
        iso_deferred: None,
    })
}

#[cfg(feature = "heif-native")]
pub(crate) fn list_heif_auxiliary_evidence(
    handle: *const libheif_sys::heif_image_handle,
) -> Vec<HeifAuxiliaryEvidence> {
    let count = unsafe { libheif_sys::heif_image_handle_get_number_of_auxiliary_images(handle, 0) };
    if count <= 0 {
        return Vec::new();
    }
    let mut ids = vec![0_u32; count as usize];
    let written = unsafe {
        libheif_sys::heif_image_handle_get_list_of_auxiliary_image_IDs(
            handle,
            0,
            ids.as_mut_ptr(),
            count,
        )
    };
    ids.truncate(written.max(0) as usize);

    let mut evidence = Vec::new();
    for id in ids {
        let mut aux_handle = std::ptr::null_mut();
        let status = unsafe {
            libheif_sys::heif_image_handle_get_auxiliary_image_handle(handle, id, &mut aux_handle)
        };
        if status.code != libheif_sys::heif_error_Ok || aux_handle.is_null() {
            continue;
        }
        let aux = HeifAuxiliaryImageHandle(aux_handle);
        let mut aux_type_ptr = std::ptr::null();
        let type_status =
            unsafe { libheif_sys::heif_image_handle_get_auxiliary_type(aux.0, &mut aux_type_ptr) };
        if type_status.code != libheif_sys::heif_error_Ok || aux_type_ptr.is_null() {
            continue;
        }
        let aux_type = unsafe { CStr::from_ptr(aux_type_ptr) }
            .to_string_lossy()
            .into_owned();
        unsafe { libheif_sys::heif_image_handle_release_auxiliary_type(aux.0, &mut aux_type_ptr) };
        evidence.push(HeifAuxiliaryEvidence {
            item_id: id,
            classification: classify_heif_auxiliary_type(&aux_type),
            aux_type,
        });
    }
    evidence
}

#[cfg(feature = "heif-native")]
pub(crate) fn heif_sample_bit_depth(
    image: *const libheif_sys::heif_image,
    handle: *const libheif_sys::heif_image_handle,
) -> Result<u32, String> {
    let decoded = unsafe {
        libheif_sys::heif_image_get_bits_per_pixel_range(
            image,
            libheif_sys::heif_channel_interleaved,
        )
    };
    let luma = unsafe { libheif_sys::heif_image_handle_get_luma_bits_per_pixel(handle) };
    let chroma = unsafe { libheif_sys::heif_image_handle_get_chroma_bits_per_pixel(handle) };
    let bit_depth = decoded.max(luma).max(chroma).max(8);
    if bit_depth <= 0 || bit_depth > 16 {
        return Err(format!("unsupported HEIF bit depth {bit_depth}"));
    }
    Ok(bit_depth as u32)
}
