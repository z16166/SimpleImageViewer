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

//! Per-tile HDR layer compositor for disk-backed PSD/PSB documents.
//!
//! Composites a single output tile directly from the parsed layer stack,
//! never materializing a full-canvas RGBA f32 buffer. Each visible drawable
//! layer that intersects the tile is decoded (layer-sized) and blended onto a
//! tile-sized canvas via a tile-local [`ClipBlendStateF32`] with layer
//! left/top rebased into tile coordinates.
//!
//! Clipping correctness is per-tile: clip groups are masked by the base-alpha
//! silhouette clipped to the tile, which is exact for any tile a base
//! intersects. A clip whose base lies entirely outside the tile contributes
//! nothing there (its silhouette mask is empty), so skipping it is correct.

use std::sync::atomic::AtomicBool;

use crate::hdr::tiled::HdrTileBuffer;
use crate::hdr::types::{HdrColorSpace, HdrImageMetadata, HdrTransferFunction};
use crate::loader::DecodeError;
use crate::psb_hdr_composite::{
    ClipBlendStateF32, ClipLayerRefF32, LayerF32DecodeArgs, decode_layer_to_f32,
};
use crate::psb_layer_composite::LayerInfo;
use crate::psb_layer_decode::layer_channel_byte_ranges;
use crate::psb_reader::PSD_COLOR_MODE_CMYK;

/// Whether a layer rect `[left, right) x [top, bottom)` overlaps the tile
/// `[tile_x, tile_x + tile_w) x [tile_y, tile_y + tile_h)`.
///
/// All arithmetic is done in `i64` so negative layer origins and large tile
/// coordinates cannot overflow.
// A rect-vs-tile predicate is inherently two rectangles' worth of scalars;
// grouping them into structs would obscure the call sites in the tiler loop.
#[allow(clippy::too_many_arguments)]
pub(crate) fn rect_intersects_tile(
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
    tile_x: u32,
    tile_y: u32,
    tile_w: u32,
    tile_h: u32,
) -> bool {
    if right <= left || bottom <= top || tile_w == 0 || tile_h == 0 {
        return false;
    }
    let l = i64::from(left);
    let t = i64::from(top);
    let r = i64::from(right);
    let b = i64::from(bottom);
    let tx0 = i64::from(tile_x);
    let ty0 = i64::from(tile_y);
    let tx1 = tx0 + i64::from(tile_w);
    let ty1 = ty0 + i64::from(tile_h);
    l < tx1 && r > tx0 && t < ty1 && b > ty0
}

/// Composite one output tile from a parsed layer stack with an explicit
/// per-record visibility mask (bottom-to-top order).
///
/// Returns a linear-light RGBA f32 [`HdrTileBuffer`] of `tile_w x tile_h`.
/// `doc_metadata` supplies ICC / primaries for the tile buffer (same profile
/// as the parent document).
#[allow(clippy::too_many_arguments)]
pub(crate) fn composite_hdr_tile_with_visibility(
    layer_info: &LayerInfo<'_>,
    visible: &[bool],
    tile_x: u32,
    tile_y: u32,
    tile_w: u32,
    tile_h: u32,
    transfer: HdrTransferFunction,
    sdr_white_nits: f32,
    doc_metadata: &HdrImageMetadata,
    cancel: Option<&AtomicBool>,
) -> Result<HdrTileBuffer, DecodeError> {
    if visible.len() != layer_info.records.len() {
        return Err(DecodeError::Message(
            "PSD/PSB HDR tile visibility mask length mismatch".into(),
        ));
    }
    if tile_w == 0 || tile_h == 0 {
        return Err(DecodeError::Message(
            "PSD/PSB HDR tile dimensions must be non-zero".into(),
        ));
    }

    let pixel_count = (tile_w as usize)
        .checked_mul(tile_h as usize)
        .ok_or_else(|| DecodeError::Message("PSD/PSB HDR tile pixel count overflow".into()))?;
    let canvas_len = pixel_count
        .checked_mul(4)
        .ok_or_else(|| DecodeError::Message("PSD/PSB HDR tile RGBA f32 length overflow".into()))?;

    // CMYK composites over paper white; every other mode starts transparent black.
    let mut canvas = if layer_info.color_mode == PSD_COLOR_MODE_CMYK {
        vec![1.0f32; canvas_len]
    } else {
        vec![0.0f32; canvas_len]
    };

    let ranges = layer_channel_byte_ranges(&layer_info.records, layer_info.channel_data.len())?;
    let mut clip_state = ClipBlendStateF32::new(tile_w, tile_h);

    for (i, record) in layer_info.records.iter().enumerate() {
        crate::psb_reader::check_decode_cancel(cancel)?;

        let will_decode = visible.get(i).copied().unwrap_or(false)
            && !record.is_section_divider
            && !record.is_empty_bounds()
            && record.opacity > 0;
        if !will_decode {
            continue;
        }

        let intersects = rect_intersects_tile(
            record.left,
            record.top,
            record.right,
            record.bottom,
            tile_x,
            tile_y,
            tile_w,
            tile_h,
        );

        if record.clipping == 0 {
            // A base layer always delimits clip groups. When it does not touch
            // the tile, flush the open group without opening a new one (its
            // clips would be masked to an out-of-tile silhouette anyway).
            if !intersects {
                clip_state.finish(&mut canvas, cancel)?;
                continue;
            }
        } else if !intersects {
            // Non-intersecting clip contributes nothing to this tile.
            continue;
        }

        let (start, end) = ranges[i];
        match decode_layer_to_f32(LayerF32DecodeArgs {
            channel_data: &layer_info.channel_data[start..end],
            record,
            color_mode: layer_info.color_mode,
            depth: layer_info.depth,
            is_psb: layer_info.is_psb,
            transfer,
            sdr_white_nits,
            cancel,
        }) {
            Ok(Some(rgba_f32)) => {
                let clip_ref = ClipLayerRefF32 {
                    // Rebase layer origin into tile-local coordinates.
                    left: record.left.saturating_sub(tile_x as i32),
                    top: record.top.saturating_sub(tile_y as i32),
                    width: record.width(),
                    height: record.height(),
                    blend: record.blend,
                    clipping: record.clipping,
                    rgba: &rgba_f32,
                };
                clip_state.push_layer(&mut canvas, &clip_ref, cancel)?;
            }
            // A base that failed to decode still delimits the open group.
            Ok(None) => {
                if record.clipping == 0 {
                    clip_state.finish(&mut canvas, cancel)?;
                }
            }
            Err(e) if e.is_cancelled() => return Err(e),
            Err(e) => {
                log::debug!("PSD/PSB HDR tile layer {i} decode failed (skipped): {e}");
                if record.clipping == 0 {
                    clip_state.finish(&mut canvas, cancel)?;
                }
            }
        }
    }
    clip_state.finish(&mut canvas, cancel)?;

    // Prefer document ICC primaries (Rec.2020 / Display P3 / sRGB) over a
    // hardcoded LinearSrgb tag so wide-gamut tiles match the parent buffer.
    let color_space = match doc_metadata.color_space_hint() {
        HdrColorSpace::Unknown => HdrColorSpace::LinearSrgb,
        cs => cs,
    };
    let mut metadata = doc_metadata.clone();
    metadata.transfer_function = HdrTransferFunction::Linear;

    Ok(HdrTileBuffer::new_with_metadata(
        tile_w,
        tile_h,
        color_space,
        metadata,
        std::sync::Arc::new(canvas),
    ))
}

#[cfg(test)]
mod tests {
    use super::rect_intersects_tile;

    #[test]
    fn rect_intersects_tile_overlap_and_disjoint() {
        // Layer [0,4)x[0,4) vs tile at (2,2) 4x4 -> overlap.
        assert!(rect_intersects_tile(0, 0, 4, 4, 2, 2, 4, 4));
        // Adjacent (shares only edge x==4) -> no overlap.
        assert!(!rect_intersects_tile(0, 0, 4, 4, 4, 0, 4, 4));
        // Fully disjoint.
        assert!(!rect_intersects_tile(0, 0, 4, 4, 10, 10, 4, 4));
        // Negative origin layer overlapping tile at origin.
        assert!(rect_intersects_tile(-5, -5, 5, 5, 0, 0, 8, 8));
    }

    #[test]
    fn rect_intersects_tile_rejects_empty() {
        // Empty layer rect.
        assert!(!rect_intersects_tile(4, 4, 4, 4, 0, 0, 8, 8));
        // Zero-size tile.
        assert!(!rect_intersects_tile(0, 0, 4, 4, 0, 0, 0, 8));
    }
}
