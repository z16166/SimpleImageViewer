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

//! PSD/PSB vector mask (vmsk/vsms) path parsing and rasterisation.
//!
//! Extracted from `psb_layer_decode` to keep that module under the 2000-line
//! threshold (checklist #12). Path records are 26 bytes each (i16 selector +
//! three Q16.16 sub-points); the rasteriser produces a layer-sized alpha matte
//! using even-odd or non-zero winding fill.

use crate::psb_layer_composite::{VMSK_RECORD_LEN, VectorMaskData, checked_layer_pixel_count};

// ---------------------------------------------------------------------------
// Path record coordinate helpers
// ---------------------------------------------------------------------------

/// Read a single fixed-point coordinate (Q16.16) from a path record byte
/// slice at the given offset. The coordinate is an i32 big-endian stored in
/// 4 bytes; dividing by 65536.0 yields the document-pixel float value.
fn read_path_coord(rec: &[u8; VMSK_RECORD_LEN], offset: usize) -> f64 {
    i32::from_be_bytes([
        rec[offset],
        rec[offset + 1],
        rec[offset + 2],
        rec[offset + 3],
    ]) as f64
        / 65536.0
}

/// Read a (x, y) pair from a path sub-point starting at `base`.
fn read_path_point(rec: &[u8; VMSK_RECORD_LEN], base: usize) -> (f64, f64) {
    (read_path_coord(rec, base), read_path_coord(rec, base + 4))
}

// ---------------------------------------------------------------------------
// Path selector constants (Adobe PSD spec)
// ---------------------------------------------------------------------------

const PATH_SELECTOR_CLOSED_START: i16 = 0; // closed subpath start (first knot)
const PATH_SELECTOR_OPEN_START: i16 = 2; // open subpath start (first knot)
const PATH_SELECTOR_KNOT_LINKED: i16 = 4; // bezier knot, linked
const PATH_SELECTOR_KNOT_UNLINKED: i16 = 5; // bezier knot, unlinked
const PATH_SELECTOR_SUBPATH_END: i16 = 6; // subpath end (padding)
const PATH_SELECTOR_EVEN_ODD: i16 = -2; // 0xFFFE: even-odd fill rule
const PATH_SELECTOR_NON_ZERO: i16 = -3; // 0xFFFD: non-zero winding fill rule

// ---------------------------------------------------------------------------
// Path parsing
// ---------------------------------------------------------------------------

/// Parse a vector mask's raw path records into subpaths and fill rule.
struct ParsedVectorPaths {
    even_odd: bool,
    /// Each subpath is a list of (anchor_x, anchor_y) tuples in document
    /// pixel space. Open subpaths (not usable for fill) are stored but
    /// may be filtered by the caller.
    subpaths: Vec<Vec<(f64, f64)>>,
}

fn parse_vector_paths(data: &VectorMaskData) -> ParsedVectorPaths {
    let mut even_odd = false;
    let mut subpaths: Vec<Vec<(f64, f64)>> = Vec::new();
    let mut current: Vec<(f64, f64, f64, f64, f64, f64)> = Vec::new(); // (bx,by, ax,ay, cx,cy)
    let mut open = false;

    for rec in &data.0 {
        let sel = i16::from_be_bytes([rec[0], rec[1]]);
        match sel {
            PATH_SELECTOR_EVEN_ODD => even_odd = true,
            PATH_SELECTOR_NON_ZERO => even_odd = false,
            PATH_SELECTOR_CLOSED_START | PATH_SELECTOR_OPEN_START => {
                // Flush previous subpath if any.
                if !current.is_empty() {
                    finalize_subpath(&current, &mut subpaths, open);
                }
                open = sel == PATH_SELECTOR_OPEN_START;
                let (ax, ay) = read_path_point(rec, 10);
                let (cx, cy) = read_path_point(rec, 18);
                current.clear();
                current.push((ax, ay, ax, ay, cx, cy));
            }
            PATH_SELECTOR_KNOT_LINKED | PATH_SELECTOR_KNOT_UNLINKED => {
                if !current.is_empty() {
                    let (bx, by) = read_path_point(rec, 2);
                    let (ax, ay) = read_path_point(rec, 10);
                    let (cx, cy) = read_path_point(rec, 18);
                    current.push((bx, by, ax, ay, cx, cy));
                }
            }
            PATH_SELECTOR_SUBPATH_END => {
                if !current.is_empty() {
                    finalize_subpath(&current, &mut subpaths, open);
                    current.clear();
                }
            }
            _ => {} // -1 = end of path, also handled by break in collecting loop
        }
    }
    // Flush last partial subpath if any (no terminating selector 6).
    if !current.is_empty() {
        finalize_subpath(&current, &mut subpaths, true);
    }

    ParsedVectorPaths { even_odd, subpaths }
}

// ---------------------------------------------------------------------------
// Subpath finalisation (Bezier → polygon)
// ---------------------------------------------------------------------------

fn finalize_subpath(
    knots: &[(f64, f64, f64, f64, f64, f64)],
    subpaths: &mut Vec<Vec<(f64, f64)>>,
    is_open: bool,
) {
    if knots.len() < 2 || is_open {
        // Open subpaths cannot form a closed fill shape; ignore.
        return;
    }

    // Subdivide cubic bezier segments into line segments.
    let mut poly: Vec<(f64, f64)> = Vec::new();
    const SEGMENTS_PER_CURVE: usize = 32;

    for i in 0..knots.len() - 1 {
        let (_, _, ax1, ay1, cx1, cy1) = knots[i];
        let (bx2, by2, ax2, ay2, _, _) = knots[i + 1];
        for step in 0..=SEGMENTS_PER_CURVE {
            let t = step as f64 / SEGMENTS_PER_CURVE as f64;
            let mt = 1.0 - t;
            let x = mt * mt * mt * ax1
                + 3.0 * mt * mt * t * cx1
                + 3.0 * mt * t * t * bx2
                + t * t * t * ax2;
            let y = mt * mt * mt * ay1
                + 3.0 * mt * mt * t * cy1
                + 3.0 * mt * t * t * by2
                + t * t * t * ay2;
            if step > 0 {
                poly.push((x, y));
            }
        }
    }
    // Close the loop: last knot's anchor → first knot's anchor.
    let (_, _, last_ax, last_ay, last_cx, last_cy) = knots[knots.len() - 1];
    let (first_bx, first_by, first_ax, first_ay, _, _) = knots[0];
    for step in 0..=SEGMENTS_PER_CURVE {
        let t = step as f64 / SEGMENTS_PER_CURVE as f64;
        let mt = 1.0 - t;
        let x = mt * mt * mt * last_ax
            + 3.0 * mt * mt * t * last_cx
            + 3.0 * mt * t * t * first_bx
            + t * t * t * first_ax;
        let y = mt * mt * mt * last_ay
            + 3.0 * mt * mt * t * last_cy
            + 3.0 * mt * t * t * first_by
            + t * t * t * first_ay;
        if step > 0 {
            // Push every non-zero step (including t=1.0) so the closing
            // segment emits the same number of points as a normal segment.
            poly.push((x, y));
        }
    }

    if !poly.is_empty() {
        subpaths.push(poly);
    }
}

// ---------------------------------------------------------------------------
// Rasterisation
// ---------------------------------------------------------------------------

/// Rasterise a vector mask into a layer-sized alpha matte, or `None` when
/// the mask is empty or has only open subpaths (caller keeps no-mask).
///
/// Path coordinates are in document pixel space; the layer rect (`left`,
/// `top`, `w`, `h`) offsets them to layer-local pixels. The returned
/// `Vec<u8>` has one byte per layer pixel (`255` = fully opaque / visible).
pub(crate) fn rasterize_vector_mask(
    vector_mask: &VectorMaskData,
    left: i32,
    top: i32,
    w: u32,
    h: u32,
) -> Option<Vec<u8>> {
    let Some(pixel_count) = checked_layer_pixel_count(w, h) else {
        return None;
    };

    let parsed = parse_vector_paths(vector_mask);
    if parsed.subpaths.is_empty() {
        return None;
    }

    let mut mask = vec![0u8; pixel_count];

    // Layer-local coordinate offset.
    let ox = left as f64;
    let oy = top as f64;

    let even_odd = parsed.even_odd;

    for poly in &parsed.subpaths {
        if poly.len() < 3 {
            continue;
        }

        // Compute bounding box in layer-local coords for quick rejection.
        let mut min_x = f64::MAX;
        let mut max_x = f64::MIN;
        let mut min_y = f64::MAX;
        let mut max_y = f64::MIN;
        for &(px, py) in poly {
            if px < min_x {
                min_x = px;
            }
            if px > max_x {
                max_x = px;
            }
            if py < min_y {
                min_y = py;
            }
            if py > max_y {
                max_y = py;
            }
        }
        let bbox_min_x = (min_x - ox).floor().max(0.0) as u32;
        let bbox_max_x = (max_x - ox).ceil().min(w as f64) as u32;
        let bbox_min_y = (min_y - oy).floor().max(0.0) as u32;
        let bbox_max_y = (max_y - oy).ceil().min(h as f64) as u32;

        if bbox_min_x >= bbox_max_x || bbox_min_y >= bbox_max_y {
            continue;
        }

        // Fill within the bounding box using the appropriate winding rule.
        let n = poly.len();
        for py in bbox_min_y..bbox_max_y {
            let y = py as f64 + oy + 0.5;
            let Some(row_start) = (py as usize).checked_mul(w as usize) else {
                continue;
            };
            for px in bbox_min_x..bbox_max_x {
                let x = px as f64 + ox + 0.5;

                let mut crossings = 0i32;
                let mut j = n - 1;
                for i in 0..n {
                    let (x1, y1) = poly[j];
                    let (x2, y2) = poly[i];
                    if y1 <= y {
                        if y2 > y {
                            let cross = (x2 - x1) * (y - y1) - (y2 - y1) * (x - x1);
                            if cross > 0.0 {
                                crossings += 1;
                            }
                        }
                    } else if y2 <= y {
                        let cross = (x2 - x1) * (y - y1) - (y2 - y1) * (x - x1);
                        if cross < 0.0 {
                            crossings -= 1;
                        }
                    }
                    j = i;
                }

                let inside = if even_odd {
                    crossings & 1 != 0
                } else {
                    crossings != 0
                };
                if inside {
                    let Some(idx) = row_start.checked_add(px as usize) else {
                        continue;
                    };
                    if idx < mask.len() {
                        mask[idx] = 255;
                    }
                }
            }
        }
    }

    Some(mask)
}
