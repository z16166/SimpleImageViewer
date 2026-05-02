#[cfg(feature = "jpegxl")]
use crate::hdr::gain_map::{
    GainMapMetadata, append_hdr_pixel_from_sdr_and_gain, gain_map_metadata_diagnostic,
    parse_iso_gain_map_metadata, sample_gain_map_rgb,
};
use crate::hdr::types::{
    HdrColorProfile, HdrImageMetadata, HdrLuminanceMetadata, HdrReference, HdrTransferFunction,
};
#[cfg(feature = "jpegxl")]
use crate::hdr::types::{HdrGainMapMetadata, HdrImageBuffer, HdrPixelFormat};
#[cfg(feature = "jpegxl")]
use std::sync::Arc;

pub(crate) fn is_jxl_header(header: &[u8]) -> bool {
    header.starts_with(&[0xff, 0x0a])
        || header.starts_with(&[0x00, 0x00, 0x00, 0x0c, b'J', b'X', b'L', b' '])
}

#[allow(dead_code)]
pub(crate) fn jxl_color_encoding_to_metadata(
    color_primaries: u16,
    transfer_characteristics: u16,
    intensity_target_nits: Option<f32>,
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
pub(crate) fn load_jxl_hdr(path: &std::path::Path) -> Result<crate::loader::ImageData, String> {
    let hdr = decode_jxl_hdr(path)?;
    let fallback_pixels = crate::hdr::decode::hdr_to_sdr_rgba8(&hdr, 0.0)?;
    let fallback = crate::loader::DecodedImage::new(hdr.width, hdr.height, fallback_pixels);

    Ok(crate::loader::ImageData::Hdr { hdr, fallback })
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
) -> Result<crate::loader::ImageData, String> {
    let hdr = decode_jxl_hdr_with_target_capacity(path, target_hdr_capacity)?;
    let fallback_pixels = crate::hdr::decode::hdr_to_sdr_rgba8(&hdr, 0.0)?;
    let fallback = crate::loader::DecodedImage::new(hdr.width, hdr.height, fallback_pixels);

    Ok(crate::loader::ImageData::Hdr { hdr, fallback })
}

#[cfg(feature = "jpegxl")]
pub(crate) fn decode_jxl_hdr_with_target_capacity(
    path: &std::path::Path,
    target_hdr_capacity: f32,
) -> Result<HdrImageBuffer, String> {
    let bytes = std::fs::read(path).map_err(|err| format!("Failed to read JPEG XL: {err}"))?;
    decode_jxl_hdr_bytes_with_target_capacity(&bytes, target_hdr_capacity)
}

#[cfg(feature = "jpegxl")]
pub(crate) fn decode_jxl_hdr_bytes_with_target_capacity(
    bytes: &[u8],
    target_hdr_capacity: f32,
) -> Result<HdrImageBuffer, String> {
    struct JxlDecoder(*mut libjxl_sys::JxlDecoder);
    impl Drop for JxlDecoder {
        fn drop(&mut self) {
            unsafe { libjxl_sys::JxlDecoderDestroy(self.0) };
        }
    }

    let decoder = JxlDecoder(unsafe { libjxl_sys::JxlDecoderCreate(std::ptr::null()) });
    if decoder.0.is_null() {
        return Err("Failed to create libjxl decoder".to_string());
    }

    let subscribed = libjxl_sys::JXL_DEC_BASIC_INFO
        | libjxl_sys::JXL_DEC_COLOR_ENCODING
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

    let mut basic_info = None;
    let mut metadata = HdrImageMetadata::default();
    let mut rgba_f32 = Vec::<f32>::new();
    let mut current_box_type = [0_u8; 4];
    let mut current_box_buffer = Vec::<u8>::new();
    let mut current_box_pos = 0_usize;
    let mut jhgm_box = None::<Vec<u8>>;

    loop {
        match unsafe { libjxl_sys::JxlDecoderProcessInput(decoder.0) } {
            libjxl_sys::JXL_DEC_SUCCESS => {
                break;
            }
            libjxl_sys::JXL_DEC_ERROR => {
                return Err("libjxl decode failed".to_string());
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
                basic_info = Some(info);
            }
            libjxl_sys::JXL_DEC_COLOR_ENCODING => {
                metadata = read_jxl_metadata(decoder.0, metadata);
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
                let color_space = metadata.color_space_hint();
                if let Some(jhgm_box) = jhgm_box.as_deref() {
                    match decode_jxl_gain_map(
                        jhgm_box,
                        target_hdr_capacity,
                        &rgba_f32,
                        info.xsize,
                        info.ysize,
                    ) {
                        Ok((gain_metadata, gain_width, gain_height, gain_rgba)) => {
                            let diagnostic =
                                gain_map_metadata_diagnostic(gain_metadata, target_hdr_capacity);
                            let mut composed = Vec::with_capacity(expected_len);
                            for y in 0..info.ysize {
                                for x in 0..info.xsize {
                                    let index = (y as usize * info.xsize as usize + x as usize) * 4;
                                    let sdr_rgba = [
                                        (linear_to_srgb_u8(rgba_f32[index])),
                                        (linear_to_srgb_u8(rgba_f32[index + 1])),
                                        (linear_to_srgb_u8(rgba_f32[index + 2])),
                                        (rgba_f32[index + 3] * 255.0).round().clamp(0.0, 255.0)
                                            as u8,
                                    ];
                                    let gain_value = sample_gain_map_rgb(
                                        &gain_rgba,
                                        gain_width,
                                        gain_height,
                                        x,
                                        y,
                                        info.xsize,
                                        info.ysize,
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
                            });
                            rgba_f32 = composed;
                        }
                        Err(err) => {
                            log::warn!("[HDR] JPEG XL jhgm gain-map fallback: {err}");
                        }
                    }
                }
                return Ok(HdrImageBuffer {
                    width: info.xsize,
                    height: info.ysize,
                    format: HdrPixelFormat::Rgba32Float,
                    color_space,
                    metadata,
                    rgba_f32: Arc::new(rgba_f32),
                });
            }
            status => {
                return Err(format!("unsupported libjxl decoder status {status}"));
            }
        }
    }

    Err("libjxl decode completed without an image".to_string())
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
fn read_jxl_metadata(
    decoder: *const libjxl_sys::JxlDecoder,
    mut metadata: HdrImageMetadata,
) -> HdrImageMetadata {
    let mut color = std::mem::MaybeUninit::<libjxl_sys::JxlColorEncoding>::zeroed();
    let encoded_status = unsafe {
        libjxl_sys::JxlDecoderGetColorAsEncodedProfile(
            decoder,
            libjxl_sys::JXL_COLOR_PROFILE_TARGET_DATA,
            color.as_mut_ptr(),
        )
    };
    if encoded_status == libjxl_sys::JXL_DEC_SUCCESS {
        let color = unsafe { color.assume_init() };
        let intensity_target = metadata.luminance.mastering_max_nits;
        return jxl_color_encoding_to_metadata(
            color.primaries as u16,
            color.transfer_function as u16,
            intensity_target,
        );
    }

    let mut icc_size = 0_usize;
    let icc_status = unsafe {
        libjxl_sys::JxlDecoderGetICCProfileSize(
            decoder,
            libjxl_sys::JXL_COLOR_PROFILE_TARGET_DATA,
            &mut icc_size,
        )
    };
    if icc_status == libjxl_sys::JXL_DEC_SUCCESS && icc_size > 0 {
        let mut icc = vec![0_u8; icc_size];
        let profile_status = unsafe {
            libjxl_sys::JxlDecoderGetColorAsICCProfile(
                decoder,
                libjxl_sys::JXL_COLOR_PROFILE_TARGET_DATA,
                icc.as_mut_ptr(),
                icc.len(),
            )
        };
        if profile_status == libjxl_sys::JXL_DEC_SUCCESS {
            metadata.color_profile = HdrColorProfile::Icc(Arc::new(icc));
            metadata.transfer_function = HdrTransferFunction::Unknown;
            metadata.reference = HdrReference::Unknown;
        }
    }

    metadata
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "jpegxl")]
    use crate::hdr::jpegxl::read_jxl_gain_map_bundle;
    use crate::hdr::jpegxl::{is_jxl_header, jxl_color_encoding_to_metadata};
    use crate::hdr::types::{HdrReference, HdrTransferFunction};

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
        let metadata = jxl_color_encoding_to_metadata(9, 16, Some(4000.0));

        assert_eq!(metadata.transfer_function, HdrTransferFunction::Pq);
        assert_eq!(metadata.reference, HdrReference::DisplayReferred);
        assert_eq!(metadata.luminance.mastering_max_nits, Some(4000.0));
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
}
