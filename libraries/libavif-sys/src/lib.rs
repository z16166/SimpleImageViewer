#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

pub type avifResult = libc::c_int;
pub type avifBool = libc::c_int;
pub type avifColorPrimaries = libc::c_int;
pub type avifTransferCharacteristics = libc::c_int;
pub type avifMatrixCoefficients = libc::c_int;
pub type avifPixelFormat = libc::c_int;
pub type avifRange = libc::c_int;
pub type avifChromaSamplePosition = libc::c_int;
pub type avifTransformFlags = u32;
pub type avifPlanesFlags = u32;
pub type avifRGBFormat = libc::c_int;
pub type avifChromaUpsampling = libc::c_int;
pub type avifChromaDownsampling = libc::c_int;
pub type avifImageContentTypeFlags = u32;

pub const AVIF_RESULT_OK: avifResult = 0;
pub const AVIF_IMAGE_CONTENT_COLOR_AND_ALPHA: u32 = (1 << 0) | (1 << 1);
pub const AVIF_IMAGE_CONTENT_GAIN_MAP: u32 = 1 << 2;
pub const AVIF_IMAGE_CONTENT_ALL: u32 =
    AVIF_IMAGE_CONTENT_COLOR_AND_ALPHA | AVIF_IMAGE_CONTENT_GAIN_MAP;

// `avifStrictFlags` / `avifStrictFlag` (libavif `avif.h`). Default is `AVIF_STRICT_ENABLED`;
// viewers often set `strictFlags` to 0 after `avifDecoderCreate()` for maximum compatibility.
pub const AVIF_STRICT_DISABLED: u32 = 0;
pub const AVIF_STRICT_PIXI_REQUIRED: u32 = 1 << 0;
pub const AVIF_STRICT_CLAP_VALID: u32 = 1 << 1;
pub const AVIF_STRICT_ALPHA_ISPE_REQUIRED: u32 = 1 << 2;
pub const AVIF_STRICT_ENABLED: u32 =
    AVIF_STRICT_PIXI_REQUIRED | AVIF_STRICT_CLAP_VALID | AVIF_STRICT_ALPHA_ISPE_REQUIRED;
pub const AVIF_RGB_FORMAT_RGBA: avifRGBFormat = 1;
pub const AVIF_COLOR_PRIMARIES_BT709: avifColorPrimaries = 1;
pub const AVIF_TRANSFER_CHARACTERISTICS_LINEAR: avifTransferCharacteristics = 8;
/// SMPTE ST 2084 (PQ). libavif's `linearToGamma` for PQ encodes "extended SDR" linear
/// (1.0 = SDR white = 203 nits) into [0,1] without the `LINEAR` clamp — preserves HDR.
pub const AVIF_TRANSFER_CHARACTERISTICS_SMPTE2084: avifTransferCharacteristics = 16;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct avifROData {
    pub data: *const u8,
    pub size: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct avifRWData {
    pub data: *mut u8,
    pub size: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct avifContentLightLevelInformationBox {
    pub maxCLL: u16,
    pub maxPALL: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct avifSignedFraction {
    pub n: i32,
    pub d: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct avifUnsignedFraction {
    pub n: u32,
    pub d: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct avifPixelAspectRatioBox {
    pub hSpacing: u32,
    pub vSpacing: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct avifCleanApertureBox {
    pub widthN: u32,
    pub widthD: u32,
    pub heightN: u32,
    pub heightD: u32,
    pub horizOffN: u32,
    pub horizOffD: u32,
    pub vertOffN: u32,
    pub vertOffD: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct avifImageRotation {
    pub angle: u8,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct avifImageMirror {
    pub axis: u8,
}

#[repr(C)]
pub struct avifGainMap {
    pub image: *mut avifImage,
    pub gainMapMin: [avifSignedFraction; 3],
    pub gainMapMax: [avifSignedFraction; 3],
    pub gainMapGamma: [avifUnsignedFraction; 3],
    pub baseOffset: [avifSignedFraction; 3],
    pub alternateOffset: [avifSignedFraction; 3],
    pub baseHdrHeadroom: avifUnsignedFraction,
    pub alternateHdrHeadroom: avifUnsignedFraction,
    pub useBaseColorSpace: avifBool,
    pub altICC: avifRWData,
    pub altColorPrimaries: avifColorPrimaries,
    pub altTransferCharacteristics: avifTransferCharacteristics,
    pub altMatrixCoefficients: avifMatrixCoefficients,
    pub altYUVRange: avifRange,
    pub altDepth: u32,
    pub altPlaneCount: u32,
    pub altCLLI: avifContentLightLevelInformationBox,
}

#[repr(C)]
pub struct avifDiagnostics {
    pub error: [libc::c_char; 256],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct avifImageItemProperty {
    pub boxtype: [u8; 4],
    pub usertype: [u8; 16],
    pub boxPayload: avifRWData,
}

#[repr(C)]
pub struct avifImage {
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub yuvFormat: avifPixelFormat,
    pub yuvRange: avifRange,
    pub yuvChromaSamplePosition: avifChromaSamplePosition,
    pub yuvPlanes: [*mut u8; 3],
    pub yuvRowBytes: [u32; 3],
    pub imageOwnsYUVPlanes: avifBool,
    pub alphaPlane: *mut u8,
    pub alphaRowBytes: u32,
    pub imageOwnsAlphaPlane: avifBool,
    pub alphaPremultiplied: avifBool,
    /// `avifImage.icc` from `avif.h`. **Must** appear between `alphaPremultiplied` and the CICP
    /// fields — omitting it shifts every later field by 16 bytes, causing `colorPrimaries` to read
    /// the low half of the `icc.data` pointer (garbage for ICC-bearing files; lucky 0 for
    /// ICC-less files because `avifImageCreateEmpty` zero-inits and the NULL pointer's low bits
    /// happen to land on `AVIF_COLOR_PRIMARIES_UNKNOWN`).
    pub icc: avifRWData,
    pub colorPrimaries: avifColorPrimaries,
    pub transferCharacteristics: avifTransferCharacteristics,
    pub matrixCoefficients: avifMatrixCoefficients,
    pub clli: avifContentLightLevelInformationBox,
    pub transformFlags: avifTransformFlags,
    pub pasp: avifPixelAspectRatioBox,
    pub clap: avifCleanApertureBox,
    pub irot: avifImageRotation,
    pub imir: avifImageMirror,
    pub exif: avifRWData,
    pub xmp: avifRWData,
    pub properties: *mut avifImageItemProperty,
    pub numProperties: usize,
    pub gainMap: *mut avifGainMap,
}

#[repr(C)]
pub struct avifRGBImage {
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub format: avifRGBFormat,
    pub chromaUpsampling: avifChromaUpsampling,
    pub chromaDownsampling: avifChromaDownsampling,
    pub avoidLibYUV: avifBool,
    pub ignoreAlpha: avifBool,
    pub alphaPremultiplied: avifBool,
    pub isFloat: avifBool,
    pub maxThreads: libc::c_int,
    pub pixels: *mut u8,
    pub rowBytes: u32,
}

#[repr(C)]
pub struct avifDecoder {
    _private: [u8; 0],
}

unsafe extern "C" {
    pub fn avifVersion() -> *const libc::c_char;
    pub fn avifResultToString(result: avifResult) -> *const libc::c_char;
    pub fn avifImageCreateEmpty() -> *mut avifImage;
    pub fn avifImageDestroy(image: *mut avifImage);
    pub fn avifDecoderCreate() -> *mut avifDecoder;
    pub fn avifDecoderDestroy(decoder: *mut avifDecoder);
    pub fn avifDecoderReadMemory(
        decoder: *mut avifDecoder,
        image: *mut avifImage,
        data: *const u8,
        size: usize,
    ) -> avifResult;
    pub fn avifRGBImageSetDefaults(rgb: *mut avifRGBImage, image: *const avifImage);
    pub fn avifRGBImageFreePixels(rgb: *mut avifRGBImage);
    pub fn avifImageYUVToRGB(image: *const avifImage, rgb: *mut avifRGBImage) -> avifResult;
    pub fn avifImageApplyGainMap(
        baseImage: *const avifImage,
        gainMap: *const avifGainMap,
        hdrHeadroom: f32,
        outputColorPrimaries: avifColorPrimaries,
        outputTransferCharacteristics: avifTransferCharacteristics,
        toneMappedImage: *mut avifRGBImage,
        clli: *mut avifContentLightLevelInformationBox,
        diag: *mut avifDiagnostics,
    ) -> avifResult;
    pub fn siv_avif_decoder_decode_all_content(decoder: *mut avifDecoder);
    pub fn siv_avif_decoder_set_image_content_flags(decoder: *mut avifDecoder, flags: u32);
    pub fn siv_avif_decoder_set_strict_flags(decoder: *mut avifDecoder, flags: u32);
}
