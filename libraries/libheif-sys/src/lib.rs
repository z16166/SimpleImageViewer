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

#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

pub type heif_error_code = libc::c_int;
pub type heif_suberror_code = libc::c_int;
pub type heif_colorspace = libc::c_int;
pub type heif_chroma = libc::c_int;
pub type heif_channel = libc::c_int;
pub type heif_color_primaries = libc::c_int;
pub type heif_transfer_characteristics = libc::c_int;
pub type heif_matrix_coefficients = libc::c_int;
pub type heif_item_id = u32;
pub type heif_property_id = u32;
pub type heif_item_property_type = libc::c_int;

pub const heif_error_Ok: heif_error_code = 0;

/// `heif_item_property_type` values use big-endian FourCC (ISO BMFF short box types).
pub const heif_item_property_type_invalid: heif_item_property_type = 0;
pub const heif_item_property_type_transform_mirror: heif_item_property_type =
    i32::from_be_bytes(*b"imir");
pub const heif_item_property_type_transform_rotation: heif_item_property_type =
    i32::from_be_bytes(*b"irot");
pub const heif_item_property_type_transform_crop: heif_item_property_type =
    i32::from_be_bytes(*b"clap");

pub const heif_transform_mirror_direction_invalid: libc::c_int = -1;
pub const heif_transform_mirror_direction_vertical: libc::c_int = 0;
pub const heif_transform_mirror_direction_horizontal: libc::c_int = 1;
/// Matches `enum heif_colorspace` / `enum heif_chroma` / `enum heif_channel` in upstream `heif_image.h`.
pub const heif_colorspace_YCbCr: heif_colorspace = 0;
pub const heif_colorspace_RGB: heif_colorspace = 1;
pub const heif_chroma_420: heif_chroma = 1;
pub const heif_chroma_422: heif_chroma = 2;
pub const heif_chroma_444: heif_chroma = 3;
pub const heif_chroma_interleaved_RGB: heif_chroma = 10;
pub const heif_chroma_interleaved_RGBA: heif_chroma = 11;
pub const heif_chroma_interleaved_RRGGBB_BE: heif_chroma = 12;
pub const heif_chroma_interleaved_RRGGBBAA_BE: heif_chroma = 13;
pub const heif_chroma_interleaved_RRGGBB_LE: heif_chroma = 14;
pub const heif_chroma_interleaved_RRGGBBAA_LE: heif_chroma = 15;
pub const heif_channel_Y: heif_channel = 0;
pub const heif_channel_Cb: heif_channel = 1;
pub const heif_channel_Cr: heif_channel = 2;
pub const heif_channel_R: heif_channel = 3;
pub const heif_channel_G: heif_channel = 4;
pub const heif_channel_B: heif_channel = 5;
pub const heif_channel_Alpha: heif_channel = 6;
pub const heif_channel_interleaved: heif_channel = 10;

#[repr(C)]
pub struct heif_context {
    _private: [u8; 0],
}

#[repr(C)]
pub struct heif_image_handle {
    _private: [u8; 0],
}

#[repr(C)]
pub struct heif_image {
    _private: [u8; 0],
}

/// Allocate with [`heif_decoding_options_alloc`] — the struct may grow in future
/// libheif versions; only access fields through the guard's safe setters/getters.
///
/// Field layout matches `struct heif_decoding_options` in `<libheif/heif_decoding.h>`.
/// We only declare the version-1 fields we actually use; `heif_decoding_options_alloc`
/// zero-initializes the full allocation including any trailing fields added by newer
/// libheif versions, so accessing only the declared prefix is sound.
#[repr(C)]
pub struct heif_decoding_options {
    pub version: u8,
    pub ignore_transformations: u8,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct heif_error {
    pub code: heif_error_code,
    pub subcode: heif_suberror_code,
    pub message: *const libc::c_char,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct heif_color_profile_nclx {
    pub version: u8,
    pub color_primaries: heif_color_primaries,
    pub transfer_characteristics: heif_transfer_characteristics,
    pub matrix_coefficients: heif_matrix_coefficients,
    pub full_range_flag: u8,
    pub color_primary_red_x: f32,
    pub color_primary_red_y: f32,
    pub color_primary_green_x: f32,
    pub color_primary_green_y: f32,
    pub color_primary_blue_x: f32,
    pub color_primary_blue_y: f32,
    pub color_primary_white_x: f32,
    pub color_primary_white_y: f32,
}

unsafe extern "C" {
    pub fn heif_get_version() -> *const libc::c_char;
    pub fn heif_context_alloc() -> *mut heif_context;
    pub fn heif_context_free(context: *mut heif_context);
    pub fn heif_context_read_from_memory_without_copy(
        context: *mut heif_context,
        mem: *const libc::c_void,
        size: libc::size_t,
        options: *const libc::c_void,
    ) -> heif_error;
    pub fn heif_context_get_primary_image_handle(
        context: *mut heif_context,
        handle: *mut *mut heif_image_handle,
    ) -> heif_error;
    pub fn heif_image_handle_release(handle: *const heif_image_handle);
    pub fn heif_image_handle_get_luma_bits_per_pixel(
        handle: *const heif_image_handle,
    ) -> libc::c_int;
    pub fn heif_image_handle_get_chroma_bits_per_pixel(
        handle: *const heif_image_handle,
    ) -> libc::c_int;
    pub fn heif_image_handle_get_raw_color_profile_size(
        handle: *const heif_image_handle,
    ) -> libc::size_t;
    pub fn heif_image_handle_get_raw_color_profile(
        handle: *const heif_image_handle,
        out_data: *mut libc::c_void,
    ) -> heif_error;
    pub fn heif_image_handle_get_nclx_color_profile(
        handle: *const heif_image_handle,
        out_data: *mut *mut heif_color_profile_nclx,
    ) -> heif_error;
    pub fn heif_nclx_color_profile_free(nclx_profile: *mut heif_color_profile_nclx);
    pub fn heif_image_handle_get_number_of_auxiliary_images(
        handle: *const heif_image_handle,
        aux_filter: libc::c_int,
    ) -> libc::c_int;
    pub fn heif_image_handle_get_list_of_auxiliary_image_IDs(
        handle: *const heif_image_handle,
        aux_filter: libc::c_int,
        ids: *mut heif_item_id,
        count: libc::c_int,
    ) -> libc::c_int;
    pub fn heif_image_handle_get_auxiliary_type(
        handle: *const heif_image_handle,
        out_type: *mut *const libc::c_char,
    ) -> heif_error;
    pub fn heif_image_handle_release_auxiliary_type(
        handle: *const heif_image_handle,
        out_type: *mut *const libc::c_char,
    );
    pub fn heif_image_handle_get_auxiliary_image_handle(
        main_image_handle: *const heif_image_handle,
        auxiliary_id: heif_item_id,
        out_auxiliary_handle: *mut *mut heif_image_handle,
    ) -> heif_error;
    pub fn heif_decoding_options_alloc() -> *mut heif_decoding_options;
    pub fn heif_decoding_options_free(options: *mut heif_decoding_options);

    pub fn heif_decode_image(
        handle: *const heif_image_handle,
        out_img: *mut *mut heif_image,
        colorspace: heif_colorspace,
        chroma: heif_chroma,
        options: *const heif_decoding_options,
    ) -> heif_error;
    pub fn heif_image_release(image: *const heif_image);
    pub fn heif_image_get_primary_width(image: *const heif_image) -> libc::c_int;
    pub fn heif_image_get_primary_height(image: *const heif_image) -> libc::c_int;
    pub fn heif_image_get_bits_per_pixel_range(
        image: *const heif_image,
        channel: heif_channel,
    ) -> libc::c_int;
    pub fn heif_image_has_channel(image: *const heif_image, channel: heif_channel) -> libc::c_int;
    pub fn heif_image_get_width(image: *const heif_image, channel: heif_channel) -> libc::c_int;
    pub fn heif_image_get_height(image: *const heif_image, channel: heif_channel) -> libc::c_int;
    pub fn heif_image_get_bits_per_pixel(
        image: *const heif_image,
        channel: heif_channel,
    ) -> libc::c_int;
    pub fn heif_image_get_plane_readonly2(
        image: *const heif_image,
        channel: heif_channel,
        out_stride: *mut libc::size_t,
    ) -> *const u8;

    pub fn heif_image_handle_get_width(handle: *const heif_image_handle) -> libc::c_int;
    pub fn heif_image_handle_get_height(handle: *const heif_image_handle) -> libc::c_int;
    pub fn heif_image_handle_get_ispe_width(handle: *const heif_image_handle) -> libc::c_int;
    pub fn heif_image_handle_get_ispe_height(handle: *const heif_image_handle) -> libc::c_int;

    pub fn heif_image_handle_get_item_id(handle: *const heif_image_handle) -> heif_item_id;

    pub fn heif_item_get_transformation_properties(
        context: *const heif_context,
        item_id: heif_item_id,
        out_list: *mut heif_property_id,
        count: libc::c_int,
    ) -> libc::c_int;

    pub fn heif_item_get_property_type(
        context: *const heif_context,
        item_id: heif_item_id,
        property_id: heif_property_id,
    ) -> heif_item_property_type;

    pub fn heif_item_get_property_transform_mirror(
        context: *const heif_context,
        item_id: heif_item_id,
        property_id: heif_property_id,
    ) -> libc::c_int;

    pub fn heif_item_get_property_transform_rotation_ccw(
        context: *const heif_context,
        item_id: heif_item_id,
        property_id: heif_property_id,
    ) -> libc::c_int;

    pub fn heif_image_handle_get_number_of_metadata_blocks(
        handle: *const heif_image_handle,
        type_filter: *const libc::c_char,
    ) -> libc::c_int;

    pub fn heif_image_handle_get_list_of_metadata_block_IDs(
        handle: *const heif_image_handle,
        type_filter: *const libc::c_char,
        ids: *mut heif_item_id,
        count: libc::c_int,
    ) -> libc::c_int;

    pub fn heif_image_handle_get_metadata_type(
        handle: *const heif_image_handle,
        metadata_id: heif_item_id,
    ) -> *const libc::c_char;

    pub fn heif_image_handle_get_metadata_size(
        handle: *const heif_image_handle,
        metadata_id: heif_item_id,
    ) -> libc::size_t;

    pub fn heif_image_handle_get_metadata(
        handle: *const heif_image_handle,
        metadata_id: heif_item_id,
        out_data: *mut libc::c_void,
    ) -> heif_error;
}

// ── RAII guards ────────────────────────────────────────────────────────
//
// Drop order convention for libheif: image → handle → context (bottom-up).
// Rust struct field drop order is declaration order, so declare guards in
// reverse of the C release order when storing them in the same struct.

use std::fmt;
use std::ptr::NonNull;

// ── heif_context ──────────────────────────────────────────────────────

/// Owns a [`heif_context`] and calls [`heif_context_free`] on drop.
#[must_use = "HeifContextGuard will free the context on drop"]
pub struct HeifContextGuard {
    ptr: NonNull<heif_context>,
}

impl HeifContextGuard {
    /// Allocate a new context. Returns `None` on allocation failure.
    pub fn new() -> Option<Self> {
        let ptr = NonNull::new(unsafe { heif_context_alloc() })?;
        Some(Self { ptr })
    }

    #[inline]
    pub fn as_ptr(&self) -> *mut heif_context {
        self.ptr.as_ptr()
    }
}

impl Drop for HeifContextGuard {
    fn drop(&mut self) {
        unsafe { heif_context_free(self.ptr.as_ptr()) };
    }
}

impl fmt::Debug for HeifContextGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HeifContextGuard")
            .field("ptr", &self.ptr)
            .finish()
    }
}

// ── heif_image_handle ─────────────────────────────────────────────────

/// Owns a [`heif_image_handle`] and calls [`heif_image_handle_release`] on drop.
#[must_use = "HeifImageHandleGuard will release the handle on drop"]
pub struct HeifImageHandleGuard {
    ptr: NonNull<heif_image_handle>,
}

impl HeifImageHandleGuard {
    /// Wrap a raw handle obtained from e.g. [`heif_context_get_primary_image_handle`].
    ///
    /// # Safety
    ///
    /// `ptr` must be a valid, non-null handle that has not been passed to another guard.
    #[inline]
    pub unsafe fn from_ptr(ptr: *mut heif_image_handle) -> Self {
        Self {
            ptr: NonNull::new(ptr).expect("HeifImageHandleGuard constructed with null handle"),
        }
    }

    #[inline]
    pub fn as_ptr(&self) -> *const heif_image_handle {
        self.ptr.as_ptr().cast_const()
    }

    #[inline]
    pub fn as_mut_ptr(&self) -> *mut heif_image_handle {
        self.ptr.as_ptr()
    }
}

impl Drop for HeifImageHandleGuard {
    fn drop(&mut self) {
        unsafe { heif_image_handle_release(self.ptr.as_ptr().cast_const()) };
    }
}

impl fmt::Debug for HeifImageHandleGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HeifImageHandleGuard")
            .field("ptr", &self.ptr)
            .finish()
    }
}

// ── heif_image ────────────────────────────────────────────────────────

/// Owns a decoded [`heif_image`] and calls [`heif_image_release`] on drop.
#[must_use = "HeifImageGuard will release the image on drop"]
pub struct HeifImageGuard {
    ptr: NonNull<heif_image>,
}

impl HeifImageGuard {
    /// Wrap a raw image obtained from [`heif_decode_image`].
    ///
    /// # Safety
    ///
    /// `ptr` must be a valid, non-null image that has not been passed to another guard.
    #[inline]
    pub unsafe fn from_ptr(ptr: *mut heif_image) -> Self {
        Self {
            ptr: NonNull::new(ptr).expect("HeifImageGuard constructed with null image"),
        }
    }

    #[inline]
    pub fn as_ptr(&self) -> *const heif_image {
        self.ptr.as_ptr().cast_const()
    }
}

impl Drop for HeifImageGuard {
    fn drop(&mut self) {
        unsafe { heif_image_release(self.ptr.as_ptr().cast_const()) };
    }
}

impl fmt::Debug for HeifImageGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HeifImageGuard")
            .field("ptr", &self.ptr)
            .finish()
    }
}

// ── heif_decoding_options ─────────────────────────────────────────────

/// Owns a [`heif_decoding_options`] and calls [`heif_decoding_options_free`] on drop.
#[must_use = "HeifDecodingOptionsGuard will free the options on drop"]
pub struct HeifDecodingOptionsGuard {
    ptr: NonNull<heif_decoding_options>,
}

impl HeifDecodingOptionsGuard {
    /// Allocate new decoding options. Returns `None` on allocation failure.
    pub fn new() -> Option<Self> {
        let ptr = NonNull::new(unsafe { heif_decoding_options_alloc() })?;
        Some(Self { ptr })
    }

    #[inline]
    pub fn as_ptr(&self) -> *const heif_decoding_options {
        self.ptr.as_ptr().cast_const()
    }

    /// Set the version-1 `ignore_transformations` flag. When `true`, the decoder
    /// skips embedded crop/rotation/mirror geometry and returns raw raster pixels.
    #[inline]
    pub fn set_ignore_transformations(&mut self, ignore: bool) {
        // SAFETY: self.ptr is a valid NonNull<heif_decoding_options> from
        // heif_decoding_options_alloc, and &mut self guarantees exclusive access.
        let p = unsafe { self.ptr.as_mut() };
        p.ignore_transformations = ignore as u8;
    }
}

impl Drop for HeifDecodingOptionsGuard {
    fn drop(&mut self) {
        unsafe { heif_decoding_options_free(self.ptr.as_ptr()) };
    }
}

impl fmt::Debug for HeifDecodingOptionsGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HeifDecodingOptionsGuard")
            .field("ptr", &self.ptr)
            .finish()
    }
}

// ── heif_color_profile_nclx ───────────────────────────────────────────

/// Owns a [`heif_color_profile_nclx`] and calls [`heif_nclx_color_profile_free`] on drop.
#[must_use = "HeifNclxProfileGuard will free the NCLX profile on drop"]
pub struct HeifNclxProfileGuard {
    ptr: NonNull<heif_color_profile_nclx>,
}

impl HeifNclxProfileGuard {
    /// Wrap a raw NCLX profile pointer obtained from [`heif_image_handle_get_nclx_color_profile`].
    ///
    /// # Safety
    ///
    /// `ptr` must be a valid, non-null profile that has not been passed to another guard.
    #[inline]
    pub unsafe fn from_ptr(ptr: *mut heif_color_profile_nclx) -> Self {
        Self {
            ptr: NonNull::new(ptr)
                .expect("HeifNclxProfileGuard constructed with null profile"),
        }
    }

    /// Access the NCLX profile data.
    #[inline]
    pub fn as_ref(&self) -> &heif_color_profile_nclx {
        unsafe { self.ptr.as_ref() }
    }
}

impl Drop for HeifNclxProfileGuard {
    fn drop(&mut self) {
        unsafe { heif_nclx_color_profile_free(self.ptr.as_ptr()) };
    }
}

impl fmt::Debug for HeifNclxProfileGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HeifNclxProfileGuard")
            .field("ptr", &self.ptr)
            .finish()
    }
}
