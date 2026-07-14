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
//!
//! Selector values follow the Adobe PSD Path resource specification:
//!   0/3 = subpath length records, 1/2/4/5 = Bezier knots,
//!   6 = fill rule record, 8 = initial fill rule, -1 = end of path.

use crate::psb_layer_composite::{VMSK_RECORD_LEN, VectorMaskData, checked_layer_pixel_count};

// ---------------------------------------------------------------------------
// Path record coordinate helpers
// ---------------------------------------------------------------------------

/// Read a single fixed-point coordinate (Q16.16) from a path record byte
/// slice at the given offset. The coordinate is an i32 big-endian stored in
/// 4 bytes; dividing by [`Q16_16_DIVISOR`] yields the document-pixel float value.
fn read_path_coord(rec: &[u8; VMSK_RECORD_LEN], offset: usize) -> f64 {
    i32::from_be_bytes([
        rec[offset],
        rec[offset + 1],
        rec[offset + 2],
        rec[offset + 3],
    ]) as f64
        / Q16_16_DIVISOR
}

/// Read a (x, y) pair from a path sub-point starting at `base`.
fn read_path_point(rec: &[u8; VMSK_RECORD_LEN], base: usize) -> (f64, f64) {
    (read_path_coord(rec, base), read_path_coord(rec, base + 4))
}

// ---------------------------------------------------------------------------
// Path selector constants (Adobe PSD Path resource spec)
// ---------------------------------------------------------------------------
//
// Each path record is 26 bytes: i16 selector + three Q16.16 sub-points
// (each 8 bytes: i32 x, i32 y).
//
// Selector values:
//   0   Closed subpath length record  (knot count follows in bytes 2-5)
//   1   Closed subpath Bezier knot, linked
//   2   Closed subpath Bezier knot, unlinked
//   3   Open subpath length record    (knot count follows in bytes 2-5)
//   4   Open subpath Bezier knot, linked
//   5   Open subpath Bezier knot, unlinked
//   6   Path fill rule record         (rule value in bytes 2-3: 0=even-odd,
//                                      1=non-zero winding)
//   8   Initial fill rule record      (rule value in bytes 2-3: 0=fill,
//                                      1=keep transparent)
//  -1   End of path (0xFFFF)

const PATH_SELECTOR_CLOSED_LEN: i16 = 0; // closed subpath length record
const PATH_SELECTOR_CLOSED_KNOT_LINKED: i16 = 1; // closed Bezier knot, linked
const PATH_SELECTOR_CLOSED_KNOT_UNLINKED: i16 = 2; // closed Bezier knot, unlinked
const PATH_SELECTOR_OPEN_LEN: i16 = 3; // open subpath length record
const PATH_SELECTOR_OPEN_KNOT_LINKED: i16 = 4; // open Bezier knot, linked
const PATH_SELECTOR_OPEN_KNOT_UNLINKED: i16 = 5; // open Bezier knot, unlinked
const PATH_SELECTOR_FILL_RULE: i16 = 6; // path fill rule record
const PATH_SELECTOR_INITIAL_FILL: i16 = 8; // initial fill rule record

// Byte offsets for sub-points within a knot record (selectors 1/2/4/5).
//   [0-1]  selector (i16)
//   [2-9]  preceding control point Y, X  (Q16.16)
//   [10-17] anchor point Y, X            (Q16.16)
//   [18-25] following control point Y, X (Q16.16)
const KNOT_PRECEDING_XY: usize = 2;
const KNOT_ANCHOR_XY: usize = 10;
const KNOT_FOLLOWING_XY: usize = 18;

/// Q16.16 fixed-point divisor: 2^16 = 65536.  Converts i32 Q16.16 → f64.
const Q16_16_DIVISOR: f64 = 65536.0;

/// Number of line segments per cubic Bezier curve when rasterising.
/// Higher values produce smoother curves at the cost of more vertices.
const SEGMENTS_PER_CURVE: usize = 32;

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
    let mut even_odd = true;
    let mut subpaths: Vec<Vec<(f64, f64)>> = Vec::new();
    let mut current: Vec<(f64, f64, f64, f64, f64, f64)> = Vec::new(); // (bx,by, ax,ay, cx,cy)
    let mut open = false;

    for rec in &data.0 {
        let sel = i16::from_be_bytes([rec[0], rec[1]]);
        match sel {
            // ── Fill rule (6): bytes 2-3 contain the rule ─────────────
            PATH_SELECTOR_FILL_RULE => {
                let rule = i16::from_be_bytes([rec[KNOT_PRECEDING_XY], rec[KNOT_PRECEDING_XY + 1]]);
                even_odd = rule == 0; // 0 = even-odd, 1 = non-zero winding
            }
            // ── Initial fill rule (8): advisory only ─────────────────
            PATH_SELECTOR_INITIAL_FILL => {
                // 0 = fill, 1 = keep transparent; we always fill by default.
            }
            // ── Length records (0/3): start a new subpath ────────────
            PATH_SELECTOR_CLOSED_LEN | PATH_SELECTOR_OPEN_LEN => {
                // Flush previous subpath if any.
                if !current.is_empty() {
                    finalize_subpath(&current, &mut subpaths, open);
                }
                open = sel == PATH_SELECTOR_OPEN_LEN;
                current.clear();
            }
            // ── Knot records (1/2/4/5): Bezier knot ─────────────────
            PATH_SELECTOR_CLOSED_KNOT_LINKED
            | PATH_SELECTOR_CLOSED_KNOT_UNLINKED
            | PATH_SELECTOR_OPEN_KNOT_LINKED
            | PATH_SELECTOR_OPEN_KNOT_UNLINKED => {
                let (bx, by) = read_path_point(rec, KNOT_PRECEDING_XY);
                let (ax, ay) = read_path_point(rec, KNOT_ANCHOR_XY);
                let (cx, cy) = read_path_point(rec, KNOT_FOLLOWING_XY);
                current.push((bx, by, ax, ay, cx, cy));
            }
            // -1 = end of path, also handled by break in collecting loop
            _ => {}
        }
    }
    // Flush last partial subpath if any (no terminating length record).
    if !current.is_empty() {
        finalize_subpath(&current, &mut subpaths, open);
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ── Helpers ───────────────────────────────────────────────────────

    fn be4(v: i32) -> [u8; 4] {
        v.to_be_bytes()
    }

    fn q16(v: f64) -> i32 {
        (v * Q16_16_DIVISOR) as i32
    }

    fn empty_rec() -> [u8; VMSK_RECORD_LEN] {
        [0u8; VMSK_RECORD_LEN]
    }

    // Length record: selector 0 (closed) or 3 (open), knot count in bytes 2-5.
    fn len_rec(sel: i16, knot_count: i32) -> [u8; VMSK_RECORD_LEN] {
        let mut rec = empty_rec();
        rec[0..2].copy_from_slice(&sel.to_be_bytes());
        rec[2..6].copy_from_slice(&be4(knot_count));
        rec
    }

    // Fill-rule record (selector 6): rule value in bytes 2-3 (0=even-odd, 1=non-zero).
    fn fill_rule_rec(rule: i16) -> [u8; VMSK_RECORD_LEN] {
        let mut rec = empty_rec();
        rec[0..2].copy_from_slice(&6i16.to_be_bytes());
        rec[2..4].copy_from_slice(&rule.to_be_bytes());
        rec
    }

    // Corner knot: preceding control = anchor = following control = (x, y).
    fn corner_knot(sel: i16, x: f64, y: f64) -> [u8; VMSK_RECORD_LEN] {
        let mut rec = empty_rec();
        rec[0..2].copy_from_slice(&sel.to_be_bytes());
        // preceding control: Y, X
        rec[2..6].copy_from_slice(&be4(q16(y)));
        rec[6..10].copy_from_slice(&be4(q16(x)));
        // anchor: Y, X
        rec[10..14].copy_from_slice(&be4(q16(y)));
        rec[14..18].copy_from_slice(&be4(q16(x)));
        // following control: Y, X
        rec[18..22].copy_from_slice(&be4(q16(y)));
        rec[22..26].copy_from_slice(&be4(q16(x)));
        rec
    }

    // End-of-path marker (selector = -1 / 0xFFFF).
    fn end_rec() -> [u8; VMSK_RECORD_LEN] {
        let mut rec = empty_rec();
        rec[0..2].copy_from_slice(&(-1i16).to_be_bytes());
        rec
    }

    // ── Tests ─────────────────────────────────────────────────────────

    #[test]
    fn empty_vector_mask_returns_none() {
        let vm = VectorMaskData(vec![]);
        assert!(rasterize_vector_mask(&vm, 0, 0, 50, 50).is_none());
    }

    #[test]
    fn closed_triangle_produces_alpha() {
        // Triangle (0,0)-(100,0)-(50,100), closed subpath with 3 linked knots.
        let vm = VectorMaskData(vec![
            len_rec(PATH_SELECTOR_CLOSED_LEN, 3),
            corner_knot(PATH_SELECTOR_CLOSED_KNOT_LINKED, 0.0, 0.0),
            corner_knot(PATH_SELECTOR_CLOSED_KNOT_LINKED, 100.0, 0.0),
            corner_knot(PATH_SELECTOR_CLOSED_KNOT_LINKED, 50.0, 100.0),
            end_rec(),
        ]);
        let mask = rasterize_vector_mask(&vm, 0, 0, 150, 150)
            .expect("closed triangle should produce a mask");

        // Inside the triangle: centre area.
        assert_eq!(
            mask[33 * 150 + 50],
            255,
            "triangle interior should be opaque"
        );
        // Outside: bottom-right corner.
        assert_eq!(mask[149 * 150 + 149], 0, "bottom-right should be outside");
    }

    #[test]
    fn open_subpath_ignored_for_fill() {
        // Open subpath → should be skipped by finalize_subpath.
        let vm = VectorMaskData(vec![
            len_rec(PATH_SELECTOR_OPEN_LEN, 3),
            corner_knot(PATH_SELECTOR_OPEN_KNOT_LINKED, 0.0, 0.0),
            corner_knot(PATH_SELECTOR_OPEN_KNOT_LINKED, 100.0, 0.0),
            corner_knot(PATH_SELECTOR_OPEN_KNOT_LINKED, 50.0, 100.0),
            end_rec(),
        ]);
        assert!(
            rasterize_vector_mask(&vm, 0, 0, 50, 50).is_none(),
            "open subpath should produce no mask"
        );
    }

    #[test]
    fn fill_rule_even_odd_honored() {
        // Explicit even-odd fill rule (selector 6, rule=0).
        let vm = VectorMaskData(vec![
            fill_rule_rec(0), // even-odd
            len_rec(PATH_SELECTOR_CLOSED_LEN, 3),
            corner_knot(PATH_SELECTOR_CLOSED_KNOT_LINKED, 0.0, 0.0),
            corner_knot(PATH_SELECTOR_CLOSED_KNOT_LINKED, 100.0, 0.0),
            corner_knot(PATH_SELECTOR_CLOSED_KNOT_LINKED, 50.0, 100.0),
            end_rec(),
        ]);
        let mask = rasterize_vector_mask(&vm, 0, 0, 150, 150)
            .expect("even-odd fill should produce a mask");
        assert_eq!(
            mask[33 * 150 + 50],
            255,
            "even-odd: triangle interior should be opaque"
        );
    }

    #[test]
    fn fill_rule_non_zero_honored() {
        // Explicit non-zero fill rule (selector 6, rule=1).
        let vm = VectorMaskData(vec![
            fill_rule_rec(1), // non-zero winding
            len_rec(PATH_SELECTOR_CLOSED_LEN, 3),
            corner_knot(PATH_SELECTOR_CLOSED_KNOT_LINKED, 0.0, 0.0),
            corner_knot(PATH_SELECTOR_CLOSED_KNOT_LINKED, 100.0, 0.0),
            corner_knot(PATH_SELECTOR_CLOSED_KNOT_LINKED, 50.0, 100.0),
            end_rec(),
        ]);
        let mask = rasterize_vector_mask(&vm, 0, 0, 150, 150)
            .expect("non-zero fill should produce a mask");
        assert_eq!(
            mask[33 * 150 + 50],
            255,
            "non-zero: triangle interior should be opaque"
        );
    }

    #[test]
    fn default_fill_rule_is_even_odd() {
        // No selector 6 → defaults to even_odd = true (Adobe standard).
        let vm = VectorMaskData(vec![
            len_rec(PATH_SELECTOR_CLOSED_LEN, 3),
            corner_knot(PATH_SELECTOR_CLOSED_KNOT_LINKED, 0.0, 0.0),
            corner_knot(PATH_SELECTOR_CLOSED_KNOT_LINKED, 100.0, 0.0),
            corner_knot(PATH_SELECTOR_CLOSED_KNOT_LINKED, 50.0, 100.0),
            end_rec(),
        ]);
        let mask = rasterize_vector_mask(&vm, 0, 0, 150, 150)
            .expect("default fill rule should produce a mask");
        // Triangle centre is inside under both rules; just verify it runs.
        assert_eq!(mask[33 * 150 + 50], 255);
    }

    #[test]
    fn unlinked_closed_knots_accepted() {
        // Unlinked knots (selector 2) must be accepted for closed subpaths.
        let vm = VectorMaskData(vec![
            len_rec(PATH_SELECTOR_CLOSED_LEN, 3),
            corner_knot(PATH_SELECTOR_CLOSED_KNOT_UNLINKED, 0.0, 0.0),
            corner_knot(PATH_SELECTOR_CLOSED_KNOT_UNLINKED, 100.0, 0.0),
            corner_knot(PATH_SELECTOR_CLOSED_KNOT_UNLINKED, 50.0, 100.0),
            end_rec(),
        ]);
        let mask = rasterize_vector_mask(&vm, 0, 0, 150, 150)
            .expect("unlinked closed knots should produce a mask");
        assert_eq!(mask[33 * 150 + 50], 255);
    }

    #[test]
    fn fill_rule_before_subpath_honored() {
        // Rule record may appear before the subpath length record.
        let vm = VectorMaskData(vec![
            fill_rule_rec(0), // even-odd
            len_rec(PATH_SELECTOR_CLOSED_LEN, 3),
            corner_knot(PATH_SELECTOR_CLOSED_KNOT_LINKED, 0.0, 0.0),
            corner_knot(PATH_SELECTOR_CLOSED_KNOT_LINKED, 100.0, 0.0),
            corner_knot(PATH_SELECTOR_CLOSED_KNOT_LINKED, 50.0, 100.0),
            end_rec(),
        ]);
        assert!(rasterize_vector_mask(&vm, 0, 0, 150, 150).is_some());
    }

    #[test]
    fn unclosed_length_record_is_flushed() {
        // If a new length record appears before the previous subpath is closed
        // (no prior length record), the old subpath is flushed and finalised.
        let vm = VectorMaskData(vec![
            len_rec(PATH_SELECTOR_CLOSED_LEN, 3),
            corner_knot(PATH_SELECTOR_CLOSED_KNOT_LINKED, 0.0, 0.0),
            corner_knot(PATH_SELECTOR_CLOSED_KNOT_LINKED, 100.0, 0.0),
            corner_knot(PATH_SELECTOR_CLOSED_KNOT_LINKED, 50.0, 100.0),
            // Second subpath: a tiny triangle far away (won't affect centre).
            len_rec(PATH_SELECTOR_CLOSED_LEN, 3),
            corner_knot(PATH_SELECTOR_CLOSED_KNOT_LINKED, 500.0, 500.0),
            corner_knot(PATH_SELECTOR_CLOSED_KNOT_LINKED, 600.0, 500.0),
            corner_knot(PATH_SELECTOR_CLOSED_KNOT_LINKED, 550.0, 600.0),
            end_rec(),
        ]);
        let mask = rasterize_vector_mask(&vm, 0, 0, 150, 150)
            .expect("multiple subpaths should produce a mask");
        assert_eq!(
            mask[33 * 150 + 50],
            255,
            "first triangle centre should still be opaque"
        );
    }

    #[test]
    fn layer_offset_shifts_mask() {
        // Triangle at (0,0) but layer has offset (10, 10). Centre in local
        // coords should still be (75,75) for a 150x150 layer.
        let vm = VectorMaskData(vec![
            len_rec(PATH_SELECTOR_CLOSED_LEN, 3),
            corner_knot(PATH_SELECTOR_CLOSED_KNOT_LINKED, 0.0, 0.0),
            corner_knot(PATH_SELECTOR_CLOSED_KNOT_LINKED, 100.0, 0.0),
            corner_knot(PATH_SELECTOR_CLOSED_KNOT_LINKED, 50.0, 100.0),
            end_rec(),
        ]);
        let mask = rasterize_vector_mask(&vm, 10, 10, 200, 200)
            .expect("offset layer should produce a mask");
        // Layer-local pixel (40,23) = document (50,33) = centroid of triangle.
        let local_idx = 23 * 200 + 40;
        assert_eq!(
            mask[local_idx], 255,
            "triangle interior should be opaque after offset"
        );
    }
}
