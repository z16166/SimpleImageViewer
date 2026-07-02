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

#![cfg(feature = "avif-native")]

use crate::hdr::types::HdrImageBuffer;

use super::decode::{avif_ftyp_major_brand, libavif_result_to_string};

fn avif_open_image_sequence_decoder(
    bytes: &[u8],
) -> Result<Option<(libavif_sys::AvifDecoderOwned, usize)>, String> {
    let Some(decoder) = libavif_sys::AvifDecoderOwned::new() else {
        return Err("Failed to create libavif decoder".to_string());
    };

    unsafe {
        libavif_sys::siv_avif_decoder_set_strict_flags(
            decoder.as_ptr(),
            libavif_sys::AVIF_STRICT_DISABLED,
        );
        libavif_sys::siv_avif_decoder_set_image_content_flags(
            decoder.as_ptr(),
            libavif_sys::AVIF_IMAGE_CONTENT_COLOR_AND_ALPHA,
        );
    }

    if let Some(major) = avif_ftyp_major_brand(bytes)
        && &major == b"avis"
    {
        let r = unsafe {
            libavif_sys::avifDecoderSetSource(
                decoder.as_ptr(),
                libavif_sys::AVIF_DECODER_SOURCE_TRACKS,
            )
        };
        if r != libavif_sys::AVIF_RESULT_OK {
            return Err(format!(
                "libavif SetSource(TRACKS): {}",
                libavif_result_to_string(r)
            ));
        }
    }

    let r = unsafe {
        libavif_sys::avifDecoderSetIOMemory(decoder.as_ptr(), bytes.as_ptr(), bytes.len())
    };
    if r != libavif_sys::AVIF_RESULT_OK {
        return Err(format!(
            "libavif SetIOMemory: {}",
            libavif_result_to_string(r)
        ));
    }

    let r = unsafe { libavif_sys::avifDecoderParse(decoder.as_ptr()) };
    if r != libavif_sys::AVIF_RESULT_OK {
        return Ok(None);
    }

    let seq =
        unsafe { libavif_sys::siv_avif_decoder_image_sequence_track_present(decoder.as_ptr()) };
    let count = unsafe { libavif_sys::siv_avif_decoder_get_image_count(decoder.as_ptr()) };
    if seq == 0 || count <= 1 {
        return Ok(None);
    }

    let count =
        usize::try_from(count).map_err(|_| "libavif imageCount does not fit usize".to_string())?;
    Ok(Some((decoder, count)))
}

fn decode_avif_sequence_frames_from_decoder(
    decoder: &libavif_sys::AvifDecoderOwned,
    target_hdr_capacity: f32,
    max_frames: Option<usize>,
) -> Result<Vec<(std::time::Duration, HdrImageBuffer)>, String> {
    use crate::constants::{DEFAULT_ANIMATION_DELAY_MS, MIN_ANIMATION_DELAY_THRESHOLD_MS};
    use std::time::Duration;

    let total = unsafe { libavif_sys::siv_avif_decoder_get_image_count(decoder.as_ptr()) };
    let total =
        usize::try_from(total).map_err(|_| "libavif imageCount does not fit usize".to_string())?;
    let limit = max_frames.unwrap_or(total).min(total);
    let mut frames = Vec::with_capacity(limit);
    for _ in 0..limit {
        let r = unsafe { libavif_sys::avifDecoderNextImage(decoder.as_ptr()) };
        if r != libavif_sys::AVIF_RESULT_OK {
            return Err(format!(
                "libavif NextImage: {}",
                libavif_result_to_string(r)
            ));
        }

        let mut timing = std::mem::MaybeUninit::<libavif_sys::avifImageTiming>::zeroed();
        unsafe {
            libavif_sys::siv_avif_decoder_copy_image_timing(decoder.as_ptr(), timing.as_mut_ptr());
        }
        let timing = unsafe { timing.assume_init() };

        let img_ptr = unsafe { libavif_sys::siv_avif_decoder_get_image(decoder.as_ptr()) };
        if img_ptr.is_null() {
            return Err("libavif decoder image is null".to_string());
        }
        let hdr = super::avif_image_to_hdr_buffer(img_ptr, target_hdr_capacity)?;

        let delay_ms = (timing.duration * 1000.0)
            .round()
            .clamp(0.0, u32::MAX as f64) as u32;
        let delay_ms = if delay_ms <= MIN_ANIMATION_DELAY_THRESHOLD_MS {
            DEFAULT_ANIMATION_DELAY_MS
        } else {
            delay_ms
        };
        frames.push((Duration::from_millis(delay_ms as u64), hdr));
    }

    Ok(frames)
}

pub(crate) struct AvifSequenceDecode {
    pub frames: Vec<(std::time::Duration, HdrImageBuffer)>,
    pub total_frame_count: usize,
}

/// Decode an AVIF image sequence. When `max_frames` is `Some(n)`, decode at most `n` frames
/// but still report the container [`AvifSequenceDecode::total_frame_count`].
pub(crate) fn try_decode_avif_image_sequence_hdr_limited(
    bytes: &[u8],
    target_hdr_capacity: f32,
    max_frames: Option<usize>,
) -> Result<Option<AvifSequenceDecode>, String> {
    let Some((decoder, total_frame_count)) = avif_open_image_sequence_decoder(bytes)? else {
        return Ok(None);
    };
    let frames =
        decode_avif_sequence_frames_from_decoder(&decoder, target_hdr_capacity, max_frames)?;
    Ok(Some(AvifSequenceDecode {
        frames,
        total_frame_count,
    }))
}
