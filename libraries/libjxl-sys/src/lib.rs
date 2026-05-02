#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

pub type JxlDecoderStatus = libc::c_int;
pub type JXL_BOOL = libc::c_int;
pub type JxlDataType = libc::c_int;
pub type JxlEndianness = libc::c_int;
pub type JxlColorProfileTarget = libc::c_int;
pub type JxlColorSpace = libc::c_int;
pub type JxlWhitePoint = libc::c_int;
pub type JxlPrimaries = libc::c_int;
pub type JxlTransferFunction = libc::c_int;
pub type JxlRenderingIntent = libc::c_int;
pub type JxlOrientation = libc::c_int;
pub type JxlBoxType = [u8; 4];

pub const JXL_DEC_SUCCESS: JxlDecoderStatus = 0;
pub const JXL_DEC_ERROR: JxlDecoderStatus = 1;
pub const JXL_DEC_NEED_MORE_INPUT: JxlDecoderStatus = 2;
pub const JXL_DEC_NEED_IMAGE_OUT_BUFFER: JxlDecoderStatus = 5;
pub const JXL_DEC_BOX_NEED_MORE_OUTPUT: JxlDecoderStatus = 7;
pub const JXL_DEC_BASIC_INFO: JxlDecoderStatus = 0x40;
pub const JXL_DEC_COLOR_ENCODING: JxlDecoderStatus = 0x100;
pub const JXL_DEC_FULL_IMAGE: JxlDecoderStatus = 0x1000;
pub const JXL_DEC_BOX: JxlDecoderStatus = 0x4000;
pub const JXL_DEC_BOX_COMPLETE: JxlDecoderStatus = 0x10000;

pub const JXL_TYPE_FLOAT: JxlDataType = 0;
pub const JXL_NATIVE_ENDIAN: JxlEndianness = 0;

pub const JXL_COLOR_PROFILE_TARGET_ORIGINAL: JxlColorProfileTarget = 0;
pub const JXL_COLOR_PROFILE_TARGET_DATA: JxlColorProfileTarget = 1;

pub const JXL_PRIMARIES_SRGB: JxlPrimaries = 1;
pub const JXL_PRIMARIES_2100: JxlPrimaries = 9;
pub const JXL_PRIMARIES_P3: JxlPrimaries = 11;

pub const JXL_TRANSFER_FUNCTION_709: JxlTransferFunction = 1;
pub const JXL_TRANSFER_FUNCTION_UNKNOWN: JxlTransferFunction = 2;
pub const JXL_TRANSFER_FUNCTION_LINEAR: JxlTransferFunction = 8;
pub const JXL_TRANSFER_FUNCTION_SRGB: JxlTransferFunction = 13;
pub const JXL_TRANSFER_FUNCTION_PQ: JxlTransferFunction = 16;
pub const JXL_TRANSFER_FUNCTION_HLG: JxlTransferFunction = 18;
pub const JXL_TRANSFER_FUNCTION_GAMMA: JxlTransferFunction = 65535;

#[repr(C)]
pub struct JxlDecoder {
    _private: [u8; 0],
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct JxlPixelFormat {
    pub num_channels: u32,
    pub data_type: JxlDataType,
    pub endianness: JxlEndianness,
    pub align: libc::size_t,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct JxlPreviewHeader {
    pub xsize: u32,
    pub ysize: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct JxlAnimationHeader {
    pub tps_numerator: u32,
    pub tps_denominator: u32,
    pub num_loops: u32,
    pub have_timecodes: JXL_BOOL,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct JxlBasicInfo {
    pub have_container: JXL_BOOL,
    pub xsize: u32,
    pub ysize: u32,
    pub bits_per_sample: u32,
    pub exponent_bits_per_sample: u32,
    pub intensity_target: f32,
    pub min_nits: f32,
    pub relative_to_max_display: JXL_BOOL,
    pub linear_below: f32,
    pub uses_original_profile: JXL_BOOL,
    pub have_preview: JXL_BOOL,
    pub have_animation: JXL_BOOL,
    pub orientation: JxlOrientation,
    pub num_color_channels: u32,
    pub num_extra_channels: u32,
    pub alpha_bits: u32,
    pub alpha_exponent_bits: u32,
    pub alpha_premultiplied: JXL_BOOL,
    pub preview: JxlPreviewHeader,
    pub animation: JxlAnimationHeader,
    pub intrinsic_xsize: u32,
    pub intrinsic_ysize: u32,
    pub padding: [u8; 100],
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct JxlColorEncoding {
    pub color_space: JxlColorSpace,
    pub white_point: JxlWhitePoint,
    pub white_point_xy: [f64; 2],
    pub primaries: JxlPrimaries,
    pub primaries_red_xy: [f64; 2],
    pub primaries_green_xy: [f64; 2],
    pub primaries_blue_xy: [f64; 2],
    pub transfer_function: JxlTransferFunction,
    pub gamma: f64,
    pub rendering_intent: JxlRenderingIntent,
}

unsafe extern "C" {
    pub fn JxlDecoderVersion() -> u32;
    pub fn JxlDecoderCreate(memory_manager: *const libc::c_void) -> *mut JxlDecoder;
    pub fn JxlDecoderDestroy(decoder: *mut JxlDecoder);
    pub fn JxlDecoderSubscribeEvents(
        decoder: *mut JxlDecoder,
        events_wanted: libc::c_int,
    ) -> JxlDecoderStatus;
    pub fn JxlDecoderSetInput(
        decoder: *mut JxlDecoder,
        data: *const u8,
        size: libc::size_t,
    ) -> JxlDecoderStatus;
    pub fn JxlDecoderCloseInput(decoder: *mut JxlDecoder);
    pub fn JxlDecoderProcessInput(decoder: *mut JxlDecoder) -> JxlDecoderStatus;
    pub fn JxlDecoderGetBasicInfo(
        decoder: *const JxlDecoder,
        info: *mut JxlBasicInfo,
    ) -> JxlDecoderStatus;
    pub fn JxlDecoderGetColorAsEncodedProfile(
        decoder: *const JxlDecoder,
        target: JxlColorProfileTarget,
        color_encoding: *mut JxlColorEncoding,
    ) -> JxlDecoderStatus;
    pub fn JxlDecoderGetICCProfileSize(
        decoder: *const JxlDecoder,
        target: JxlColorProfileTarget,
        size: *mut libc::size_t,
    ) -> JxlDecoderStatus;
    pub fn JxlDecoderGetColorAsICCProfile(
        decoder: *const JxlDecoder,
        target: JxlColorProfileTarget,
        icc_profile: *mut u8,
        size: libc::size_t,
    ) -> JxlDecoderStatus;
    pub fn JxlDecoderImageOutBufferSize(
        decoder: *const JxlDecoder,
        format: *const JxlPixelFormat,
        size: *mut libc::size_t,
    ) -> JxlDecoderStatus;
    pub fn JxlDecoderSetImageOutBuffer(
        decoder: *mut JxlDecoder,
        format: *const JxlPixelFormat,
        buffer: *mut libc::c_void,
        size: libc::size_t,
    ) -> JxlDecoderStatus;
    pub fn JxlDecoderSetBoxBuffer(
        decoder: *mut JxlDecoder,
        data: *mut u8,
        size: libc::size_t,
    ) -> JxlDecoderStatus;
    pub fn JxlDecoderReleaseBoxBuffer(decoder: *mut JxlDecoder) -> libc::size_t;
    pub fn JxlDecoderSetDecompressBoxes(
        decoder: *mut JxlDecoder,
        decompress: JXL_BOOL,
    ) -> JxlDecoderStatus;
    pub fn JxlDecoderGetBoxType(
        decoder: *mut JxlDecoder,
        type_: *mut u8,
        decompressed: JXL_BOOL,
    ) -> JxlDecoderStatus;
    pub fn JxlDecoderGetBoxSizeContents(
        decoder: *const JxlDecoder,
        size: *mut u64,
    ) -> JxlDecoderStatus;
}
