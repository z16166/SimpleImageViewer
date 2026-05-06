#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

pub type JxlDecoderStatus = libc::c_int;
pub type JXL_BOOL = libc::c_int;
pub const JXL_TRUE: JXL_BOOL = 1;
pub const JXL_FALSE: JXL_BOOL = 0;
pub type JxlDataType = libc::c_int;
pub type JxlEndianness = libc::c_int;
pub type JxlColorProfileTarget = libc::c_int;
pub type JxlColorSpace = libc::c_int;
pub type JxlWhitePoint = libc::c_int;
pub type JxlPrimaries = libc::c_int;
pub type JxlTransferFunction = libc::c_int;
pub type JxlRenderingIntent = libc::c_int;
pub type JxlOrientation = libc::c_int;

/// `jxl/color_encoding.h` — CIE D65 (matches H.273 default illuminant).
pub const JXL_WHITE_POINT_D65: JxlWhitePoint = 1;
/// `jxl/color_encoding.h` — media-relative colorimetric.
pub const JXL_RENDERING_INTENT_PERCEPTUAL: JxlRenderingIntent = 0;
pub const JXL_RENDERING_INTENT_RELATIVE: JxlRenderingIntent = 1;
pub type JxlBoxType = [u8; 4];

pub const JXL_DEC_SUCCESS: JxlDecoderStatus = 0;
pub const JXL_DEC_ERROR: JxlDecoderStatus = 1;
pub const JXL_DEC_NEED_MORE_INPUT: JxlDecoderStatus = 2;
pub const JXL_DEC_NEED_PREVIEW_OUT_BUFFER: JxlDecoderStatus = 3;
pub const JXL_DEC_NEED_IMAGE_OUT_BUFFER: JxlDecoderStatus = 5;
pub const JXL_DEC_JPEG_NEED_MORE_OUTPUT: JxlDecoderStatus = 6;
pub const JXL_DEC_BOX_NEED_MORE_OUTPUT: JxlDecoderStatus = 7;
pub const JXL_DEC_BASIC_INFO: JxlDecoderStatus = 0x40;
pub const JXL_DEC_COLOR_ENCODING: JxlDecoderStatus = 0x100;
pub const JXL_DEC_PREVIEW_IMAGE: JxlDecoderStatus = 0x200;
/// Beginning of a displayed frame (required for correct multi-frame / animation decode ordering).
pub const JXL_DEC_FRAME: JxlDecoderStatus = 0x400;
/// Deprecated DC / low-frequency preview step; ignore if received.
pub const JXL_DEC_DC_IMAGE: JxlDecoderStatus = 0x800;
pub const JXL_DEC_FULL_IMAGE: JxlDecoderStatus = 0x1000;
pub const JXL_DEC_JPEG_RECONSTRUCTION: JxlDecoderStatus = 0x2000;
pub const JXL_DEC_BOX: JxlDecoderStatus = 0x4000;
pub const JXL_DEC_FRAME_PROGRESSION: JxlDecoderStatus = 0x8000;
pub const JXL_DEC_BOX_COMPLETE: JxlDecoderStatus = 0x10000;

pub type JxlParallelRetCode = libc::c_int;

pub type JxlParallelRunInit = unsafe extern "C" fn(
    jpegxl_opaque: *mut libc::c_void,
    num_threads: libc::size_t,
) -> JxlParallelRetCode;

pub type JxlParallelRunFunction =
    unsafe extern "C" fn(jpegxl_opaque: *mut libc::c_void, value: u32, thread_id: libc::size_t);

pub type JxlParallelRunner = unsafe extern "C" fn(
    runner_opaque: *mut libc::c_void,
    jpegxl_opaque: *mut libc::c_void,
    init: JxlParallelRunInit,
    func: JxlParallelRunFunction,
    start_range: u32,
    end_range: u32,
) -> JxlParallelRetCode;

pub const JXL_PARALLEL_RET_SUCCESS: JxlParallelRetCode = 0;

pub const JXL_TYPE_FLOAT: JxlDataType = 0;
pub const JXL_NATIVE_ENDIAN: JxlEndianness = 0;

pub const JXL_COLOR_PROFILE_TARGET_ORIGINAL: JxlColorProfileTarget = 0;
pub const JXL_COLOR_PROFILE_TARGET_DATA: JxlColorProfileTarget = 1;

pub const JXL_PRIMARIES_SRGB: JxlPrimaries = 1;
/// Primaries are given in `primaries_*_xy` (or ICC), not a CICP enum index.
pub const JXL_PRIMARIES_CUSTOM: JxlPrimaries = 2;
pub const JXL_PRIMARIES_2100: JxlPrimaries = 9;
pub const JXL_PRIMARIES_P3: JxlPrimaries = 11;

/// Tristimulus RGB (`jxl/color_encoding.h` `JxlColorSpace`).
pub const JXL_COLOR_SPACE_RGB: JxlColorSpace = 0;

pub const JXL_TRANSFER_FUNCTION_709: JxlTransferFunction = 1;
pub const JXL_TRANSFER_FUNCTION_UNKNOWN: JxlTransferFunction = 2;
pub const JXL_TRANSFER_FUNCTION_LINEAR: JxlTransferFunction = 8;
pub const JXL_TRANSFER_FUNCTION_SRGB: JxlTransferFunction = 13;
pub const JXL_TRANSFER_FUNCTION_PQ: JxlTransferFunction = 16;
pub const JXL_TRANSFER_FUNCTION_HLG: JxlTransferFunction = 18;
pub const JXL_TRANSFER_FUNCTION_GAMMA: JxlTransferFunction = 65535;

pub type JxlBlendMode = libc::c_int;

pub const JXL_BLEND_REPLACE: JxlBlendMode = 0;
pub const JXL_BLEND_ADD: JxlBlendMode = 1;
pub const JXL_BLEND_BLEND: JxlBlendMode = 2;
pub const JXL_BLEND_MULADD: JxlBlendMode = 3;
pub const JXL_BLEND_MUL: JxlBlendMode = 4;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct JxlBlendInfo {
    pub blendmode: JxlBlendMode,
    pub source: u32,
    pub alpha: u32,
    pub clamp: JXL_BOOL,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct JxlLayerInfo {
    pub have_crop: JXL_BOOL,
    pub crop_x0: i32,
    pub crop_y0: i32,
    pub xsize: u32,
    pub ysize: u32,
    pub blend_info: JxlBlendInfo,
    pub save_as_reference: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct JxlFrameHeader {
    pub duration: u32,
    pub timecode: u32,
    pub name_length: u32,
    pub is_last: JXL_BOOL,
    pub layer_info: JxlLayerInfo,
}

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

/// `JxlExtraChannelType` from `jxl/codestream_header.h`. Identifies the semantic role
/// of an extra channel beyond the 1/3 main color channels (e.g. alpha, depth, K
/// for CMYK, spot colors, etc.).
pub type JxlExtraChannelType = u32;
pub const JXL_CHANNEL_ALPHA: JxlExtraChannelType = 0;
pub const JXL_CHANNEL_DEPTH: JxlExtraChannelType = 1;
pub const JXL_CHANNEL_SPOT_COLOR: JxlExtraChannelType = 2;
pub const JXL_CHANNEL_SELECTION_MASK: JxlExtraChannelType = 3;
pub const JXL_CHANNEL_BLACK: JxlExtraChannelType = 4;
pub const JXL_CHANNEL_CFA: JxlExtraChannelType = 5;
pub const JXL_CHANNEL_THERMAL: JxlExtraChannelType = 6;
pub const JXL_CHANNEL_OPTIONAL: JxlExtraChannelType = 16;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct JxlExtraChannelInfo {
    pub type_: JxlExtraChannelType,
    pub bits_per_sample: u32,
    pub exponent_bits_per_sample: u32,
    pub dim_shift: u32,
    pub name_length: u32,
    pub alpha_premultiplied: JXL_BOOL,
    pub spot_color: [f32; 4],
    pub cfa_channel: u32,
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
    /// See `jxl/decode.h`: must be called before decoding starts. `JXL_TRUE` returns straight RGB
    /// for associated-alpha images (default is premultiplied).
    pub fn JxlDecoderSetUnpremultiplyAlpha(
        decoder: *mut JxlDecoder,
        unpremul_alpha: JXL_BOOL,
    ) -> JxlDecoderStatus;
    /// Steer XYB→RGB conversion when the codestream uses ICC (`jxl/decode.h` — see libjxl notes on
    /// ICC + XYB vs default linear-sRGB fallback).
    pub fn JxlDecoderSetPreferredColorProfile(
        decoder: *mut JxlDecoder,
        color_encoding: *const JxlColorEncoding,
    ) -> JxlDecoderStatus;
    /// Optional tone map toward a peak display luminance (nits); see `jxl/decode.h`.
    pub fn JxlDecoderSetDesiredIntensityTarget(
        decoder: *mut JxlDecoder,
        desired_intensity_target: f32,
    ) -> JxlDecoderStatus;
    pub fn JxlDecoderSetParallelRunner(
        decoder: *mut JxlDecoder,
        parallel_runner: Option<JxlParallelRunner>,
        parallel_runner_opaque: *mut libc::c_void,
    ) -> JxlDecoderStatus;
    pub fn JxlResizableParallelRunner(
        runner_opaque: *mut libc::c_void,
        jpegxl_opaque: *mut libc::c_void,
        init: JxlParallelRunInit,
        func: JxlParallelRunFunction,
        start_range: u32,
        end_range: u32,
    ) -> JxlParallelRetCode;
    pub fn JxlResizableParallelRunnerCreate(memory_manager: *const libc::c_void) -> *mut libc::c_void;
    pub fn JxlResizableParallelRunnerSetThreads(runner_opaque: *mut libc::c_void, num_threads: usize);
    pub fn JxlResizableParallelRunnerSuggestThreads(xsize: u64, ysize: u64) -> u32;
    pub fn JxlResizableParallelRunnerDestroy(runner_opaque: *mut libc::c_void);
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
    pub fn JxlDecoderGetFrameHeader(
        decoder: *const JxlDecoder,
        header: *mut JxlFrameHeader,
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
    pub fn JxlDecoderPreviewOutBufferSize(
        decoder: *const JxlDecoder,
        format: *const JxlPixelFormat,
        size: *mut libc::size_t,
    ) -> JxlDecoderStatus;
    pub fn JxlDecoderSetPreviewOutBuffer(
        decoder: *mut JxlDecoder,
        format: *const JxlPixelFormat,
        buffer: *mut libc::c_void,
        size: libc::size_t,
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

    /// Per-extra-channel info (one call per `index < basic_info.num_extra_channels`).
    /// `index` is 0-based; on success `info` is populated with the channel role,
    /// bit depth, and (for spot colors) the spot-color components.
    pub fn JxlDecoderGetExtraChannelInfo(
        decoder: *const JxlDecoder,
        index: libc::size_t,
        info: *mut JxlExtraChannelInfo,
    ) -> JxlDecoderStatus;

    /// Copy the UTF-8 channel name to `name`. `size` must be `name_length + 1`
    /// (libjxl writes a NUL terminator). Use `JxlExtraChannelInfo::name_length`
    /// from `JxlDecoderGetExtraChannelInfo` to size the buffer.
    pub fn JxlDecoderGetExtraChannelName(
        decoder: *const JxlDecoder,
        index: libc::size_t,
        name: *mut libc::c_char,
        size: libc::size_t,
    ) -> JxlDecoderStatus;

    /// Per-extra-channel pixel buffer size (e.g. for the K channel of CMYK).
    /// Pixel format `num_channels` must be `1` for extra channels.
    pub fn JxlDecoderExtraChannelBufferSize(
        decoder: *const JxlDecoder,
        format: *const JxlPixelFormat,
        size: *mut libc::size_t,
        index: u32,
    ) -> JxlDecoderStatus;

    /// Provide a buffer for one extra channel; called between
    /// `JXL_DEC_NEED_IMAGE_OUT_BUFFER` and decoding so libjxl writes the channel
    /// pixels alongside the main color buffer.
    pub fn JxlDecoderSetExtraChannelBuffer(
        decoder: *mut JxlDecoder,
        format: *const JxlPixelFormat,
        buffer: *mut libc::c_void,
        size: libc::size_t,
        index: u32,
    ) -> JxlDecoderStatus;

    /// Sets the desired output color profile as a `JxlColorEncoding`. Must be
    /// called AFTER `JXL_DEC_COLOR_ENCODING` event and BEFORE other events. To
    /// actually convert non-XYB images (e.g. CMYK), a CMS must be installed
    /// first via `JxlDecoderSetCms` (per `jxl/decode.h`).
    pub fn JxlDecoderSetOutputColorProfile(
        decoder: *mut JxlDecoder,
        color_encoding: *const JxlColorEncoding,
        icc_data: *const u8,
        icc_size: libc::size_t,
    ) -> JxlDecoderStatus;

    /// Installs a color management system (CMS) used by libjxl for color
    /// conversions. Must be called BEFORE decoding starts and BEFORE
    /// `JxlDecoderSetOutputColorProfile`.
    pub fn JxlDecoderSetCms(decoder: *mut JxlDecoder, cms: JxlCmsInterface) -> JxlDecoderStatus;
}

// libjxl bundled CMS (links against `jxl_cms`, internally skcms-based). NOTE:
// the bundled CMS does NOT auto-convert non-XYB CMYK output (per libjxl PR
// #237); CMYK files require external CMS handling — see the lcms2 bindings
// below.
unsafe extern "C" {
    #[link_name = "JxlGetDefaultCms"]
    pub fn JxlGetDefaultCms() -> *const JxlCmsInterface;
}

/// `JxlColorProfile` from `jxl/cms_interface.h` — pair of (ICC bytes,
/// structured encoding, channel count) describing one end of a CMS transform.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct JxlColorProfile {
    pub icc_data: *const u8,
    pub icc_size: libc::size_t,
    pub color_encoding: JxlColorEncoding,
    pub num_channels: libc::size_t,
}

/// `JxlCmsInterface` from `jxl/cms_interface.h`. Treated as opaque function
/// pointers from Rust — we only ever pass it back to libjxl unchanged via
/// `JxlDecoderSetCms`. Using `*mut c_void` for the function pointers keeps the
/// struct ABI-compatible regardless of how libjxl was compiled (`__cdecl` /
/// `__stdcall` differences on Windows etc.).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct JxlCmsInterface {
    pub set_fields_data: *mut libc::c_void,
    pub set_fields_from_icc: *mut libc::c_void,
    pub init_data: *mut libc::c_void,
    pub init: *mut libc::c_void,
    pub get_src_buf: *mut libc::c_void,
    pub get_dst_buf: *mut libc::c_void,
    pub run: *mut libc::c_void,
    pub destroy: *mut libc::c_void,
}

// =====================================================================
// Little CMS 2 (lcms2) minimal FFI — for ICC-managed CMYK → sRGB
// conversion of JPEG-XL files whose source has a black extra channel
// (e.g. JPEG-recompressed CMYK in conformance `cmyk_layers/input.jxl`).
// libjxl's bundled CMS does NOT auto-convert non-XYB CMYK output — per
// libjxl PR #237, applications must apply the embedded CMYK ICC profile
// externally with a 4-channel CMYK input. lcms2 is already shipped as a
// transitive vcpkg dep (typically through libheif).
// =====================================================================

pub type cmsHPROFILE = *mut libc::c_void;
pub type cmsHTRANSFORM = *mut libc::c_void;
pub type cmsContext = *mut libc::c_void;
pub type cmsUInt32Number = u32;
pub type cmsColorSpaceSignature = cmsUInt32Number;
pub type cmsTagSignature = cmsUInt32Number;

/// `cmsSigRgbData` — input/output space is RGB (`lcms2.h`).
pub const CMS_SIG_RGB_DATA: cmsColorSpaceSignature = u32::from_be_bytes(*b"RGB ");
/// ICC `rXYZ` / `gXYZ` / `bXYZ` device primary tags (`lcms2.h` `cmsSigRedColorantTag`, …).
pub const CMS_SIG_RED_COLORANT: cmsTagSignature = u32::from_be_bytes(*b"rXYZ");
pub const CMS_SIG_GREEN_COLORANT: cmsTagSignature = u32::from_be_bytes(*b"gXYZ");
pub const CMS_SIG_BLUE_COLORANT: cmsTagSignature = u32::from_be_bytes(*b"bXYZ");

/// `cmsCIEXYZ` from `lcms2.h` (`cmsFloat64Number` ≡ `double`).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CmsCiexyz {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

/// `INTENT_PERCEPTUAL` from `lcms2.h`. Other intents (relative, saturation,
/// absolute) follow standard ICC numbering 1, 2, 3.
pub const LCMS_INTENT_PERCEPTUAL: cmsUInt32Number = 0;

/// Encoded as `FLOAT_SH(1) | COLORSPACE_SH(PT_CMYK=6) | CHANNELS_SH(4) | BYTES_SH(4)`
/// from `lcms2.h` — interleaved 4-channel f32 CMYK with the standard ICC ink
/// convention (`0 = no ink, 1 = max ink`). Note this is the **opposite** of
/// libjxl's CMS convention (`0 = max ink, 1 = no ink`); callers must invert
/// values when bridging between the two.
pub const LCMS_TYPE_CMYK_FLT: cmsUInt32Number = 0x00460024;

/// Encoded as `FLOAT_SH(1) | COLORSPACE_SH(PT_RGB=4) | EXTRA_SH(1) | CHANNELS_SH(3) | BYTES_SH(4)`
/// from `lcms2.h` — interleaved RGBA f32 with the alpha channel passing through
/// untouched (lcms2 keeps "extra" channels as a copy from input to output).
pub const LCMS_TYPE_RGBA_FLT: cmsUInt32Number = 0x0044009C;

#[link(name = "lcms2", kind = "static")]
unsafe extern "C" {
    /// Builds an `cmsHPROFILE` from raw ICC bytes. Returns NULL on failure.
    pub fn cmsOpenProfileFromMem(
        mem_ptr: *const libc::c_void,
        mem_size: cmsUInt32Number,
    ) -> cmsHPROFILE;

    pub fn cmsGetColorSpace(hProfile: cmsHPROFILE) -> cmsColorSpaceSignature;

    /// Returns a pointer to **read-only** tag data owned by the profile; `NULL` if missing.
    /// Caller must not free; copy out `CmsCiexyz` immediately if needed.
    pub fn cmsReadTag(hProfile: cmsHPROFILE, sig: cmsTagSignature) -> *mut libc::c_void;

    /// Returns a freshly-allocated profile representing standard sRGB
    /// (D65 white point, sRGB primaries, sRGB transfer). Caller must
    /// `cmsCloseProfile` when done.
    pub fn cmsCreate_sRGBProfile() -> cmsHPROFILE;

    /// Build a colorspace transform. Returns NULL on failure (e.g. profiles
    /// have incompatible color spaces / channel counts).
    ///
    /// Pixel formats are bit-encoded constants (`LCMS_TYPE_*`).
    pub fn cmsCreateTransform(
        input: cmsHPROFILE,
        in_format: cmsUInt32Number,
        output: cmsHPROFILE,
        out_format: cmsUInt32Number,
        intent: cmsUInt32Number,
        flags: cmsUInt32Number,
    ) -> cmsHTRANSFORM;

    /// Run the transform on `num_pixels` samples. `input_buffer` and
    /// `output_buffer` may overlap only when output has fewer channels than
    /// input (in which case lcms2 allows the same pointer for both).
    pub fn cmsDoTransform(
        transform: cmsHTRANSFORM,
        input_buffer: *const libc::c_void,
        output_buffer: *mut libc::c_void,
        num_pixels: cmsUInt32Number,
    );

    pub fn cmsCloseProfile(profile: cmsHPROFILE) -> i32;
    pub fn cmsDeleteTransform(transform: cmsHTRANSFORM);
}

// ---------------------------------------------------------------------------
// lcms2 RAII — profiles must stay valid for the lifetime of any transform built
// from them. Declare [`CmsTransform`] after both profiles so it is dropped first.
// ---------------------------------------------------------------------------

/// Owned lcms2 profile handle (`cmsHPROFILE`).
pub struct CmsProfile(cmsHPROFILE);

impl CmsProfile {
    /// Parse ICC bytes; returns `None` when lcms rejects the profile.
    pub fn open_from_mem(bytes: &[u8]) -> Option<Self> {
        let p = unsafe {
            cmsOpenProfileFromMem(bytes.as_ptr().cast(), bytes.len() as cmsUInt32Number)
        };
        (!p.is_null()).then_some(Self(p))
    }

    /// Standard sRGB display profile.
    pub fn new_srgb() -> Option<Self> {
        let p = unsafe { cmsCreate_sRGBProfile() };
        (!p.is_null()).then_some(Self(p))
    }

    pub fn as_ptr(&self) -> cmsHPROFILE {
        self.0
    }

    /// Data (device) color space, e.g. [`CMS_SIG_RGB_DATA`].
    pub fn data_color_space(&self) -> cmsColorSpaceSignature {
        unsafe { cmsGetColorSpace(self.0) }
    }

    /// Reads an ICC **`XYZType`** tag (e.g. `rXYZ`) as CIEXYZ tristimulus values.
    pub fn read_tag_ciexyz(&self, tag: cmsTagSignature) -> Option<CmsCiexyz> {
        let p = unsafe { cmsReadTag(self.0, tag) };
        if p.is_null() {
            return None;
        }
        Some(unsafe { std::ptr::read(p.cast::<CmsCiexyz>()) })
    }
}

impl Drop for CmsProfile {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                cmsCloseProfile(self.0);
            }
            self.0 = std::ptr::null_mut();
        }
    }
}

/// Owned lcms2 transform (`cmsHTRANSFORM`). Destroy before closing the profiles
/// that were used to create it — call sites should hold transforms in an inner
/// scope or declare this field after the profile fields in a struct.
pub struct CmsTransform(cmsHTRANSFORM);

impl CmsTransform {
    pub fn new(
        input: &CmsProfile,
        in_format: cmsUInt32Number,
        output: &CmsProfile,
        out_format: cmsUInt32Number,
        intent: cmsUInt32Number,
        flags: cmsUInt32Number,
    ) -> Option<Self> {
        let p = unsafe {
            cmsCreateTransform(
                input.as_ptr(),
                in_format,
                output.as_ptr(),
                out_format,
                intent,
                flags,
            )
        };
        (!p.is_null()).then_some(Self(p))
    }

    pub fn as_ptr(&self) -> cmsHTRANSFORM {
        self.0
    }

    pub fn do_transform(
        &self,
        input: *const libc::c_void,
        output: *mut libc::c_void,
        num_pixels: cmsUInt32Number,
    ) {
        unsafe {
            cmsDoTransform(self.0, input, output, num_pixels);
        }
    }
}

impl Drop for CmsTransform {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                cmsDeleteTransform(self.0);
            }
            self.0 = std::ptr::null_mut();
        }
    }
}
