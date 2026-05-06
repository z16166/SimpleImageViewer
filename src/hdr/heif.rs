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

use crate::hdr::types::{
    HdrColorProfile, HdrImageMetadata, HdrLuminanceMetadata, HdrReference, HdrTransferFunction,
};
#[cfg(feature = "heif-native")]
use crate::hdr::types::{HdrGainMapMetadata, HdrImageBuffer, HdrPixelFormat, HdrToneMapSettings};
#[cfg(feature = "heif-native")]
use std::ffi::CStr;
#[cfg(feature = "heif-native")]
use std::path::Path;
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
    // ITU-T H.273 CICP transfer characteristics (not libjxl enums).
    let transfer_function = match transfer_characteristics {
        8 => HdrTransferFunction::Linear,
        13 => HdrTransferFunction::Srgb,
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
fn append_heif_unci_build_hint(msg: String) -> String {
    let lower = msg.to_lowercase();
    let unci_related = lower.contains("unci")
        || lower.contains("23001-17")
        || lower.contains("uncompressed image type");
    let brotli_unc = lower.contains("brotli") && (unci_related || lower.contains("generic compression"));

    if brotli_unc {
        return format!(
            "{msg} UNC with Brotli needs libheif built with ISO 23001-17 plus Brotli: feature `iso23001-17` pulls `zlib` + `brotli` and the CMake lock must allow `find_package(Brotli)` (`VCPKG_LOCK_FIND_PACKAGE_Brotli`). Re-run `vcpkg install`, then `cargo clean -p libheif-sys`."
        );
    }
    if unci_related {
        return format!(
            "{msg} For UNCI / ISO 23001-17 HEIFs, enable libheif feature `iso23001-17` (then `vcpkg install` / rebuild)."
        );
    }
    msg
}

/// Low-overhead / `mini` ISOBMFF (libheif overlay `experimental-mini`). Upstream corpora such as
/// `tests/data/simple_osm_tile_meta.avif` are **valid** reference tiles; without `mini` support,
/// `read_from_memory` may report `iloc`/extent past EOF even when the blob is intact.
#[cfg(feature = "heif-native")]
fn append_mini_format_read_hint(action: &str, msg: String) -> String {
    let lower = msg.to_ascii_lowercase();
    if action != "read HEIF from memory" {
        return msg;
    }
    if lower.contains("iloc")
        || lower.contains("extent")
        || lower.contains("outside of file bounds")
    {
        return format!(
            "{msg} Compact mini/low-overhead containers (libheif overlay `experimental-mini`, e.g. `simple_osm_tile_*.avif` in upstream `tests/data`) need libheif built with that overlay; without it, `iloc`/extent errors may appear on intact reference blobs. Rule out truncation too (size/checksum vs repo), then `cargo clean -p libheif-sys` after rebuilding libheif."
        );
    }
    if lower.contains("insufficient") {
        return format!(
            "{msg} Experimental mini / low-overhead HEIF/AVIF needs overlay `experimental-mini`; rebuild libheif, then `cargo clean -p libheif-sys`."
        );
    }
    msg
}

// --- libheif session (context + primary handle) ---------------------------------------------

#[cfg(feature = "heif-native")]
struct HeifCtxGuard(pub *mut libheif_sys::heif_context);

#[cfg(feature = "heif-native")]
impl Drop for HeifCtxGuard {
    fn drop(&mut self) {
        unsafe {
            libheif_sys::heif_context_free(self.0);
        }
    }
}

#[cfg(feature = "heif-native")]
struct HeifPrimaryGuard(pub *mut libheif_sys::heif_image_handle);

#[cfg(feature = "heif-native")]
impl Drop for HeifPrimaryGuard {
    fn drop(&mut self) {
        unsafe {
            libheif_sys::heif_image_handle_release(self.0);
        }
    }
}

#[cfg(feature = "heif-native")]
fn heif_error_to_string_lib(err: libheif_sys::heif_error) -> String {
    if err.message.is_null() {
        return format!("libheif error code {} subcode {}", err.code, err.subcode);
    }
    unsafe { CStr::from_ptr(err.message) }
        .to_string_lossy()
        .into_owned()
}

#[cfg(feature = "heif-native")]
fn ensure_heif_ok_lib(err: libheif_sys::heif_error, action: &str) -> Result<(), String> {
    if err.code == libheif_sys::heif_error_Ok {
        Ok(())
    } else {
        let raw = format!("Failed to {action}: {}", heif_error_to_string_lib(err));
        let expanded = append_heif_unci_build_hint(raw);
        let expanded = append_mini_format_read_hint(action, expanded);
        Err(expanded)
    }
}

/// Allocate libheif context, read the blob, and resolve the primary image handle.
#[cfg(feature = "heif-native")]
fn open_heif_primary_from_bytes(bytes: &[u8]) -> Result<(HeifCtxGuard, HeifPrimaryGuard), String> {
    {
        use std::sync::Once;
        static LOG_VERSION: Once = Once::new();
        LOG_VERSION.call_once(|| unsafe {
            let p = libheif_sys::heif_get_version();
            if !p.is_null() {
                log::debug!(
                    "[HEIF] linked libheif version: {}",
                    CStr::from_ptr(p).to_string_lossy()
                );
            }
        });
    }

    let context = HeifCtxGuard(unsafe { libheif_sys::heif_context_alloc() });
    if context.0.is_null() {
        return Err("Failed to allocate libheif context".to_string());
    }

    ensure_heif_ok_lib(
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
    ensure_heif_ok_lib(
        unsafe { libheif_sys::heif_context_get_primary_image_handle(context.0, &mut handle_ptr) },
        "get HEIF primary image",
    )?;
    if handle_ptr.is_null() {
        return Err("libheif returned a null primary image handle".to_string());
    }

    Ok((context, HeifPrimaryGuard(handle_ptr)))
}

/// Parse embedded Exif item payload (`Exif` metadata). Mirrors [`kamadak_exif::isobmff::get_exif_attr`]
/// stripping of the TIFF offset; falls back to treating the whole blob as TIFF if needed.
#[cfg(feature = "heif-native")]
fn orientation_from_heif_exif_item_blob(buf: &[u8]) -> Option<u16> {
    fn from_exif(exif: &exif::Exif) -> Option<u16> {
        let field = exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY)?;
        let o = field.value.get_uint(0)? as u16;
        ((1..=8).contains(&o)).then_some(o)
    }

    if buf.len() >= 6 {
        let offset = u32::from_be_bytes(buf.get(0..4)?.try_into().ok()?) as usize;
        if buf.len() >= 4 + offset {
            let tiff_tail = buf.get(4 + offset..)?.to_vec();
            if let Ok(exif) = exif::Reader::new().read_raw(tiff_tail) {
                if let Some(o) = from_exif(&exif) {
                    return Some(o);
                }
            }
        }
    }
    if let Ok(exif) = exif::Reader::new().read_raw(buf.to_vec()) {
        return from_exif(&exif);
    }
    None
}

#[cfg(feature = "heif-native")]
fn heif_exif_orientation_from_handle(primary: &HeifPrimaryGuard) -> Option<u16> {
    let handle = primary.0;
    unsafe {
        let total =
            libheif_sys::heif_image_handle_get_number_of_metadata_blocks(handle, std::ptr::null());
        if total <= 0 {
            return None;
        }
        let total = total as usize;
        let mut ids = vec![0u32; total];
        let n = libheif_sys::heif_image_handle_get_list_of_metadata_block_IDs(
            handle,
            std::ptr::null(),
            ids.as_mut_ptr(),
            total as i32,
        );
        let n = n.max(0) as usize;
        for &id in ids.iter().take(n) {
            let typ = libheif_sys::heif_image_handle_get_metadata_type(handle, id);
            if typ.is_null() {
                continue;
            }
            let typ_bytes = CStr::from_ptr(typ).to_bytes();
            if typ_bytes != b"Exif" {
                continue;
            }
            let sz = libheif_sys::heif_image_handle_get_metadata_size(handle, id);
            if sz == 0 {
                continue;
            }
            let mut buf = vec![0u8; sz];
            let err =
                libheif_sys::heif_image_handle_get_metadata(handle, id, buf.as_mut_ptr().cast());
            if err.code != libheif_sys::heif_error_Ok {
                continue;
            }
            if let Some(o) = orientation_from_heif_exif_item_blob(&buf) {
                return Some(o);
            }
        }
        None
    }
}

/// Read [`exif::Tag::Orientation`] from libheif-attached `Exif` metadata items (works when pure ISOBMFF
/// scanning in [`crate::metadata_utils::get_exif_orientation`] misses the `Exif` item).
#[cfg(feature = "heif-native")]
pub(crate) fn libheif_exif_orientation_tag(path: &Path) -> Option<u16> {
    let mmap = crate::mmap_util::map_file(path).ok()?;
    let (_ctx, primary) = open_heif_primary_from_bytes(&mmap[..]).ok()?;
    heif_exif_orientation_from_handle(&primary)
}

/// When the decoded raster's width/height are the **swap** of libheif’s `ispe` width/height (non-square),
/// decoder has already applied a 90°/270° HEIF transform on the pixel grid — suppress applying EXIF
/// Orientation again to avoid double rotation.
#[cfg(feature = "heif-native")]
pub(crate) fn decoded_pixels_match_swapped_ispe(path: &Path, decoded_w: u32, decoded_h: u32) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ext != "heic" && ext != "heif" && ext != "hif" {
        return false;
    }
    let mmap = match crate::mmap_util::map_file(path) {
        Ok(m) => m,
        Err(_) => return false,
    };
    let (_ctx, primary) = match open_heif_primary_from_bytes(&mmap[..]) {
        Ok(x) => x,
        Err(_) => return false,
    };
    unsafe {
        let iw = libheif_sys::heif_image_handle_get_ispe_width(primary.0);
        let ih = libheif_sys::heif_image_handle_get_ispe_height(primary.0);
        if iw <= 0 || ih <= 0 {
            return false;
        }
        let iw = iw as u32;
        let ih = ih as u32;
        if iw == ih {
            return false;
        }
        decoded_w == ih && decoded_h == iw
    }
}

#[cfg(feature = "heif-native")]
pub(crate) fn load_heif_hdr(
    path: &std::path::Path,
    hdr_target_capacity: f32,
    tone_map: HdrToneMapSettings,
) -> Result<crate::loader::ImageData, String> {
    let hdr = decode_heif_hdr(path)?;
    let fallback_pixels = if crate::loader::hdr_display_requests_sdr_preview(hdr_target_capacity)
    {
        crate::hdr::decode::hdr_to_sdr_rgba8_with_tone_settings(
            &hdr,
            tone_map.exposure_ev,
            &tone_map,
        )?
    } else {
        crate::loader::cheap_hdr_sdr_placeholder_rgba8(hdr.width, hdr.height)?
    };
    let fallback = crate::loader::DecodedImage::new(hdr.width, hdr.height, fallback_pixels);

    Ok(crate::loader::ImageData::Hdr { hdr, fallback })
}

#[cfg(feature = "heif-native")]
pub(crate) fn decode_heif_hdr(path: &std::path::Path) -> Result<HdrImageBuffer, String> {
    let mmap = crate::mmap_util::map_file(path)
        .map_err(|err| format!("Failed to read HEIF: {err}"))?;
    decode_heif_hdr_bytes(&mmap[..])
}

#[cfg(feature = "heif-native")]
pub(crate) fn decode_heif_hdr_bytes(bytes: &[u8]) -> Result<HdrImageBuffer, String> {
    let (_ctx, handle) = open_heif_primary_from_bytes(bytes)?;

    let mut metadata = read_heif_metadata(handle.0);
    if let Some(diagnostic) = inspect_heif_gain_map_auxiliaries(handle.0) {
        metadata.gain_map = Some(diagnostic);
    }
    refine_heif_transfer_for_primary_bit_depth(handle.0, &mut metadata);
    crate::hdr::types::log_unrecognized_embedded_icc_after_decode(&metadata);

    decode_primary_heif_to_hdr(handle.0, metadata)
}

/// Decode the primary HEIF tile to HDR float RGBA. Tries interleaved 16-bit RGBA first, then other
/// interleaved layouts, YCbCr (`4:2:2` / `4:4:4` / `4:2:0`), planar RGB, and 8-bit interleaved fallbacks.
#[cfg(feature = "heif-native")]
fn decode_primary_heif_to_hdr(
    handle: *const libheif_sys::heif_image_handle,
    metadata: HdrImageMetadata,
) -> Result<HdrImageBuffer, String> {
    let interleaved_aa = match decode_primary_interleaved_rrggbbaa_le(handle, &metadata) {
        Ok(img) => return Ok(img),
        Err(e) => e,
    };

    let interleaved_rgb16 = match decode_primary_interleaved_rrggbbe_le(handle, &metadata) {
        Ok(img) => return Ok(img),
        Err(e) => e,
    };

    let y422 = match decode_primary_ycbcr(handle, &metadata, libheif_sys::heif_chroma_422) {
        Ok(b) => return Ok(b),
        Err(e) => e,
    };

    let y444 = match decode_primary_ycbcr(handle, &metadata, libheif_sys::heif_chroma_444) {
        Ok(b) => return Ok(b),
        Err(e) => e,
    };

    let y420 = match decode_primary_ycbcr(handle, &metadata, libheif_sys::heif_chroma_420) {
        Ok(b) => return Ok(b),
        Err(e) => e,
    };

    let planar = match decode_primary_planar_rgb444(handle, &metadata) {
        Ok(b) => return Ok(b),
        Err(e) => e,
    };

    let rgba8 = match decode_primary_interleaved_rgba8(handle, &metadata) {
        Ok(b) => return Ok(b),
        Err(e) => e,
    };

    let rgb8 = match decode_primary_interleaved_rgb8(handle, &metadata) {
        Ok(b) => return Ok(b),
        Err(e) => e,
    };

    Err(append_heif_unci_build_hint(format!(
        "decode HEIF (all targets failed): RGBA16 interleaved: {interleaved_aa}; RGB16 interleaved RRGGBB LE: {interleaved_rgb16}; YCbCr 422: {y422}; YCbCr 444: {y444}; YCbCr 420: {y420}; planar RGB444: {planar}; RGBA8 interleaved: {rgba8}; RGB8 interleaved: {rgb8}"
    )))
}

#[cfg(feature = "heif-native")]
struct RawHeifImage(pub *mut libheif_sys::heif_image);

#[cfg(feature = "heif-native")]
impl Drop for RawHeifImage {
    fn drop(&mut self) {
        unsafe { libheif_sys::heif_image_release(self.0) };
    }
}

#[cfg(feature = "heif-native")]
fn heif_try_decode_into(
    handle: *const libheif_sys::heif_image_handle,
    cs: libheif_sys::heif_colorspace,
    chroma: libheif_sys::heif_chroma,
    _detail: &'static str,
) -> Result<RawHeifImage, libheif_sys::heif_error> {
    let mut image_ptr = std::ptr::null_mut();
    let err = unsafe {
        libheif_sys::heif_decode_image(
            handle,
            &mut image_ptr,
            cs,
            chroma,
            std::ptr::null(),
        )
    };
    if err.code != libheif_sys::heif_error_Ok {
        return Err(err);
    }
    if image_ptr.is_null() {
        return Err(libheif_sys::heif_error {
            code: -1,
            subcode: 0,
            message: std::ptr::null(),
        });
    }
    Ok(RawHeifImage(image_ptr))
}

#[cfg(feature = "heif-native")]
fn heif_err_to_plain(err: libheif_sys::heif_error) -> String {
    use std::ffi::CStr;
    if err.message.is_null() {
        return format!("libheif error code {} subcode {}", err.code, err.subcode);
    }
    unsafe { CStr::from_ptr(err.message) }
        .to_string_lossy()
        .into_owned()
}

#[cfg(feature = "heif-native")]
fn decode_primary_interleaved_rrggbbaa_le(
    handle: *const libheif_sys::heif_image_handle,
    metadata: &HdrImageMetadata,
) -> Result<HdrImageBuffer, String> {
    let img = match heif_try_decode_into(
        handle,
        libheif_sys::heif_colorspace_RGB,
        libheif_sys::heif_chroma_interleaved_RRGGBBAA_LE,
        "RGBA16",
    ) {
        Ok(i) => i,
        Err(e) => {
            return Err(format!(
                "Failed to decode HEIF image as interleaved 16-bit RGBA ({})",
                heif_err_to_plain(e),
            ));
        }
    };

    hdr_buffer_from_interleaved_rgb16_le(handle, metadata, img.0, 4)
}

#[cfg(feature = "heif-native")]
fn decode_primary_interleaved_rrggbbe_le(
    handle: *const libheif_sys::heif_image_handle,
    metadata: &HdrImageMetadata,
) -> Result<HdrImageBuffer, String> {
    let img = match heif_try_decode_into(
        handle,
        libheif_sys::heif_colorspace_RGB,
        libheif_sys::heif_chroma_interleaved_RRGGBB_LE,
        "RGB16 triple",
    ) {
        Ok(i) => i,
        Err(e) => {
            return Err(format!(
                "Failed to decode HEIF image as interleaved 16-bit RRGGBB LE ({})",
                heif_err_to_plain(e),
            ));
        }
    };

    hdr_buffer_from_interleaved_rgb16_le(handle, metadata, img.0, 3)
}

#[cfg(feature = "heif-native")]
fn decode_primary_interleaved_rgba8(
    handle: *const libheif_sys::heif_image_handle,
    metadata: &HdrImageMetadata,
) -> Result<HdrImageBuffer, String> {
    let img = match heif_try_decode_into(
        handle,
        libheif_sys::heif_colorspace_RGB,
        libheif_sys::heif_chroma_interleaved_RGBA,
        "RGBA8",
    ) {
        Ok(i) => i,
        Err(e) => {
            return Err(format!(
                "Failed to decode HEIF image as interleaved RGBA8 ({})",
                heif_err_to_plain(e),
            ));
        }
    };

    hdr_buffer_from_interleaved_rgb8_packed(handle, metadata, img.0, 4)
}

#[cfg(feature = "heif-native")]
fn decode_primary_interleaved_rgb8(
    handle: *const libheif_sys::heif_image_handle,
    metadata: &HdrImageMetadata,
) -> Result<HdrImageBuffer, String> {
    let img = match heif_try_decode_into(
        handle,
        libheif_sys::heif_colorspace_RGB,
        libheif_sys::heif_chroma_interleaved_RGB,
        "RGB8",
    ) {
        Ok(i) => i,
        Err(e) => {
            return Err(format!(
                "Failed to decode HEIF image as interleaved RGB8 ({})",
                heif_err_to_plain(e),
            ));
        }
    };

    hdr_buffer_from_interleaved_rgb8_packed(handle, metadata, img.0, 3)
}

#[cfg(feature = "heif-native")]
fn decode_primary_planar_rgb444(
    handle: *const libheif_sys::heif_image_handle,
    metadata: &HdrImageMetadata,
) -> Result<HdrImageBuffer, String> {
    let img = match heif_try_decode_into(
        handle,
        libheif_sys::heif_colorspace_RGB,
        libheif_sys::heif_chroma_444,
        "RGB444 planar",
    ) {
        Ok(i) => i,
        Err(e) => {
            return Err(format!(
                "Failed to decode HEIF image as planar RGB444 ({})",
                heif_err_to_plain(e),
            ));
        }
    };

    hdr_buffer_from_planar_rgb444(handle, metadata, img.0)
}

#[cfg(feature = "heif-native")]
fn decode_primary_ycbcr(
    handle: *const libheif_sys::heif_image_handle,
    metadata: &HdrImageMetadata,
    chroma: libheif_sys::heif_chroma,
) -> Result<HdrImageBuffer, String> {
    let chroma_detail = chroma_plane_label(chroma);
    let img = match heif_try_decode_into(
        handle,
        libheif_sys::heif_colorspace_YCbCr,
        chroma,
        chroma_detail,
    ) {
        Ok(i) => i,
        Err(e) => {
            return Err(format!(
                "Failed to decode HEIF image as YCbCr ({chroma_detail}) ({})",
                heif_err_to_plain(e),
            ));
        }
    };

    hdr_buffer_from_ycbcr(handle, metadata, img.0, chroma)
}

#[cfg(feature = "heif-native")]
fn chroma_plane_label(chroma: libheif_sys::heif_chroma) -> &'static str {
    match chroma {
        c if c == libheif_sys::heif_chroma_420 => "420",
        c if c == libheif_sys::heif_chroma_422 => "422",
        c if c == libheif_sys::heif_chroma_444 => "444",
        _ => "YCbCr",
    }
}



#[cfg(feature = "heif-native")]
fn hdr_buffer_from_interleaved_rgb16_le(
    handle: *const libheif_sys::heif_image_handle,
    metadata: &HdrImageMetadata,
    image: *const libheif_sys::heif_image,
    components: u8,
) -> Result<HdrImageBuffer, String> {
    if components != 3 && components != 4 {
        return Err(format!(
            "unsupported interleaved 16-bit component count ({components}); expected 3 (RGB) or 4 (RGBA)"
        ));
    }

    let width_i = unsafe { libheif_sys::heif_image_get_primary_width(image) };
    let height_i = unsafe { libheif_sys::heif_image_get_primary_height(image) };
    if width_i <= 0 || height_i <= 0 {
        return Err("libheif decoded zero-sized image".to_string());
    }
    let mut stride = 0_usize;
    let plane = unsafe {
        libheif_sys::heif_image_get_plane_readonly2(
            image,
            libheif_sys::heif_channel_interleaved,
            &mut stride,
        )
    };
    if plane.is_null() {
        return Err("libheif did not expose an interleaved RGB/RGBA plane".to_string());
    }

    let width = width_i as u32;
    let height = height_i as u32;
    let bytes_per_pixel = (components as usize) * std::mem::size_of::<u16>();
    let row_bytes = width as usize * bytes_per_pixel;
    if stride < row_bytes {
        return Err(format!(
            "libheif row stride too small: got {stride}, expected at least {row_bytes}",
        ));
    }

    let bit_depth = heif_sample_bit_depth(image, handle)?;
    let scale = ((1_u32 << bit_depth.min(16)) - 1) as f32;
    let mut rgba_f32 = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height as usize {
        let row = unsafe { std::slice::from_raw_parts(plane.add(y * stride), row_bytes) };
        for px in row.chunks_exact(bytes_per_pixel) {
            rgba_f32.push(u16::from_le_bytes([px[0], px[1]]) as f32 / scale);
            rgba_f32.push(u16::from_le_bytes([px[2], px[3]]) as f32 / scale);
            rgba_f32.push(u16::from_le_bytes([px[4], px[5]]) as f32 / scale);
            if components == 4 {
                rgba_f32.push(u16::from_le_bytes([px[6], px[7]]) as f32 / scale);
            } else {
                rgba_f32.push(1.0);
            }
        }
    }

    let color_space = metadata.color_space_hint();
    Ok(HdrImageBuffer {
        width,
        height,
        format: HdrPixelFormat::Rgba32Float,
        color_space,
        metadata: metadata.clone(),
        rgba_f32: Arc::new(rgba_f32),
    })
}

#[cfg(feature = "heif-native")]
fn hdr_buffer_from_interleaved_rgb8_packed(
    handle: *const libheif_sys::heif_image_handle,
    metadata: &HdrImageMetadata,
    image: *const libheif_sys::heif_image,
    components: u8,
) -> Result<HdrImageBuffer, String> {
    if components != 3 && components != 4 {
        return Err(format!(
            "unsupported interleaved 8-bit component count ({components}); expected 3 (RGB) or 4 (RGBA)"
        ));
    }

    let width_i = unsafe { libheif_sys::heif_image_get_primary_width(image) };
    let height_i = unsafe { libheif_sys::heif_image_get_primary_height(image) };
    if width_i <= 0 || height_i <= 0 {
        return Err("libheif decoded zero-sized image".to_string());
    }
    let mut stride = 0_usize;
    let plane = unsafe {
        libheif_sys::heif_image_get_plane_readonly2(
            image,
            libheif_sys::heif_channel_interleaved,
            &mut stride,
        )
    };
    if plane.is_null() {
        return Err("libheif did not expose an interleaved RGB/RGBA plane".to_string());
    }

    let width = width_i as u32;
    let height = height_i as u32;
    let bytes_per_pixel = components as usize;
    let row_bytes = width as usize * bytes_per_pixel;
    if stride < row_bytes {
        return Err(format!(
            "libheif row stride too small: got {stride}, expected at least {row_bytes}",
        ));
    }

    let bit_depth = heif_sample_bit_depth(image, handle)?.min(8).max(1);
    let scale = ((1_u32 << bit_depth as u32) - 1) as f32;
    let mut rgba_f32 = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height as usize {
        let row = unsafe { std::slice::from_raw_parts(plane.add(y * stride), row_bytes) };
        for px in row.chunks_exact(bytes_per_pixel) {
            rgba_f32.push(px[0] as f32 / scale);
            rgba_f32.push(px[1] as f32 / scale);
            rgba_f32.push(px[2] as f32 / scale);
            if components == 4 {
                rgba_f32.push(px[3] as f32 / scale);
            } else {
                rgba_f32.push(1.0);
            }
        }
    }

    let color_space = metadata.color_space_hint();
    Ok(HdrImageBuffer {
        width,
        height,
        format: HdrPixelFormat::Rgba32Float,
        color_space,
        metadata: metadata.clone(),
        rgba_f32: Arc::new(rgba_f32),
    })
}

#[cfg(feature = "heif-native")]
fn planar_storage_span_bytes(
    image: *const libheif_sys::heif_image,
    channel: libheif_sys::heif_channel,
) -> usize {
    let bpp = unsafe { libheif_sys::heif_image_get_bits_per_pixel(image, channel).max(8) };
    ((bpp + 7) / 8) as usize
}

#[cfg(feature = "heif-native")]
fn planar_semantic_depth_bits(
    image: *const libheif_sys::heif_image,
    handle: *const libheif_sys::heif_image_handle,
    channel: libheif_sys::heif_channel,
) -> Result<i32, String> {
    let decoded_range =
        unsafe { libheif_sys::heif_image_get_bits_per_pixel_range(image, channel) };
    let luma = unsafe { libheif_sys::heif_image_handle_get_luma_bits_per_pixel(handle) };
    let chroma =
        unsafe { libheif_sys::heif_image_handle_get_chroma_bits_per_pixel(handle) };
    let per_ch = decoded_range.max(luma).max(chroma).max(8);
    Ok(per_ch.min(32))
}

#[cfg(feature = "heif-native")]
fn planar_scale_from_depth(semantic_bits: i32) -> f32 {
    let d = semantic_bits.clamp(1, 32);
    let maxv = (1_u64 << d as u32).saturating_sub(1).max(1);
    maxv as f32
}

#[cfg(feature = "heif-native")]
fn planar_read_sample(
    row_base: *const u8,
    x: usize,
    stride_bytes: usize,
    storage_span: usize,
) -> Result<u32, String> {
    let offset = x
        .checked_mul(storage_span)
        .ok_or_else(|| "planar sample offset overflow".to_string())?;
    if offset + storage_span > stride_bytes {
        return Err("planar sample read past row stride".to_string());
    }
    unsafe {
        match storage_span {
            1 => Ok(*row_base.add(offset) as u32),
            2 => {
                let bytes = std::slice::from_raw_parts(row_base.add(offset), 2);
                Ok(u16::from_le_bytes([bytes[0], bytes[1]]) as u32)
            }
            4 => {
                let bytes = std::slice::from_raw_parts(row_base.add(offset), 4);
                Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
            }
            n => Err(format!(
                "unsupported planar sample storage width ({n}); extend reader for this HEIF variant"
            )),
        }
    }
}

#[cfg(feature = "heif-native")]
fn hdr_buffer_from_planar_rgb444(
    handle: *const libheif_sys::heif_image_handle,
    metadata: &HdrImageMetadata,
    image: *const libheif_sys::heif_image,
) -> Result<HdrImageBuffer, String> {
    use libheif_sys::{heif_channel_Alpha, heif_channel_B, heif_channel_G, heif_channel_R};

    for ch in [heif_channel_R, heif_channel_G, heif_channel_B] {
        if unsafe { libheif_sys::heif_image_has_channel(image, ch) } == 0 {
            return Err("planar RGB444: missing R/G/B channel".to_string());
        }
    }

    let width_i = unsafe { libheif_sys::heif_image_get_width(image, heif_channel_R) };
    let height_i = unsafe { libheif_sys::heif_image_get_height(image, heif_channel_R) };
    if width_i <= 0 || height_i <= 0 {
        return Err("planar RGB: zero-sized plane".to_string());
    }
    let w = width_i as usize;
    let h = height_i as usize;

    let has_alpha = unsafe { libheif_sys::heif_image_has_channel(image, heif_channel_Alpha) != 0 };

    let mut stride_r = 0usize;
    let ptr_r = unsafe {
        libheif_sys::heif_image_get_plane_readonly2(image, heif_channel_R, &mut stride_r)
    };
    let mut stride_g = 0usize;
    let ptr_g = unsafe {
        libheif_sys::heif_image_get_plane_readonly2(image, heif_channel_G, &mut stride_g)
    };
    let mut stride_b = 0usize;
    let ptr_b = unsafe {
        libheif_sys::heif_image_get_plane_readonly2(image, heif_channel_B, &mut stride_b)
    };
    let alpha_pack = if has_alpha {
        let mut stride_a = 0usize;
        let ptr_a =
            unsafe { libheif_sys::heif_image_get_plane_readonly2(image, heif_channel_Alpha, &mut stride_a) };
        if ptr_a.is_null() || stride_a == 0 {
            None
        } else {
            let span_a_val = planar_storage_span_bytes(image, heif_channel_Alpha);
            let scale_a_val = planar_scale_from_depth(planar_semantic_depth_bits(image, handle, heif_channel_Alpha)?);
            Some((ptr_a, stride_a, span_a_val, scale_a_val))
        }
    } else {
        None
    };

    if ptr_r.is_null() || ptr_g.is_null() || ptr_b.is_null() {
        return Err("planar RGB: null plane pointer".to_string());
    }

    let span_r = planar_storage_span_bytes(image, heif_channel_R);
    let span_g = planar_storage_span_bytes(image, heif_channel_G);
    let span_b = planar_storage_span_bytes(image, heif_channel_B);

    let scale_r = planar_scale_from_depth(planar_semantic_depth_bits(image, handle, heif_channel_R)?);
    let scale_g = planar_scale_from_depth(planar_semantic_depth_bits(image, handle, heif_channel_G)?);
    let scale_b = planar_scale_from_depth(planar_semantic_depth_bits(image, handle, heif_channel_B)?);

    let mut rgba_f32 = Vec::with_capacity(w * h * 4);

    for y in 0..h {
        let row_r = unsafe { ptr_r.byte_add(y * stride_r) };
        let row_g = unsafe { ptr_g.byte_add(y * stride_g) };
        let row_b = unsafe { ptr_b.byte_add(y * stride_b) };

        let min_stride_need_r = span_r * w.max(1);
        let min_stride_need_g = span_g * w.max(1);
        let min_stride_need_b = span_b * w.max(1);
        if stride_r < min_stride_need_r || stride_g < min_stride_need_g || stride_b < min_stride_need_b {
            return Err("planar RGB: stride inconsistent with dimensions".to_string());
        }

        if let Some((_, alpha_stride_px, alpha_span_px, _)) = alpha_pack
            && alpha_stride_px < alpha_span_px * w.max(1)
        {
            return Err("planar RGB: alpha stride inconsistent".to_string());
        }

        for x_px in 0..w {
            let rn = planar_read_sample(row_r, x_px, stride_r, span_r)?;
            let gn = planar_read_sample(row_g, x_px, stride_g, span_g)?;
            let bn = planar_read_sample(row_b, x_px, stride_b, span_b)?;

            rgba_f32.push(rn as f32 / scale_r.max(1.0));
            rgba_f32.push(gn as f32 / scale_g.max(1.0));
            rgba_f32.push(bn as f32 / scale_b.max(1.0));

            if let Some((ap_base, sar, spam_a_px, scl_a)) = alpha_pack {
                let row_a = unsafe { ap_base.byte_add(y * sar) };
                let an = planar_read_sample(row_a, x_px, sar, spam_a_px)?;
                rgba_f32.push((an as f32 / scl_a.max(1.0)).clamp(0.0, 1.0));
            } else {
                rgba_f32.push(1.0);
            }
        }
    }

    let color_space = metadata.color_space_hint();
    Ok(HdrImageBuffer {
        width: width_i as u32,
        height: height_i as u32,
        format: HdrPixelFormat::Rgba32Float,
        color_space,
        metadata: metadata.clone(),
        rgba_f32: Arc::new(rgba_f32),
    })
}

#[cfg(feature = "heif-native")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HeifYcbcrMatrix {
    Bt601,
    Bt709,
    /// Rec. ITU-R BT.2020 Y'Cb'Cr' to R'G'B' via non-constant luminance Kr/Kb (CICP 9 and 10). True
    /// constant-luminance coding for MC=9 only is not split out; stills usually match the NCL matrix.
    Bt2020Ncl,
    /// CICP matrix_coefficients 0 — no colour difference; replicate luma.
    /// True Y′-only path (R=G=B=Y′) when chroma is absent — not selected from NCLX `matrix_coefficients`
    /// alone (code 0 in HEIF often means “unspecified YUV”, not monochrome video).
    #[allow(dead_code)]
    Monochrome,
}

#[cfg(feature = "heif-native")]
fn heif_ycbcr_matrix_from_nclx(
    metadata: &HdrImageMetadata,
    y_width: usize,
    y_height: usize,
) -> HeifYcbcrMatrix {
    match &metadata.color_profile {
        HdrColorProfile::Cicp {
            matrix_coefficients: mc,
            ..
        } => match *mc {
            // H.273 matrix 0 = RGB identity (non‑YCbCr); **HEIF stills** sometimes tag 0 / 2 when the
            // encoder meant “unspecified”. Interpreting that as monochrome destroys colour — use a
            // simple SD vs HD **luma resolution** split (common broadcast rule of thumb).
            0 | 2 => {
                let hdish = y_width >= 1280 || y_height >= 720;
                if hdish {
                    HeifYcbcrMatrix::Bt709
                } else {
                    HeifYcbcrMatrix::Bt601
                }
            }
            5 | 6 => HeifYcbcrMatrix::Bt601,
            9 | 10 | 12 => HeifYcbcrMatrix::Bt2020Ncl,
            _ => HeifYcbcrMatrix::Bt709,
        },
        _ => HeifYcbcrMatrix::Bt709,
    }
}

#[cfg(feature = "heif-native")]
fn bt2020_ncl_chroma_derived_constants() -> (f32, f32, f32, f32) {
    let kr = 0.2627_f32;
    let kb = 0.0593_f32;
    let kg = 1.0_f32 - kr - kb;
    let k_rr = 2.0_f32 * (1.0_f32 - kr);
    let k_bb = 2.0_f32 * (1.0_f32 - kb);
    let k_gr = -2.0_f32 * kr * (1.0_f32 - kr) / kg;
    let k_gb = -2.0_f32 * kb * (1.0_f32 - kb) / kg;
    (k_rr, k_bb, k_gr, k_gb)
}

/// Converts **electrical** Y′ and centred chroma (**Pb/Pr**, i.e. Cb−mid / Cr−mid in normalized space —
/// JPEG full-pack uses `Cb_norm - 0.5`; narrow-range uses studio `Epb`/`Epr`) to non‑linear R′G′B′.
#[cfg(feature = "heif-native")]
fn ycbcr_linear_to_rgb(ey: f32, pb: f32, pr: f32, matrix: HeifYcbcrMatrix) -> [f32; 3] {
    match matrix {
        HeifYcbcrMatrix::Monochrome => [ey, ey, ey],
        HeifYcbcrMatrix::Bt601 => {
            let r = ey + 1.402_f32 * pr;
            let g = ey - 0.344_136_f32 * pb - 0.714_136_f32 * pr;
            let b = ey + 1.772_f32 * pb;
            [r, g, b]
        }
        HeifYcbcrMatrix::Bt709 => {
            let r = ey + 1.5748_f32 * pr;
            let g = ey - 0.187_324_f32 * pb - 0.468_124_f32 * pr;
            let b = ey + 1.8556_f32 * pb;
            [r, g, b]
        }
        HeifYcbcrMatrix::Bt2020Ncl => {
            let (k_rr, k_bb, k_gr, k_gb) = bt2020_ncl_chroma_derived_constants();
            let r = ey + k_rr * pr;
            let g = ey + k_gb * pb + k_gr * pr;
            let b = ey + k_bb * pb;
            [r, g, b]
        }
    }
}

#[cfg(feature = "heif-native")]
fn nclx_limited_range_from_metadata(metadata: &HdrImageMetadata) -> bool {
    matches!(
        &metadata.color_profile,
        HdrColorProfile::Cicp {
            full_range: false,
            ..
        }
    )
}

/// Limited-range studio swing: Ey = (Y - 16·2^(n-8)) / (219·2^(n-8)), Epb/Epr = (C - 128·2^(n-8)) / (224·2^(n-8)).
#[cfg(feature = "heif-native")]
fn studio_digital_sample_to_normalized(
    code: u32,
    semantic_bits: i32,
    is_luma: bool,
) -> Result<f32, String> {
    let d = semantic_bits.clamp(8, 16);
    let shift = (d - 8).clamp(0, 8) as u32;
    let y_floor = (16_i32
        .checked_shl(shift)
        .ok_or_else(|| "studio Y offset shift".to_string())?) as f32;
    let y_span = (219_i32
        .checked_shl(shift)
        .ok_or_else(|| "studio Y span shift".to_string())?) as f32;
    let c_mid = (128_i32
        .checked_shl(shift)
        .ok_or_else(|| "studio chroma midpoint shift".to_string())?) as f32;
    let c_span = (224_i32
        .checked_shl(shift)
        .ok_or_else(|| "studio chroma span shift".to_string())?) as f32;

    if is_luma {
        if y_span <= 0.0 {
            return Err("invalid studio Y span".to_string());
        }
        Ok((code as f32 - y_floor) / y_span)
    } else if c_span <= 0.0 {
        Err("invalid studio chroma span".to_string())
    } else {
        Ok((code as f32 - c_mid) / c_span)
    }
}

#[cfg(feature = "heif-native")]
fn chroma_column_index(x: usize, chroma: libheif_sys::heif_chroma, chroma_plane_w: usize) -> usize {
    let subsamp_h = chroma != libheif_sys::heif_chroma_444;
    let ix = if subsamp_h {
        x / 2
    } else {
        x
    };
    ix.min(chroma_plane_w.saturating_sub(1))
}

#[cfg(feature = "heif-native")]
fn chroma_row_index(y_px: usize, chroma: libheif_sys::heif_chroma, chroma_plane_h: usize) -> usize {
    let subsamp_v = chroma == libheif_sys::heif_chroma_420;
    let iy = if subsamp_v {
        y_px / 2
    } else {
        y_px
    };
    iy.min(chroma_plane_h.saturating_sub(1))
}

#[cfg(feature = "heif-native")]
/// Planar YCbCr from libheif. NCLX `full_range: false` uses studio swing; full-pack path uses
/// `Cb/Cr` normalized to `[0, 1]` minus `0.5`. Matrix from CICP: 0 mono, 5/6 BT.601, 9/10 BT.2020 NCL,
/// else BT.709; ICC-only defaults to BT.709.
fn hdr_buffer_from_ycbcr(
    handle: *const libheif_sys::heif_image_handle,
    metadata: &HdrImageMetadata,
    image: *const libheif_sys::heif_image,
    chroma: libheif_sys::heif_chroma,
) -> Result<HdrImageBuffer, String> {
    use libheif_sys::{heif_channel_Alpha, heif_channel_Cb, heif_channel_Cr, heif_channel_Y};

    if unsafe { libheif_sys::heif_image_has_channel(image, heif_channel_Y) } == 0 {
        return Err("YCbCr decode: missing luma".to_string());
    }
    if unsafe { libheif_sys::heif_image_has_channel(image, heif_channel_Cb) } == 0
        || unsafe { libheif_sys::heif_image_has_channel(image, heif_channel_Cr) } == 0
    {
        return Err("YCbCr decode: missing chroma plane".to_string());
    }

    let y_w = unsafe { libheif_sys::heif_image_get_width(image, heif_channel_Y) } as usize;
    let y_h = unsafe { libheif_sys::heif_image_get_height(image, heif_channel_Y) } as usize;
    if y_w == 0 || y_h == 0 {
        return Err("YCbCr: zero-sized luma".to_string());
    }

    let cb_w = unsafe { libheif_sys::heif_image_get_width(image, heif_channel_Cb) } as usize;
    let cb_h = unsafe { libheif_sys::heif_image_get_height(image, heif_channel_Cb) } as usize;

    let mut stride_y = 0usize;
    let ptr_y =
        unsafe { libheif_sys::heif_image_get_plane_readonly2(image, heif_channel_Y, &mut stride_y) };
    let mut stride_cb = 0usize;
    let ptr_cb =
        unsafe { libheif_sys::heif_image_get_plane_readonly2(image, heif_channel_Cb, &mut stride_cb) };
    let mut stride_cr = 0usize;
    let ptr_cr =
        unsafe { libheif_sys::heif_image_get_plane_readonly2(image, heif_channel_Cr, &mut stride_cr) };

    let has_alpha_channel =
        unsafe { libheif_sys::heif_image_has_channel(image, heif_channel_Alpha) != 0 };
    let mut alpha_stride = 0usize;
    let alpha_ptr = if has_alpha_channel {
        unsafe { libheif_sys::heif_image_get_plane_readonly2(image, heif_channel_Alpha, &mut alpha_stride) }
    } else {
        std::ptr::null()
    };
    let alpha_valid = has_alpha_channel && !alpha_ptr.is_null() && alpha_stride > 0;

    if ptr_y.is_null() || ptr_cb.is_null() || ptr_cr.is_null() {
        return Err("YCbCr: null plane".to_string());
    }

    let span_y = planar_storage_span_bytes(image, heif_channel_Y);
    let span_cb = planar_storage_span_bytes(image, heif_channel_Cb);
    let span_cr = planar_storage_span_bytes(image, heif_channel_Cr);

    let scale_y = planar_scale_from_depth(planar_semantic_depth_bits(image, handle, heif_channel_Y)?);
    let scale_cb =
        planar_scale_from_depth(planar_semantic_depth_bits(image, handle, heif_channel_Cb)?);
    let scale_cr =
        planar_scale_from_depth(planar_semantic_depth_bits(image, handle, heif_channel_Cr)?);

    let sem_y = planar_semantic_depth_bits(image, handle, heif_channel_Y)?;
    let sem_cb = planar_semantic_depth_bits(image, handle, heif_channel_Cb)?;
    let sem_cr = planar_semantic_depth_bits(image, handle, heif_channel_Cr)?;
    let nclx_studio_swing = nclx_limited_range_from_metadata(metadata);

    let span_alpha = if alpha_valid {
        planar_storage_span_bytes(image, heif_channel_Alpha)
    } else {
        0
    };
    let scale_alpha = if alpha_valid {
        planar_scale_from_depth(planar_semantic_depth_bits(image, handle, heif_channel_Alpha)?)
    } else {
        1.0
    };

    let yuv_matrix = heif_ycbcr_matrix_from_nclx(metadata, y_w, y_h);

    let min_y_need = span_y * y_w.max(1);
    if stride_y < min_y_need {
        return Err("YCbCr: luma stride too small".to_string());
    }
    let min_cb_w = cb_w.max(1);
    if stride_cb < span_cb * min_cb_w || stride_cr < span_cr * min_cb_w {
        return Err("YCbCr: chroma stride too small".to_string());
    }
    if alpha_valid && alpha_stride < span_alpha * y_w.max(1) {
        return Err("YCbCr: alpha stride too small".to_string());
    }

    let mut rgba_f32 = Vec::with_capacity(y_w * y_h * 4);

    for y_px in 0..y_h {
        let row_y = unsafe { ptr_y.byte_add(y_px * stride_y) };

        let yc = chroma_row_index(y_px, chroma, cb_h);
        let row_cb = unsafe { ptr_cb.byte_add(yc * stride_cb) };
        let row_cr = unsafe { ptr_cr.byte_add(yc * stride_cr) };

        let row_alpha = alpha_valid.then(|| unsafe { alpha_ptr.byte_add(y_px * alpha_stride) });

        for x_px in 0..y_w {
            let y_raw = planar_read_sample(row_y, x_px, stride_y, span_y)?;
            let xc = chroma_column_index(x_px, chroma, cb_w);
            let cb_raw = planar_read_sample(row_cb, xc, stride_cb, span_cb)?;
            let cr_raw = planar_read_sample(row_cr, xc, stride_cr, span_cr)?;

            let [r_, g_, b_] = if nclx_studio_swing {
                let ey = studio_digital_sample_to_normalized(y_raw, sem_y, true)?;
                let ecb = studio_digital_sample_to_normalized(cb_raw, sem_cb, false)?;
                let ecr = studio_digital_sample_to_normalized(cr_raw, sem_cr, false)?;
                ycbcr_linear_to_rgb(ey, ecb, ecr, yuv_matrix)
            } else {
                let yv = y_raw as f32 / scale_y.max(1.0);
                let cbv = cb_raw as f32 / scale_cb.max(1.0);
                let crv = cr_raw as f32 / scale_cr.max(1.0);
                ycbcr_linear_to_rgb(yv, cbv - 0.5, crv - 0.5, yuv_matrix)
            };

            rgba_f32.push(r_.clamp(0.0, 1.0));
            rgba_f32.push(g_.clamp(0.0, 1.0));
            rgba_f32.push(b_.clamp(0.0, 1.0));

            if let Some(ar) = row_alpha {
                let av =
                    planar_read_sample(ar, x_px, alpha_stride, span_alpha)? as f32 / scale_alpha.max(1.0);
                rgba_f32.push(av.clamp(0.0, 1.0));
            } else {
                rgba_f32.push(1.0);
            }
        }
    }

    let color_space = metadata.color_space_hint();
    Ok(HdrImageBuffer {
        width: y_w as u32,
        height: y_h as u32,
        format: HdrPixelFormat::Rgba32Float,
        color_space,
        metadata: metadata.clone(),
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
            };
        }
    }

    HdrImageMetadata::default()
}

/// Apple-style **composite HDR HEIC**: NCLX may mark **PQ** while the **primary** decoded surface is
/// an **8-bit SDR** compatible base; decoding that through PQ in WGSL crushes luminance (HDR too dark).
/// **Unknown** skips `srgb_to_linear` and often reads as linear (SDR too bright). Heuristic: ≤8-bit
/// luma on the **handle** ⇒ treat transfer as sRGB-like for the GPU decode path.
#[cfg(feature = "heif-native")]
fn refine_heif_transfer_for_primary_bit_depth(
    handle: *const libheif_sys::heif_image_handle,
    metadata: &mut HdrImageMetadata,
) {
    let luma =
        unsafe { libheif_sys::heif_image_handle_get_luma_bits_per_pixel(handle) }.max(0);
    apply_heif_transfer_depth_heuristics(luma, metadata);
}

#[cfg(feature = "heif-native")]
fn apply_heif_transfer_depth_heuristics(luma_bits: i32, metadata: &mut HdrImageMetadata) {
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
        capped_display_referred: false,
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
    fn heif_transfer_depth_heuristic_pq_8bit_primary_to_srgb() {
        use super::apply_heif_transfer_depth_heuristics;

        let mut m = heif_nclx_to_metadata(9, 16, 9, false);
        assert_eq!(m.transfer_function, HdrTransferFunction::Pq);
        apply_heif_transfer_depth_heuristics(8, &mut m);
        assert_eq!(m.transfer_function, HdrTransferFunction::Srgb);
    }

    #[cfg(feature = "heif-native")]
    #[test]
    fn heif_transfer_depth_heuristic_pq_10bit_primary_unchanged() {
        use super::apply_heif_transfer_depth_heuristics;

        let mut m = heif_nclx_to_metadata(9, 16, 9, false);
        apply_heif_transfer_depth_heuristics(10, &mut m);
        assert_eq!(m.transfer_function, HdrTransferFunction::Pq);
    }

    #[cfg(feature = "heif-native")]
    #[test]
    fn heif_transfer_depth_heuristic_unknown_8bit_to_srgb() {
        use super::apply_heif_transfer_depth_heuristics;

        let mut m = heif_nclx_to_metadata(9, 99, 9, false);
        assert_eq!(m.transfer_function, HdrTransferFunction::Unknown);
        apply_heif_transfer_depth_heuristics(8, &mut m);
        assert_eq!(m.transfer_function, HdrTransferFunction::Srgb);
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

    #[cfg(feature = "heif-native")]
    #[test]
    fn heif_studio_swing_8bit_neutral_gray_bt709() {
        use super::{studio_digital_sample_to_normalized, HeifYcbcrMatrix, ycbcr_linear_to_rgb};

        let ey = studio_digital_sample_to_normalized(110, 8, true).unwrap();
        assert!((ey - 94.0 / 219.0).abs() < 1e-5);

        let ecb = studio_digital_sample_to_normalized(128, 8, false).unwrap();
        let ecr = studio_digital_sample_to_normalized(128, 8, false).unwrap();
        assert!(ecb.abs() < 1e-5 && ecr.abs() < 1e-5);

        let [r, g, b] = ycbcr_linear_to_rgb(ey, ecb, ecr, HeifYcbcrMatrix::Bt709);
        assert!(
            (r - g).abs() < 2e-4 && (g - b).abs() < 2e-4,
            "neutral chroma should yield R≈G≈B, got ({r},{g},{b})"
        );
    }

    #[cfg(feature = "heif-native")]
    #[test]
    fn heif_ycbcr_bt2020_neutral_chroma_gray_axis() {
        use super::{HeifYcbcrMatrix, ycbcr_linear_to_rgb};
        let ey = 0.4123_f32;
        let [r, g, b] = ycbcr_linear_to_rgb(ey, 0.0, 0.0, HeifYcbcrMatrix::Bt2020Ncl);
        assert!((r - ey).abs() < 1e-5);
        assert!((g - ey).abs() < 1e-5);
        assert!((b - ey).abs() < 1e-5);
    }

    #[cfg(feature = "heif-native")]
    #[test]
    fn heif_ycbcr_monochrome_replicates_y() {
        use super::{HeifYcbcrMatrix, ycbcr_linear_to_rgb};
        let [r, g, b] =
            ycbcr_linear_to_rgb(0.42, 0.9, -0.3, HeifYcbcrMatrix::Monochrome);
        assert!((r - 0.42).abs() < 1e-6 && r == g && g == b);
    }

    #[cfg(feature = "heif-native")]
    #[test]
    fn heif_nclx_maps_matrix_coefficients_to_ycbcr_matrix() {
        use super::{heif_ycbcr_matrix_from_nclx, HeifYcbcrMatrix};
        use crate::hdr::types::{HdrColorProfile, HdrImageMetadata};

        fn meta(mc: u16) -> HdrImageMetadata {
            HdrImageMetadata {
                color_profile: HdrColorProfile::Cicp {
                    color_primaries: 1,
                    transfer_characteristics: 1,
                    matrix_coefficients: mc,
                    full_range: true,
                },
                ..Default::default()
            }
        }

        assert_eq!(
            heif_ycbcr_matrix_from_nclx(&meta(0), 640, 480),
            HeifYcbcrMatrix::Bt601
        );
        assert_eq!(
            heif_ycbcr_matrix_from_nclx(&meta(0), 1920, 1080),
            HeifYcbcrMatrix::Bt709
        );
        assert_eq!(
            heif_ycbcr_matrix_from_nclx(&meta(5), 100, 100),
            HeifYcbcrMatrix::Bt601
        );
        assert_eq!(
            heif_ycbcr_matrix_from_nclx(&meta(6), 100, 100),
            HeifYcbcrMatrix::Bt601
        );
        assert_eq!(
            heif_ycbcr_matrix_from_nclx(&meta(9), 100, 100),
            HeifYcbcrMatrix::Bt2020Ncl
        );
        assert_eq!(
            heif_ycbcr_matrix_from_nclx(&meta(10), 100, 100),
            HeifYcbcrMatrix::Bt2020Ncl
        );
        assert_eq!(
            heif_ycbcr_matrix_from_nclx(&meta(12), 100, 100),
            HeifYcbcrMatrix::Bt2020Ncl
        );
        assert_eq!(
            heif_ycbcr_matrix_from_nclx(&meta(1), 100, 100),
            HeifYcbcrMatrix::Bt709
        );
        assert_eq!(
            heif_ycbcr_matrix_from_nclx(&meta(255), 100, 100),
            HeifYcbcrMatrix::Bt709
        );
        assert_eq!(
            heif_ycbcr_matrix_from_nclx(&HdrImageMetadata::default(), 1, 1),
            HeifYcbcrMatrix::Bt709
        );
    }
}
