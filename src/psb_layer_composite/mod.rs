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

//! Layer-aware PSD/PSB compositor.
//!
//! Used when the flattened Image Data section cannot be decoded structurally
//! (see `psb_sdr_main::decode_psd_sdr_main_from_bytes_with_cancel`). Decodes
//! each layer's channels (depth 8) and composites them bottom to top with
//! Normal / Screen / Linear Dodge / Multiply blend + opacity + user mask +
//! clipping groups, respecting strict Photoshop layer/group visibility only
//! (no viewer heuristics that open hidden layers).
//!
//! For 16/32-bit documents the SDR main state machine routes through the
//! f32 compositor in `psb_hdr_composite` and tone-maps to RGBA8; this module's
//! u8 entry points keep the depth==8 guard.

// ── Sub-modules ──────────────────────────────────────────────────────────

mod mask;
mod parse;
#[cfg(test)]
mod tests;
mod types;

// ── Re-exports ────────────────────────────────────────────────────────────

#[allow(unused_imports)]
pub(crate) use mask::{MAX_MASK_FEATHER, apply_mask_density, apply_mask_feather};
#[allow(unused_imports)]
pub(crate) use parse::{
    MAX_LAYER_CHANNELS_PER_RECORD, MAX_LAYER_RECORDS, MAX_TAGGED_BLOCK_RESYNCS_PER_LAYER,
    TaggedBlockScan, parse_layer_records, parse_layer_records_from_index, scan_extra_tagged_blocks,
};
pub(crate) use types::{
    CompositeTiming, LayerChannel, LayerInfo, LayerMaskInfo, LayerRecord,
    MAX_COMPOSITE_DECODED_BYTES, MAX_LAYER_PIXELS, SECTION_TYPE_BOUNDING_DIVIDER,
    SECTION_TYPE_CLOSED_FOLDER, SECTION_TYPE_LAYER_GROUP, SECTION_TYPE_OPEN_FOLDER,
    VMSK_RECORD_LEN, VectorMaskData, VectorMaskFlags, accumulate_decoded_layer_bytes,
    check_streaming_pair_budget, checked_layer_pixel_count, dimensions_within_limit,
};

// ── External imports (pub(crate) so child modules like tests can see them) ─

pub(crate) use crate::psb_layer_decode::{
    StreamingPeakTracker, gpu_batch_eligible_decoded_bytes, run_composite_pass_cpu_streaming,
    run_composite_pass_gpu_batch,
};
use crate::psb_reader::PSD_COLOR_MODE_CMYK;

// ── LAYER_PREFETCH_WINDOW ────────────────────────────────────────────────

/// Max [`DecodedLayer`]s resident at once on the CPU streaming composite
/// path: the layer currently being blended, plus the next one prefetched in
/// parallel on [`crate::psb_layer_decode_pool::PSD_LAYER_DECODE_POOL`].
pub(crate) const LAYER_PREFETCH_WINDOW: usize = 2;
// `run_composite_pass_cpu_streaming` overlaps exactly one prefetch with the
// current layer's blend; the design does not support a wider window.
const _: () = assert!(LAYER_PREFETCH_WINDOW == 2);

// ── Group visibility ─────────────────────────────────────────────────────

// -- Group visibility ------------------------------------------------------

/// Compute per-record effective visibility: a layer is visible only if it
/// and every ancestor group is visible.
///
/// Photoshop stores layer records **bottom to top** in the file (index 0 is
/// the bottommost layer, the last index is the topmost). A group's lsct
/// bounding section divider (type 3, hidden in the UI) is therefore its
/// *first* record in file order (the bottom of the group), while the actual
/// folder record (type 1 open / type 2 closed, carrying the group's own
/// hidden flag) is its *last* record in file order (the top of the group).
///
/// So this walks records in **reverse** (top to bottom, visually): the
/// folder record is seen first and pushes a nested visibility scope (using
/// the group's own hidden flag, which is only known at that point), and the
/// bounding divider is seen last and pops it.
pub(crate) fn compute_effective_visibility(records: &[LayerRecord]) -> Vec<bool> {
    compute_effective_visibility_with_flags(records, None)
}

/// Same as [`compute_effective_visibility`], but optionally overrides each
/// layer's `flags` byte (used by Layer Comp `cmls` without cloning records).
pub(crate) fn compute_effective_visibility_with_flags(
    records: &[LayerRecord],
    flags_override: Option<&[u8]>,
) -> Vec<bool> {
    if let Some(flags) = flags_override {
        debug_assert_eq!(records.len(), flags.len());
    }
    let mut visible = vec![false; records.len()];
    let mut stack: Vec<bool> = vec![true];

    for (i, layer) in records.iter().enumerate().rev() {
        let self_visible = match flags_override {
            Some(flags) => flags.get(i).is_some_and(|f| f & 2 == 0),
            None => !layer.is_hidden(),
        };
        let current = *stack.last().unwrap_or(&true) && self_visible;
        visible[i] = current;

        if layer.is_section_divider {
            match layer.section_type {
                Some(SECTION_TYPE_OPEN_FOLDER)
                | Some(SECTION_TYPE_CLOSED_FOLDER)
                | Some(SECTION_TYPE_LAYER_GROUP) => stack.push(current),
                Some(SECTION_TYPE_BOUNDING_DIVIDER) if stack.len() > 1 => {
                    stack.pop();
                }
                _ => {}
            }
        }
    }

    visible
}

/// True when strict visibility yields at least one pixel layer that can affect
/// the canvas (flag + geometry only; no pixel sampling).
pub(crate) fn strict_visibility_has_drawable_output(
    canvas_w: u32,
    canvas_h: u32,
    records: &[LayerRecord],
    visible: &[bool],
) -> bool {
    if visible.len() != records.len() || canvas_w == 0 || canvas_h == 0 {
        return false;
    }
    let canvas_l = 0i64;
    let canvas_t = 0i64;
    let canvas_r = i64::from(canvas_w);
    let canvas_b = i64::from(canvas_h);

    for (i, record) in records.iter().enumerate() {
        if !visible[i] || record.is_section_divider || record.opacity == 0 {
            continue;
        }
        if record.is_empty_bounds() {
            continue;
        }
        // Empty-rect mask: default_color 0 hides the whole layer; 255 means
        // "fully revealed" (Photoshop) and must not count as no output.
        if let Some(mask) = &record.mask
            && !mask.disabled
            && mask.is_empty_bounds()
            && mask.default_color == 0
        {
            continue;
        }
        let l = i64::from(record.left).max(canvas_l);
        let t = i64::from(record.top).max(canvas_t);
        let r = i64::from(record.right).min(canvas_r);
        let b = i64::from(record.bottom).min(canvas_b);
        if r > l && b > t {
            return true;
        }
    }
    false
}

// -- Full composite ---------------------------------------------------------

/// Display text for [`crate::loader::DecodeError::NoDrawableVisibleLayers`].
pub use crate::loader::STRICT_LAYER_COMPOSITE_BLANK;

/// Decode a PSD/PSB layer stack and composite it into a single RGBA8 canvas
/// (depth 8 only: Normal / Screen / Linear Dodge / Multiply + opacity + user
/// mask + clipping groups + strict group/leaf visibility).
///
/// When `gpu` is provided, the canvas is large enough, every decoded layer
/// uses a GPU-separable blend mode, blending may run on an offscreen wgpu
/// compute path; failures or non-separable stacks fall back to CPU.
///
/// Returns [`crate::loader::DecodeError::NoDrawableVisibleLayers`] when no
/// visible layer intersects the canvas (no pixel work is performed).
pub fn composite_layers_from_bytes_with_cancel(
    bytes: &[u8],
    cancel: Option<&std::sync::atomic::AtomicBool>,
    gpu: Option<&crate::psb_layer_blend_gpu::PsdGpuContext>,
) -> Result<crate::psb_reader::PsbComposite, crate::loader::DecodeError> {
    let total_t0 = std::time::Instant::now();
    crate::psb_reader::check_decode_cancel(cancel)?;
    let parse_t0 = std::time::Instant::now();
    let info = parse_layer_records(bytes)?;
    let parse_ms = parse_t0.elapsed().as_secs_f64() * 1000.0;
    composite_layers_from_info(&info, parse_ms, total_t0, cancel, gpu)
}

/// Same as [`composite_layers_from_bytes_with_cancel`], but reuses an
/// already-parsed [`crate::psb_section_index::PsdSectionIndex`] instead of
/// re-walking the header/color-mode/image-resources/layer-mask sections.
pub fn composite_layers_from_index(
    index: &crate::psb_section_index::PsdSectionIndex,
    bytes: &[u8],
    cancel: Option<&std::sync::atomic::AtomicBool>,
    gpu: Option<&crate::psb_layer_blend_gpu::PsdGpuContext>,
) -> Result<crate::psb_reader::PsbComposite, crate::loader::DecodeError> {
    let total_t0 = std::time::Instant::now();
    crate::psb_reader::check_decode_cancel(cancel)?;
    let parse_t0 = std::time::Instant::now();
    let info = parse_layer_records_from_index(index, bytes)?;
    let parse_ms = parse_t0.elapsed().as_secs_f64() * 1000.0;
    composite_layers_from_info(&info, parse_ms, total_t0, cancel, gpu)
}

/// Strict-visibility composite from an already-parsed [`LayerInfo`].
///
/// Used by the SDR main state machine so P2/P2.5a/P2.5b share one layer-record
/// walk instead of each stage calling [`parse_layer_records_from_index`].
pub(crate) fn composite_layers_from_info(
    info: &LayerInfo<'_>,
    parse_ms: f64,
    total_t0: std::time::Instant,
    cancel: Option<&std::sync::atomic::AtomicBool>,
    gpu: Option<&crate::psb_layer_blend_gpu::PsdGpuContext>,
) -> Result<crate::psb_reader::PsbComposite, crate::loader::DecodeError> {
    let visible = compute_effective_visibility(&info.records);
    composite_layers_with_visibility_from_info(info, &visible, parse_ms, total_t0, cancel, gpu)
}

/// Same as [`composite_layers_from_info`], but takes an explicit per-record
/// `visible` mask instead of deriving it from strict Photoshop layer/group
/// flags via [`compute_effective_visibility`].
///
/// Used by callers that need to override strict flag-based visibility (e.g.
/// a future Layer Comp or max-bounding-box "reveal" pass); ordinary decode
/// paths should go through [`composite_layers_from_info`] instead.
pub(crate) fn composite_layers_with_visibility_from_info(
    info: &LayerInfo<'_>,
    visible: &[bool],
    parse_ms: f64,
    total_t0: std::time::Instant,
    cancel: Option<&std::sync::atomic::AtomicBool>,
    gpu: Option<&crate::psb_layer_blend_gpu::PsdGpuContext>,
) -> Result<crate::psb_reader::PsbComposite, crate::loader::DecodeError> {
    if info.depth != 8 {
        return Err(format!(
            "PSD/PSB layer composite requires 8-bit depth (found {}-bit)",
            info.depth
        )
        .into());
    }
    if visible.len() != info.records.len() {
        return Err("PSD/PSB visibility mask length mismatch".into());
    }

    let canvas_w = info.width;
    let canvas_h = info.height;
    if !dimensions_within_limit(canvas_w, canvas_h) {
        return Err(format!(
            "PSD/PSB composite canvas {canvas_w}x{canvas_h} exceeds document limits"
        )
        .into());
    }
    if !strict_visibility_has_drawable_output(canvas_w, canvas_h, &info.records, visible) {
        return Err(crate::loader::DecodeError::NoDrawableVisibleLayers);
    }

    let canvas_len = (canvas_w as usize)
        .checked_mul(canvas_h as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| "PSD/PSB layer composite canvas size overflow".to_string())?;
    // CMYK documents composite over white paper in Photoshop; starting from
    // transparent black leaves unpainted holes looking like a dark/black page.
    let mut canvas = allocate_composite_canvas(canvas_len, info.color_mode);

    let mut timing = CompositeTiming {
        parse_ms,
        unpack_ms: 0.0,
        cmyk_ms: 0.0,
        blend_ms: 0.0,
        readback_ms: 0.0,
        mode: "cpu",
        layers: 0,
    };

    run_composite_pass(
        info,
        visible,
        &mut canvas,
        canvas_w,
        canvas_h,
        cancel,
        gpu,
        &mut timing,
    )?;

    let total_ms = total_t0.elapsed().as_secs_f64() * 1000.0;
    #[cfg(feature = "preload-debug")]
    crate::preload_debug!(
        "[PreloadDebug][PsdComposite] mode={} parse_ms={:.1} unpack_ms={:.1} cmyk_ms={:.1} \
         blend_ms={:.1} readback_ms={:.1} total_ms={:.1} layers={} {}x{}",
        timing.mode,
        timing.parse_ms,
        timing.unpack_ms,
        timing.cmyk_ms,
        timing.blend_ms,
        timing.readback_ms,
        total_ms,
        timing.layers,
        canvas_w,
        canvas_h
    );
    #[cfg(not(feature = "preload-debug"))]
    let _ = (total_ms, &timing);

    Ok(crate::psb_reader::PsbComposite {
        width: canvas_w,
        height: canvas_h,
        pixels: canvas,
    })
}

fn allocate_composite_canvas(len: usize, color_mode: u16) -> Vec<u8> {
    let mut canvas = vec![0u8; len];
    clear_composite_canvas(&mut canvas, color_mode);
    canvas
}

fn clear_composite_canvas(canvas: &mut [u8], color_mode: u16) {
    if color_mode == PSD_COLOR_MODE_CMYK {
        // CMYK paper white is 255 per channel; SIMD fill beats per-pixel scalar stores.
        crate::psb_packbits_simd::fill_bytes(canvas, 255);
    } else {
        canvas.fill(0);
    }
}

/// Whether `record` will actually decode into a
/// [`crate::psb_layer_decode::DecodedLayer`] (mirrors the skip conditions
/// applied in `decode_one_layer`/`decode_at`): visible, not a section
/// divider, non-empty bounds, non-zero opacity, and within the per-layer
/// dimension/pixel cap. Metadata-only -- never touches channel data.
pub(crate) fn layer_will_decode(record: &LayerRecord, visible: bool) -> bool {
    let should_decode =
        visible && !record.is_section_divider && !record.is_empty_bounds() && record.opacity > 0;
    should_decode && dimensions_within_limit(record.width(), record.height())
}

/// Decode and blend every eligible visible layer bottom to top, returning how
/// many were actually composited.
///
/// Dispatches to one of two strategies:
/// - GPU all-at-once batch ([`run_composite_pass_gpu_batch`]): only when a GPU
///   context is available AND [`gpu_batch_eligible_decoded_bytes`] finds the
///   canvas worthwhile, every composited layer GPU-separable, and peak VRAM
///   (layers + canvas + readback + optional clip scratch) within budget.
/// - CPU streaming ([`run_composite_pass_cpu_streaming`]): the default, and
///   the fallback whenever the GPU batch is not eligible.
#[allow(clippy::too_many_arguments)]
fn run_composite_pass(
    info: &LayerInfo<'_>,
    visible: &[bool],
    canvas: &mut Vec<u8>,
    canvas_w: u32,
    canvas_h: u32,
    cancel: Option<&std::sync::atomic::AtomicBool>,
    gpu: Option<&crate::psb_layer_blend_gpu::PsdGpuContext>,
    timing: &mut CompositeTiming,
) -> Result<usize, crate::loader::DecodeError> {
    let gpu_batch_ctx = gpu
        .filter(|_| gpu_batch_eligible_decoded_bytes(info, visible, canvas_w, canvas_h).is_some());
    if let Some(gpu_ctx) = gpu_batch_ctx {
        return run_composite_pass_gpu_batch(
            info, visible, canvas, canvas_w, canvas_h, cancel, gpu_ctx, timing,
        );
    }
    let peak_tracker = StreamingPeakTracker::default();
    run_composite_pass_cpu_streaming(
        info,
        visible,
        canvas,
        canvas_w,
        canvas_h,
        cancel,
        timing,
        &peak_tracker,
    )
}
