// Simple Image Viewer - A high-performance, cross-platform image viewer
// Copyright (C) 2024-2026 Simple Image Viewer Contributors
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

#![allow(dead_code)]

use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::{CStr, CString, c_char};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

use openexr_core_sys as sys;
use rayon::prelude::*;

const DEFAULT_DECODED_CHUNK_CACHE_BYTES: usize = 512 * 1024 * 1024;
const MAX_DECODED_CHUNK_CACHE_BYTES: usize = 4 * 1024 * 1024 * 1024;
// These knobs are intentionally scoped to OpenEXR scanline preview generation.
// They do not affect SDR images, Radiance HDR, JPEG_R/Ultra HDR, final EXR tiles, or tiled EXR files.
//
// A 512px request is the synchronous bootstrap shown before the UI leaves "loading".
// For giant scanline PIZ files, decoding every sampled row can take many seconds because each
// native chunk is a full-width strip. We therefore use a larger-but-still-budgeted 1024px preview:
// it is less blocky than 512px, while the row budget keeps initial load bounded.
const SCANLINE_BOOTSTRAP_PREVIEW_MAX_SIDE: u32 = 1024;
const SCANLINE_BOOTSTRAP_PREVIEW_SOURCE_ROW_BUDGET: u32 = 192;
// Refined preview is generated off the UI-critical path, so keep full row sampling for quality.
// A zero budget means "do not collapse preview rows into representative buckets".
const SCANLINE_REFINED_PREVIEW_SOURCE_ROW_BUDGET: u32 = 0;
const SCANLINE_PREVIEW_MAX_PARALLEL_CHUNKS: usize = 4;

#[derive(Debug)]
pub(crate) struct OpenExrCoreReadContext {
    path: PathBuf,
    raw: sys::ExrContext,
    part_count: usize,
    decoded_chunks: Mutex<OpenExrCoreDecodedChunkCache>,
    decoded_chunk_ready: Condvar,
}

// OpenEXRCore read contexts parse headers up front and are documented as safe
// for concurrent chunk requests when each thread uses its own decode pipeline.
unsafe impl Send for OpenExrCoreReadContext {}
unsafe impl Sync for OpenExrCoreReadContext {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OpenExrCorePartInfo {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) data_window_min: (i32, i32),
    pub(crate) data_window_max: (i32, i32),
    pub(crate) storage: i32,
    pub(crate) chunk_count: u32,
    pub(crate) channels: Vec<OpenExrCoreChannelInfo>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OpenExrCoreChannelInfo {
    pub(crate) name: String,
    pub(crate) pixel_type: i32,
    pub(crate) x_sampling: i32,
    pub(crate) y_sampling: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OpenExrCoreRgbaTile {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) rgba: Vec<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChannelRole {
    Red,
    Green,
    Blue,
    Luma,
    Alpha,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct OpenExrCoreDecodedChunkKey {
    part_index: i32,
    chunk_index: i32,
    origin: (u32, u32),
    size: (u32, u32),
}

#[derive(Debug)]
struct OpenExrCoreDecodedChunk {
    origin: (u32, u32),
    width: u32,
    height: u32,
    rgba: Arc<Vec<f32>>,
    byte_size: usize,
}

#[derive(Debug)]
struct OpenExrCoreDecodedChunkCache {
    max_bytes: usize,
    current_bytes: usize,
    entries: HashMap<OpenExrCoreDecodedChunkKey, Arc<OpenExrCoreDecodedChunk>>,
    in_flight: HashSet<OpenExrCoreDecodedChunkKey>,
    lru: VecDeque<OpenExrCoreDecodedChunkKey>,
    #[cfg(test)]
    hits: usize,
    #[cfg(test)]
    misses: usize,
}

impl OpenExrCoreDecodedChunkCache {
    fn new(max_bytes: usize) -> Self {
        Self {
            max_bytes,
            current_bytes: 0,
            entries: HashMap::new(),
            in_flight: HashSet::new(),
            lru: VecDeque::new(),
            #[cfg(test)]
            hits: 0,
            #[cfg(test)]
            misses: 0,
        }
    }

    fn get(&mut self, key: &OpenExrCoreDecodedChunkKey) -> Option<Arc<OpenExrCoreDecodedChunk>> {
        let chunk = self.entries.get(key).cloned();
        if chunk.is_some() {
            #[cfg(test)]
            {
                self.hits += 1;
            }
            self.touch(*key);
        } else {
            #[cfg(test)]
            {
                self.misses += 1;
            }
        }
        chunk
    }

    fn begin_decode(&mut self, key: OpenExrCoreDecodedChunkKey) -> bool {
        self.in_flight.insert(key)
    }

    fn finish_decode(&mut self, key: &OpenExrCoreDecodedChunkKey) {
        self.in_flight.remove(key);
    }

    fn insert(&mut self, key: OpenExrCoreDecodedChunkKey, chunk: Arc<OpenExrCoreDecodedChunk>) {
        if chunk.byte_size > self.max_bytes {
            return;
        }

        if let Some(old) = self.entries.insert(key, Arc::clone(&chunk)) {
            self.current_bytes = self.current_bytes.saturating_sub(old.byte_size);
        }
        self.current_bytes = self.current_bytes.saturating_add(chunk.byte_size);
        self.touch(key);
        self.evict_over_budget();
    }

    #[cfg(test)]
    fn hit_count(&self) -> usize {
        self.hits
    }

    #[cfg(test)]
    fn miss_count(&self) -> usize {
        self.misses
    }

    fn touch(&mut self, key: OpenExrCoreDecodedChunkKey) {
        self.lru.retain(|candidate| candidate != &key);
        self.lru.push_back(key);
    }

    fn evict_over_budget(&mut self) {
        while self.current_bytes > self.max_bytes {
            let Some(key) = self.lru.pop_front() else {
                break;
            };
            if let Some(chunk) = self.entries.remove(&key) {
                self.current_bytes = self.current_bytes.saturating_sub(chunk.byte_size);
            }
        }
    }
}

impl OpenExrCoreReadContext {
    pub(crate) fn open(path: &Path) -> Result<Self, String> {
        let filename = CString::new(path.to_string_lossy().as_bytes())
            .map_err(|_| format!("EXR path contains an interior NUL: {}", path.display()))?;
        let mut raw = ptr::null_mut();
        exr_result(unsafe { sys::exr_start_read(&mut raw, filename.as_ptr(), ptr::null()) })?;
        if raw.is_null() {
            return Err(format!(
                "OpenEXRCore returned a null context for {}",
                path.display()
            ));
        }

        let mut part_count = 0;
        if let Err(err) =
            exr_result(unsafe { sys::exr_get_count(raw.cast_const(), &mut part_count) })
        {
            let _ = unsafe { sys::exr_finish(&mut raw) };
            return Err(err);
        }
        let part_count = usize::try_from(part_count)
            .map_err(|_| "OpenEXRCore reported a negative part count".to_string())?;

        Ok(Self {
            path: path.to_path_buf(),
            raw,
            part_count,
            decoded_chunks: Mutex::new(OpenExrCoreDecodedChunkCache::new(
                configured_decoded_chunk_cache_max_bytes(),
            )),
            decoded_chunk_ready: Condvar::new(),
        })
    }

    pub(crate) fn part_count(&self) -> usize {
        self.part_count
    }

    fn source_name(&self) -> String {
        self.path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string())
    }

    pub(crate) fn part(&self, part_index: usize) -> Result<OpenExrCorePartInfo, String> {
        if part_index >= self.part_count {
            return Err(format!(
                "EXR part index {part_index} is out of range for {} part(s) in {}",
                self.part_count,
                self.path.display()
            ));
        }

        let part_index =
            i32::try_from(part_index).map_err(|_| "EXR part index exceeds i32".to_string())?;
        let mut storage = 0;
        exr_result(unsafe {
            sys::exr_get_storage(self.raw.cast_const(), part_index, &mut storage)
        })?;

        let mut data_window = sys::ExrAttrBox2i::default();
        exr_result(unsafe {
            sys::exr_get_data_window(self.raw.cast_const(), part_index, &mut data_window)
        })?;
        let width = extent_from_window_axis(data_window.min.x, data_window.max.x, "width")?;
        let height = extent_from_window_axis(data_window.min.y, data_window.max.y, "height")?;

        let mut chunk_count = 0_i32;
        exr_result(unsafe {
            sys::exr_get_chunk_count(self.raw.cast_const(), part_index, &mut chunk_count)
        })?;
        let chunk_count = u32::try_from(chunk_count)
            .map_err(|_| "OpenEXRCore reported a negative chunk count".to_string())?;

        let mut chlist = ptr::null();
        exr_result(unsafe {
            sys::exr_get_channels(self.raw.cast_const(), part_index, &mut chlist)
        })?;
        let channels = copy_channels(chlist)?;

        Ok(OpenExrCorePartInfo {
            width,
            height,
            data_window_min: (data_window.min.x, data_window.min.y),
            data_window_max: (data_window.max.x, data_window.max.y),
            storage,
            chunk_count,
            channels,
        })
    }

    pub(crate) fn extract_scanline_rgba32f_tile(
        &self,
        part_index: usize,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    ) -> Result<OpenExrCoreRgbaTile, String> {
        let part = self.part(part_index)?;
        validate_tile_bounds(part.width, part.height, x, y, width, height)?;
        #[cfg(feature = "tile-debug")]
        let tile_start = Instant::now();
        #[cfg(feature = "tile-debug")]
        let mut decoded_chunk_count = 0_u32;

        let part_index =
            i32::try_from(part_index).map_err(|_| "EXR part index exceeds i32".to_string())?;
        let mut rgba = vec![0.0_f32; width as usize * height as usize * 4];
        for alpha in rgba.chunks_exact_mut(4).map(|pixel| &mut pixel[3]) {
            *alpha = 1.0;
        }

        match part.storage {
            sys::EXR_STORAGE_SCANLINE => {
                let mut decoded_starts = std::collections::BTreeSet::new();
                for source_y in y..y + height {
                    let mut chunk = sys::ExrChunkInfo::default();
                    exr_result(unsafe {
                        sys::exr_read_scanline_chunk_info(
                            self.raw.cast_const(),
                            part_index,
                            i32::try_from(source_y)
                                .map_err(|_| "EXR scanline y exceeds i32".to_string())?
                                + part.data_window_min.1,
                            &mut chunk,
                        )
                    })?;
                    if !decoded_starts.insert(chunk.start_y) {
                        continue;
                    }
                    if chunk.height <= 0 || chunk.width <= 0 {
                        continue;
                    }

                    let chunk_origin_x = u32::try_from(chunk.start_x - part.data_window_min.0)
                        .map_err(|_| {
                            "OpenEXRCore chunk start_x is outside data window".to_string()
                        })?;
                    let chunk_origin_y = u32::try_from(chunk.start_y - part.data_window_min.1)
                        .map_err(|_| {
                            "OpenEXRCore chunk start_y is outside data window".to_string()
                        })?;
                    let timing = self.decode_chunk_to_tile(
                        part_index,
                        &chunk,
                        (chunk_origin_x, chunk_origin_y),
                        (x, y, width, height),
                        &mut rgba,
                    )?;
                    #[cfg(not(feature = "tile-debug"))]
                    let _ = timing;
                    #[cfg(feature = "tile-debug")]
                    {
                        decoded_chunk_count += 1;
                        self.log_tile_chunk_decode(
                            part_index,
                            &part,
                            &chunk,
                            (chunk_origin_x, chunk_origin_y),
                            (x, y, width, height),
                            timing,
                        );
                    }
                }
            }
            sys::EXR_STORAGE_TILED => {
                let tile_grid = self.tile_grid(part_index)?;
                #[cfg(feature = "tile-debug")]
                log::info!(
                    "[HDR][tile][openexr-core] file=\"{}\" part={} request=({}, {}) size={}x{} storage={} native_tile={}x{} native_tile_count={}x{}",
                    self.source_name(),
                    part_index,
                    x,
                    y,
                    width,
                    height,
                    storage_name(part.storage),
                    tile_grid.tile_width,
                    tile_grid.tile_height,
                    tile_grid.count_x,
                    tile_grid.count_y
                );
                let start_tile_x = x / tile_grid.tile_width;
                let end_tile_x = (x + width - 1) / tile_grid.tile_width;
                let start_tile_y = y / tile_grid.tile_height;
                let end_tile_y = (y + height - 1) / tile_grid.tile_height;

                for tile_y_index in start_tile_y..=end_tile_y {
                    for tile_x_index in start_tile_x..=end_tile_x {
                        if tile_x_index >= tile_grid.count_x || tile_y_index >= tile_grid.count_y {
                            continue;
                        }
                        let mut chunk = sys::ExrChunkInfo::default();
                        exr_result(unsafe {
                            sys::exr_read_tile_chunk_info(
                                self.raw.cast_const(),
                                part_index,
                                i32::try_from(tile_x_index)
                                    .map_err(|_| "EXR tile x index exceeds i32".to_string())?,
                                i32::try_from(tile_y_index)
                                    .map_err(|_| "EXR tile y index exceeds i32".to_string())?,
                                0,
                                0,
                                &mut chunk,
                            )
                        })?;
                        if chunk.height <= 0 || chunk.width <= 0 {
                            continue;
                        }
                        let chunk_origin = (
                            tile_x_index * tile_grid.tile_width,
                            tile_y_index * tile_grid.tile_height,
                        );
                        let timing = self.decode_chunk_to_tile(
                            part_index,
                            &chunk,
                            chunk_origin,
                            (x, y, width, height),
                            &mut rgba,
                        )?;
                        #[cfg(not(feature = "tile-debug"))]
                        let _ = timing;
                        #[cfg(feature = "tile-debug")]
                        {
                            decoded_chunk_count += 1;
                            self.log_tile_chunk_decode(
                                part_index,
                                &part,
                                &chunk,
                                chunk_origin,
                                (x, y, width, height),
                                timing,
                            );
                        }
                    }
                }
            }
            _ => {
                return Err(
                    "OpenEXRCore tile extraction supports only flat scanline or tiled EXR"
                        .to_string(),
                );
            }
        }

        #[cfg(feature = "tile-debug")]
        log::info!(
            "[HDR][tile][openexr-core] done file=\"{}\" part={} request=({}, {}) size={}x{} storage={} decoded_chunks={} elapsed_ms={:.2}",
            self.source_name(),
            part_index,
            x,
            y,
            width,
            height,
            storage_name(part.storage),
            decoded_chunk_count,
            tile_start.elapsed().as_secs_f64() * 1000.0
        );

        Ok(OpenExrCoreRgbaTile {
            width,
            height,
            rgba,
        })
    }

    pub(crate) fn extract_scanline_rgba32f_preview_nearest(
        &self,
        part_index: usize,
        max_w: u32,
        max_h: u32,
    ) -> Result<OpenExrCoreRgbaTile, String> {
        let started_at = Instant::now();
        let part = self.part(part_index)?;
        if part.storage != sys::EXR_STORAGE_SCANLINE {
            return Err("OpenEXRCore scanline preview supports only flat scanline EXR".to_string());
        }
        let (width, height) = scanline_preview_dimensions(part.width, part.height, max_w, max_h);
        if width == 0 || height == 0 {
            return Err("EXR preview dimensions must be non-zero".to_string());
        }

        let part_index =
            i32::try_from(part_index).map_err(|_| "EXR part index exceeds i32".to_string())?;
        let mut rows_by_chunk = std::collections::BTreeMap::<
            i32,
            (sys::ExrChunkInfo, (u32, u32), Vec<(u32, u32)>),
        >::new();
        let source_row_budget = scanline_preview_source_row_budget(max_w);
        for preview_y in 0..height {
            let source_y = budgeted_scanline_preview_source_y(
                preview_y,
                height,
                part.height,
                source_row_budget,
            );
            let mut chunk = sys::ExrChunkInfo::default();
            exr_result(unsafe {
                sys::exr_read_scanline_chunk_info(
                    self.raw.cast_const(),
                    part_index,
                    i32::try_from(source_y)
                        .map_err(|_| "EXR scanline y exceeds i32".to_string())?
                        + part.data_window_min.1,
                    &mut chunk,
                )
            })?;
            if chunk.height <= 0 || chunk.width <= 0 {
                continue;
            }
            let chunk_origin = (
                u32::try_from(chunk.start_x - part.data_window_min.0)
                    .map_err(|_| "OpenEXRCore chunk start_x is outside data window".to_string())?,
                u32::try_from(chunk.start_y - part.data_window_min.1)
                    .map_err(|_| "OpenEXRCore chunk start_y is outside data window".to_string())?,
            );
            rows_by_chunk
                .entry(chunk.start_y)
                .or_insert_with(|| (chunk, chunk_origin, Vec::new()))
                .2
                .push((preview_y, source_y));
        }

        let mut rgba = vec![0.0_f32; width as usize * height as usize * 4];
        for alpha in rgba.chunks_exact_mut(4).map(|pixel| &mut pixel[3]) {
            *alpha = 1.0;
        }
        let unique_chunks = rows_by_chunk.len();
        let mut cache_hits = 0usize;
        let mut cache_misses = 0usize;
        let mut decode_ms = 0.0_f64;
        let mut copy_ms = 0.0_f64;
        let chunk_jobs = rows_by_chunk.into_values().collect::<Vec<_>>();
        let parallel_chunks = scanline_preview_decode_parallelism(unique_chunks);
        for chunk_batch in chunk_jobs.chunks(parallel_chunks) {
            let fetched_batch = chunk_batch
                .par_iter()
                .map(|(chunk, chunk_origin, _rows)| {
                    self.fetch_decoded_chunk(part_index, chunk, *chunk_origin)
                })
                .collect::<Result<Vec<_>, _>>()?;

            for (fetched, (_chunk, _chunk_origin, rows)) in
                fetched_batch.into_iter().zip(chunk_batch.iter())
            {
                if fetched.cache_hit {
                    cache_hits += 1;
                } else {
                    cache_misses += 1;
                }
                decode_ms += fetched.decode_ms;
                let copy_started = Instant::now();
                sample_decoded_scanline_chunk_into_preview(
                    &fetched.decoded,
                    part.width,
                    width,
                    height,
                    rows,
                    &mut rgba,
                )?;
                copy_ms += copy_started.elapsed().as_secs_f64() * 1000.0;
            }
        }
        log::info!(
            "[HDR][preview][openexr-core] file=\"{}\" part={} requested={}x{} effective={}x{} source={}x{} storage=scanline row_budget={} unique_chunks={} parallel_chunks={} cache_hit={} cache_miss={} decode_ms={:.2} copy_ms={:.2} elapsed_ms={:.2}",
            self.path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("<unknown>"),
            part_index,
            max_w,
            max_h,
            width,
            height,
            part.width,
            part.height,
            source_row_budget,
            unique_chunks,
            parallel_chunks,
            cache_hits,
            cache_misses,
            decode_ms,
            copy_ms,
            started_at.elapsed().as_secs_f64() * 1000.0
        );

        Ok(OpenExrCoreRgbaTile {
            width,
            height,
            rgba,
        })
    }

    #[cfg(feature = "tile-debug")]
    fn log_tile_chunk_decode(
        &self,
        part_index: i32,
        part: &OpenExrCorePartInfo,
        chunk: &sys::ExrChunkInfo,
        chunk_origin: (u32, u32),
        request: (u32, u32, u32, u32),
        timing: OpenExrCoreChunkDecodeTiming,
    ) {
        let (request_x, request_y, request_width, request_height) = request;
        log::info!(
            "[HDR][tile][openexr-core] chunk file=\"{}\" part={} request=({}, {}) size={}x{} storage={} native_coord=({}, {}) native_size={}x{} compression={} packed_bytes={} unpacked_bytes={} cache={} decode_ms={:.2} copy_ms={:.2}",
            self.source_name(),
            part_index,
            request_x,
            request_y,
            request_width,
            request_height,
            storage_name(part.storage),
            chunk_origin.0,
            chunk_origin.1,
            chunk.width,
            chunk.height,
            compression_name(chunk.compression),
            chunk.packed_size,
            chunk.unpacked_size,
            if timing.cache_hit { "hit" } else { "miss" },
            timing.decode_ms,
            timing.copy_ms
        );
    }

    fn tile_grid(&self, part_index: i32) -> Result<OpenExrCoreTileGrid, String> {
        let mut tile_width = 0_i32;
        let mut tile_height = 0_i32;
        exr_result(unsafe {
            sys::exr_get_tile_sizes(
                self.raw.cast_const(),
                part_index,
                0,
                0,
                &mut tile_width,
                &mut tile_height,
            )
        })?;
        let mut count_x = 0_i32;
        let mut count_y = 0_i32;
        exr_result(unsafe {
            sys::exr_get_tile_counts(
                self.raw.cast_const(),
                part_index,
                0,
                0,
                &mut count_x,
                &mut count_y,
            )
        })?;

        Ok(OpenExrCoreTileGrid {
            tile_width: u32::try_from(tile_width)
                .map_err(|_| "OpenEXRCore tile width is invalid".to_string())?,
            tile_height: u32::try_from(tile_height)
                .map_err(|_| "OpenEXRCore tile height is invalid".to_string())?,
            count_x: u32::try_from(count_x)
                .map_err(|_| "OpenEXRCore tile count x is invalid".to_string())?,
            count_y: u32::try_from(count_y)
                .map_err(|_| "OpenEXRCore tile count y is invalid".to_string())?,
        })
    }

    fn decode_chunk_to_tile(
        &self,
        part_index: i32,
        chunk: &sys::ExrChunkInfo,
        chunk_origin: (u32, u32),
        tile: (u32, u32, u32, u32),
        rgba: &mut [f32],
    ) -> Result<OpenExrCoreChunkDecodeTiming, String> {
        let fetched = self.fetch_decoded_chunk(part_index, chunk, chunk_origin)?;
        let copy_ms = copy_decoded_chunk_to_tile(&fetched.decoded, tile, rgba)?;

        Ok(OpenExrCoreChunkDecodeTiming {
            decode_ms: fetched.decode_ms,
            copy_ms,
            cache_hit: fetched.cache_hit,
        })
    }

    fn fetch_decoded_chunk(
        &self,
        part_index: i32,
        chunk: &sys::ExrChunkInfo,
        chunk_origin: (u32, u32),
    ) -> Result<OpenExrCoreDecodedChunkFetch, String> {
        let key = decoded_chunk_key(part_index, chunk, chunk_origin)?;
        let mut cache = self
            .decoded_chunks
            .lock()
            .map_err(|_| "OpenEXRCore decoded chunk cache mutex was poisoned".to_string())?;
        loop {
            if let Some(decoded) = cache.get(&key) {
                return Ok(OpenExrCoreDecodedChunkFetch {
                    decoded,
                    decode_ms: 0.0,
                    cache_hit: true,
                });
            }
            if cache.begin_decode(key) {
                break;
            }
            cache = self
                .decoded_chunk_ready
                .wait(cache)
                .map_err(|_| "OpenEXRCore decoded chunk cache mutex was poisoned".to_string())?;
        }
        drop(cache);

        let decode_result = self.decode_chunk_to_rgba(part_index, chunk, chunk_origin);
        let (decoded, decode_ms) = match decode_result {
            Ok(decoded) => decoded,
            Err(err) => {
                if let Ok(mut cache) = self.decoded_chunks.lock() {
                    cache.finish_decode(&key);
                }
                self.decoded_chunk_ready.notify_all();
                return Err(err);
            }
        };
        let mut cache = self
            .decoded_chunks
            .lock()
            .map_err(|_| "OpenEXRCore decoded chunk cache mutex was poisoned".to_string())?;
        cache.finish_decode(&key);
        cache.insert(key, Arc::clone(&decoded));
        drop(cache);
        self.decoded_chunk_ready.notify_all();

        Ok(OpenExrCoreDecodedChunkFetch {
            decoded,
            decode_ms,
            cache_hit: false,
        })
    }

    fn decode_chunk_to_rgba(
        &self,
        part_index: i32,
        chunk: &sys::ExrChunkInfo,
        chunk_origin: (u32, u32),
    ) -> Result<(Arc<OpenExrCoreDecodedChunk>, f64), String> {
        let chunk_width = usize::try_from(chunk.width)
            .map_err(|_| "OpenEXRCore chunk width is negative".to_string())?;
        let chunk_height = usize::try_from(chunk.height)
            .map_err(|_| "OpenEXRCore chunk height is negative".to_string())?;
        let sample_count = chunk_width
            .checked_mul(chunk_height)
            .ok_or_else(|| "OpenEXRCore chunk sample count overflowed".to_string())?;

        let mut pipeline = sys::ExrDecodePipeline::default();
        exr_result(unsafe {
            sys::exr_decoding_initialize(self.raw.cast_const(), part_index, chunk, &mut pipeline)
        })?;
        let _pipeline_guard = DecodePipelineGuard {
            context: self.raw.cast_const(),
            pipeline: &mut pipeline,
        };

        let (roles, buffers) = {
            let channels = decode_pipeline_channels(&mut pipeline)?;
            let roles: Vec<_> = channels
                .iter()
                .map(|channel| channel_name_to_role(channel.channel_name))
                .collect();
            let mut buffers = vec![Vec::<f32>::new(); channels.len()];
            for (index, channel) in channels.iter_mut().enumerate() {
                if roles[index].is_none() {
                    channel.decode_to_ptr = ptr::null_mut();
                    continue;
                }

                buffers[index] = vec![0.0_f32; sample_count];
                channel.user_bytes_per_element = 4;
                channel.user_data_type = sys::EXR_PIXEL_FLOAT;
                channel.user_pixel_stride = 4;
                channel.user_line_stride = i32::try_from(chunk_width * 4)
                    .map_err(|_| "OpenEXRCore chunk line stride exceeds i32".to_string())?;
                channel.decode_to_ptr = buffers[index].as_mut_ptr().cast::<u8>();
            }
            (roles, buffers)
        };

        exr_result(unsafe {
            sys::exr_decoding_choose_default_routines(
                self.raw.cast_const(),
                part_index,
                &mut pipeline,
            )
        })?;
        let decode_start = Instant::now();
        exr_result(unsafe {
            sys::exr_decoding_run(self.raw.cast_const(), part_index, &mut pipeline)
        })?;
        let decode_ms = decode_start.elapsed().as_secs_f64() * 1000.0;

        let mut rgba = vec![0.0_f32; sample_count * 4];
        for alpha in rgba.chunks_exact_mut(4).map(|pixel| &mut pixel[3]) {
            *alpha = 1.0;
        }
        for (channel_index, role) in roles.iter().enumerate() {
            let Some(role) = role else {
                continue;
            };
            let buffer = &buffers[channel_index];
            for sample_index in 0..sample_count {
                let dest = sample_index * 4;
                let sample = buffer[sample_index];
                match *role {
                    ChannelRole::Red => rgba[dest] = sample,
                    ChannelRole::Green => rgba[dest + 1] = sample,
                    ChannelRole::Blue => rgba[dest + 2] = sample,
                    ChannelRole::Luma => {
                        rgba[dest] = sample;
                        rgba[dest + 1] = sample;
                        rgba[dest + 2] = sample;
                    }
                    ChannelRole::Alpha => rgba[dest + 3] = sample,
                }
            }
        }
        let rgba = Arc::new(rgba);
        let byte_size = rgba.len() * std::mem::size_of::<f32>();

        Ok((
            Arc::new(OpenExrCoreDecodedChunk {
                origin: chunk_origin,
                width: u32::try_from(chunk_width)
                    .map_err(|_| "OpenEXRCore chunk width exceeds u32".to_string())?,
                height: u32::try_from(chunk_height)
                    .map_err(|_| "OpenEXRCore chunk height exceeds u32".to_string())?,
                rgba,
                byte_size,
            }),
            decode_ms,
        ))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct OpenExrCoreChunkDecodeTiming {
    decode_ms: f64,
    copy_ms: f64,
    cache_hit: bool,
}

#[derive(Clone, Debug)]
struct OpenExrCoreDecodedChunkFetch {
    decoded: Arc<OpenExrCoreDecodedChunk>,
    decode_ms: f64,
    cache_hit: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct OpenExrCoreTileGrid {
    tile_width: u32,
    tile_height: u32,
    count_x: u32,
    count_y: u32,
}

struct DecodePipelineGuard {
    context: sys::ExrConstContext,
    pipeline: *mut sys::ExrDecodePipeline,
}

impl Drop for DecodePipelineGuard {
    fn drop(&mut self) {
        let _ = unsafe { sys::exr_decoding_destroy(self.context, self.pipeline) };
    }
}

impl Drop for OpenExrCoreReadContext {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            let _ = unsafe { sys::exr_finish(&mut self.raw) };
        }
    }
}

fn extent_from_window_axis(min: i32, max: i32, label: &str) -> Result<u32, String> {
    let extent = i64::from(max) - i64::from(min) + 1;
    u32::try_from(extent).map_err(|_| format!("EXR data window {label} is invalid: {min}..={max}"))
}

fn validate_tile_bounds(
    image_width: u32,
    image_height: u32,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
) -> Result<(), String> {
    if width == 0 || height == 0 {
        return Err("EXR tile dimensions must be non-zero".to_string());
    }
    if x >= image_width || y >= image_height {
        return Err("EXR tile origin is outside the image".to_string());
    }
    if x.checked_add(width).is_none_or(|right| right > image_width)
        || y.checked_add(height)
            .is_none_or(|bottom| bottom > image_height)
    {
        return Err("EXR tile bounds exceed image dimensions".to_string());
    }
    Ok(())
}

fn decoded_chunk_key(
    part_index: i32,
    chunk: &sys::ExrChunkInfo,
    origin: (u32, u32),
) -> Result<OpenExrCoreDecodedChunkKey, String> {
    let width = u32::try_from(chunk.width)
        .map_err(|_| "OpenEXRCore chunk width is negative".to_string())?;
    let height = u32::try_from(chunk.height)
        .map_err(|_| "OpenEXRCore chunk height is negative".to_string())?;
    Ok(OpenExrCoreDecodedChunkKey {
        part_index,
        chunk_index: chunk.idx,
        origin,
        size: (width, height),
    })
}

fn copy_decoded_chunk_to_tile(
    decoded: &OpenExrCoreDecodedChunk,
    tile: (u32, u32, u32, u32),
    rgba: &mut [f32],
) -> Result<f64, String> {
    let (tile_x, tile_y, tile_width, tile_height) = tile;
    let tile_right = tile_x + tile_width;
    let tile_bottom = tile_y + tile_height;
    let (chunk_x, chunk_y) = decoded.origin;
    let copy_start_x = chunk_x.max(tile_x);
    let copy_end_x = (chunk_x + decoded.width).min(tile_right);
    let copy_start_y = chunk_y.max(tile_y);
    let copy_end_y = (chunk_y + decoded.height).min(tile_bottom);

    if copy_start_x >= copy_end_x || copy_start_y >= copy_end_y {
        return Ok(0.0);
    }

    let expected_len = tile_width as usize * tile_height as usize * 4;
    if rgba.len() != expected_len {
        return Err("EXR destination tile buffer has unexpected length".to_string());
    }

    let copy_start = Instant::now();
    let copy_width = (copy_end_x - copy_start_x) as usize;
    let chunk_width = decoded.width as usize;
    let tile_width = tile_width as usize;
    for source_y in copy_start_y..copy_end_y {
        let src_row = (source_y - chunk_y) as usize;
        let dst_row = (source_y - tile_y) as usize;
        let src_col = (copy_start_x - chunk_x) as usize;
        let dst_col = (copy_start_x - tile_x) as usize;
        let src_start = (src_row * chunk_width + src_col) * 4;
        let src_end = src_start + copy_width * 4;
        let dst_start = (dst_row * tile_width + dst_col) * 4;
        let dst_end = dst_start + copy_width * 4;
        rgba[dst_start..dst_end].copy_from_slice(&decoded.rgba[src_start..src_end]);
    }

    Ok(copy_start.elapsed().as_secs_f64() * 1000.0)
}

fn sample_decoded_scanline_chunk_into_preview(
    decoded: &OpenExrCoreDecodedChunk,
    source_width: u32,
    preview_width: u32,
    preview_height: u32,
    preview_rows: &[(u32, u32)],
    rgba: &mut [f32],
) -> Result<(), String> {
    let expected_len = preview_width as usize * preview_height as usize * 4;
    if rgba.len() != expected_len {
        return Err("EXR preview buffer has unexpected length".to_string());
    }

    let (chunk_x, chunk_y) = decoded.origin;
    let chunk_right = chunk_x + decoded.width;
    let chunk_bottom = chunk_y + decoded.height;
    for &(preview_y, source_y) in preview_rows {
        if preview_y >= preview_height {
            return Err("EXR preview row is outside preview bounds".to_string());
        }
        if source_y < chunk_y || source_y >= chunk_bottom {
            continue;
        }
        let source_row = (source_y - chunk_y) as usize;
        for preview_x in 0..preview_width {
            let source_x =
                crate::hdr::tiled::preview_sample_coord(preview_x, preview_width, source_width);
            if source_x < chunk_x || source_x >= chunk_right {
                continue;
            }
            let source_col = (source_x - chunk_x) as usize;
            let source_offset = (source_row * decoded.width as usize + source_col) * 4;
            let dest_offset =
                (preview_y as usize * preview_width as usize + preview_x as usize) * 4;
            rgba[dest_offset..dest_offset + 4]
                .copy_from_slice(&decoded.rgba[source_offset..source_offset + 4]);
        }
    }
    Ok(())
}

fn scanline_preview_dimensions(
    source_width: u32,
    source_height: u32,
    requested_max_w: u32,
    requested_max_h: u32,
) -> (u32, u32) {
    let (max_w, max_h) = if requested_max_w <= crate::constants::DEFAULT_PREVIEW_SIZE
        && requested_max_h <= crate::constants::DEFAULT_PREVIEW_SIZE
    {
        (
            SCANLINE_BOOTSTRAP_PREVIEW_MAX_SIDE,
            SCANLINE_BOOTSTRAP_PREVIEW_MAX_SIDE,
        )
    } else {
        (requested_max_w, requested_max_h)
    };
    crate::hdr::tiled::preview_dimensions(source_width, source_height, max_w, max_h)
}

fn scanline_preview_source_row_budget(requested_preview_width: u32) -> u32 {
    if requested_preview_width <= crate::constants::DEFAULT_PREVIEW_SIZE {
        SCANLINE_BOOTSTRAP_PREVIEW_SOURCE_ROW_BUDGET
    } else {
        SCANLINE_REFINED_PREVIEW_SOURCE_ROW_BUDGET
    }
}

fn scanline_preview_decode_parallelism(unique_chunks: usize) -> usize {
    unique_chunks.clamp(1, SCANLINE_PREVIEW_MAX_PARALLEL_CHUNKS)
}

fn budgeted_scanline_preview_source_y(
    preview_y: u32,
    preview_height: u32,
    source_height: u32,
    max_source_rows: u32,
) -> u32 {
    if preview_height == 0 || source_height == 0 {
        return 0;
    }
    if max_source_rows == 0 || preview_height <= max_source_rows {
        return crate::hdr::tiled::preview_sample_coord(preview_y, preview_height, source_height);
    }

    let bucket = (u64::from(preview_y) * u64::from(max_source_rows) / u64::from(preview_height))
        .min(u64::from(max_source_rows - 1));
    let bucket_start = bucket * u64::from(preview_height) / u64::from(max_source_rows);
    let bucket_end = ((bucket + 1) * u64::from(preview_height) / u64::from(max_source_rows))
        .min(u64::from(preview_height))
        .max(bucket_start + 1);
    let representative_preview_y = ((bucket_start + bucket_end - 1) / 2) as u32;
    crate::hdr::tiled::preview_sample_coord(representative_preview_y, preview_height, source_height)
}

fn decode_pipeline_channels(
    pipeline: &mut sys::ExrDecodePipeline,
) -> Result<&mut [sys::ExrCodingChannelInfo], String> {
    let count = usize::try_from(pipeline.channel_count)
        .map_err(|_| "OpenEXRCore reported a negative decode channel count".to_string())?;
    if count == 0 {
        return Ok(&mut []);
    }
    if pipeline.channels.is_null() {
        return Err("OpenEXRCore returned null decode channel info".to_string());
    }
    Ok(unsafe { std::slice::from_raw_parts_mut(pipeline.channels, count) })
}

#[cfg(feature = "tile-debug")]
fn storage_name(storage: i32) -> &'static str {
    match storage {
        sys::EXR_STORAGE_SCANLINE => "scanline",
        sys::EXR_STORAGE_TILED => "tiled",
        2 => "deep-scanline",
        3 => "deep-tiled",
        _ => "unknown",
    }
}

#[cfg(feature = "tile-debug")]
fn compression_name(compression: u8) -> &'static str {
    match compression {
        0 => "none",
        1 => "rle",
        2 => "zips",
        3 => "zip",
        4 => "piz",
        5 => "pxr24",
        6 => "b44",
        7 => "b44a",
        8 => "dwaa",
        9 => "dwab",
        10 => "htj2k256",
        11 => "htj2k32",
        _ => "unknown",
    }
}

fn channel_name_to_role(name: *const c_char) -> Option<ChannelRole> {
    if name.is_null() {
        return None;
    }
    let name = unsafe { CStr::from_ptr(name) }.to_string_lossy();
    if name.eq_ignore_ascii_case("R") {
        Some(ChannelRole::Red)
    } else if name.eq_ignore_ascii_case("G") {
        Some(ChannelRole::Green)
    } else if name.eq_ignore_ascii_case("B") {
        Some(ChannelRole::Blue)
    } else if name.eq_ignore_ascii_case("Y") {
        Some(ChannelRole::Luma)
    } else if name.eq_ignore_ascii_case("A") {
        Some(ChannelRole::Alpha)
    } else {
        None
    }
}

fn copy_channels(chlist: *const sys::ExrAttrChlist) -> Result<Vec<OpenExrCoreChannelInfo>, String> {
    if chlist.is_null() {
        return Err("OpenEXRCore returned a null channel list".to_string());
    }

    let chlist = unsafe { &*chlist };
    let count = usize::try_from(chlist.num_channels)
        .map_err(|_| "OpenEXRCore reported a negative channel count".to_string())?;
    if count == 0 {
        return Ok(Vec::new());
    }
    if chlist.entries.is_null() {
        return Err("OpenEXRCore returned null channel entries".to_string());
    }

    let entries = unsafe { std::slice::from_raw_parts(chlist.entries, count) };
    entries
        .iter()
        .map(|entry| {
            let name = exr_attr_string_to_string(entry.name)?;
            Ok(OpenExrCoreChannelInfo {
                name,
                pixel_type: entry.pixel_type,
                x_sampling: entry.x_sampling,
                y_sampling: entry.y_sampling,
            })
        })
        .collect()
}

fn exr_attr_string_to_string(value: sys::ExrAttrString) -> Result<String, String> {
    if value.str_.is_null() {
        return Ok(String::new());
    }
    let len = usize::try_from(value.length)
        .map_err(|_| "OpenEXRCore returned a negative string length".to_string())?;
    let bytes = unsafe { std::slice::from_raw_parts(value.str_.cast::<u8>(), len) };
    String::from_utf8(bytes.to_vec()).map_err(|err| err.to_string())
}

fn configured_decoded_chunk_cache_max_bytes() -> usize {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    decoded_chunk_cache_budget_for_memory(sys.total_memory() as usize)
}

fn decoded_chunk_cache_budget_for_memory(total_memory_bytes: usize) -> usize {
    (total_memory_bytes / 16).clamp(
        DEFAULT_DECODED_CHUNK_CACHE_BYTES,
        MAX_DECODED_CHUNK_CACHE_BYTES,
    )
}

fn exr_result(result: sys::ExrResult) -> Result<(), String> {
    if result == sys::EXR_ERR_SUCCESS {
        return Ok(());
    }
    let message = unsafe {
        let ptr = sys::exr_get_default_error_message(result);
        if ptr.is_null() {
            "unknown OpenEXRCore error".to_string()
        } else {
            CStr::from_ptr(ptr).to_string_lossy().into_owned()
        }
    };
    Err(format!("OpenEXRCore error {result}: {message}"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn decoded_chunk_cache_reuses_native_chunk_across_horizontal_tiles() {
        let key = super::OpenExrCoreDecodedChunkKey {
            part_index: 0,
            chunk_index: 7,
            origin: (0, 6144),
            size: (24576, 32),
        };
        let chunk = std::sync::Arc::new(super::OpenExrCoreDecodedChunk {
            origin: (0, 6144),
            width: 24576,
            height: 32,
            rgba: std::sync::Arc::new(vec![0.0; 4]),
            byte_size: 16,
        });
        let mut cache = super::OpenExrCoreDecodedChunkCache::new(64);

        assert!(cache.get(&key).is_none());
        cache.insert(key, std::sync::Arc::clone(&chunk));
        let cached = cache.get(&key).expect("chunk should be cached");

        assert!(std::sync::Arc::ptr_eq(&cached, &chunk));
        assert_eq!(cache.miss_count(), 1);
        assert_eq!(cache.hit_count(), 1);
    }

    #[test]
    fn decoded_chunk_cache_tracks_in_flight_native_chunk_decode() {
        let key = super::OpenExrCoreDecodedChunkKey {
            part_index: 0,
            chunk_index: 7,
            origin: (0, 6144),
            size: (24576, 32),
        };
        let mut cache = super::OpenExrCoreDecodedChunkCache::new(64);

        assert!(cache.begin_decode(key));
        assert!(!cache.begin_decode(key));
        cache.finish_decode(&key);
        assert!(cache.begin_decode(key));
    }

    #[test]
    fn decoded_chunk_cache_budget_scales_with_physical_memory() {
        let gib = 1024 * 1024 * 1024;

        assert_eq!(
            super::decoded_chunk_cache_budget_for_memory(4 * gib),
            512 * 1024 * 1024
        );
        assert_eq!(
            super::decoded_chunk_cache_budget_for_memory(32 * gib),
            2 * gib
        );
        assert_eq!(
            super::decoded_chunk_cache_budget_for_memory(128 * gib),
            4 * gib
        );
    }

    #[test]
    fn decoded_scanline_chunk_samples_nearest_preview_pixels_directly() {
        let decoded = super::OpenExrCoreDecodedChunk {
            origin: (0, 8),
            width: 4,
            height: 2,
            rgba: std::sync::Arc::new(vec![
                0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0, 1.0, 2.0, 2.0, 2.0, 1.0, 3.0, 3.0, 3.0, 1.0,
                4.0, 4.0, 4.0, 1.0, 5.0, 5.0, 5.0, 1.0, 6.0, 6.0, 6.0, 1.0, 7.0, 7.0, 7.0, 1.0,
            ]),
            byte_size: 32 * std::mem::size_of::<f32>(),
        };
        let mut preview = vec![0.0; 2 * 2 * 4];

        super::sample_decoded_scanline_chunk_into_preview(
            &decoded,
            4,
            2,
            2,
            &[(0, 8), (1, 9)],
            &mut preview,
        )
        .expect("sample preview from decoded chunk");

        assert_eq!(
            preview,
            vec![
                0.0, 0.0, 0.0, 1.0, 3.0, 3.0, 3.0, 1.0, 4.0, 4.0, 4.0, 1.0, 7.0, 7.0, 7.0, 1.0,
            ]
        );
    }

    #[test]
    fn budgeted_scanline_preview_sampling_limits_unique_source_rows() {
        let preview_height = 1024;
        let source_height = 12_288;
        let max_rows = super::SCANLINE_BOOTSTRAP_PREVIEW_SOURCE_ROW_BUDGET;
        let sampled = (0..preview_height)
            .map(|preview_y| {
                super::budgeted_scanline_preview_source_y(
                    preview_y,
                    preview_height,
                    source_height,
                    max_rows,
                )
            })
            .collect::<std::collections::BTreeSet<_>>();

        assert!(sampled.len() <= max_rows as usize);
        assert!(sampled.iter().all(|source_y| *source_y < source_height));
    }

    #[test]
    fn scanline_bootstrap_preview_uses_exr_specific_quality_floor() {
        let source_width = 24_576;
        let source_height = 12_288;

        let scanline_preview = super::scanline_preview_dimensions(
            source_width,
            source_height,
            crate::constants::DEFAULT_PREVIEW_SIZE,
            crate::constants::DEFAULT_PREVIEW_SIZE,
        );
        let standard_preview = crate::hdr::tiled::preview_dimensions(
            source_width,
            source_height,
            crate::constants::DEFAULT_PREVIEW_SIZE,
            crate::constants::DEFAULT_PREVIEW_SIZE,
        );

        assert_eq!(scanline_preview, (1024, 512));
        assert_eq!(standard_preview, (512, 256));
    }

    #[test]
    fn scanline_refined_preview_samples_all_preview_rows() {
        let preview_height = 1024;
        let source_height = 12_288;
        let sampled = (0..preview_height)
            .map(|preview_y| {
                super::budgeted_scanline_preview_source_y(
                    preview_y,
                    preview_height,
                    source_height,
                    super::SCANLINE_REFINED_PREVIEW_SOURCE_ROW_BUDGET,
                )
            })
            .collect::<std::collections::BTreeSet<_>>();

        assert_eq!(super::SCANLINE_REFINED_PREVIEW_SOURCE_ROW_BUDGET, 0);
        assert_eq!(sampled.len(), preview_height as usize);
    }

    #[test]
    fn scanline_preview_decode_parallelism_is_bounded() {
        assert_eq!(super::scanline_preview_decode_parallelism(0), 1);
        assert_eq!(super::scanline_preview_decode_parallelism(1), 1);
        assert_eq!(super::scanline_preview_decode_parallelism(2), 2);
        assert_eq!(
            super::scanline_preview_decode_parallelism(384),
            super::SCANLINE_PREVIEW_MAX_PARALLEL_CHUNKS
        );
    }
}
