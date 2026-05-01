// Simple Image Viewer - A high-performance, cross-platform image viewer
// Copyright (C) 2024-2026 Simple Image Viewer Contributors
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

use libc::{c_char, c_int, c_void, size_t};

pub type ExrResult = i32;
pub type ExrContext = *mut c_void;
pub type ExrConstContext = *const c_void;

pub const EXR_ERR_SUCCESS: ExrResult = 0;
pub const EXR_STORAGE_SCANLINE: c_int = 0;
pub const EXR_STORAGE_TILED: c_int = 1;
pub const EXR_PIXEL_FLOAT: u16 = 2;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExrAttrV2i {
    pub x: i32,
    pub y: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExrAttrBox2i {
    pub min: ExrAttrV2i,
    pub max: ExrAttrV2i,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ExrAttrString {
    pub length: i32,
    pub alloc_size: i32,
    pub str_: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ExrAttrChlistEntry {
    pub name: ExrAttrString,
    pub pixel_type: c_int,
    pub p_linear: u8,
    pub reserved: [u8; 3],
    pub x_sampling: i32,
    pub y_sampling: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ExrAttrChlist {
    pub num_channels: c_int,
    pub num_alloced: c_int,
    pub entries: *const ExrAttrChlistEntry,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ExrChunkInfo {
    pub idx: i32,
    pub start_x: i32,
    pub start_y: i32,
    pub height: i32,
    pub width: i32,
    pub level_x: u8,
    pub level_y: u8,
    pub type_: u8,
    pub compression: u8,
    pub data_offset: u64,
    pub packed_size: u64,
    pub unpacked_size: u64,
    pub sample_count_data_offset: u64,
    pub sample_count_table_size: u64,
}

impl Default for ExrChunkInfo {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ExrCodingChannelInfo {
    pub channel_name: *const c_char,
    pub height: i32,
    pub width: i32,
    pub x_samples: i32,
    pub y_samples: i32,
    pub p_linear: u8,
    pub bytes_per_element: i8,
    pub data_type: u16,
    pub user_bytes_per_element: i16,
    pub user_data_type: u16,
    pub user_pixel_stride: i32,
    pub user_line_stride: i32,
    pub decode_to_ptr: *mut u8,
}

impl Default for ExrCodingChannelInfo {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

type ExrPipelineAllocFn = Option<unsafe extern "C" fn(c_int, size_t) -> *mut c_void>;
type ExrPipelineFreeFn = Option<unsafe extern "C" fn(c_int, *mut c_void)>;
type ExrPipelineFn = Option<unsafe extern "C" fn(*mut ExrDecodePipeline) -> ExrResult>;
type ExrPipelineReallocFn = Option<unsafe extern "C" fn(*mut ExrDecodePipeline) -> ExrResult>;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ExrDecodePipeline {
    pub pipe_size: size_t,
    pub channels: *mut ExrCodingChannelInfo,
    pub channel_count: i16,
    pub decode_flags: u16,
    pub part_index: c_int,
    pub context: ExrConstContext,
    pub chunk: ExrChunkInfo,
    pub user_line_begin_skip: i32,
    pub user_line_end_ignore: i32,
    pub bytes_decompressed: u64,
    pub decoding_user_data: *mut c_void,
    pub packed_buffer: *mut c_void,
    pub packed_alloc_size: size_t,
    pub unpacked_buffer: *mut c_void,
    pub unpacked_alloc_size: size_t,
    pub packed_sample_count_table: *mut c_void,
    pub packed_sample_count_alloc_size: size_t,
    pub sample_count_table: *mut i32,
    pub sample_count_alloc_size: size_t,
    pub scratch_buffer_1: *mut c_void,
    pub scratch_alloc_size_1: size_t,
    pub scratch_buffer_2: *mut c_void,
    pub scratch_alloc_size_2: size_t,
    pub alloc_fn: ExrPipelineAllocFn,
    pub free_fn: ExrPipelineFreeFn,
    pub read_fn: ExrPipelineFn,
    pub decompress_fn: ExrPipelineFn,
    pub realloc_nonimage_data_fn: ExrPipelineReallocFn,
    pub unpack_and_convert_fn: ExrPipelineFn,
    pub quick_chan_store: [ExrCodingChannelInfo; 5],
}

impl Default for ExrDecodePipeline {
    fn default() -> Self {
        let mut pipeline: Self = unsafe { std::mem::zeroed() };
        pipeline.pipe_size = std::mem::size_of::<Self>();
        pipeline
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ExrContextInitializer {
    pub size: size_t,
    pub error_handler_fn: Option<unsafe extern "C" fn(ExrConstContext, ExrResult, *const c_char)>,
    pub alloc_fn: Option<unsafe extern "C" fn(size_t) -> *mut c_void>,
    pub free_fn: Option<unsafe extern "C" fn(*mut c_void)>,
    pub user_data: *mut c_void,
    pub read_fn: Option<
        unsafe extern "C" fn(
            ExrConstContext,
            *mut c_void,
            *mut c_void,
            u64,
            u64,
            *mut c_void,
        ) -> i64,
    >,
    pub size_fn: Option<unsafe extern "C" fn(ExrConstContext, *mut c_void) -> i64>,
    pub write_fn: Option<
        unsafe extern "C" fn(
            ExrConstContext,
            *mut c_void,
            *const c_void,
            u64,
            u64,
            *mut c_void,
        ) -> i64,
    >,
    pub destroy_fn: Option<unsafe extern "C" fn(ExrConstContext, *mut c_void, c_int)>,
    pub max_image_width: c_int,
    pub max_image_height: c_int,
    pub max_tile_width: c_int,
    pub max_tile_height: c_int,
    pub zip_level: c_int,
    pub dwa_quality: f32,
    pub flags: c_int,
    pub pad: [u8; 4],
}

unsafe extern "C" {
    pub fn exr_get_library_version(
        major: *mut c_int,
        minor: *mut c_int,
        patch: *mut c_int,
        extra: *mut *const c_char,
    );
    pub fn exr_get_default_error_message(code: ExrResult) -> *const c_char;
    pub fn exr_start_read(
        ctxt: *mut ExrContext,
        filename: *const c_char,
        ctxtdata: *const ExrContextInitializer,
    ) -> ExrResult;
    pub fn exr_finish(ctxt: *mut ExrContext) -> ExrResult;
    pub fn exr_get_count(ctxt: ExrConstContext, count: *mut c_int) -> ExrResult;
    pub fn exr_get_storage(ctxt: ExrConstContext, part_index: c_int, out: *mut c_int)
    -> ExrResult;
    pub fn exr_get_data_window(
        ctxt: ExrConstContext,
        part_index: c_int,
        out: *mut ExrAttrBox2i,
    ) -> ExrResult;
    pub fn exr_get_channels(
        ctxt: ExrConstContext,
        part_index: c_int,
        chlist: *mut *const ExrAttrChlist,
    ) -> ExrResult;
    pub fn exr_get_chunk_count(
        ctxt: ExrConstContext,
        part_index: c_int,
        out: *mut i32,
    ) -> ExrResult;
    pub fn exr_read_scanline_chunk_info(
        ctxt: ExrConstContext,
        part_index: c_int,
        y: c_int,
        cinfo: *mut ExrChunkInfo,
    ) -> ExrResult;
    pub fn exr_get_tile_sizes(
        ctxt: ExrConstContext,
        part_index: c_int,
        levelx: c_int,
        levely: c_int,
        tilew: *mut i32,
        tileh: *mut i32,
    ) -> ExrResult;
    pub fn exr_get_tile_counts(
        ctxt: ExrConstContext,
        part_index: c_int,
        levelx: c_int,
        levely: c_int,
        countx: *mut i32,
        county: *mut i32,
    ) -> ExrResult;
    pub fn exr_read_tile_chunk_info(
        ctxt: ExrConstContext,
        part_index: c_int,
        tilex: c_int,
        tiley: c_int,
        levelx: c_int,
        levely: c_int,
        cinfo: *mut ExrChunkInfo,
    ) -> ExrResult;
    pub fn exr_decoding_initialize(
        ctxt: ExrConstContext,
        part_index: c_int,
        cinfo: *const ExrChunkInfo,
        decode: *mut ExrDecodePipeline,
    ) -> ExrResult;
    pub fn exr_decoding_choose_default_routines(
        ctxt: ExrConstContext,
        part_index: c_int,
        decode: *mut ExrDecodePipeline,
    ) -> ExrResult;
    pub fn exr_decoding_run(
        ctxt: ExrConstContext,
        part_index: c_int,
        decode: *mut ExrDecodePipeline,
    ) -> ExrResult;
    pub fn exr_decoding_destroy(
        ctxt: ExrConstContext,
        decode: *mut ExrDecodePipeline,
    ) -> ExrResult;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpenExrCoreVersion {
    pub major: c_int,
    pub minor: c_int,
    pub patch: c_int,
}

pub fn library_version() -> OpenExrCoreVersion {
    let mut major = 0;
    let mut minor = 0;
    let mut patch = 0;
    let mut extra = std::ptr::null();
    unsafe {
        exr_get_library_version(&mut major, &mut minor, &mut patch, &mut extra);
    }
    OpenExrCoreVersion {
        major,
        minor,
        patch,
    }
}
