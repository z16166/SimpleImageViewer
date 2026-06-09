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
use super::types::{
    DelayedFallbackJob, ImageLoader, TileInFlightKey, TileRequest, should_spawn_load_task,
};

use crate::hdr::types::HdrToneMapSettings;
use crate::loader::decode::load_image_file;
use crate::loader::preview_caps::{
    REFINEMENT_POOL, finalize_raw_hq_developed_image, finalize_raw_hq_hdr_buffer,
};
use crate::loader::{
    DecodedImage, HdrSdrFallbackResult, LoadResult, LoaderOutput, PreviewBundle, PreviewResult,
    RefinementRequest, TileDecodeSource, TilePixelKind, TileResult,
    hdr_display_requests_sdr_preview, hdr_sdr_fallback_rgba8_eager_or_placeholder,
    hq_preview_max_side, source_key_for_path,
};
use crate::raw_processor::RawProcessor;
use crossbeam_channel::{Receiver, Sender, TryRecvError};
use image::DynamicImage;
use parking_lot::{Condvar, Mutex};

enum EitherDevelop {
    Sdr(DynamicImage),
    Hdr(crate::hdr::types::HdrImageBuffer),
}
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU32;
use std::time::Duration;


impl ImageLoader {
    pub fn request_tile(
        &self,
        index: usize,
        generation: u64,
        priority: f32,
        source: TileDecodeSource,
        col: u32,
        row: u32,
    ) {
        let (lock, cvar) = &*self.tile_queue;
        let mut heap = lock.lock();
        heap.push(TileRequest {
            generation,
            priority,
            index,
            col,
            row,
            source,
        });
        cvar.notify_one();
    }

}
