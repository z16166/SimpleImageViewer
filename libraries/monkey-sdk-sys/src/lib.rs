// Simple Image Viewer - A high-performance, cross-platform image viewer
// Copyright (C) 2024-2026 Simple Image Viewer Contributors
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

use std::ffi::c_void;

unsafe extern "C" {
    pub fn monkey_decoder_open(filename: *const c_void) -> *mut c_void;
    pub fn monkey_decoder_close(decoder: *mut c_void);
    pub fn monkey_decoder_get_info(
        decoder: *mut c_void,
        sample_rate: *mut i32,
        bits_per_sample: *mut i32,
        channels: *mut i32,
        total_blocks: *mut i64,
    ) -> i32;
    pub fn monkey_decoder_decode_blocks(
        decoder: *mut c_void,
        buffer: *mut u8,
        blocks_to_decode: i32,
        blocks_decoded: *mut i32,
    ) -> i32;
    pub fn monkey_decoder_seek(decoder: *mut c_void, block_offset: i64) -> i32;
}
