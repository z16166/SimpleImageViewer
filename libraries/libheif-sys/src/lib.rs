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

pub const heif_error_Ok: heif_error_code = 0;
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
    pub fn heif_image_handle_get_luma_bits_per_pixel(handle: *const heif_image_handle)
    -> libc::c_int;
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
    pub fn heif_decode_image(
        handle: *const heif_image_handle,
        out_img: *mut *mut heif_image,
        colorspace: heif_colorspace,
        chroma: heif_chroma,
        options: *const libc::c_void,
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
}
