#[cfg(feature = "avif-native")]
use crate::hdr::gain_map::{
    GainMapMetadata, IsoGainMapFraction, append_hdr_pixel_from_sdr_and_gain,
    gain_map_metadata_diagnostic, sample_gain_map_rgb,
};
use crate::hdr::types::{
    HdrColorProfile, HdrImageMetadata, HdrLuminanceMetadata, HdrReference, HdrTransferFunction,
};
#[cfg(feature = "avif-native")]
use crate::hdr::types::{HdrGainMapMetadata, HdrImageBuffer, HdrPixelFormat};
#[cfg(feature = "avif-native")]
use std::sync::Arc;

pub(crate) fn is_avif_brand(brand: &[u8]) -> bool {
    matches!(brand, b"avif" | b"avis")
}

#[allow(dead_code)]
pub(crate) fn avif_cicp_to_metadata(
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

#[cfg(feature = "avif-native")]
#[allow(dead_code)]
pub(crate) fn decode_avif_hdr(path: &std::path::Path) -> Result<HdrImageBuffer, String> {
    let bytes = std::fs::read(path).map_err(|err| format!("Failed to read AVIF: {err}"))?;
    decode_avif_hdr_bytes(&bytes)
}

#[cfg(feature = "avif-native")]
#[allow(dead_code)]
pub(crate) fn decode_avif_hdr_bytes(bytes: &[u8]) -> Result<HdrImageBuffer, String> {
    decode_avif_hdr_bytes_with_target_capacity(
        bytes,
        crate::hdr::types::HdrToneMapSettings::default().target_hdr_capacity(),
    )
}

#[cfg(feature = "avif-native")]
pub(crate) fn decode_avif_hdr_with_target_capacity(
    path: &std::path::Path,
    target_hdr_capacity: f32,
) -> Result<HdrImageBuffer, String> {
    let bytes = std::fs::read(path).map_err(|err| format!("Failed to read AVIF: {err}"))?;
    decode_avif_hdr_bytes_with_target_capacity(&bytes, target_hdr_capacity)
}

#[cfg(feature = "avif-native")]
pub(crate) fn decode_avif_hdr_bytes_with_target_capacity(
    bytes: &[u8],
    target_hdr_capacity: f32,
) -> Result<HdrImageBuffer, String> {
    use std::ffi::CStr;

    struct AvifImage(*mut libavif_sys::avifImage);
    impl Drop for AvifImage {
        fn drop(&mut self) {
            unsafe { libavif_sys::avifImageDestroy(self.0) };
        }
    }

    struct AvifDecoder(*mut libavif_sys::avifDecoder);
    impl Drop for AvifDecoder {
        fn drop(&mut self) {
            unsafe { libavif_sys::avifDecoderDestroy(self.0) };
        }
    }

    fn result_to_string(result: libavif_sys::avifResult) -> String {
        unsafe {
            let ptr = libavif_sys::avifResultToString(result);
            if ptr.is_null() {
                return format!("libavif error {result}");
            }
            CStr::from_ptr(ptr).to_string_lossy().into_owned()
        }
    }

    let decoder = AvifDecoder(unsafe { libavif_sys::avifDecoderCreate() });
    if decoder.0.is_null() {
        return Err("Failed to create libavif decoder".to_string());
    }
    unsafe { libavif_sys::siv_avif_decoder_decode_all_content(decoder.0) };
    let image = AvifImage(unsafe { libavif_sys::avifImageCreateEmpty() });
    if image.0.is_null() {
        return Err("Failed to create libavif image".to_string());
    }

    let result = unsafe {
        libavif_sys::avifDecoderReadMemory(decoder.0, image.0, bytes.as_ptr(), bytes.len())
    };
    if result != libavif_sys::AVIF_RESULT_OK {
        return Err(format!(
            "libavif decode failed: {}",
            result_to_string(result)
        ));
    }

    let image_ref = unsafe { &*image.0 };
    if image_ref.width == 0 || image_ref.height == 0 {
        return Err("libavif decoded zero-sized image".to_string());
    }
    if image_ref.depth == 0 || image_ref.depth > 16 {
        return Err(format!("unsupported AVIF bit depth {}", image_ref.depth));
    }

    let mut metadata = avif_cicp_to_metadata(
        image_ref.colorPrimaries as u16,
        image_ref.transferCharacteristics as u16,
        image_ref.matrixCoefficients as u16,
        true,
    )
    .with_clli(image_ref.clli.maxCLL, image_ref.clli.maxPALL);
    let color_space = metadata.color_space_hint();

    let rgba_u16 = decode_avif_image_rgba_u16(image.0, image_ref, result_to_string)?;

    if let Some((gain_metadata, gain_width, gain_height, gain_rgba)) =
        decode_avif_gain_map(image_ref, result_to_string)
    {
        let diagnostic = gain_map_metadata_diagnostic(gain_metadata, target_hdr_capacity);
        let mut rgba_f32 =
            Vec::with_capacity(image_ref.width as usize * image_ref.height as usize * 4);
        let scale_to_u8 = ((1_u32 << image_ref.depth.min(16)) - 1) as f32 / 255.0;
        for y in 0..image_ref.height {
            for x in 0..image_ref.width {
                let index = (y as usize * image_ref.width as usize + x as usize) * 4;
                let sdr_rgba = [
                    (rgba_u16[index] as f32 / scale_to_u8)
                        .round()
                        .clamp(0.0, 255.0) as u8,
                    (rgba_u16[index + 1] as f32 / scale_to_u8)
                        .round()
                        .clamp(0.0, 255.0) as u8,
                    (rgba_u16[index + 2] as f32 / scale_to_u8)
                        .round()
                        .clamp(0.0, 255.0) as u8,
                    (rgba_u16[index + 3] as f32 / scale_to_u8)
                        .round()
                        .clamp(0.0, 255.0) as u8,
                ];
                let gain_value = sample_gain_map_rgb(
                    &gain_rgba,
                    gain_width,
                    gain_height,
                    x,
                    y,
                    image_ref.width,
                    image_ref.height,
                );
                append_hdr_pixel_from_sdr_and_gain(
                    &mut rgba_f32,
                    &sdr_rgba,
                    gain_value,
                    gain_metadata,
                    target_hdr_capacity,
                );
            }
        }
        metadata.gain_map = Some(HdrGainMapMetadata {
            source: "AVIF",
            target_hdr_capacity: Some(target_hdr_capacity),
            diagnostic,
        });
        return Ok(HdrImageBuffer {
            width: image_ref.width,
            height: image_ref.height,
            format: HdrPixelFormat::Rgba32Float,
            color_space,
            metadata,
            rgba_f32: Arc::new(rgba_f32),
        });
    }

    let scale = ((1_u32 << image_ref.depth.min(16)) - 1) as f32;
    let rgba_f32 = rgba_u16
        .into_iter()
        .map(|value| value as f32 / scale)
        .collect::<Vec<_>>();

    Ok(HdrImageBuffer {
        width: image_ref.width,
        height: image_ref.height,
        format: HdrPixelFormat::Rgba32Float,
        color_space,
        metadata,
        rgba_f32: Arc::new(rgba_f32),
    })
}

#[cfg(feature = "avif-native")]
fn decode_avif_image_rgba_u16(
    image: *const libavif_sys::avifImage,
    image_ref: &libavif_sys::avifImage,
    result_to_string: impl Fn(libavif_sys::avifResult) -> String,
) -> Result<Vec<u16>, String> {
    let mut rgb = std::mem::MaybeUninit::<libavif_sys::avifRGBImage>::zeroed();
    unsafe { libavif_sys::avifRGBImageSetDefaults(rgb.as_mut_ptr(), image) };
    let mut rgb = unsafe { rgb.assume_init() };
    rgb.format = libavif_sys::AVIF_RGB_FORMAT_RGBA;
    rgb.depth = 16;
    rgb.isFloat = 0;
    rgb.maxThreads = 0;

    let pixel_count = image_ref.width as usize * image_ref.height as usize;
    let mut rgba_u16 = vec![0_u16; pixel_count * 4];
    rgb.pixels = rgba_u16.as_mut_ptr().cast::<u8>();
    rgb.rowBytes = image_ref.width * 4 * std::mem::size_of::<u16>() as u32;

    let result = unsafe { libavif_sys::avifImageYUVToRGB(image, &mut rgb) };
    if result != libavif_sys::AVIF_RESULT_OK {
        return Err(format!(
            "libavif RGB conversion failed: {}",
            result_to_string(result)
        ));
    }

    Ok(rgba_u16)
}

#[cfg(feature = "avif-native")]
fn decode_avif_gain_map(
    image_ref: &libavif_sys::avifImage,
    result_to_string: impl Fn(libavif_sys::avifResult) -> String,
) -> Option<(GainMapMetadata, u32, u32, Vec<u8>)> {
    if image_ref.gainMap.is_null() {
        return None;
    }
    let gain_map = unsafe { &*image_ref.gainMap };
    if gain_map.image.is_null() {
        log::warn!("[HDR] AVIF gain map metadata present without gain-map pixels");
        return None;
    }
    let metadata = match avif_gain_map_to_metadata(gain_map) {
        Ok(metadata) => metadata,
        Err(err) => {
            log::warn!("[HDR] AVIF gain map metadata is not usable: {err}");
            return None;
        }
    };
    let gain_image = unsafe { &*gain_map.image };
    let gain_rgba_u16 =
        match decode_avif_image_rgba_u16(gain_map.image, gain_image, result_to_string) {
            Ok(pixels) => pixels,
            Err(err) => {
                log::warn!("[HDR] AVIF gain map pixel decode failed: {err}");
                return None;
            }
        };
    let scale = ((1_u32 << gain_image.depth.min(16)) - 1) as f32 / 255.0;
    let gain_rgba = gain_rgba_u16
        .into_iter()
        .map(|value| (value as f32 / scale).round().clamp(0.0, 255.0) as u8)
        .collect();
    Some((metadata, gain_image.width, gain_image.height, gain_rgba))
}

#[cfg(feature = "avif-native")]
pub(crate) fn avif_gain_map_to_metadata(
    gain_map: &libavif_sys::avifGainMap,
) -> Result<GainMapMetadata, String> {
    let mut fraction = IsoGainMapFraction::default();
    for channel in 0..3 {
        fraction.gain_map_min[channel] = signed(gain_map.gainMapMin[channel]);
        fraction.gain_map_max[channel] = signed(gain_map.gainMapMax[channel]);
        fraction.gamma[channel] = unsigned(gain_map.gainMapGamma[channel]);
        fraction.base_offset[channel] = signed(gain_map.baseOffset[channel]);
        fraction.alternate_offset[channel] = signed(gain_map.alternateOffset[channel]);
    }
    fraction.base_hdr_headroom = unsigned(gain_map.baseHdrHeadroom);
    fraction.alternate_hdr_headroom = unsigned(gain_map.alternateHdrHeadroom);
    fraction.into_gain_map_metadata()
}

#[cfg(feature = "avif-native")]
fn signed(value: libavif_sys::avifSignedFraction) -> (i32, u32) {
    (value.n, value.d)
}

#[cfg(feature = "avif-native")]
fn unsigned(value: libavif_sys::avifUnsignedFraction) -> (u32, u32) {
    (value.n, value.d)
}

#[cfg(feature = "avif-native")]
trait AvifMetadataExt {
    fn with_clli(self, max_cll: u16, max_fall: u16) -> Self;
}

#[cfg(feature = "avif-native")]
impl AvifMetadataExt for HdrImageMetadata {
    fn with_clli(mut self, max_cll: u16, max_fall: u16) -> Self {
        if max_cll > 0 {
            self.luminance.max_cll_nits = Some(max_cll as f32);
        }
        if max_fall > 0 {
            self.luminance.max_fall_nits = Some(max_fall as f32);
        }
        self
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "avif-native")]
    use crate::hdr::avif::avif_gain_map_to_metadata;
    use crate::hdr::avif::{avif_cicp_to_metadata, is_avif_brand};
    use crate::hdr::types::{HdrColorProfile, HdrColorSpace, HdrReference, HdrTransferFunction};

    #[test]
    fn avif_cicp_maps_bt2020_pq_to_hdr_metadata() {
        let metadata = avif_cicp_to_metadata(9, 16, 9, false);

        assert_eq!(metadata.transfer_function, HdrTransferFunction::Pq);
        assert_eq!(metadata.reference, HdrReference::DisplayReferred);
        assert_eq!(
            metadata.color_profile,
            HdrColorProfile::Cicp {
                color_primaries: 9,
                transfer_characteristics: 16,
                matrix_coefficients: 9,
                full_range: false,
            }
        );
    }

    #[test]
    fn avif_cicp_maps_bt2020_hlg_to_rec2020_linear_color_space() {
        let metadata = avif_cicp_to_metadata(9, 18, 9, true);

        assert_eq!(metadata.transfer_function, HdrTransferFunction::Hlg);
        assert_eq!(metadata.reference, HdrReference::SceneLinear);
        assert_eq!(metadata.color_space_hint(), HdrColorSpace::Rec2020Linear);
    }

    #[test]
    fn avif_brand_detection_accepts_avif_and_avis() {
        assert!(is_avif_brand(b"avif"));
        assert!(is_avif_brand(b"avis"));
        assert!(!is_avif_brand(b"heic"));
    }

    #[cfg(feature = "avif-native")]
    #[test]
    fn avif_gain_map_fractions_convert_to_shared_metadata() {
        let gain_map = libavif_sys::avifGainMap {
            image: std::ptr::null_mut(),
            gainMapMin: [signed(0, 10), signed(1, 10), signed(2, 10)],
            gainMapMax: [signed(20, 10), signed(30, 10), signed(40, 10)],
            gainMapGamma: [unsigned(10, 10), unsigned(11, 10), unsigned(12, 10)],
            baseOffset: [signed(0, 10), signed(1, 10), signed(2, 10)],
            alternateOffset: [signed(3, 10), signed(4, 10), signed(5, 10)],
            baseHdrHeadroom: unsigned(0, 10),
            alternateHdrHeadroom: unsigned(20, 10),
            useBaseColorSpace: 1,
            altICC: libavif_sys::avifRWData {
                data: std::ptr::null_mut(),
                size: 0,
            },
            altColorPrimaries: 9,
            altTransferCharacteristics: 16,
            altMatrixCoefficients: 9,
            altYUVRange: 1,
            altDepth: 10,
            altPlaneCount: 3,
            altCLLI: libavif_sys::avifContentLightLevelInformationBox {
                maxCLL: 0,
                maxPALL: 0,
            },
        };

        let metadata = avif_gain_map_to_metadata(&gain_map).expect("convert metadata");

        assert_eq!(metadata.gain_map_min, [0.0, 0.1, 0.2]);
        assert_eq!(metadata.gain_map_max, [2.0, 3.0, 4.0]);
        assert_eq!(metadata.gamma, [1.0, 1.1, 1.2]);
        assert_eq!(metadata.offset_sdr, [0.0, 0.1, 0.2]);
        assert_eq!(metadata.offset_hdr, [0.3, 0.4, 0.5]);
        assert_eq!(metadata.hdr_capacity_min, 1.0);
        assert_eq!(metadata.hdr_capacity_max, 4.0);
    }

    #[cfg(feature = "avif-native")]
    fn signed(n: i32, d: u32) -> libavif_sys::avifSignedFraction {
        libavif_sys::avifSignedFraction { n, d }
    }

    #[cfg(feature = "avif-native")]
    fn unsigned(n: u32, d: u32) -> libavif_sys::avifUnsignedFraction {
        libavif_sys::avifUnsignedFraction { n, d }
    }
}
