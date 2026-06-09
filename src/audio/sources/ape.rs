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
use super::super::norm::{NORM_I8, NORM_I16, NORM_I24, NORM_I32};

use crate::constants::{
    DEFAULT_CHANNELS, DEFAULT_SAMPLE_RATE,
};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use std::ffi::c_void;
use std::num::NonZero;
#[cfg(target_os = "windows")]
use std::os::windows::ffi::OsStrExt;

#[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
use monkey_sdk_sys::*;

// ---------------------------------------------------------------------------
// Custom APE Source using official Monkey's Audio SDK (Native)
// ---------------------------------------------------------------------------

pub(crate) struct ApeSource {
    decoder: *mut c_void,
    sample_rate: u32,
    channels: u16,
    bits_per_sample: u16,
    total_blocks: i64,
    current_block: i64,
    buffer: Vec<f32>,
    buffer_pos: usize,
    shutdown_flag: Arc<AtomicBool>,
}

// Explicitly mark as Send because the raw pointer is managed carefully
unsafe impl Send for ApeSource {}

impl ApeSource {
    pub fn new_with_offset(
        path: &Path,
        shutdown_flag: Arc<AtomicBool>,
        offset: Duration,
    ) -> Option<Self> {
        let decoder_ptr = {
            #[cfg(target_os = "windows")]
            {
                let mut wide_path: Vec<u16> = path.as_os_str().encode_wide().collect();
                wide_path.push(0);
                unsafe { monkey_decoder_open(wide_path.as_ptr() as *const _) }
            }

            #[cfg(any(target_os = "linux", target_os = "macos"))]
            {
                // On Linux/macOS, str_utfn is wchar_t which is 32-bit (UTF-32)
                let s = path.to_string_lossy();
                let mut wide_path: Vec<u32> = s.chars().map(|c| c as u32).collect();
                wide_path.push(0);
                unsafe { monkey_decoder_open(wide_path.as_ptr() as *const _) }
            }

            #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
            {
                log::error!("[AUDIO] Native APE SDK is not supported on this platform.");
                return None;
            }
        };

        if decoder_ptr.is_null() {
            log::error!(
                "[AUDIO] Native Monkey's Audio SDK failed to open: {:?}",
                path.file_name()
            );
            return None;
        }

        let mut sample_rate: i32 = 0;
        let mut bits_per_sample: i32 = 0;
        let mut channels: i32 = 0;
        let mut total_blocks: i64 = 0;

        if unsafe {
            monkey_decoder_get_info(
                decoder_ptr,
                &mut sample_rate,
                &mut bits_per_sample,
                &mut channels,
                &mut total_blocks,
            )
        } != 0
        {
            log::error!(
                "[AUDIO] Native Monkey's Audio SDK failed to get info: {:?}",
                path.file_name()
            );
            unsafe { monkey_decoder_close(decoder_ptr) };
            return None;
        }

        log::info!(
            "[AUDIO] Native APE Info: Rate={}, Bits={}, Chan={}, Blocks={}",
            sample_rate,
            bits_per_sample,
            channels,
            total_blocks
        );

        let mut source = Self {
            decoder: decoder_ptr,
            sample_rate: sample_rate as u32,
            channels: channels as u16,
            bits_per_sample: bits_per_sample as u16,
            total_blocks,
            current_block: 0,
            buffer: Vec::new(),
            buffer_pos: 0,
            shutdown_flag,
        };

        if offset > Duration::ZERO {
            let offset_blocks = (offset.as_secs_f64() * sample_rate as f64) as i64;
            let target_block = offset_blocks.min(total_blocks.saturating_sub(1));
            if unsafe { monkey_decoder_seek(decoder_ptr, target_block) } == 0 {
                source.current_block = target_block;
            } else {
                log::warn!(
                    "[AUDIO] Native APE seek failed to block {} for {:?}",
                    target_block,
                    path.file_name()
                );
            }
        }

        Some(source)
    }

    fn decode_next_blocks(&mut self) -> bool {
        if self.shutdown_flag.load(Ordering::Relaxed) {
            return false;
        }

        const BLOCKS_TO_DECODE: i32 = 4096;
        let bytes_per_block = (self.channels as i32 * (self.bits_per_sample as i32 / 8)) as usize;
        let mut raw_buffer = vec![0u8; BLOCKS_TO_DECODE as usize * bytes_per_block];
        let mut blocks_retrieved: i32 = 0;

        #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
        let ret = unsafe {
            monkey_decoder_decode_blocks(
                self.decoder,
                raw_buffer.as_mut_ptr(),
                BLOCKS_TO_DECODE,
                &mut blocks_retrieved,
            )
        };

        #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
        let ret = {
            let _ = raw_buffer;
            -1
        };

        if ret != 0 || blocks_retrieved == 0 {
            return false;
        }

        self.buffer.clear();
        self.buffer_pos = 0;
        self.current_block += blocks_retrieved as i64;

        let bits = self.bits_per_sample;
        let bytes_per_sample = (bits / 8) as usize;

        for chunk in
            raw_buffer[..blocks_retrieved as usize * bytes_per_block].chunks_exact(bytes_per_sample)
        {
            let sample = match bits {
                8 => (chunk[0] as i8 as f32) / NORM_I8,
                16 => (i16::from_le_bytes([chunk[0], chunk[1]]) as f32) / NORM_I16,
                24 => {
                    let val = i32::from_le_bytes([
                        chunk[0],
                        chunk[1],
                        chunk[2],
                        if chunk[2] & 0x80 != 0 { 0xFF } else { 0x00 },
                    ]);
                    val as f32 / NORM_I24
                }
                32 => {
                    (i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]) as f32) / NORM_I32
                }
                _ => 0.0,
            };
            self.buffer.push(sample);
        }

        true
    }
}

impl Drop for ApeSource {
    fn drop(&mut self) {
        #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
        unsafe {
            monkey_decoder_close(self.decoder);
        }
    }
}

impl Iterator for ApeSource {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        if self.buffer_pos >= self.buffer.len() {
            if !self.decode_next_blocks() {
                return None;
            }
        }

        let sample = self.buffer[self.buffer_pos];
        self.buffer_pos += 1;
        Some(sample)
    }
}

impl rodio::Source for ApeSource {
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> NonZero<u16> {
        NonZero::new(self.channels).unwrap_or(NonZero::new(DEFAULT_CHANNELS).unwrap())
    }

    fn sample_rate(&self) -> NonZero<u32> {
        NonZero::new(self.sample_rate).unwrap_or(NonZero::new(DEFAULT_SAMPLE_RATE).unwrap())
    }

    fn total_duration(&self) -> Option<Duration> {
        if self.sample_rate > 0 {
            Some(Duration::from_secs_f64(
                self.total_blocks as f64 / self.sample_rate as f64,
            ))
        } else {
            None
        }
    }
}
