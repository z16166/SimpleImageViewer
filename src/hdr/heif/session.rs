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

use crate::hdr::cicp::{self, H273_TRANSFER_ITU_BT709, H273_TRANSFER_SMPTE170M};
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
#[cfg(feature = "heif-native")]
use std::sync::OnceLock;

#[cfg(feature = "heif-native")]
pub(crate) fn append_heif_unci_build_hint(msg: String) -> String {
    let lower = msg.to_lowercase();
    let unci_related = lower.contains("unci")
        || lower.contains("23001-17")
        || lower.contains("uncompressed image type");
    let brotli_unc =
        lower.contains("brotli") && (unci_related || lower.contains("generic compression"));

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
pub(crate) fn append_mini_format_read_hint(action: &str, msg: String) -> String {
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
pub(crate) struct HeifCtxGuard(pub *mut libheif_sys::heif_context);

#[cfg(feature = "heif-native")]
impl Drop for HeifCtxGuard {
    fn drop(&mut self) {
        unsafe {
            libheif_sys::heif_context_free(self.0);
        }
    }
}

#[cfg(feature = "heif-native")]
pub(crate) struct HeifPrimaryGuard(pub *mut libheif_sys::heif_image_handle);

#[cfg(feature = "heif-native")]
impl Drop for HeifPrimaryGuard {
    fn drop(&mut self) {
        unsafe {
            libheif_sys::heif_image_handle_release(self.0);
        }
    }
}

#[cfg(feature = "heif-native")]
pub(crate) fn heif_error_to_string_lib(err: libheif_sys::heif_error) -> String {
    if err.message.is_null() {
        return format!("libheif error code {} subcode {}", err.code, err.subcode);
    }
    unsafe { CStr::from_ptr(err.message) }
        .to_string_lossy()
        .into_owned()
}

#[cfg(feature = "heif-native")]
pub(crate) fn ensure_heif_ok_lib(err: libheif_sys::heif_error, action: &str) -> Result<(), String> {
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
pub(crate) fn open_heif_primary_from_bytes(bytes: &[u8]) -> Result<(HeifCtxGuard, HeifPrimaryGuard), String> {
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
pub(crate) fn orientation_from_heif_exif_item_blob(buf: &[u8]) -> Option<u16> {
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
