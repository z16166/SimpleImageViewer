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

use memmap2::Mmap;
use parking_lot::{Condvar, Mutex};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::Arc;
use std::time::Instant;

use openexr_core_sys as sys;

type ScanlinePreviewChunkJob = (i32, sys::ExrChunkInfo, (u32, u32), (u32, u32));
type ScanlinePreviewRowsByChunk =
    std::collections::BTreeMap<i32, (sys::ExrChunkInfo, (u32, u32), Vec<(u32, u32)>)>;

use super::channels::{
    DecodePipelineGuard, OpenExrCoreChannelChunkLayout, OpenExrCoreDecodedChunkFetch,
    OpenExrCoreTileGrid, assign_channel_roles, budgeted_scanline_preview_source_y,
    channel_sample_f32, channel_sample_f32_filtered, configured_decoded_chunk_cache_max_bytes,
    copy_channels, copy_decoded_chunk_to_tile, decode_pipeline_channels, decoded_chunk_key,
    exr_result, extent_from_window_axis, sample_decoded_scanline_chunk_into_preview,
    scanline_preview_decode_parallelism, scanline_preview_dimensions,
    scanline_preview_source_row_budget, validate_tile_bounds,
};
#[cfg(feature = "tile-debug")]
use super::channels::{OpenExrCoreChunkDecodeTiming, compression_name, storage_name};
use super::chromaticities::{
    hdr_color_space_from_chromaticities_xy, imf_exr_chromaticities_from_path,
    openexr_luminance_weights_from_chromaticities_xy,
};
use super::mmap::{ExrMmapCookieGuard, openexr_file_initializer, openexr_memory_map_initializer};
use super::types::{
    ChannelRole, OpenExrCoreDecodedChunk, OpenExrCoreDecodedChunkCache, OpenExrCorePartInfo,
    OpenExrCoreRgbaTile,
};

#[derive(Debug)]
pub(crate) struct OpenExrCoreReadContext {
    pub(crate) path: PathBuf,
    pub(crate) raw: sys::ExrContext,
    pub(crate) part_count: usize,
    /// `Y = w[0]*R + w[1]*G + w[2]*B` weights from `chromaticities` (OpenEXR `computeYw`), else Rec.709.
    pub(crate) exr_luma_weights: [f32; 3],
    pub(crate) decoded_chunks: Mutex<OpenExrCoreDecodedChunkCache>,
    pub(crate) decoded_chunk_ready: Condvar,
}

// OpenEXRCore read contexts parse headers up front and are documented as safe
// for concurrent chunk requests when each thread uses its own decode pipeline.
unsafe impl Send for OpenExrCoreReadContext {}
unsafe impl Sync for OpenExrCoreReadContext {}
impl OpenExrCoreReadContext {
    pub(crate) fn open(path: &Path) -> Result<Self, String> {
        match crate::mmap_util::map_file(path) {
            Ok((m, _)) => Self::open_from_mmap(path, Arc::new(m)),
            Err(map_err) => {
                log::debug!(
                    "EXR mmap unavailable ({}); using file-handle reader for {}",
                    map_err,
                    path.display()
                );
                Self::open_via_file_handle(path)
            }
        }
    }

    /// Open from caller-provided mmap (avoids a second file map per checklist #29).
    pub(crate) fn open_from_mmap(path: &Path, mmap: Arc<Mmap>) -> Result<Self, String> {
        let raw = Self::start_mmap_read(path, mmap)?;
        Self::finish_open(path, raw)
    }

    fn start_mmap_read(path: &Path, mmap: Arc<Mmap>) -> Result<sys::ExrContext, String> {
        let filename = sys::imf_io::path_utf8_cstr(path)
            .map_err(|err| format!("{err} for {}", path.display()))?;

        let mut raw = ptr::null_mut();
        let mut cookie = ExrMmapCookieGuard::from_shared(mmap);
        let ctxt_init = openexr_memory_map_initializer(cookie.as_mut_ptr());

        let start_result = unsafe { sys::exr_start_read(&mut raw, filename.as_ptr(), &ctxt_init) };

        match exr_result(start_result) {
            Ok(()) => {
                if raw.is_null() {
                    return Err(format!(
                        "OpenEXRCore returned a null context for {}",
                        path.display()
                    ));
                }
                cookie.mark_context_alive();
                Ok(raw)
            }
            Err(e) => {
                // Do not fall back to file I/O here. The mmap reader already classified the
                // file and malformed EXR-like inputs previously crashed on Windows when this
                // error path retried through OpenEXRCore's native file reader.
                log::debug!(
                    "EXR mmap read via OpenEXRCore failed ({}) for {}",
                    e,
                    path.display()
                );
                Err(e)
            }
        }
    }

    fn open_via_file_handle(path: &Path) -> Result<Self, String> {
        let filename = sys::imf_io::path_utf8_cstr(path)
            .map_err(|err| format!("{err} for {}", path.display()))?;

        let mut raw = ptr::null_mut();
        let ctxt_init = openexr_file_initializer();
        exr_result(unsafe { sys::exr_start_read(&mut raw, filename.as_ptr(), &ctxt_init) })?;
        if raw.is_null() {
            return Err(format!(
                "OpenEXRCore returned a null context for {}",
                path.display()
            ));
        }
        Self::finish_open(path, raw)
    }

    fn finish_open(path: &Path, mut raw: sys::ExrContext) -> Result<Self, String> {
        let mut part_count = 0;
        if let Err(err) =
            exr_result(unsafe { sys::exr_get_count(raw.cast_const(), &mut part_count) })
        {
            let _ = unsafe { sys::exr_finish(&mut raw) };
            return Err(err);
        }
        let part_count = usize::try_from(part_count)
            .map_err(|_| "OpenEXRCore reported a negative part count".to_string())?;

        let exr_luma_weights = imf_exr_chromaticities_from_path(path)
            .as_ref()
            .and_then(openexr_luminance_weights_from_chromaticities_xy)
            .unwrap_or([0.2126_f32, 0.7152_f32, 0.0722_f32]);

        Ok(Self {
            path: path.to_path_buf(),
            raw,
            part_count,
            exr_luma_weights,
            decoded_chunks: Mutex::new(OpenExrCoreDecodedChunkCache::new(
                configured_decoded_chunk_cache_max_bytes(),
            )),
            decoded_chunk_ready: Condvar::new(),
        })
    }

    pub(crate) fn infer_exr_display_color_space_for_path(
        path: &Path,
    ) -> crate::hdr::types::HdrColorSpace {
        match imf_exr_chromaticities_from_path(path) {
            Some(ch) => hdr_color_space_from_chromaticities_xy(&ch),
            None => crate::hdr::types::HdrColorSpace::LinearSrgb,
        }
    }

    pub(crate) fn part_count(&self) -> usize {
        self.part_count
    }

    #[cfg_attr(not(feature = "tile-debug"), allow(dead_code))]
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
        cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<OpenExrCoreRgbaTile, String> {
        crate::loader::check_decode_cancel_str(cancel)?;
        let part = self.part(part_index)?;
        validate_tile_bounds(part.width, part.height, x, y, width, height)?;
        #[cfg(feature = "tile-debug")]
        let tile_start = Instant::now();
        #[cfg(feature = "tile-debug")]
        let mut decoded_chunk_count = 0_u32;

        let part_index =
            i32::try_from(part_index).map_err(|_| "EXR part index exceeds i32".to_string())?;
        let buf_len = crate::constants::checked_rgba_buffer_len(width as usize, height as usize)
            .ok_or_else(|| {
                format!("OpenEXR scanline tile buffer size overflow for {width}x{height}")
            })?;
        let mut rgba = vec![0.0_f32; buf_len];
        for alpha in rgba.chunks_exact_mut(4).map(|pixel| &mut pixel[3]) {
            *alpha = 1.0;
        }
        const CANCEL_POLL_CHUNKS: usize = 8;

        match part.storage {
            sys::EXR_STORAGE_SCANLINE => {
                use rayon::prelude::*;

                let mut decoded_starts = std::collections::BTreeSet::new();
                let mut chunk_work = Vec::<(sys::ExrChunkInfo, u32, u32)>::new();
                for (row_i, source_y) in (y..y + height).enumerate() {
                    if row_i % CANCEL_POLL_CHUNKS == 0 {
                        crate::loader::check_decode_cancel_str(cancel)?;
                    }
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
                    chunk_work.push((chunk, chunk_origin_x, chunk_origin_y));
                }

                crate::loader::check_decode_cancel_str(cancel)?;
                let fetched = chunk_work
                    .par_iter()
                    .map(|(chunk, ox, oy)| self.fetch_decoded_chunk(part_index, chunk, (*ox, *oy)))
                    .collect::<Result<Vec<_>, String>>()?;

                let tile_rect = (x, y, width, height);
                for (i, fetch) in fetched.iter().enumerate() {
                    if i % CANCEL_POLL_CHUNKS == 0 {
                        crate::loader::check_decode_cancel_str(cancel)?;
                    }
                    let copy_ms = copy_decoded_chunk_to_tile(&fetch.decoded, tile_rect, &mut rgba)?;
                    #[cfg(feature = "tile-debug")]
                    {
                        let (chunk, chunk_origin_x, chunk_origin_y) = &chunk_work[i];
                        decoded_chunk_count += 1;
                        self.log_tile_chunk_decode(
                            part_index,
                            &part,
                            chunk,
                            (*chunk_origin_x, *chunk_origin_y),
                            tile_rect,
                            OpenExrCoreChunkDecodeTiming {
                                decode_ms: fetch.decode_ms,
                                copy_ms,
                                cache_hit: fetch.cache_hit,
                            },
                        );
                    }
                    #[cfg(not(feature = "tile-debug"))]
                    let _ = copy_ms;
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
                use rayon::prelude::*;

                let start_tile_x = x / tile_grid.tile_width;
                let end_tile_x = (x + width - 1) / tile_grid.tile_width;
                let start_tile_y = y / tile_grid.tile_height;
                let end_tile_y = (y + height - 1) / tile_grid.tile_height;

                let mut chunk_work = Vec::<(sys::ExrChunkInfo, u32, u32)>::new();
                let mut tile_i = 0usize;
                for tile_y_index in start_tile_y..=end_tile_y {
                    for tile_x_index in start_tile_x..=end_tile_x {
                        if tile_i % CANCEL_POLL_CHUNKS == 0 {
                            crate::loader::check_decode_cancel_str(cancel)?;
                        }
                        tile_i += 1;
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
                        let chunk_origin_x = tile_x_index * tile_grid.tile_width;
                        let chunk_origin_y = tile_y_index * tile_grid.tile_height;
                        chunk_work.push((chunk, chunk_origin_x, chunk_origin_y));
                    }
                }

                crate::loader::check_decode_cancel_str(cancel)?;
                let fetched = chunk_work
                    .par_iter()
                    .map(|(chunk, ox, oy)| self.fetch_decoded_chunk(part_index, chunk, (*ox, *oy)))
                    .collect::<Result<Vec<_>, String>>()?;

                let tile_rect = (x, y, width, height);
                for (i, fetch) in fetched.iter().enumerate() {
                    if i % CANCEL_POLL_CHUNKS == 0 {
                        crate::loader::check_decode_cancel_str(cancel)?;
                    }
                    let copy_ms = copy_decoded_chunk_to_tile(&fetch.decoded, tile_rect, &mut rgba)?;
                    #[cfg(feature = "tile-debug")]
                    {
                        let (chunk, chunk_origin_x, chunk_origin_y) = &chunk_work[i];
                        decoded_chunk_count += 1;
                        self.log_tile_chunk_decode(
                            part_index,
                            &part,
                            chunk,
                            (*chunk_origin_x, *chunk_origin_y),
                            tile_rect,
                            OpenExrCoreChunkDecodeTiming {
                                decode_ms: fetch.decode_ms,
                                copy_ms,
                                cache_hit: fetch.cache_hit,
                            },
                        );
                    }
                    #[cfg(not(feature = "tile-debug"))]
                    let _ = copy_ms;
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
        #[cfg(feature = "tile-debug")]
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
        let source_row_budget = scanline_preview_source_row_budget(max_w);
        use rayon::prelude::*;
        let chunk_jobs = (0..height)
            .into_par_iter()
            .map(
                |preview_y| -> Result<Option<ScanlinePreviewChunkJob>, String> {
                    let source_y = budgeted_scanline_preview_source_y(
                        preview_y,
                        height,
                        part.height,
                        source_row_budget,
                    );
                    let mut chunk = sys::ExrChunkInfo::default();
                    let res = unsafe {
                        sys::exr_read_scanline_chunk_info(
                            self.raw.cast_const(),
                            part_index,
                            i32::try_from(source_y)
                                .map_err(|_| "EXR scanline y exceeds i32".to_string())?
                                + part.data_window_min.1,
                            &mut chunk,
                        )
                    };
                    if res != 0 {
                        return Err(format!(
                            "OpenEXRCore failed to read scanline chunk info at y={source_y}: {res}"
                        ));
                    }
                    if chunk.height <= 0 || chunk.width <= 0 {
                        return Ok(None);
                    }

                    let chunk_origin = (
                        u32::try_from(chunk.start_x - part.data_window_min.0).map_err(|_| {
                            "OpenEXRCore chunk start_x is outside data window".to_string()
                        })?,
                        u32::try_from(chunk.start_y - part.data_window_min.1).map_err(|_| {
                            "OpenEXRCore chunk start_y is outside data window".to_string()
                        })?,
                    );
                    Ok(Some((
                        chunk.start_y,
                        chunk,
                        chunk_origin,
                        (preview_y, source_y),
                    )))
                },
            )
            .collect::<Result<Vec<_>, String>>()?;

        let chunk_jobs: Vec<ScanlinePreviewChunkJob> = chunk_jobs.into_iter().flatten().collect();

        let mut rows_by_chunk = ScanlinePreviewRowsByChunk::new();
        for (start_y, chunk, chunk_origin, row) in chunk_jobs {
            let entry = rows_by_chunk
                .entry(start_y)
                .or_insert_with(|| (chunk, chunk_origin, Vec::new()));
            entry.2.push(row);
        }

        let buf_len = crate::constants::checked_rgba_buffer_len(width as usize, height as usize)
            .ok_or_else(|| {
                format!("OpenEXR scanline preview buffer size overflow for {width}x{height}")
            })?;
        let mut rgba = vec![0.0_f32; buf_len];
        for alpha in rgba.chunks_exact_mut(4).map(|pixel| &mut pixel[3]) {
            *alpha = 1.0;
        }
        let unique_chunks = rows_by_chunk.len();
        let mut cache_hits = 0usize;
        let mut cache_misses = 0usize;
        #[cfg(feature = "tile-debug")]
        let mut decode_ms = 0.0_f64;
        #[cfg(feature = "tile-debug")]
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
                #[cfg(feature = "tile-debug")]
                {
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
                #[cfg(not(feature = "tile-debug"))]
                sample_decoded_scanline_chunk_into_preview(
                    &fetched.decoded,
                    part.width,
                    width,
                    height,
                    rows,
                    &mut rgba,
                )?;
            }
        }

        #[cfg(not(feature = "tile-debug"))]
        let _ = (cache_hits, cache_misses);

        #[cfg(feature = "tile-debug")]
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
        self.tile_grid_at_level(part_index, 0, 0)
    }

    pub(crate) fn tile_grid_at_level(
        &self,
        part_index: i32,
        level_x: i32,
        level_y: i32,
    ) -> Result<OpenExrCoreTileGrid, String> {
        let mut tile_width = 0_i32;
        let mut tile_height = 0_i32;
        exr_result(unsafe {
            sys::exr_get_tile_sizes(
                self.raw.cast_const(),
                part_index,
                level_x,
                level_y,
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
                level_x,
                level_y,
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

    /// Decode a tiled OpenEXR part at the best mip/ripmap level for `max_w` x `max_h`.
    pub(crate) fn extract_tiled_mip_rgba32f_preview(
        &self,
        part_index: usize,
        max_w: u32,
        max_h: u32,
    ) -> Result<OpenExrCoreRgbaTile, String> {
        use super::mip_preview::{
            exr_mip_level_dimensions, exr_mip_level_tile_grid_valid, probe_exr_max_mipmap_level,
            probe_exr_max_ripmap_levels, select_exr_mip_level_for_max_side,
        };

        let part = self.part(part_index)?;
        if part.storage != sys::EXR_STORAGE_TILED {
            return Err("OpenEXRCore mip preview requires tiled storage".to_string());
        }
        let part_index_i32 =
            i32::try_from(part_index).map_err(|_| "EXR part index exceeds i32".to_string())?;

        let max_mipmap = probe_exr_max_mipmap_level(self, part_index_i32);
        let (max_rip_x, max_rip_y) = probe_exr_max_ripmap_levels(self, part_index_i32);
        if max_mipmap == 0 && max_rip_x == 0 && max_rip_y == 0 {
            return Err("OpenEXRCore file has no mip levels above 0".to_string());
        }

        let max_side = max_w.max(max_h);
        let selection = select_exr_mip_level_for_max_side(
            part.width,
            part.height,
            max_side,
            max_mipmap,
            max_rip_x,
            max_rip_y,
            |level_x, level_y| {
                exr_mip_level_tile_grid_valid(self, part_index_i32, level_x as i32, level_y as i32)
            },
        );
        if selection.level_x == 0 && selection.level_y == 0 {
            return Err("OpenEXRCore mip preview selected level 0".to_string());
        }

        let (level_width, level_height) = exr_mip_level_dimensions(
            part.width,
            part.height,
            selection.level_x as u32,
            selection.level_y as u32,
        );
        let tile_grid =
            self.tile_grid_at_level(part_index_i32, selection.level_x, selection.level_y)?;
        let width = (tile_grid.count_x * tile_grid.tile_width).min(level_width);
        let height = (tile_grid.count_y * tile_grid.tile_height).min(level_height);
        if width == 0 || height == 0 {
            return Err("OpenEXRCore mip level dimensions are zero".to_string());
        }

        let buf_len = crate::constants::checked_rgba_buffer_len(width as usize, height as usize)
            .ok_or_else(|| {
                format!("OpenEXR mip level buffer size overflow for {width}x{height}")
            })?;
        let mut rgba = vec![0.0_f32; buf_len];
        let tile_rect = (0, 0, width, height);
        for tile_y_index in 0..tile_grid.count_y {
            for tile_x_index in 0..tile_grid.count_x {
                let mut chunk = sys::ExrChunkInfo::default();
                exr_result(unsafe {
                    sys::exr_read_tile_chunk_info(
                        self.raw.cast_const(),
                        part_index_i32,
                        i32::try_from(tile_x_index)
                            .map_err(|_| "EXR tile x index exceeds i32".to_string())?,
                        i32::try_from(tile_y_index)
                            .map_err(|_| "EXR tile y index exceeds i32".to_string())?,
                        selection.level_x,
                        selection.level_y,
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
                let fetched = self.fetch_decoded_chunk(part_index_i32, &chunk, chunk_origin)?;
                copy_decoded_chunk_to_tile(&fetched.decoded, tile_rect, &mut rgba)?;
            }
        }

        let (out_w, out_h) = crate::hdr::tiled::preview_dimensions(width, height, max_w, max_h);
        if out_w == width && out_h == height {
            return Ok(OpenExrCoreRgbaTile {
                width,
                height,
                rgba,
            });
        }

        let buf_len = crate::constants::checked_rgba_buffer_len(out_w as usize, out_h as usize)
            .ok_or_else(|| {
                format!("OpenEXR mip preview buffer size overflow for {out_w}x{out_h}")
            })?;
        let mut preview = vec![0.0_f32; buf_len];
        let width_usize = width as usize;
        let out_w_usize = out_w as usize;
        for preview_y in 0..out_h {
            let source_y =
                crate::hdr::tiled::preview_sample_coord(preview_y, out_h, height) as usize;
            let src_y_offset = source_y
                .checked_mul(width_usize)
                .and_then(|p| p.checked_mul(4))
                .ok_or_else(|| {
                    format!(
                        "EXR mip preview src row index overflow: source_y={source_y} width={width}"
                    )
                })?;
            let dst_y_offset = (preview_y as usize)
                .checked_mul(out_w_usize)
                .and_then(|p| p.checked_mul(4))
                .ok_or_else(|| {
                    format!(
                        "EXR mip preview dst row index overflow: preview_y={preview_y} out_w={out_w}"
                    )
                })?;
            for preview_x in 0..out_w {
                let source_x =
                    crate::hdr::tiled::preview_sample_coord(preview_x, out_w, width) as usize;
                let src = src_y_offset + source_x * 4;
                let dst = dst_y_offset + (preview_x as usize) * 4;
                preview[dst..dst + 4].copy_from_slice(&rgba[src..src + 4]);
            }
        }

        Ok(OpenExrCoreRgbaTile {
            width: out_w,
            height: out_h,
            rgba: preview,
        })
    }

    fn fetch_decoded_chunk(
        &self,
        part_index: i32,
        chunk: &sys::ExrChunkInfo,
        chunk_origin: (u32, u32),
    ) -> Result<OpenExrCoreDecodedChunkFetch, String> {
        let key = decoded_chunk_key(part_index, chunk, chunk_origin)?;
        let mut cache = self.decoded_chunks.lock();
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
            self.decoded_chunk_ready.wait(&mut cache);
        }
        drop(cache);

        let decode_result = self.decode_chunk_to_rgba(part_index, chunk, chunk_origin);
        let (decoded, decode_ms) = match decode_result {
            Ok(decoded) => decoded,
            Err(err) => {
                self.decoded_chunks.lock().finish_decode(&key);
                self.decoded_chunk_ready.notify_all();
                return Err(err);
            }
        };
        let mut cache = self.decoded_chunks.lock();
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

        let (roles, buffers, channel_layouts) = {
            let channels = decode_pipeline_channels(&mut pipeline)?;
            let roles = assign_channel_roles(channels);
            let mut buffers = vec![Vec::<f32>::new(); channels.len()];
            let mut channel_layouts = vec![None; channels.len()];
            for (index, channel) in channels.iter_mut().enumerate() {
                if roles[index].is_none() {
                    channel.decode_to_ptr = ptr::null_mut();
                    continue;
                }

                let ch_w = usize::try_from(channel.width)
                    .map_err(|_| "OpenEXRCore channel width is invalid".to_string())?;
                let ch_h = usize::try_from(channel.height)
                    .map_err(|_| "OpenEXRCore channel height is invalid".to_string())?;
                let ch_samples = ch_w
                    .checked_mul(ch_h)
                    .ok_or_else(|| "OpenEXRCore channel sample count overflowed".to_string())?;
                if ch_samples == 0 {
                    return Err(format!(
                        "OpenEXRCore channel has zero samples (width={}, height={})",
                        channel.width, channel.height
                    ));
                }

                buffers[index] = vec![0.0_f32; ch_samples];
                channel_layouts[index] = Some(OpenExrCoreChannelChunkLayout {
                    width: channel.width,
                    height: channel.height,
                    x_samples: channel.x_samples,
                    y_samples: channel.y_samples,
                });
                channel.user_bytes_per_element = 4;
                channel.user_data_type = sys::EXR_PIXEL_FLOAT;
                channel.user_pixel_stride = 4;
                channel.user_line_stride = i32::try_from(ch_w * 4)
                    .map_err(|_| "OpenEXRCore channel line stride exceeds i32".to_string())?;
                channel.decode_to_ptr = buffers[index].as_mut_ptr().cast::<u8>();
            }
            (roles, buffers, channel_layouts)
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

        let rgba_len = sample_count
            .checked_mul(4)
            .ok_or_else(|| "OpenEXR chunk RGBA sample count overflow".to_string())?;
        let mut rgba = vec![0.0_f32; rgba_len];
        for alpha in rgba.chunks_exact_mut(4).map(|pixel| &mut pixel[3]) {
            *alpha = 1.0;
        }

        let mut r_idx = None;
        let mut g_idx = None;
        let mut b_idx = None;
        let mut a_idx = None;
        let mut y_idx = None;
        let mut ry_idx = None;
        let mut by_idx = None;

        for (i, role) in roles.iter().enumerate() {
            match role {
                Some(ChannelRole::Red) => r_idx = Some(i),
                Some(ChannelRole::Green) => g_idx = Some(i),
                Some(ChannelRole::Blue) => b_idx = Some(i),
                Some(ChannelRole::Alpha) => a_idx = Some(i),
                Some(ChannelRole::Luma) => y_idx = Some(i),
                Some(ChannelRole::Ry) => ry_idx = Some(i),
                Some(ChannelRole::By) => by_idx = Some(i),
                None => {}
            }
        }

        let is_yryby = y_idx.is_some()
            && ry_idx.is_some()
            && by_idx.is_some()
            && r_idx.is_none()
            && g_idx.is_none()
            && b_idx.is_none();

        let chunk_width_u32 = u32::try_from(chunk_width)
            .map_err(|e| format!("OpenEXRCore chunk width overflow: {e}"))?;
        let chunk_height_u32 = u32::try_from(chunk_height)
            .map_err(|e| format!("OpenEXRCore chunk height overflow: {e}"))?;

        // FAST PATH: All involved channels are 1:1 resolution (no subsampling)
        let mut can_use_fast_path = !is_yryby;
        if can_use_fast_path {
            for idx_opt in [r_idx, g_idx, b_idx, a_idx] {
                if let Some(i) = idx_opt
                    && let Some(layout) = channel_layouts[i]
                    && (layout.x_samples != 1 || layout.y_samples != 1)
                {
                    can_use_fast_path = false;
                    break;
                }
            }
        }
        if can_use_fast_path
            && y_idx.is_some()
            && r_idx.is_none()
            && g_idx.is_none()
            && b_idx.is_none()
        {
            // Luminance-only (non-YC) must use per-sample resampling, not the RGB fast path.
            can_use_fast_path = false;
        }

        if can_use_fast_path {
            let r_buf = r_idx.map(|i| &buffers[i]);
            let g_buf = g_idx.map(|i| &buffers[i]);
            let b_buf = b_idx.map(|i| &buffers[i]);
            let a_buf = a_idx.map(|i| &buffers[i]);

            for row in 0..chunk_height_u32 {
                let row_offset = (row as usize)
                    .checked_mul(chunk_width)
                    .ok_or_else(|| format!("EXR fast path row_offset overflow: row={row}"))?;
                let dest_row_offset = row_offset
                    .checked_mul(4)
                    .ok_or_else(|| format!("EXR fast path dest_row_offset overflow: row={row}"))?;
                for col in 0..chunk_width_u32 {
                    let i = row_offset + col as usize;
                    let dest = dest_row_offset + col as usize * 4;

                    rgba[dest] = r_buf.map(|b| b[i]).unwrap_or(0.0);
                    rgba[dest + 1] = g_buf.map(|b| b[i]).unwrap_or(0.0);
                    rgba[dest + 2] = b_buf.map(|b| b[i]).unwrap_or(0.0);
                    rgba[dest + 3] = a_buf.map(|b| b[i]).unwrap_or(1.0);
                }
            }
        } else {
            // SLOW PATH: Handles subsampling (YCbCr etc)
            let (y_idx_val, ry_idx_val, by_idx_val) = if is_yryby {
                (
                    y_idx.expect("is_yryby guarantees Y index"),
                    ry_idx.expect("is_yryby guarantees RY index"),
                    by_idx.expect("is_yryby guarantees BY index"),
                )
            } else {
                (0, 0, 0)
            };

            for row_u in 0..chunk_height_u32 {
                let row_dest_base = (row_u as usize)
                    .checked_mul(chunk_width)
                    .and_then(|p| p.checked_mul(4))
                    .ok_or_else(|| {
                        format!("EXR slow path row_dest_base overflow: row_u={row_u}")
                    })?;
                for col_u in 0..chunk_width_u32 {
                    let dest = row_dest_base + (col_u as usize) * 4;

                    rgba[dest + 3] = a_idx
                        .map(|i| {
                            channel_sample_f32(
                                &buffers,
                                &channel_layouts,
                                i,
                                chunk_origin,
                                col_u,
                                row_u,
                            )
                        })
                        .unwrap_or(1.0);

                    if is_yryby {
                        let y = channel_sample_f32(
                            &buffers,
                            &channel_layouts,
                            y_idx_val,
                            chunk_origin,
                            col_u,
                            row_u,
                        );
                        let ry_ratio = channel_sample_f32_filtered(
                            &buffers,
                            &channel_layouts,
                            ry_idx_val,
                            chunk_origin,
                            col_u,
                            row_u,
                            true,
                        );
                        let by_ratio = channel_sample_f32_filtered(
                            &buffers,
                            &channel_layouts,
                            by_idx_val,
                            chunk_origin,
                            col_u,
                            row_u,
                            true,
                        );
                        let [wr, wg, wb] = self.exr_luma_weights;
                        if ry_ratio == 0.0 && by_ratio == 0.0 {
                            rgba[dest] = y;
                            rgba[dest + 1] = y;
                            rgba[dest + 2] = y;
                        } else {
                            let r = (ry_ratio + 1.0) * y;
                            let b = (by_ratio + 1.0) * y;
                            let g = (y - wr * r - wb * b) / wg;
                            rgba[dest] = r;
                            rgba[dest + 1] = g;
                            rgba[dest + 2] = b;
                        }
                    } else if r_idx.is_some() || g_idx.is_some() || b_idx.is_some() {
                        rgba[dest] = r_idx
                            .map(|i| {
                                channel_sample_f32(
                                    &buffers,
                                    &channel_layouts,
                                    i,
                                    chunk_origin,
                                    col_u,
                                    row_u,
                                )
                            })
                            .unwrap_or(0.0);
                        rgba[dest + 1] = g_idx
                            .map(|i| {
                                channel_sample_f32(
                                    &buffers,
                                    &channel_layouts,
                                    i,
                                    chunk_origin,
                                    col_u,
                                    row_u,
                                )
                            })
                            .unwrap_or(0.0);
                        rgba[dest + 2] = b_idx
                            .map(|i| {
                                channel_sample_f32(
                                    &buffers,
                                    &channel_layouts,
                                    i,
                                    chunk_origin,
                                    col_u,
                                    row_u,
                                )
                            })
                            .unwrap_or(0.0);
                    } else if let Some(i) = y_idx {
                        let y = channel_sample_f32(
                            &buffers,
                            &channel_layouts,
                            i,
                            chunk_origin,
                            col_u,
                            row_u,
                        );
                        rgba[dest] = y;
                        rgba[dest + 1] = y;
                        rgba[dest + 2] = y;
                    } else {
                        rgba[dest] = 0.0;
                        rgba[dest + 1] = 0.0;
                        rgba[dest + 2] = 0.0;
                    }
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
