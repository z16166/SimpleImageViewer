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

use parking_lot::Mutex;
use std::io::{BufRead, Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::hdr::tiled::{
    HdrTileBuffer, HdrTileCache, HdrTiledSource, HdrTiledSourceKind,
    configured_hdr_tile_cache_max_bytes, validate_tile_bounds,
};
use crate::hdr::types::{HdrColorSpace, HdrImageBuffer, HdrImageMetadata, HdrPixelFormat};

#[derive(Clone, Copy, Debug, Default)]
struct Rgbe8Pixel {
    rgb: [u8; 3],
    exponent: u8,
}

/// Axes in the Radiance resolution line (`+X`, `-Y`, …). Data is stored as `outer` scanlines of
/// `inner_len` RGBE pixels; see [`RadianceRasterLayout::logical_xy`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RadianceScanAxis {
    X,
    Y,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RadianceScanSign {
    Positive,
    Negative,
}

/// Parsed resolution line (`-Y H +X W`, `+X W -Y H`, …): display `width`×`height` in top-left origin,
/// `+y` downward, `+x` rightward. File order is `(outer_idx, inner_idx)` with mapping per Greg Ward /
/// RFC-style HDR semantics.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RadianceRasterLayout {
    pub(crate) width: u32,
    pub(crate) height: u32,
    outer_axis: RadianceScanAxis,
    outer_sign: RadianceScanSign,
    pub(crate) outer_len: u32,
    inner_axis: RadianceScanAxis,
    inner_sign: RadianceScanSign,
    pub(crate) inner_len: u32,
}

impl RadianceRasterLayout {
    /// Map file-order indices to logical image `(x,y)`.
    ///
    /// Hot decode paths use [`Self::stride_plan`] instead; this stays as the spec reference for
    /// tests ([`Self::file_indices_for_logical_xy`] inverse).
    #[allow(dead_code)]
    pub(crate) fn logical_xy(self, outer_a: u32, inner_b: u32) -> (u32, u32) {
        let w = self.width;
        let h = self.height;
        let x = match self.outer_axis {
            RadianceScanAxis::X => {
                if self.outer_sign == RadianceScanSign::Positive {
                    outer_a
                } else {
                    w - 1 - outer_a
                }
            }
            RadianceScanAxis::Y => {
                if self.inner_sign == RadianceScanSign::Positive {
                    inner_b
                } else {
                    w - 1 - inner_b
                }
            }
        };
        let y = match self.outer_axis {
            RadianceScanAxis::Y => {
                if self.outer_sign == RadianceScanSign::Negative {
                    outer_a
                } else {
                    h - 1 - outer_a
                }
            }
            RadianceScanAxis::X => {
                if self.inner_sign == RadianceScanSign::Negative {
                    inner_b
                } else {
                    h - 1 - inner_b
                }
            }
        };
        (x, y)
    }

    /// Inverse of [`Self::logical_xy`]: which file scanline and in-line index hold logical `(lx,ly)`.
    pub(crate) fn file_indices_for_logical_xy(self, lx: u32, ly: u32) -> (u32, u32) {
        match self.outer_axis {
            RadianceScanAxis::Y => {
                let outer_a = if self.outer_sign == RadianceScanSign::Negative {
                    ly
                } else {
                    self.height - 1 - ly
                };
                let inner_b = if self.inner_sign == RadianceScanSign::Positive {
                    lx
                } else {
                    self.width - 1 - lx
                };
                (outer_a, inner_b)
            }
            RadianceScanAxis::X => {
                let outer_a = if self.outer_sign == RadianceScanSign::Positive {
                    lx
                } else {
                    self.width - 1 - lx
                };
                let inner_b = if self.inner_sign == RadianceScanSign::Negative {
                    ly
                } else {
                    self.height - 1 - ly
                };
                (outer_a, inner_b)
            }
        }
    }

    /// `-Y … +X …` without flips — file scanlines match display rows left-to-right, top-to-bottom.
    pub(crate) fn is_row_major_top_left(self) -> bool {
        matches!(
            (
                self.outer_axis,
                self.outer_sign,
                self.inner_axis,
                self.inner_sign,
            ),
            (
                RadianceScanAxis::Y,
                RadianceScanSign::Negative,
                RadianceScanAxis::X,
                RadianceScanSign::Positive,
            )
        )
    }

    /// Starts and ±1 strides for stepping logical `(x,y)` without branches in the pixel hot loop,
    /// matching the resolution-line semantics (`outer_axis`/signs lifted out of inner loops).
    pub(crate) fn stride_plan(self) -> RadianceStridePlan {
        let w_i = self.width as i32;
        let h_i = self.height as i32;
        if self.outer_axis == RadianceScanAxis::Y {
            let y_start = if self.outer_sign == RadianceScanSign::Negative {
                0
            } else {
                h_i - 1
            };
            let y_step = if self.outer_sign == RadianceScanSign::Negative {
                1
            } else {
                -1
            };
            let x_start = if self.inner_sign == RadianceScanSign::Positive {
                0
            } else {
                w_i - 1
            };
            let x_step = if self.inner_sign == RadianceScanSign::Positive {
                1
            } else {
                -1
            };
            RadianceStridePlan {
                outer_major_is_y: true,
                outer_len: self.outer_len,
                inner_len: self.inner_len,
                x_start,
                x_step,
                y_start,
                y_step,
            }
        } else {
            let x_start = if self.outer_sign == RadianceScanSign::Positive {
                0
            } else {
                w_i - 1
            };
            let x_step = if self.outer_sign == RadianceScanSign::Positive {
                1
            } else {
                -1
            };
            let y_start = if self.inner_sign == RadianceScanSign::Negative {
                0
            } else {
                h_i - 1
            };
            let y_step = if self.inner_sign == RadianceScanSign::Negative {
                1
            } else {
                -1
            };
            RadianceStridePlan {
                outer_major_is_y: false,
                outer_len: self.outer_len,
                inner_len: self.inner_len,
                x_start,
                x_step,
                y_start,
                y_step,
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct RadianceStridePlan {
    outer_major_is_y: bool,
    outer_len: u32,
    inner_len: u32,
    x_start: i32,
    x_step: i32,
    y_start: i32,
    y_step: i32,
}

/// Indices `inner ∈ [imin,imax]` whose `coord = coord_start + inner * coord_step` lie in `[c0,c1]` (inclusive),
/// assuming `coord_step ∈ { -1, +1 }`.
fn inner_range_covering_coord_inclusive(
    coord_start: i32,
    coord_step: i32,
    inner_len: u32,
    c0: i32,
    c1: i32,
) -> Option<(u32, u32)> {
    debug_assert!(coord_step == 1 || coord_step == -1);
    let last = coord_start + coord_step * (inner_len as i32 - 1);
    let (vmin, vmax) = if coord_start <= last {
        (coord_start, last)
    } else {
        (last, coord_start)
    };
    let c0 = c0.max(vmin);
    let c1 = c1.min(vmax);
    if c1 < c0 {
        return None;
    }

    fn solve(inner_start: i32, step: i32, target: i32) -> i32 {
        (target - inner_start).div_euclid(step)
    }

    let i0 = solve(coord_start, coord_step, c0).clamp(0, inner_len as i32 - 1) as u32;
    let i1 = solve(coord_start, coord_step, c1).clamp(0, inner_len as i32 - 1) as u32;
    let imin = i0.min(i1);
    let imax = i0.max(i1);
    Some((imin, imax))
}

/// File `outer` indices whose coordinate `coord = outer_origin + outer * outer_step` falls in `[c0,c1]` (inclusive),
/// clipped to `[0, outer_len)`.
fn outer_range_covering_coord_inclusive(
    outer_origin: i32,
    outer_step: i32,
    outer_len: u32,
    c0: i32,
    c1: i32,
) -> Option<(u32, u32)> {
    debug_assert!(outer_step == 1 || outer_step == -1);
    let last_outer = outer_len as i32 - 1;
    let first_coord = outer_origin;
    let last_coord = outer_origin + outer_step * last_outer;
    let (vmin, vmax) = if first_coord <= last_coord {
        (first_coord, last_coord)
    } else {
        (last_coord, first_coord)
    };
    let c0 = c0.max(vmin);
    let c1 = c1.min(vmax);
    if c1 < c0 {
        return None;
    }

    fn solve(outer_orig: i32, step: i32, target: i32) -> i32 {
        (target - outer_orig).div_euclid(step)
    }

    let o0 = solve(outer_origin, outer_step, c0).clamp(0, last_outer) as u32;
    let o1 = solve(outer_origin, outer_step, c1).clamp(0, last_outer) as u32;
    let omin = o0.min(o1);
    let omax = o0.max(o1);
    Some((omin, omax))
}

#[derive(Debug)]
pub struct RadianceHdrTiledImageSource {
    #[allow(dead_code)]
    path: PathBuf,
    mmap: Arc<memmap2::Mmap>,
    width: u32,
    height: u32,
    raster: RadianceRasterLayout,
    params: crate::hdr::decode::RadianceHeaderParams,
    scanline_offsets: Vec<usize>,
    tile_cache: Mutex<HdrTileCache>,
}

impl RadianceHdrTiledImageSource {
    pub(crate) fn open(path: &Path) -> Result<Self, String> {
        let mmap = Arc::new(crate::mmap_util::map_file(path)?);
        let mut params = crate::hdr::decode::RadianceHeaderParams::default();
        let mut reader = Cursor::new(&mmap[..]);
        let raster = read_radiance_header(&mut reader, &mut params)?;
        let (width, height) = (raster.width, raster.height);
        let data_offset = reader.position() as usize;
        let scanline_offsets = build_radiance_scanline_offsets(&mmap, data_offset, &raster)?;
        log::debug!("[HDR] {}: {}", path.display(), params.diagnostic_label());

        Ok(Self {
            path: path.to_path_buf(),
            mmap,
            width,
            height,
            raster,
            params,
            scanline_offsets,
            tile_cache: Mutex::new(HdrTileCache::new(configured_hdr_tile_cache_max_bytes())),
        })
    }
}

impl HdrTiledSource for RadianceHdrTiledImageSource {
    fn source_kind(&self) -> HdrTiledSourceKind {
        HdrTiledSourceKind::DiskBacked
    }

    fn source_name(&self) -> String {
        self.path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string())
    }

    fn width(&self) -> u32 {
        self.width
    }

    fn height(&self) -> u32 {
        self.height
    }

    fn color_space(&self) -> HdrColorSpace {
        HdrColorSpace::LinearSrgb
    }

    fn generate_hdr_preview(&self, max_w: u32, max_h: u32) -> Result<HdrImageBuffer, String> {
        decode_radiance_hdr_preview(
            &self.mmap,
            self.width,
            self.height,
            self.raster,
            self.params,
            &self.scanline_offsets,
            max_w,
            max_h,
        )
    }

    fn generate_sdr_preview(&self, max_w: u32, max_h: u32) -> Result<(u32, u32, Vec<u8>), String> {
        decode_radiance_sdr_preview(
            &self.mmap,
            self.width,
            self.height,
            self.raster,
            self.params,
            &self.scanline_offsets,
            max_w,
            max_h,
        )
    }

    fn cached_tile_rgba32f_arc(
        &self,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    ) -> Option<Arc<HdrTileBuffer>> {
        self.tile_cache.lock().get((x, y, width, height))
    }

    fn protect_cached_tiles(&self, tiles: &[(u32, u32, u32, u32)]) {
        self.tile_cache
            .lock()
            .set_protected_keys(tiles.iter().copied());
    }

    fn extract_tile_rgba32f_arc(
        &self,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    ) -> Result<Arc<HdrTileBuffer>, String> {
        validate_tile_bounds(self.width, self.height, x, y, width, height)?;
        let key = (x, y, width, height);
        {
            let mut cache = self.tile_cache.lock();
            if let Some(tile) = cache.get(key) {
                return Ok(tile);
            }
        }

        let rgba = decode_radiance_tile_window(
            &self.mmap,
            self.raster,
            self.params,
            &self.scanline_offsets,
            x,
            y,
            width,
            height,
        )?;

        let tile = Arc::new(HdrTileBuffer::new_with_metadata(
            width,
            height,
            HdrColorSpace::LinearSrgb,
            HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
            Arc::new(rgba),
        ));

        self.tile_cache.lock().insert(key, Arc::clone(&tile));

        Ok(tile)
    }
}

fn decode_radiance_tile_window(
    mmap: &[u8],
    raster: RadianceRasterLayout,
    params: crate::hdr::decode::RadianceHeaderParams,
    scanline_offsets: &[usize],
    tile_x: u32,
    tile_y: u32,
    tile_w: u32,
    tile_h: u32,
) -> Result<Vec<f32>, String> {
    validate_tile_bounds(raster.width, raster.height, tile_x, tile_y, tile_w, tile_h)?;
    validate_scanline_offsets(raster.outer_len, scanline_offsets)?;
    let mut reader = Cursor::new(mmap);

    let mut scanline = vec![Rgbe8Pixel::default(); raster.inner_len as usize];
    let mut rgba = vec![0.0f32; tile_w as usize * tile_h as usize * 4];

    if raster.is_row_major_top_left() {
        let first_row = tile_y;
        let last_row_exclusive = tile_y + tile_h;
        for row in first_row..last_row_exclusive {
            reader.set_position(scanline_offsets[row as usize] as u64);
            read_scanline(&mut reader, &mut scanline)?;

            let start = tile_x as usize;
            let end = start + tile_w as usize;
            let out_row = row - tile_y;
            for (dx, pixel) in scanline[start..end].iter().enumerate() {
                let rgb = pixel.to_rgb_f32();
                let base = (out_row as usize * tile_w as usize + dx) * 4;
                rgba[base..base + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 1.0]);
            }
        }
    } else {
        let plan = raster.stride_plan();
        let tw = tile_w as usize;
        let tx0 = tile_x as i32;
        let ty0 = tile_y as i32;
        let tx1 = tx0 + tile_w as i32 - 1;
        let ty1 = ty0 + tile_h as i32 - 1;

        if plan.outer_major_is_y {
            let y0 = plan.y_start;
            if let Some((oa_lo, oa_hi)) =
                outer_range_covering_coord_inclusive(y0, plan.y_step, raster.outer_len, ty0, ty1)
            {
                for outer_a in oa_lo..=oa_hi {
                    reader.set_position(scanline_offsets[outer_a as usize] as u64);
                    read_scanline(&mut reader, &mut scanline)?;
                    let y = y0 + (outer_a as i32) * plan.y_step;
                    let dy = (y - ty0) as usize;
                    if let Some((imin, imax)) = inner_range_covering_coord_inclusive(
                        plan.x_start,
                        plan.x_step,
                        raster.inner_len,
                        tx0,
                        tx1,
                    ) {
                        for inner_b in imin..=imax {
                            let rgb = scanline[inner_b as usize].to_rgb_f32();
                            let x = plan.x_start + (inner_b as i32) * plan.x_step;
                            let dx = (x - tx0) as usize;
                            let base = (dy * tw + dx) * 4;
                            rgba[base..base + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 1.0]);
                        }
                    }
                }
            }
        } else {
            let x0 = plan.x_start;
            if let Some((oa_lo, oa_hi)) =
                outer_range_covering_coord_inclusive(x0, plan.x_step, raster.outer_len, tx0, tx1)
            {
                for outer_a in oa_lo..=oa_hi {
                    reader.set_position(scanline_offsets[outer_a as usize] as u64);
                    read_scanline(&mut reader, &mut scanline)?;
                    let x = x0 + (outer_a as i32) * plan.x_step;
                    let dx = (x - tx0) as usize;
                    if let Some((imin, imax)) = inner_range_covering_coord_inclusive(
                        plan.y_start,
                        plan.y_step,
                        raster.inner_len,
                        ty0,
                        ty1,
                    ) {
                        for inner_b in imin..=imax {
                            let rgb = scanline[inner_b as usize].to_rgb_f32();
                            let y = plan.y_start + (inner_b as i32) * plan.y_step;
                            let dy = (y - ty0) as usize;
                            let base = (dy * tw + dx) * 4;
                            rgba[base..base + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 1.0]);
                        }
                    }
                }
            }
        }
    }
    params.apply_to_pixels(&mut rgba);

    Ok(rgba)
}

fn decode_radiance_sdr_preview(
    mmap: &[u8],
    logical_width: u32,
    logical_height: u32,
    raster: RadianceRasterLayout,
    params: crate::hdr::decode::RadianceHeaderParams,
    scanline_offsets: &[usize],
    max_w: u32,
    max_h: u32,
) -> Result<(u32, u32, Vec<u8>), String> {
    let preview = decode_radiance_hdr_preview(
        mmap,
        logical_width,
        logical_height,
        raster,
        params,
        scanline_offsets,
        max_w,
        max_h,
    )?;
    let pixels = crate::hdr::decode::hdr_to_sdr_rgba8(&preview, 0.0)?;
    Ok((preview.width, preview.height, pixels))
}

fn decode_radiance_hdr_preview(
    mmap: &[u8],
    logical_width: u32,
    logical_height: u32,
    raster: RadianceRasterLayout,
    params: crate::hdr::decode::RadianceHeaderParams,
    scanline_offsets: &[usize],
    max_w: u32,
    max_h: u32,
) -> Result<HdrImageBuffer, String> {
    let (preview_width, preview_height) =
        preview_dimensions(logical_width, logical_height, max_w, max_h);
    if preview_width == 0 || preview_height == 0 {
        return Err("Radiance HDR preview dimensions must be non-zero".to_string());
    }

    validate_scanline_offsets(raster.outer_len, scanline_offsets)?;
    let mut reader = Cursor::new(mmap);

    let mut scanline = vec![Rgbe8Pixel::default(); raster.inner_len as usize];
    let mut rgba = vec![0.0f32; preview_width as usize * preview_height as usize * 4];

    if raster.is_row_major_top_left() {
        for preview_y in 0..preview_height {
            let source_y = preview_sample_coord(preview_y, preview_height, logical_height);
            reader.set_position(scanline_offsets[source_y as usize] as u64);
            read_scanline(&mut reader, &mut scanline)?;

            for preview_x in 0..preview_width {
                let source_x =
                    preview_sample_coord(preview_x, preview_width, logical_width) as usize;
                let rgb = scanline[source_x].to_rgb_f32();
                let base = (preview_y as usize * preview_width as usize + preview_x as usize) * 4;
                rgba[base..base + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 1.0]);
            }
        }
    } else {
        let mut last_outer_a: Option<u32> = None;
        for preview_y in 0..preview_height {
            for preview_x in 0..preview_width {
                let lx = preview_sample_coord(preview_x, preview_width, logical_width);
                let ly = preview_sample_coord(preview_y, preview_height, logical_height);
                let (outer_a, inner_b) = raster.file_indices_for_logical_xy(lx, ly);

                if last_outer_a != Some(outer_a) {
                    reader.set_position(scanline_offsets[outer_a as usize] as u64);
                    read_scanline(&mut reader, &mut scanline)?;
                    last_outer_a = Some(outer_a);
                }

                let rgb = scanline[inner_b as usize].to_rgb_f32();
                let base = (preview_y as usize * preview_width as usize + preview_x as usize) * 4;
                rgba[base..base + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 1.0]);
            }
        }
    }

    params.apply_to_pixels(&mut rgba);

    Ok(HdrImageBuffer {
        width: preview_width,
        height: preview_height,
        format: HdrPixelFormat::Rgba32Float,
        color_space: HdrColorSpace::LinearSrgb,
        metadata: HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
        rgba_f32: Arc::new(rgba),
    })
}

fn preview_dimensions(width: u32, height: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    if width == 0 || height == 0 || max_w == 0 || max_h == 0 {
        return (0, 0);
    }

    let scale = (max_w as f32 / width as f32)
        .min(max_h as f32 / height as f32)
        .min(1.0);
    let preview_width = ((width as f32 * scale).round() as u32).clamp(1, max_w);
    let preview_height = ((height as f32 * scale).round() as u32).clamp(1, max_h);
    (preview_width, preview_height)
}

fn preview_sample_coord(preview_coord: u32, preview_extent: u32, source_extent: u32) -> u32 {
    if preview_extent <= 1 {
        return 0;
    }

    ((u64::from(preview_coord) * u64::from(source_extent - 1)) / u64::from(preview_extent - 1))
        as u32
}

fn build_radiance_scanline_offsets(
    mmap: &[u8],
    data_offset: usize,
    raster: &RadianceRasterLayout,
) -> Result<Vec<usize>, String> {
    let mut reader = Cursor::new(mmap);
    reader.set_position(data_offset as u64);
    let mut offsets = Vec::with_capacity(raster.outer_len as usize);
    for _ in 0..raster.outer_len {
        offsets.push(reader.position() as usize);
        skip_scanline(&mut reader, raster.inner_len as usize)?;
    }
    Ok(offsets)
}

fn validate_scanline_offsets(outer_len: u32, scanline_offsets: &[usize]) -> Result<(), String> {
    if scanline_offsets.len() != outer_len as usize {
        return Err(format!(
            "Radiance HDR scanline index has {} chunks, expected {outer_len}",
            scanline_offsets.len()
        ));
    }
    Ok(())
}

fn read_radiance_header<R: BufRead>(
    reader: &mut R,
    params: &mut crate::hdr::decode::RadianceHeaderParams,
) -> Result<RadianceRasterLayout, String> {
    let mut line = String::new();
    reader.read_line(&mut line).map_err(|err| err.to_string())?;
    if line.trim_end() != "#?RADIANCE" {
        return Err("Radiance HDR signature not found".to_string());
    }

    loop {
        line.clear();
        let bytes_read = reader.read_line(&mut line).map_err(|err| err.to_string())?;
        if bytes_read == 0 {
            return Err("EOF in Radiance HDR header".to_string());
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            break;
        }
        if !trimmed.starts_with('#') {
            params.apply_header_line(trimmed);
        }
    }

    line.clear();
    reader.read_line(&mut line).map_err(|err| err.to_string())?;
    parse_radiance_dimensions_line(line.trim())
}

fn parse_axis_size_token(
    tag: &str,
    size_str: &str,
) -> Result<(RadianceScanAxis, RadianceScanSign, u32), String> {
    let b = tag.as_bytes();
    if b.len() < 2 {
        return Err(format!(
            "Invalid Radiance HDR axis token (expected ±X/±Y): {tag}"
        ));
    }
    let sign = match b[0] {
        b'+' => RadianceScanSign::Positive,
        b'-' => RadianceScanSign::Negative,
        _ => {
            return Err(format!("Invalid Radiance HDR axis sign in token: {tag}"));
        }
    };
    let axis = match b[1] {
        b'x' | b'X' => RadianceScanAxis::X,
        b'y' | b'Y' => RadianceScanAxis::Y,
        _ => {
            return Err(format!("Invalid Radiance HDR axis letter in token: {tag}"));
        }
    };
    let size = size_str
        .parse::<u32>()
        .map_err(|err| format!("Invalid Radiance HDR dimension value: {err}"))?;
    if size == 0 {
        return Err("Radiance HDR dimension must be non-zero".to_string());
    }
    Ok((axis, sign, size))
}

/// Four fields: `±Axis size ±Axis size` in any order (`-Y 1024 +X 2048` or `+X 2048 -Y 1024`, etc.).
fn parse_radiance_dimensions_line(line: &str) -> Result<RadianceRasterLayout, String> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() != 4 {
        return Err(format!(
            "Radiance HDR dimensions line must have 4 whitespace-separated fields, got: {line}"
        ));
    }
    let (o_axis, o_sign, o_size) = parse_axis_size_token(parts[0], parts[1])?;
    let (i_axis, i_sign, i_size) = parse_axis_size_token(parts[2], parts[3])?;
    if o_axis == i_axis {
        return Err(format!(
            "Radiance HDR dimensions must use one X and one Y axis: {line}"
        ));
    }
    let (width, height) = match o_axis {
        RadianceScanAxis::X => (o_size, i_size),
        RadianceScanAxis::Y => (i_size, o_size),
    };
    Ok(RadianceRasterLayout {
        width,
        height,
        outer_axis: o_axis,
        outer_sign: o_sign,
        outer_len: o_size,
        inner_axis: i_axis,
        inner_sign: i_sign,
        inner_len: i_size,
    })
}

/// Full image in logical row-major RGBA32F (for small-buffer / `decode_hdr_image` path).
pub(crate) fn decode_radiance_rgba32f_from_mmap(
    mmap: &[u8],
    params_override: Option<crate::hdr::decode::RadianceHeaderParams>,
) -> Result<HdrImageBuffer, String> {
    let mut params = params_override.unwrap_or_default();
    let mut reader = Cursor::new(mmap);
    let raster = read_radiance_header(&mut reader, &mut params)?;
    let (width, height) = (raster.width, raster.height);
    crate::hdr::decode::validate_hdr_fallback_budget(width, height)?;
    let data_offset = reader.position() as usize;
    let scanline_offsets = build_radiance_scanline_offsets(mmap, data_offset, &raster)?;
    let n = width as usize * height as usize * 4;
    let mut rgba_f32 = vec![0.0f32; n];
    validate_scanline_offsets(raster.outer_len, &scanline_offsets)?;

    let mut file_reader = Cursor::new(mmap);
    let mut scanline = vec![Rgbe8Pixel::default(); raster.inner_len as usize];

    if raster.is_row_major_top_left() {
        for ly in 0..height {
            file_reader.set_position(scanline_offsets[ly as usize] as u64);
            read_scanline(&mut file_reader, &mut scanline)?;
            let row_off = ly as usize * width as usize * 4;
            for lx in 0..width as usize {
                let rgb = scanline[lx].to_rgb_f32();
                let o = row_off + lx * 4;
                rgba_f32[o..o + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 1.0]);
            }
        }
    } else {
        let plan = raster.stride_plan();
        let w = width as usize;
        if plan.outer_major_is_y {
            let mut y = plan.y_start;
            for outer_i in 0..plan.outer_len {
                file_reader.set_position(scanline_offsets[outer_i as usize] as u64);
                read_scanline(&mut file_reader, &mut scanline)?;
                let mut x = plan.x_start;
                let row_off = y as usize * w * 4;
                for inner_i in 0..plan.inner_len as usize {
                    let rgb = scanline[inner_i].to_rgb_f32();
                    let o = row_off + (x as usize) * 4;
                    rgba_f32[o..o + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 1.0]);
                    x += plan.x_step;
                }
                y += plan.y_step;
            }
        } else {
            let mut x = plan.x_start;
            for outer_i in 0..plan.outer_len {
                file_reader.set_position(scanline_offsets[outer_i as usize] as u64);
                read_scanline(&mut file_reader, &mut scanline)?;
                let xu = x as usize;
                let mut y = plan.y_start;
                for inner_i in 0..plan.inner_len as usize {
                    let rgb = scanline[inner_i].to_rgb_f32();
                    let o = ((y as usize) * w + xu) * 4;
                    rgba_f32[o..o + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 1.0]);
                    y += plan.y_step;
                }
                x += plan.x_step;
            }
        }
    }
    params.apply_to_pixels(&mut rgba_f32);

    Ok(HdrImageBuffer {
        width,
        height,
        format: HdrPixelFormat::Rgba32Float,
        color_space: HdrColorSpace::LinearSrgb,
        metadata: HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
        rgba_f32: Arc::new(rgba_f32),
    })
}

fn read_scanline<R: Read>(reader: &mut R, scanline: &mut [Rgbe8Pixel]) -> Result<(), String> {
    if scanline.is_empty() {
        return Ok(());
    }

    let first = read_rgbe(reader)?;
    if first.rgb[0] == 2 && first.rgb[1] == 2 && first.rgb[2] < 128 {
        decode_component(reader, scanline.len(), |offset, value| {
            scanline[offset].rgb[0] = value;
        })?;
        decode_component(reader, scanline.len(), |offset, value| {
            scanline[offset].rgb[1] = value;
        })?;
        decode_component(reader, scanline.len(), |offset, value| {
            scanline[offset].rgb[2] = value;
        })?;
        decode_component(reader, scanline.len(), |offset, value| {
            scanline[offset].exponent = value;
        })?;
    } else {
        decode_old_rle(reader, first, scanline)?;
    }
    Ok(())
}

fn skip_scanline(reader: &mut Cursor<&[u8]>, width: usize) -> Result<(), String> {
    if width == 0 {
        return Ok(());
    }

    let first = read_rgbe(reader)?;
    if first.rgb[0] == 2 && first.rgb[1] == 2 && first.rgb[2] < 128 {
        for _ in 0..4 {
            skip_component(reader, width)?;
        }
    } else {
        skip_old_rle(reader, first, width)?;
    }
    Ok(())
}

fn skip_component(reader: &mut Cursor<&[u8]>, width: usize) -> Result<(), String> {
    let mut pos = 0usize;
    while pos < width {
        let run = read_byte(reader)?;
        if run <= 128 {
            let count = run as usize;
            if pos + count > width {
                return Err(format!(
                    "Wrong Radiance HDR scanline length: got {}, expected {width}",
                    pos + count
                ));
            }
            let next = reader
                .position()
                .checked_add(count as u64)
                .ok_or_else(|| "Radiance HDR scanline offset overflow".to_string())?;
            if next > reader.get_ref().len() as u64 {
                return Err("EOF in Radiance HDR scanline".to_string());
            }
            reader.set_position(next);
            pos += count;
        } else {
            let count = (run - 128) as usize;
            if pos + count > width {
                return Err(format!(
                    "Wrong Radiance HDR scanline length: got {}, expected {width}",
                    pos + count
                ));
            }
            read_byte(reader)?;
            pos += count;
        }
    }
    Ok(())
}

fn skip_old_rle(reader: &mut Cursor<&[u8]>, first: Rgbe8Pixel, width: usize) -> Result<(), String> {
    let mut x = 1usize;
    let mut run_multiplier = 1usize;
    let mut previous = first;

    while x < width {
        let pixel = read_rgbe(reader)?;
        if pixel.rgb == [1, 1, 1] {
            let count = pixel.exponent as usize * run_multiplier;
            run_multiplier *= 256;
            if x + count > width {
                return Err(format!(
                    "Wrong Radiance HDR scanline length: got {}, expected {width}",
                    x + count
                ));
            }
            x += count;
        } else {
            run_multiplier = 1;
            previous = pixel;
            x += 1;
        }
    }
    let _ = previous;
    Ok(())
}

fn decode_component<R: Read, F: FnMut(usize, u8)>(
    reader: &mut R,
    width: usize,
    mut set_component: F,
) -> Result<(), String> {
    let mut pos = 0usize;
    let mut buf = [0u8; 128];
    while pos < width {
        let run = read_byte(reader)?;
        if run <= 128 {
            let count = run as usize;
            if pos + count > width {
                return Err(format!(
                    "Wrong Radiance HDR scanline length: got {}, expected {width}",
                    pos + count
                ));
            }
            reader
                .read_exact(&mut buf[..count])
                .map_err(|err| err.to_string())?;
            for (offset, value) in buf[..count].iter().copied().enumerate() {
                set_component(pos + offset, value);
            }
            pos += count;
        } else {
            let count = (run - 128) as usize;
            if pos + count > width {
                return Err(format!(
                    "Wrong Radiance HDR scanline length: got {}, expected {width}",
                    pos + count
                ));
            }
            let value = read_byte(reader)?;
            for offset in 0..count {
                set_component(pos + offset, value);
            }
            pos += count;
        }
    }
    Ok(())
}

fn decode_old_rle<R: Read>(
    reader: &mut R,
    first: Rgbe8Pixel,
    scanline: &mut [Rgbe8Pixel],
) -> Result<(), String> {
    scanline[0] = first;
    let mut x = 1usize;
    let mut run_multiplier = 1usize;
    let mut previous = first;

    while x < scanline.len() {
        let pixel = read_rgbe(reader)?;
        if pixel.rgb == [1, 1, 1] {
            let count = pixel.exponent as usize * run_multiplier;
            run_multiplier *= 256;
            if x + count > scanline.len() {
                return Err(format!(
                    "Wrong Radiance HDR scanline length: got {}, expected {}",
                    x + count,
                    scanline.len()
                ));
            }
            for dst in &mut scanline[x..x + count] {
                *dst = previous;
            }
            x += count;
        } else {
            run_multiplier = 1;
            previous = pixel;
            scanline[x] = pixel;
            x += 1;
        }
    }
    Ok(())
}

fn read_rgbe<R: Read>(reader: &mut R) -> Result<Rgbe8Pixel, String> {
    let mut bytes = [0u8; 4];
    reader
        .read_exact(&mut bytes)
        .map_err(|err| err.to_string())?;
    Ok(Rgbe8Pixel {
        rgb: [bytes[0], bytes[1], bytes[2]],
        exponent: bytes[3],
    })
}

fn read_byte<R: Read>(reader: &mut R) -> Result<u8, String> {
    let mut byte = [0u8; 1];
    reader
        .read_exact(&mut byte)
        .map_err(|err| err.to_string())?;
    Ok(byte[0])
}

impl Rgbe8Pixel {
    /// Runs once per decoded Radiance RGBE pixel. `powi` applies the Ward-style scale (`2^(e-128-8)`
    /// on the mantissa, same role as `ldexp`). SIMD on contiguous unpacked pixels or an `exponent`→scale
    /// LUT are optional optimizations; scanline RLE/component decode tends to dominate end-to-end cost.
    fn to_rgb_f32(self) -> [f32; 3] {
        if self.exponent == 0 {
            return [0.0; 3];
        }
        let scale = 2.0_f32.powi(i32::from(self.exponent) - 128 - 8);
        [
            f32::from(self.rgb[0]) * scale,
            f32::from(self.rgb[1]) * scale,
            f32::from(self.rgb[2]) * scale,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_radiance_logical_roundtrip(line: &str) {
        let r = parse_radiance_dimensions_line(line).unwrap_or_else(|_| panic!("parse {line}"));
        for oa in 0..r.outer_len {
            for ib in 0..r.inner_len {
                let (lx, ly) = r.logical_xy(oa, ib);
                assert_eq!(
                    r.file_indices_for_logical_xy(lx, ly),
                    (oa, ib),
                    "line={line} oa={oa} ib={ib} logical=({lx},{ly})",
                );
            }
        }
    }

    #[test]
    fn radiance_dimensions_line_accepts_x_then_y_token_order() {
        let r = parse_radiance_dimensions_line("+x 200 -Y 100").unwrap();
        assert_eq!((r.width, r.height), (200, 100));
        assert_eq!((r.outer_len, r.inner_len), (200, 100));
    }

    #[test]
    fn radiance_dimensions_minus_y_plus_x_flags_row_major_native() {
        let r = parse_radiance_dimensions_line("-Y 4 +X 7").unwrap();
        assert_eq!((r.width, r.height), (7, 4));
        assert!(r.is_row_major_top_left());
    }

    #[test]
    fn radiance_logical_xy_file_indices_inverse_for_all_sign_variants() {
        for line in [
            "-Y 2 +X 3",
            "+X 3 -Y 2",
            "+Y 2 +X 3",
            "-Y 2 -X 3",
            "+X 3 +Y 2",
            "-X 3 -Y 2",
            "+Y 2 -X 3",
            "-X 3 +Y 2",
        ] {
            assert_radiance_logical_roundtrip(line);
        }
    }

    #[test]
    fn extract_tile_applies_radiance_exposure_and_colorcorr() {
        let path = std::env::temp_dir().join(format!(
            "simple_image_viewer_radiance_tile_params_{}.hdr",
            std::process::id()
        ));
        let bytes = b"#?RADIANCE\nFORMAT=32-bit_rle_rgbe\nEXPOSURE=2\nCOLORCORR=2 4 8\n\n-Y 1 +X 1\n\x80\x80\x80\x81";
        std::fs::write(&path, bytes).expect("write test HDR");

        let source = RadianceHdrTiledImageSource::open(&path).expect("open Radiance HDR source");
        let tile = source
            .extract_tile_rgba32f_arc(0, 0, 1, 1)
            .expect("extract Radiance HDR tile");
        let _ = std::fs::remove_file(&path);

        assert!((tile.rgba_f32[0] - 0.25).abs() < 0.01);
        assert!((tile.rgba_f32[1] - 0.125).abs() < 0.01);
        assert!((tile.rgba_f32[2] - 0.0625).abs() < 0.01);
        assert_eq!(tile.rgba_f32[3], 1.0);
    }

    #[test]
    fn static_and_tiled_radiance_decode_apply_same_header_params() {
        let path = std::env::temp_dir().join(format!(
            "simple_image_viewer_radiance_static_tile_consistency_{}.hdr",
            std::process::id()
        ));
        let bytes = b"#?RADIANCE\nFORMAT=32-bit_rle_rgbe\nEXPOSURE=2\nCOLORCORR=2 4 8\n\n-Y 1 +X 1\n\x80\x80\x80\x81";
        std::fs::write(&path, bytes).expect("write test HDR");

        let static_hdr = crate::hdr::decode::decode_hdr_image(&path).expect("decode static HDR");
        let source = RadianceHdrTiledImageSource::open(&path).expect("open tiled HDR");
        let tile = source
            .extract_tile_rgba32f_arc(0, 0, 1, 1)
            .expect("extract tiled HDR");
        let _ = std::fs::remove_file(&path);

        assert_eq!(static_hdr.color_space, tile.color_space);
        assert_eq!(static_hdr.rgba_f32.len(), tile.rgba_f32.len());
        for (static_value, tile_value) in static_hdr.rgba_f32.iter().zip(tile.rgba_f32.iter()) {
            assert!(
                (static_value - tile_value).abs() < 0.01,
                "static={static_value}, tile={tile_value}"
            );
        }
    }

    #[test]
    fn extract_tile_does_not_require_full_image_decode_budget() {
        let path = std::env::temp_dir().join(format!(
            "simple_image_viewer_radiance_tile_window_{}.hdr",
            std::process::id()
        ));
        let width = 8193_u32;
        let height = 8193_u32;
        let mut bytes =
            format!("#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n-Y {height} +X {width}\n").into_bytes();
        for _ in 0..height {
            append_constant_new_rle_scanline(&mut bytes, width, [128, 128, 128], 129);
        }
        std::fs::write(&path, bytes).expect("write oversized tiled HDR");

        let source = RadianceHdrTiledImageSource::open(&path).expect("open Radiance HDR source");
        let tile = source
            .extract_tile_rgba32f_arc(0, 0, 1, 1)
            .expect("extract first pixel without full-image decode");
        let _ = std::fs::remove_file(&path);

        assert_eq!((tile.width, tile.height), (1, 1));
        assert!((tile.rgba_f32[0] - 1.0).abs() < 0.01);
        assert!((tile.rgba_f32[1] - 1.0).abs() < 0.01);
        assert!((tile.rgba_f32[2] - 1.0).abs() < 0.01);
        assert_eq!(tile.rgba_f32[3], 1.0);
    }

    #[test]
    fn generate_preview_does_not_require_full_image_decode_budget() {
        let path = std::env::temp_dir().join(format!(
            "simple_image_viewer_radiance_preview_window_{}.hdr",
            std::process::id()
        ));
        let width = 8193_u32;
        let height = 8193_u32;
        let mut bytes =
            format!("#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n-Y {height} +X {width}\n").into_bytes();
        for _ in 0..height {
            append_constant_new_rle_scanline(&mut bytes, width, [128, 128, 128], 129);
        }
        std::fs::write(&path, bytes).expect("write oversized preview HDR");

        let source = RadianceHdrTiledImageSource::open(&path).expect("open Radiance HDR source");
        let (preview_width, preview_height, pixels) = source
            .generate_sdr_preview(1, 1)
            .expect("generate sampled preview without full-image decode");
        let _ = std::fs::remove_file(&path);

        assert_eq!((preview_width, preview_height), (1, 1));
        assert_eq!(pixels.len(), 4);
        assert!(pixels[0] > 0);
        assert!(pixels[1] > 0);
        assert!(pixels[2] > 0);
        assert_eq!(pixels[3], 255);
    }

    #[test]
    fn open_indexes_radiance_scanline_offsets_for_direct_tile_decode() {
        let path = std::env::temp_dir().join(format!(
            "simple_image_viewer_radiance_scanline_index_{}.hdr",
            std::process::id()
        ));
        let width = 4_u32;
        let height = 4_u32;
        let mut bytes =
            format!("#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n-Y {height} +X {width}\n").into_bytes();
        for row in 0..height {
            append_constant_new_rle_scanline(
                &mut bytes,
                width,
                [32 + row as u8, 64 + row as u8, 96 + row as u8],
                129,
            );
        }
        std::fs::write(&path, bytes).expect("write indexed HDR");

        let source = RadianceHdrTiledImageSource::open(&path).expect("open Radiance HDR source");
        assert_eq!(source.scanline_offsets.len(), height as usize);

        let tile = source
            .extract_tile_rgba32f_arc(1, 3, 2, 1)
            .expect("extract deep tile via scanline offset index");
        let _ = std::fs::remove_file(&path);

        let expected = f32::from(32_u8 + 3) * 2.0_f32.powi(129 - 128 - 8);
        assert!((tile.rgba_f32[0] - expected).abs() < 0.001);
        assert!((tile.rgba_f32[4] - expected).abs() < 0.001);
    }

    fn append_constant_new_rle_scanline(
        bytes: &mut Vec<u8>,
        width: u32,
        rgb: [u8; 3],
        exponent: u8,
    ) {
        bytes.extend_from_slice(&[2, 2, (width >> 8) as u8, (width & 0xff) as u8]);
        for value in [rgb[0], rgb[1], rgb[2], exponent] {
            let mut remaining = width;
            while remaining > 0 {
                let run = remaining.min(127);
                bytes.push(128 + run as u8);
                bytes.push(value);
                remaining -= run;
            }
        }
    }
}
