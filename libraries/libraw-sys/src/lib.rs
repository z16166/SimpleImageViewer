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

use libc::{c_char, c_double, c_float, c_int, c_uchar, c_uint, c_ushort};

/// Matches LibRaw's `enum LibRaw_image_formats` from `libraw_const.h`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(non_camel_case_types)]
pub enum LibRaw_image_formats {
    LIBRAW_IMAGE_JPEG = 1,
    LIBRAW_IMAGE_BITMAP = 2,
}

#[repr(C)]
pub struct libraw_processed_image_t {
    pub image_type: c_uint,
    pub height: c_ushort,
    pub width: c_ushort,
    pub colors: c_ushort,
    pub bits: c_ushort,
    pub data_size: c_uint,
    pub data: [c_uchar; 1],
}

#[repr(C)]
#[allow(dead_code)]
pub struct libraw_image_sizes_t {
    pub width: c_ushort,
    pub height: c_ushort,
    pub raw_width: c_ushort,
    pub raw_height: c_ushort,
    pub left_margin: c_ushort,
    pub top_margin: c_ushort,
    pub iwidth: c_ushort,
    pub iheight: c_ushort,
    pub raw_pitch: c_uint,
    pub pixel_aspect: f64,
    pub flip: c_int,
}

// libraw_data_t is treated as an opaque structure to ensure binary stability
// across different versions of LibRaw. All data access should be performed
// using the public C API functions (getters).
#[repr(C)]
pub struct libraw_data_t {
    _unused: [u8; 0],
}

unsafe extern "C" {
    pub fn libraw_version() -> *const c_char;
    pub fn libraw_init(flags: c_int) -> *mut libraw_data_t;
    pub fn libraw_close(data: *mut libraw_data_t);
    pub fn libraw_open_file(data: *mut libraw_data_t, file: *const c_char) -> c_int;
    #[cfg(target_os = "windows")]
    pub fn libraw_open_wfile(data: *mut libraw_data_t, file: *const u16) -> c_int;
    pub fn libraw_open_buffer(
        data: *mut libraw_data_t,
        buffer: *const libc::c_void,
        size: libc::size_t,
    ) -> c_int;
    pub fn libraw_unpack(data: *mut libraw_data_t) -> c_int;
    pub fn libraw_unpack_thumb(data: *mut libraw_data_t) -> c_int;
    pub fn libraw_dcraw_process(data: *mut libraw_data_t) -> c_int;
    pub fn libraw_dcraw_make_mem_image(
        data: *mut libraw_data_t,
        errc: *mut c_int,
    ) -> *mut libraw_processed_image_t;
    pub fn libraw_dcraw_make_mem_thumb(
        data: *mut libraw_data_t,
        errc: *mut c_int,
    ) -> *mut libraw_processed_image_t;
    pub fn libraw_dcraw_clear_mem(image: *mut libraw_processed_image_t);
    pub fn libraw_set_output_bps(data: *mut libraw_data_t, value: c_int);
    pub fn libraw_set_no_auto_bright(data: *mut libraw_data_t, value: c_int);

    // Custom shims (implemented in libraw_shims.cpp)
    pub fn siv_libraw_set_use_camera_wb(data: *mut libraw_data_t, value: c_int);
    pub fn siv_libraw_get_process_warnings(data: *mut libraw_data_t) -> c_uint;
    pub fn siv_libraw_get_flip(data: *mut libraw_data_t) -> c_int;
    pub fn siv_libraw_set_user_flip(data: *mut libraw_data_t, flip: c_int);
    pub fn siv_libraw_set_use_camera_matrix(data: *mut libraw_data_t, value: c_int);
    pub fn siv_libraw_set_auto_bright_thr(data: *mut libraw_data_t, value: c_float);
    pub fn siv_libraw_set_output_color(data: *mut libraw_data_t, value: c_int);
    pub fn siv_libraw_set_gamma(data: *mut libraw_data_t, power: f64, slope: f64);
    pub fn siv_libraw_set_user_qual(data: *mut libraw_data_t, qual: c_int);
    pub fn siv_libraw_set_highlight(data: *mut libraw_data_t, value: c_int);
    pub fn siv_libraw_set_half_size(data: *mut libraw_data_t, value: c_int);
    pub fn siv_libraw_get_bright(data: *mut libraw_data_t) -> c_float;
    // Size and Metadata helpers
    pub fn libraw_get_raw_height(data: *mut libraw_data_t) -> c_int;
    pub fn libraw_get_raw_width(data: *mut libraw_data_t) -> c_int;
    pub fn libraw_get_iheight(data: *mut libraw_data_t) -> c_int;
    pub fn libraw_get_iwidth(data: *mut libraw_data_t) -> c_int;
    pub fn siv_libraw_get_sizes_width(data: *mut libraw_data_t) -> c_int;
    pub fn siv_libraw_get_sizes_height(data: *mut libraw_data_t) -> c_int;

    // GPU shims
    pub fn siv_libraw_get_raw_image(data: *mut libraw_data_t) -> *mut c_ushort;
    pub fn siv_libraw_get_color_at(data: *mut libraw_data_t, row: c_int, col: c_int) -> c_int;
    pub fn siv_libraw_get_color_params(
        data: *mut libraw_data_t,
        cam_mul: *mut c_float,
        cblack: *mut c_float,
        black: *mut c_int,
        maximum: *mut c_int,
    );
    pub fn siv_libraw_get_color_diag(
        data: *mut libraw_data_t,
        black: *mut c_int,
        maximum: *mut c_int,
        data_maximum: *mut c_int,
        cblack0_3: *mut c_uint,
        cblack4: *mut c_uint,
        cblack5: *mut c_uint,
        pre_mul: *mut c_float,
        cam_mul: *mut c_float,
    );
    pub fn siv_libraw_raw_pixel_at(
        data: *mut libraw_data_t,
        row: c_uint,
        col: c_uint,
    ) -> c_ushort;
    pub fn siv_libraw_get_margins(
        data: *mut libraw_data_t,
        left_margin: *mut c_int,
        top_margin: *mut c_int,
    );
    pub fn siv_libraw_get_filters(data: *mut libraw_data_t) -> c_uint;
    pub fn siv_libraw_get_colors(data: *mut libraw_data_t) -> c_int;
    pub fn siv_libraw_is_fuji_rotated(data: *mut libraw_data_t) -> c_int;
    pub fn siv_libraw_get_pixel_aspect(data: *mut libraw_data_t) -> f64;
    pub fn siv_libraw_get_gpu_color_params(
        data: *mut libraw_data_t,
        rgb_cam_out: *mut c_float,
        cblack_rgb: *mut c_float,
        channel_scale: *mut c_float,
    );
    pub fn siv_libraw_apply_output_color(
        data: *mut libraw_data_t,
        rgb16: *mut c_ushort,
        width: c_uint,
        height: c_uint,
    );
    pub fn siv_libraw_ppg_camera_rgb_counts(
        data: *mut libraw_data_t,
        rgb16_out: *mut c_ushort,
        width_out: *mut c_uint,
        height_out: *mut c_uint,
    ) -> c_int;
    pub fn siv_libraw_ppg_camera_rgb_counts_from_scaled(
        data: *mut libraw_data_t,
        rgb16_out: *mut c_ushort,
        width_out: *mut c_uint,
        height_out: *mut c_uint,
    ) -> c_int;
    pub fn siv_libraw_extract_scaled_cfa(
        data: *mut libraw_data_t,
        out: *mut c_ushort,
        width_out: *mut c_uint,
        height_out: *mut c_uint,
    ) -> c_int;
    pub fn siv_libraw_decimated_ppg_matrix_patch_mean(
        data: *mut libraw_data_t,
        rgb_cam: *const c_float,
        mean_out: *mut c_float,
    ) -> c_int;
    pub fn siv_libraw_decimated_ppg_scene_color_scale(
        data: *mut libraw_data_t,
        rgb_cam: *const c_float,
        scale_out: *mut c_float,
    ) -> c_int;
    pub fn siv_libraw_decimated_ppg_uniform_scene_scale(
        data: *mut libraw_data_t,
        rgb_cam: *const c_float,
        uniform_out: *mut c_float,
    ) -> c_int;
    pub fn siv_libraw_decimated_ppg_scene_ab_luma_sum(
        data: *mut libraw_data_t,
        rgb_cam: *const c_float,
        ab_luma_out: *mut c_double,
    ) -> c_int;
    pub fn siv_libraw_ppg_pixel_channels(
        data: *mut libraw_data_t,
        row: c_uint,
        col: c_uint,
        out4: *mut c_ushort,
    ) -> c_int;
    pub fn siv_libraw_ppg_convert_pixel(
        data: *mut libraw_data_t,
        row: c_uint,
        col: c_uint,
        out3: *mut c_ushort,
    ) -> c_int;
    pub fn libraw_get_rgb_cam(data: *mut libraw_data_t, index1: c_int, index2: c_int) -> c_float;
}

pub fn version() -> String {
    unsafe {
        let ptr = libraw_version();
        if ptr.is_null() {
            return "unknown".to_string();
        }
        std::ffi::CStr::from_ptr(ptr).to_string_lossy().into_owned()
    }
}

// ── RAII guards ────────────────────────────────────────────────────────

/// Owns a [`libraw_data_t`] and calls [`libraw_close`] on drop.
///
/// A `libraw_data_t` is **not** `Send` by default. Callers that guarantee
/// exclusive serialized access may opt in with `unsafe impl Send`.
#[must_use = "LibRawDataGuard will close the LibRaw instance on drop"]
pub struct LibRawDataGuard {
    ptr: *mut libraw_data_t,
}

impl LibRawDataGuard {
    /// Allocate and initialise a new LibRaw instance.
    ///
    /// Returns `None` if [`libraw_init`] returns a null pointer.
    pub fn new() -> Option<Self> {
        let ptr = unsafe { libraw_init(0) };
        if ptr.is_null() {
            None
        } else {
            Some(Self { ptr })
        }
    }

    /// Raw pointer for FFI calls.
    #[inline]
    pub fn as_ptr(&self) -> *mut libraw_data_t {
        self.ptr
    }
}

impl Drop for LibRawDataGuard {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe { libraw_close(self.ptr); }
        }
    }
}

impl std::fmt::Debug for LibRawDataGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LibRawDataGuard")
            .field("ptr", &self.ptr)
            .finish()
    }
}

// ── libraw_processed_image_t ─────────────────────────────────────────

/// Owns a [`libraw_processed_image_t`] and calls [`libraw_dcraw_clear_mem`] on drop.
#[must_use = "LibRawProcessedImageGuard will free the processed image on drop"]
pub struct LibRawProcessedImageGuard {
    ptr: *mut libraw_processed_image_t,
}

impl LibRawProcessedImageGuard {
    /// Wrap a raw processed-image pointer obtained from
    /// [`libraw_dcraw_make_mem_image`] or [`libraw_dcraw_make_mem_thumb`].
    ///
    /// # Safety
    ///
    /// `ptr` must be a valid, non-null pointer that has not been passed to
    /// another guard or to [`libraw_dcraw_clear_mem`].
    #[inline]
    pub unsafe fn from_ptr(ptr: *mut libraw_processed_image_t) -> Self {
        debug_assert!(!ptr.is_null(), "LibRawProcessedImageGuard constructed with null");
        Self { ptr }
    }

    /// Reference to the underlying C struct.
    #[inline]
    pub fn as_ref(&self) -> &libraw_processed_image_t {
        unsafe { &*self.ptr }
    }

    /// Consume the guard and return the raw pointer without freeing.
    #[inline]
    pub fn into_raw(mut self) -> *mut libraw_processed_image_t {
        let ptr = self.ptr;
        self.ptr = std::ptr::null_mut();
        ptr
    }
}

impl Drop for LibRawProcessedImageGuard {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe { libraw_dcraw_clear_mem(self.ptr); }
        }
    }
}

impl std::fmt::Debug for LibRawProcessedImageGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LibRawProcessedImageGuard")
            .field("ptr", &self.ptr)
            .finish()
    }
}
