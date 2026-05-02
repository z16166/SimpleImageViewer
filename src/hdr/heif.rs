use crate::hdr::types::{
    HdrColorProfile, HdrImageMetadata, HdrLuminanceMetadata, HdrReference, HdrTransferFunction,
};
#[cfg(feature = "heif-native")]
use crate::hdr::types::{HdrGainMapMetadata, HdrImageBuffer, HdrPixelFormat};
#[cfg(feature = "heif-native")]
use std::ffi::CStr;
#[cfg(feature = "heif-native")]
use std::sync::Arc;

pub(crate) fn is_heif_brand(brand: &[u8]) -> bool {
    matches!(
        brand,
        b"heic" | b"heix" | b"hevc" | b"hevx" | b"mif1" | b"msf1"
    )
}

#[allow(dead_code)]
pub(crate) fn heif_nclx_to_metadata(
    color_primaries: u16,
    transfer_characteristics: u16,
    matrix_coefficients: u16,
    full_range: bool,
) -> HdrImageMetadata {
    let transfer_function = match transfer_characteristics {
        1 | 13 => HdrTransferFunction::Srgb,
        16 => HdrTransferFunction::Pq,
        18 => HdrTransferFunction::Hlg,
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
            matrix_coefficients,
            full_range,
        },
        luminance: HdrLuminanceMetadata::default(),
        gain_map: None,
    }
}

#[cfg(feature = "heif-native")]
pub(crate) fn load_heif_hdr(path: &std::path::Path) -> Result<crate::loader::ImageData, String> {
    let hdr = decode_heif_hdr(path)?;
    let fallback_pixels = crate::hdr::decode::hdr_to_sdr_rgba8(&hdr, 0.0)?;
    let fallback = crate::loader::DecodedImage::new(hdr.width, hdr.height, fallback_pixels);

    Ok(crate::loader::ImageData::Hdr { hdr, fallback })
}

#[cfg(feature = "heif-native")]
pub(crate) fn decode_heif_hdr(path: &std::path::Path) -> Result<HdrImageBuffer, String> {
    let bytes = std::fs::read(path).map_err(|err| format!("Failed to read HEIF: {err}"))?;
    decode_heif_hdr_bytes(&bytes)
}

#[cfg(feature = "heif-native")]
pub(crate) fn decode_heif_hdr_bytes(bytes: &[u8]) -> Result<HdrImageBuffer, String> {
    use std::ffi::CStr;

    struct HeifContext(*mut libheif_sys::heif_context);
    impl Drop for HeifContext {
        fn drop(&mut self) {
            unsafe { libheif_sys::heif_context_free(self.0) };
        }
    }

    struct HeifImageHandle(*mut libheif_sys::heif_image_handle);
    impl Drop for HeifImageHandle {
        fn drop(&mut self) {
            unsafe { libheif_sys::heif_image_handle_release(self.0) };
        }
    }

    struct HeifImage(*mut libheif_sys::heif_image);
    impl Drop for HeifImage {
        fn drop(&mut self) {
            unsafe { libheif_sys::heif_image_release(self.0) };
        }
    }

    fn heif_error_to_string(err: libheif_sys::heif_error) -> String {
        if err.message.is_null() {
            return format!("libheif error code {} subcode {}", err.code, err.subcode);
        }
        unsafe { CStr::from_ptr(err.message) }
            .to_string_lossy()
            .into_owned()
    }

    fn ensure_heif_ok(err: libheif_sys::heif_error, action: &str) -> Result<(), String> {
        if err.code == libheif_sys::heif_error_Ok {
            Ok(())
        } else {
            Err(format!("Failed to {action}: {}", heif_error_to_string(err)))
        }
    }

    let context = HeifContext(unsafe { libheif_sys::heif_context_alloc() });
    if context.0.is_null() {
        return Err("Failed to allocate libheif context".to_string());
    }

    ensure_heif_ok(
        unsafe {
            libheif_sys::heif_context_read_from_memory_without_copy(
                context.0,
                bytes.as_ptr().cast(),
                bytes.len(),
                std::ptr::null(),
            )
        },
        "read HEIF from memory",
    )?;

    let mut handle_ptr = std::ptr::null_mut();
    ensure_heif_ok(
        unsafe { libheif_sys::heif_context_get_primary_image_handle(context.0, &mut handle_ptr) },
        "get HEIF primary image",
    )?;
    if handle_ptr.is_null() {
        return Err("libheif returned a null primary image handle".to_string());
    }
    let handle = HeifImageHandle(handle_ptr);

    let mut metadata = read_heif_metadata(handle.0);
    if let Some(diagnostic) = inspect_heif_gain_map_auxiliaries(handle.0) {
        metadata.gain_map = Some(diagnostic);
    }
    let mut image_ptr = std::ptr::null_mut();
    ensure_heif_ok(
        unsafe {
            libheif_sys::heif_decode_image(
                handle.0,
                &mut image_ptr,
                libheif_sys::heif_colorspace_RGB,
                libheif_sys::heif_chroma_interleaved_RRGGBBAA_LE,
                std::ptr::null(),
            )
        },
        "decode HEIF image as 16-bit RGBA",
    )?;
    if image_ptr.is_null() {
        return Err("libheif returned a null decoded image".to_string());
    }
    let image = HeifImage(image_ptr);

    let width = unsafe { libheif_sys::heif_image_get_primary_width(image.0) };
    let height = unsafe { libheif_sys::heif_image_get_primary_height(image.0) };
    if width <= 0 || height <= 0 {
        return Err("libheif decoded zero-sized image".to_string());
    }

    let mut stride = 0_usize;
    let plane = unsafe {
        libheif_sys::heif_image_get_plane_readonly2(
            image.0,
            libheif_sys::heif_channel_interleaved,
            &mut stride,
        )
    };
    if plane.is_null() {
        return Err("libheif did not expose an interleaved RGBA plane".to_string());
    }

    let width = width as u32;
    let height = height as u32;
    let bytes_per_pixel = 4 * std::mem::size_of::<u16>();
    let row_bytes = width as usize * bytes_per_pixel;
    if stride < row_bytes {
        return Err(format!(
            "libheif row stride too small: got {stride}, expected at least {row_bytes}"
        ));
    }

    let bit_depth = heif_sample_bit_depth(image.0, handle.0)?;
    let scale = ((1_u32 << bit_depth.min(16)) - 1) as f32;
    let mut rgba_f32 = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height as usize {
        let row = unsafe { std::slice::from_raw_parts(plane.add(y * stride), row_bytes) };
        for px in row.chunks_exact(bytes_per_pixel) {
            rgba_f32.push(u16::from_le_bytes([px[0], px[1]]) as f32 / scale);
            rgba_f32.push(u16::from_le_bytes([px[2], px[3]]) as f32 / scale);
            rgba_f32.push(u16::from_le_bytes([px[4], px[5]]) as f32 / scale);
            rgba_f32.push(u16::from_le_bytes([px[6], px[7]]) as f32 / scale);
        }
    }

    let color_space = metadata.color_space_hint();
    Ok(HdrImageBuffer {
        width,
        height,
        format: HdrPixelFormat::Rgba32Float,
        color_space,
        metadata,
        rgba_f32: Arc::new(rgba_f32),
    })
}

#[cfg(feature = "heif-native")]
struct HeifAuxiliaryImageHandle(*mut libheif_sys::heif_image_handle);

#[cfg(feature = "heif-native")]
impl Drop for HeifAuxiliaryImageHandle {
    fn drop(&mut self) {
        unsafe { libheif_sys::heif_image_handle_release(self.0) };
    }
}

#[cfg(feature = "heif-native")]
fn read_heif_metadata(handle: *const libheif_sys::heif_image_handle) -> HdrImageMetadata {
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
            return HdrImageMetadata {
                color_profile: HdrColorProfile::Icc(Arc::new(icc)),
                transfer_function: HdrTransferFunction::Unknown,
                reference: HdrReference::Unknown,
                luminance: HdrLuminanceMetadata::default(),
                gain_map: None,
            };
        }
    }

    HdrImageMetadata::default()
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
fn inspect_heif_gain_map_auxiliaries(
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
    })
}

#[cfg(feature = "heif-native")]
fn list_heif_auxiliary_evidence(
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
fn heif_sample_bit_depth(
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

#[cfg(test)]
mod tests {
    #[cfg(feature = "heif-native")]
    use crate::hdr::heif::{HeifAuxiliaryClassification, classify_heif_auxiliary_type};
    use crate::hdr::heif::{heif_nclx_to_metadata, is_heif_brand};
    use crate::hdr::types::{HdrReference, HdrTransferFunction};

    #[test]
    fn heif_brand_detection_accepts_heic_family_and_generic_heif() {
        for brand in [b"heic", b"heix", b"hevc", b"hevx", b"mif1", b"msf1"] {
            assert!(is_heif_brand(brand));
        }
        assert!(!is_heif_brand(b"avif"));
    }

    #[test]
    fn heif_nclx_pq_maps_to_display_referred_metadata() {
        let metadata = heif_nclx_to_metadata(9, 16, 9, false);

        assert_eq!(metadata.transfer_function, HdrTransferFunction::Pq);
        assert_eq!(metadata.reference, HdrReference::DisplayReferred);
    }

    #[cfg(feature = "heif-native")]
    #[test]
    fn heif_auxiliary_type_classifies_gain_map_and_tmap_evidence() {
        assert_eq!(
            classify_heif_auxiliary_type("urn:com:apple:photo:2020:aux:hdrgainmap"),
            HeifAuxiliaryClassification::AppleHdrGainMap
        );
        assert_eq!(
            classify_heif_auxiliary_type("urn:mpeg:mpegB:cicp:systems:auxiliary:hdr_gain_map"),
            HeifAuxiliaryClassification::IsoGainMap
        );
        assert_eq!(
            classify_heif_auxiliary_type("urn:com:apple:photo:2023:aux:tmap"),
            HeifAuxiliaryClassification::AppleTmap
        );
        assert_eq!(
            classify_heif_auxiliary_type("urn:mpeg:mpegB:cicp:systems:auxiliary:depth"),
            HeifAuxiliaryClassification::Unknown
        );
    }
}
