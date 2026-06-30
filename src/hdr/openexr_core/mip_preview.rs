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

//! Tiled OpenEXR mip/ripmap level selection helpers.

use openexr_core_sys as sys;

use super::channels::exr_result;
use super::read_context::OpenExrCoreReadContext;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ExrMipLevelSelection {
    pub(crate) level_x: i32,
    pub(crate) level_y: i32,
}

pub(crate) fn exr_mip_level_dimensions(
    base_width: u32,
    base_height: u32,
    level_x: u32,
    level_y: u32,
) -> (u32, u32) {
    let div_x = 1_u32 << level_x.min(30);
    let div_y = 1_u32 << level_y.min(30);
    (base_width.div_ceil(div_x), base_height.div_ceil(div_y))
}

pub(crate) fn exr_mip_level_tile_grid_valid(
    context: &OpenExrCoreReadContext,
    part_index: i32,
    level_x: i32,
    level_y: i32,
) -> bool {
    if level_x < 0 || level_y < 0 {
        return false;
    }
    let mut count_x = 0_i32;
    let mut count_y = 0_i32;
    exr_result(unsafe {
        sys::exr_get_tile_counts(
            context.raw.cast_const(),
            part_index,
            level_x,
            level_y,
            &mut count_x,
            &mut count_y,
        )
    })
    .is_ok()
        && count_x > 0
        && count_y > 0
}

/// Walk diagonal mip levels until `exr_get_tile_counts` fails.
///
/// Bounded by `log2(max(width, height))` (at most ~32 for 32-bit dimensions).
pub(crate) fn probe_exr_max_mipmap_level(context: &OpenExrCoreReadContext, part_index: i32) -> u32 {
    let mut max_level = 0_u32;
    loop {
        let next = max_level + 1;
        if exr_mip_level_tile_grid_valid(context, part_index, next as i32, next as i32) {
            max_level = next;
        } else {
            break;
        }
    }
    max_level
}

/// Probe ripmap extent along each axis (levels at `(L, 0)` and `(0, L)`).
///
/// Same implicit `log2` bound as [`probe_exr_max_mipmap_level`].
pub(crate) fn probe_exr_max_ripmap_levels(
    context: &OpenExrCoreReadContext,
    part_index: i32,
) -> (u32, u32) {
    let mut max_x = 0_u32;
    loop {
        let next = max_x + 1;
        if exr_mip_level_tile_grid_valid(context, part_index, next as i32, 0) {
            max_x = next;
        } else {
            break;
        }
    }
    let mut max_y = 0_u32;
    loop {
        let next = max_y + 1;
        if exr_mip_level_tile_grid_valid(context, part_index, 0, next as i32) {
            max_y = next;
        } else {
            break;
        }
    }
    (max_x, max_y)
}

/// Candidate `(level_x, level_y)` pairs for mip/ripmap search.
///
/// Pure mipmap files only store diagonal levels `(L, L)`. Ripmaps add `(L, 0)` and `(0, L)`.
fn exr_mip_level_search_candidates(
    max_mipmap: u32,
    max_rip_x: u32,
    max_rip_y: u32,
) -> Vec<(u32, u32)> {
    let pure_mipmap = max_rip_x == 0 && max_rip_y == 0;
    let mut out = Vec::new();
    if pure_mipmap {
        for level in 0..=max_mipmap {
            out.push((level, level));
        }
        return out;
    }

    let mut seen = std::collections::HashSet::new();
    for level in 0..=max_mipmap {
        if seen.insert((level, level)) {
            out.push((level, level));
        }
    }
    for level in 0..=max_rip_x {
        if seen.insert((level, 0)) {
            out.push((level, 0));
        }
    }
    for level in 0..=max_rip_y {
        if seen.insert((0, level)) {
            out.push((0, level));
        }
    }
    out
}

pub(crate) fn select_exr_mip_level_for_max_side<F>(
    base_width: u32,
    base_height: u32,
    max_side: u32,
    max_mipmap: u32,
    max_rip_x: u32,
    max_rip_y: u32,
    level_valid: F,
) -> ExrMipLevelSelection
where
    F: Fn(u32, u32) -> bool,
{
    let mut best: Option<ExrMipLevelSelection> = None;
    for (level_x, level_y) in exr_mip_level_search_candidates(max_mipmap, max_rip_x, max_rip_y) {
        if !level_valid(level_x, level_y) {
            continue;
        }
        let (width, height) = exr_mip_level_dimensions(base_width, base_height, level_x, level_y);
        if width == 0 || height == 0 {
            continue;
        }
        if width.max(height) < max_side {
            continue;
        }
        let better = match best {
            None => true,
            Some(prev) => {
                let (best_w, best_h) = exr_mip_level_dimensions(
                    base_width,
                    base_height,
                    prev.level_x as u32,
                    prev.level_y as u32,
                );
                width.max(height) < best_w.max(best_h)
            }
        };
        if better {
            best = Some(ExrMipLevelSelection {
                level_x: level_x as i32,
                level_y: level_y as i32,
            });
        }
    }
    // Nothing reaches `max_side`: level 0 (full resolution) upscales better than a coarse mip.
    best.unwrap_or(ExrMipLevelSelection {
        level_x: 0,
        level_y: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mip_level_dimensions_halve_each_level() {
        assert_eq!(exr_mip_level_dimensions(4096, 2048, 0, 0), (4096, 2048));
        assert_eq!(exr_mip_level_dimensions(4096, 2048, 1, 1), (2048, 1024));
        assert_eq!(exr_mip_level_dimensions(4097, 2049, 1, 1), (2049, 1025));
    }

    #[test]
    fn search_candidates_pure_mipmap_is_diagonal_only() {
        let candidates = exr_mip_level_search_candidates(3, 0, 0);
        assert_eq!(candidates, vec![(0, 0), (1, 1), (2, 2), (3, 3)]);
    }

    #[test]
    fn search_candidates_ripmap_includes_axis_levels() {
        let mut candidates = exr_mip_level_search_candidates(2, 1, 1);
        candidates.sort();
        assert!(candidates.contains(&(1, 0)));
        assert!(candidates.contains(&(0, 1)));
        assert!(candidates.contains(&(2, 2)));
        assert!(!candidates.contains(&(2, 0)));
    }

    #[test]
    fn select_mip_prefers_smallest_level_covering_max_side() {
        let picked = select_exr_mip_level_for_max_side(8192, 6144, 256, 5, 0, 0, |_, _| true);
        assert_eq!(picked.level_x, 5);
        assert_eq!(picked.level_y, 5);
        let (w, h) =
            exr_mip_level_dimensions(8192, 6144, picked.level_x as u32, picked.level_y as u32);
        assert!(w.max(h) >= 256);
        let (prev_w, prev_h) = exr_mip_level_dimensions(
            8192,
            6144,
            (picked.level_x - 1) as u32,
            (picked.level_y - 1) as u32,
        );
        assert!(prev_w.max(prev_h) >= 256);
    }

    #[test]
    fn select_mip_falls_back_to_level_zero_when_source_smaller_than_target() {
        let picked = select_exr_mip_level_for_max_side(100, 100, 256, 6, 0, 0, |_, _| true);
        assert_eq!(picked.level_x, 0);
        assert_eq!(picked.level_y, 0);
    }
}
