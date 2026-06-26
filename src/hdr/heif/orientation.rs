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
use super::session::{
    HeifPrimaryGuard, open_heif_primary_from_bytes, orientation_from_heif_exif_item_blob,
};

#[cfg(feature = "heif-native")]
use std::ffi::CStr;
#[cfg(feature = "heif-native")]
use std::path::Path;
#[cfg(feature = "heif-native")]
use std::sync::OnceLock;

#[cfg(feature = "heif-native")]
pub(crate) fn heif_exif_orientation_from_raw_handle(
    handle: *const libheif_sys::heif_image_handle,
) -> Option<u16> {
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

#[cfg(feature = "heif-native")]
pub(crate) fn heif_exif_orientation_from_handle(primary: &HeifPrimaryGuard) -> Option<u16> {
    heif_exif_orientation_from_raw_handle(primary.as_ptr())
}

/// Read [`exif::Tag::Orientation`] from libheif-attached `Exif` metadata items (works when pure ISOBMFF
/// scanning in [`crate::metadata_utils::get_exif_orientation`] misses the `Exif` item).
#[cfg(feature = "heif-native")]
pub(crate) fn libheif_exif_orientation_tag(path: &Path) -> Option<u16> {
    let mmap = crate::mmap_util::map_file(path).ok()?;
    let (_ctx, primary) = open_heif_primary_from_bytes(&mmap[..]).ok()?;
    heif_exif_orientation_from_handle(&primary)
}

/// JEITA Orientation chain helper: **`T(out) ≅ T(acc) ◦ T(next)`** (apply [`next`] to pixels after [`acc`]).
#[cfg(feature = "heif-native")]
static COMPOSE_ORIENTATION_CHAIN: OnceLock<[[u8; 9]; 9]> = OnceLock::new();

#[cfg(feature = "heif-native")]
pub(crate) fn compose_orientation_chain(acc: u16, primitive_next: u16) -> u16 {
    let table = COMPOSE_ORIENTATION_CHAIN.get_or_init(build_compose_orientation_chain_table);
    table[acc as usize][primitive_next as usize] as u16
}

#[cfg(feature = "heif-native")]
pub(crate) fn build_compose_orientation_chain_table() -> [[u8; 9]; 9] {
    let mut out = [[0u8; 9]; 9];
    for a in 1..=8u16 {
        for n in 1..=8u16 {
            out[a as usize][n as usize] = brute_compose_orientation_row_col(a, n) as u8;
        }
    }
    out
}

#[cfg(feature = "heif-native")]
pub(crate) fn brute_compose_orientation_row_col(acc: u16, primitive_next: u16) -> u16 {
    const W: u32 = 5;
    const H: u32 = 4;
    // Tiny synthetic buffer (5×4): regenerate per candidate instead of cloning the full Vec for
    // each orientation probe (build table once at process start; hot path is decode, not here).
    let base = synth_gradient_rgba8(W, H);
    let (w1, h1, mid) = crate::libtiff_loader::apply_orientation_buffer(base, W, H, acc);
    let (wf, hf, composed) =
        crate::libtiff_loader::apply_orientation_buffer(mid, w1, h1, primitive_next);
    for cand in 1..=8u16 {
        let (wc, hc, pc) =
            crate::libtiff_loader::apply_orientation_buffer(synth_gradient_rgba8(W, H), W, H, cand);
        if wc == wf && hc == hf && pc == composed {
            return cand;
        }
    }
    1
}

#[cfg(feature = "heif-native")]
pub(crate) fn synth_gradient_rgba8(w: u32, h: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let p = (((y * w + x) * 17 + ((x ^ y) * 3)) & 255) as u8;
            v.extend_from_slice(&[p, p ^ 0xAA, p.rotate_left(5), 0xFF]);
        }
    }
    v
}

/// Primary item exposes only **`irot` / `imir`** transformation properties (or none). **`clap`** and other
/// geometry stay on the decoder path with default options.
#[cfg(feature = "heif-native")]
pub(crate) fn libheif_primary_geometric_mirror_rotation_only(
    context: *const libheif_sys::heif_context,
    handle: *const libheif_sys::heif_image_handle,
) -> bool {
    unsafe {
        let item_id = libheif_sys::heif_image_handle_get_item_id(handle);
        let n = libheif_sys::heif_item_get_transformation_properties(
            context,
            item_id,
            std::ptr::null_mut(),
            0,
        );
        if n <= 0 {
            return true;
        }
        let mut props = vec![0u32; n as usize];
        let wrote = libheif_sys::heif_item_get_transformation_properties(
            context,
            item_id,
            props.as_mut_ptr(),
            n,
        );
        if wrote < 0 {
            return false;
        }
        for &pid in props.iter().take(wrote as usize) {
            let ty = libheif_sys::heif_item_get_property_type(context, item_id, pid);
            let ok = ty == libheif_sys::heif_item_property_type_transform_rotation
                || ty == libheif_sys::heif_item_property_type_transform_mirror;
            if !ok {
                return false;
            }
        }
        true
    }
}

#[cfg(feature = "heif-native")]
pub(crate) fn libheif_primary_decode_should_ignore_embedded_geometry(bytes: &[u8]) -> bool {
    let Ok((ctx, primary)) = open_heif_primary_from_bytes(bytes) else {
        return false;
    };
    libheif_primary_geometric_mirror_rotation_only(ctx.as_ptr(), primary.as_ptr())
}

#[cfg(feature = "heif-native")]
pub(crate) fn libheif_transformation_props_to_manual_exif(
    context: *const libheif_sys::heif_context,
    handle: *const libheif_sys::heif_image_handle,
) -> Option<u16> {
    unsafe {
        let item_id = libheif_sys::heif_image_handle_get_item_id(handle);
        let n = libheif_sys::heif_item_get_transformation_properties(
            context,
            item_id,
            std::ptr::null_mut(),
            0,
        );
        if n <= 0 {
            return Some(1);
        }
        let mut props = vec![0u32; n as usize];
        let wrote = libheif_sys::heif_item_get_transformation_properties(
            context,
            item_id,
            props.as_mut_ptr(),
            n,
        );
        if wrote < 0 {
            return None;
        }
        let mut acc = 1u16;
        for &pid in props.iter().take(wrote as usize) {
            let ty = libheif_sys::heif_item_get_property_type(context, item_id, pid);
            match ty {
                t if t == libheif_sys::heif_item_property_type_transform_rotation => {
                    let ccw = libheif_sys::heif_item_get_property_transform_rotation_ccw(
                        context, item_id, pid,
                    );
                    let primitive = match ccw {
                        0 => 1u16,
                        90 => 8,
                        180 => 3,
                        270 => 6,
                        _ => return None,
                    };
                    acc = compose_orientation_chain(acc, primitive);
                }
                t if t == libheif_sys::heif_item_property_type_transform_mirror => {
                    let mdir =
                        libheif_sys::heif_item_get_property_transform_mirror(context, item_id, pid);
                    let primitive = if mdir == libheif_sys::heif_transform_mirror_direction_vertical
                    {
                        4u16
                    } else if mdir == libheif_sys::heif_transform_mirror_direction_horizontal {
                        2
                    } else {
                        return None;
                    };
                    acc = compose_orientation_chain(acc, primitive);
                }
                _ => return None,
            }
        }
        ((1..=8).contains(&acc)).then_some(acc)
    }
}

/// EXIF Orientation (1–8) reconstructed from **`irot`/`imir`** when the decoder is instructed to skip
/// embedded geometry (**[`HeifDecodeOptionsIgnoredGeometryOwned`]**) so pixels match AVIF-style manual rotation.
#[cfg(feature = "heif-native")]
pub(crate) fn libheif_manual_geometry_exif_orientation_from_bytes(bytes: &[u8]) -> Option<u16> {
    let (ctx, primary) = open_heif_primary_from_bytes(bytes).ok()?;
    if !libheif_primary_geometric_mirror_rotation_only(ctx.as_ptr(), primary.as_ptr()) {
        return None;
    }
    libheif_transformation_props_to_manual_exif(ctx.as_ptr(), primary.as_ptr())
}

#[cfg(feature = "heif-native")]
pub(crate) fn libheif_manual_geometry_exif_orientation_from_path(path: &Path) -> Option<u16> {
    let mmap = crate::mmap_util::map_file(path).ok()?;
    libheif_manual_geometry_exif_orientation_from_bytes(&mmap[..])
}

/// Decoding options: **`ignore_transformations = true`**. Matches `struct heif_decoding_options`: `ignore_transformations`
/// is immediately after **`version`** (confirmed for libheif ≥ 1.x).
#[cfg(feature = "heif-native")]
pub(crate) struct HeifDecodeOptionsIgnoredGeometryOwned {
    guard: Option<libheif_sys::HeifDecodingOptionsGuard>,
}

#[cfg(feature = "heif-native")]
impl HeifDecodeOptionsIgnoredGeometryOwned {
    pub(crate) fn new_ignore_transformations() -> Option<Self> {
        let guard = libheif_sys::HeifDecodingOptionsGuard::new()?;
        // Set byte at offset 1 → `ignore_transformations` in libheif's C struct.
        unsafe {
            *guard.as_mut_ptr().cast::<u8>().add(1) = 1;
        }
        Some(Self { guard: Some(guard) })
    }

    pub(crate) fn as_ptr(&self) -> *const libheif_sys::heif_decoding_options {
        self.guard
            .as_ref()
            .map(|g| g.as_ptr())
            .unwrap_or(std::ptr::null())
    }
}

#[cfg(feature = "heif-native")]
impl Drop for HeifDecodeOptionsIgnoredGeometryOwned {
    fn drop(&mut self) {
        // HeifDecodingOptionsGuard handles the free; just let the Option drop.
        self.guard.take();
    }
}

#[cfg(feature = "heif-native")]
pub(crate) fn allocate_decode_options_for_heif_manual_geometry_fixup(
    bytes: &[u8],
) -> Option<HeifDecodeOptionsIgnoredGeometryOwned> {
    if libheif_primary_decode_should_ignore_embedded_geometry(bytes) {
        HeifDecodeOptionsIgnoredGeometryOwned::new_ignore_transformations()
    } else {
        None
    }
}

/// When the decoded raster's width/height are the **swap** of libheif’s `ispe` width/height (non-square),
/// decoder has already applied a 90°/270° HEIF transform on the pixel grid — suppress applying EXIF
/// Orientation again to avoid double rotation.
#[cfg(feature = "heif-native")]
pub(crate) fn decoded_pixels_match_swapped_ispe(
    path: &Path,
    decoded_w: u32,
    decoded_h: u32,
) -> bool {
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
        let iw = libheif_sys::heif_image_handle_get_ispe_width(primary.as_ptr());
        let ih = libheif_sys::heif_image_handle_get_ispe_height(primary.as_ptr());
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
