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

use libc::{c_char, c_int, c_uchar, c_uint, c_ushort};

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

#[link(name = "raw", kind = "static")]
unsafe extern "C" {
    pub fn libraw_version() -> *const c_char;
    pub fn libraw_init(flags: c_int) -> *mut libraw_data_t;
    pub fn libraw_close(data: *mut libraw_data_t);
    pub fn libraw_open_file(data: *mut libraw_data_t, file: *const c_char) -> c_int;
    pub fn libraw_unpack(data: *mut libraw_data_t) -> c_int;
    pub fn libraw_unpack_thumb(data: *mut libraw_data_t) -> c_int;
    pub fn libraw_dcraw_process(data: *mut libraw_data_t) -> c_int;
    pub fn libraw_dcraw_make_mem_image(data: *mut libraw_data_t, errc: *mut c_int) -> *mut libraw_processed_image_t;
    pub fn libraw_dcraw_make_mem_thumb(data: *mut libraw_data_t, errc: *mut c_int) -> *mut libraw_processed_image_t;
    pub fn libraw_dcraw_clear_mem(image: *mut libraw_processed_image_t);
    
    // Params and state
    pub fn libraw_set_output_params_by_dict(data: *mut libraw_data_t, key: *const c_char, value: *const c_char) -> c_int;
    
    // Size and Metadata helpers
    pub fn libraw_get_raw_height(data: *mut libraw_data_t) -> c_int;
    pub fn libraw_get_raw_width(data: *mut libraw_data_t) -> c_int;
    pub fn libraw_get_iheight(data: *mut libraw_data_t) -> c_int;
    pub fn libraw_get_iwidth(data: *mut libraw_data_t) -> c_int;
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
