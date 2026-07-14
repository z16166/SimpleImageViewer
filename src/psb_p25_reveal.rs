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

//! Pure-geometry PSD/PSB P2.5 max-bounding-box reveal helpers.

use crate::psb_layer_composite::{
    LayerRecord, SECTION_TYPE_BOUNDING_DIVIDER, SECTION_TYPE_CLOSED_FOLDER,
    SECTION_TYPE_LAYER_GROUP, SECTION_TYPE_OPEN_FOLDER, compute_effective_visibility,
    dimensions_within_limit,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaxBboxSelection {
    pub root_index: usize,
    pub root_name: String,
    pub member_indices: Vec<usize>,
}

/// Max top-level bbox candidates tried by P2.5b heuristic strategy.
///
/// Capped at 3: trying every top-level group would re-composite large stacks
/// repeatedly; the largest few bboxes catch the usual "hero art hidden in a
/// group" cases without a full combinatorial search. Raise only with a measured
/// cost/benefit trade-off on huge layer trees.
pub const P25B_MAX_CANDIDATES: usize = 3;

/// Convenience wrapper: largest top-level bbox only (limit 1).
/// Kept for unit tests and single-candidate call sites.
#[allow(dead_code)]
pub fn select_max_bbox_top_level(records: &[LayerRecord]) -> Option<MaxBboxSelection> {
    rank_max_bbox_top_level(records, 1).into_iter().next()
}

pub fn rank_max_bbox_top_level(records: &[LayerRecord], limit: usize) -> Vec<MaxBboxSelection> {
    if limit == 0 {
        return Vec::new();
    }
    let mut ranked: Vec<(u64, MaxBboxSelection)> = Vec::new();
    let mut open_group_starts = Vec::new();

    for (index, record) in records.iter().enumerate() {
        let section = record.section_type.filter(|_| record.is_section_divider);
        if section == Some(SECTION_TYPE_BOUNDING_DIVIDER) {
            open_group_starts.push(index);
            continue;
        }
        if matches!(
            section,
            Some(SECTION_TYPE_OPEN_FOLDER)
                | Some(SECTION_TYPE_CLOSED_FOLDER)
                | Some(SECTION_TYPE_LAYER_GROUP)
        ) {
            let Some(start) = open_group_starts.pop() else {
                continue;
            };
            // Still inside a nested group — not a top-level close yet.
            if !open_group_starts.is_empty() {
                continue;
            }
            let members: Vec<usize> = (start..=index).collect();
            push_candidate(records, index, members, &mut ranked);
            continue;
        }
        if open_group_starts.is_empty() {
            let members = vec![index];
            push_candidate(records, index, members, &mut ranked);
        }
    }

    ranked.sort_by(|(area_a, sel_a), (area_b, sel_b)| {
        area_b
            .cmp(area_a)
            .then_with(|| sel_a.root_index.cmp(&sel_b.root_index))
    });
    ranked
        .into_iter()
        .take(limit)
        .map(|(_, selection)| selection)
        .collect()
}

pub fn visibility_respect_subtree(records: &[LayerRecord], members: &[usize]) -> Vec<bool> {
    let effective = compute_effective_visibility(records);
    let mut visible = vec![false; records.len()];
    for &index in members {
        if let Some(slot) = visible.get_mut(index) {
            *slot = effective.get(index).copied().unwrap_or(false);
        }
    }
    visible
}

pub fn visibility_force_open_subtree(records: &[LayerRecord], members: &[usize]) -> Vec<bool> {
    let mut visible = vec![false; records.len()];
    for &index in members {
        let Some(record) = records.get(index) else {
            continue;
        };
        if is_drawable_leaf(record) {
            visible[index] = true;
        }
    }
    ensure_clip_bases_force_open(records, &mut visible);
    visible
}

/// Force every drawable leaf visible, ignoring Photoshop visibility flags.
///
/// Used by the experimental P2.5b path that composites the full layer stack
/// instead of top-N max-bbox subtree candidates.
pub fn visibility_force_open_all(records: &[LayerRecord]) -> Vec<bool> {
    let mut visible: Vec<bool> = records.iter().map(is_drawable_leaf).collect();
    ensure_clip_bases_force_open(records, &mut visible);
    visible
}

/// When a clip leaf is force-opened, also open its nearest preceding base so
/// the compositor does not attach the clip to an unrelated open base (or drop
/// it as an orphan). If no drawable base exists, clear the clip and log.
fn ensure_clip_bases_force_open(records: &[LayerRecord], visible: &mut [bool]) {
    if visible.len() != records.len() {
        return;
    }
    for i in 0..records.len() {
        if !visible[i] {
            continue;
        }
        let Some(record) = records.get(i) else {
            continue;
        };
        if record.clipping == 0 || record.is_section_divider {
            continue;
        }
        let Some(base_idx) = find_clip_base_index(records, i) else {
            log::debug!("PSD/PSB P2.5b: force-open skipped orphan clip layer {i} (no base)");
            visible[i] = false;
            continue;
        };
        let Some(base) = records.get(base_idx) else {
            visible[i] = false;
            continue;
        };
        if is_drawable_leaf(base) {
            visible[base_idx] = true;
        } else {
            log::debug!(
                "PSD/PSB P2.5b: force-open skipped clip layer {i}; base {base_idx} is not drawable"
            );
            visible[i] = false;
        }
    }
}

/// Nearest preceding non-divider layer with `clipping == 0` (clipping base).
fn find_clip_base_index(records: &[LayerRecord], clip_index: usize) -> Option<usize> {
    for j in (0..clip_index).rev() {
        let cand = records.get(j)?;
        if cand.is_section_divider {
            // Section dividers do not break clipping chains in Photoshop's
            // bottom-to-top stack; keep scanning.
            continue;
        }
        if cand.clipping == 0 {
            return Some(j);
        }
    }
    None
}

fn push_candidate(
    records: &[LayerRecord],
    root_index: usize,
    member_indices: Vec<usize>,
    ranked: &mut Vec<(u64, MaxBboxSelection)>,
) {
    let Some(area) = bbox_area(records, &member_indices) else {
        return;
    };
    if area == 0 {
        return;
    }
    let root_name = records
        .get(root_index)
        .map(|record| record.name.clone())
        .unwrap_or_default();
    ranked.push((
        area,
        MaxBboxSelection {
            root_index,
            root_name,
            member_indices,
        },
    ));
}

fn bbox_area(records: &[LayerRecord], members: &[usize]) -> Option<u64> {
    let mut bounds: Option<(i64, i64, i64, i64)> = None;
    for &index in members {
        let record = records.get(index)?;
        if record.is_section_divider || record.is_empty_bounds() {
            continue;
        }
        let rect = (
            i64::from(record.left),
            i64::from(record.top),
            i64::from(record.right),
            i64::from(record.bottom),
        );
        bounds = Some(match bounds {
            Some((left, top, right, bottom)) => (
                left.min(rect.0),
                top.min(rect.1),
                right.max(rect.2),
                bottom.max(rect.3),
            ),
            None => rect,
        });
    }

    let (left, top, right, bottom) = bounds?;
    let width = right.checked_sub(left)?;
    let height = bottom.checked_sub(top)?;
    if width <= 0 || height <= 0 {
        return Some(0);
    }
    u64::try_from(width)
        .ok()?
        .checked_mul(u64::try_from(height).ok()?)
}

fn is_drawable_leaf(record: &LayerRecord) -> bool {
    !record.is_section_divider
        && !record.is_empty_bounds()
        && record.opacity > 0
        && dimensions_within_limit(record.width(), record.height())
}

/// Suppress `p25_reveal_err` on `NoDrawableVisibleLayers` (the caller already
/// tracks that flag separately and should prefer the more specific OSD stage).
/// Shared by SDR and HDR main state machines.
pub(crate) fn remember_p25_reveal_err(
    slot: &mut Option<crate::loader::DecodeError>,
    err: crate::loader::DecodeError,
) {
    if !err.is_no_drawable_visible_layers() {
        *slot = Some(err);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        P25B_MAX_CANDIDATES, rank_max_bbox_top_level, select_max_bbox_top_level,
        visibility_force_open_all, visibility_force_open_subtree, visibility_respect_subtree,
    };
    use crate::psb_layer_composite::{
        LayerRecord, SECTION_TYPE_BOUNDING_DIVIDER, SECTION_TYPE_OPEN_FOLDER,
    };

    fn test_record(
        name: &str,
        rect: Option<(i32, i32, i32, i32)>,
        hidden: bool,
        section_type: Option<u32>,
    ) -> LayerRecord {
        let (left, top, right, bottom) = rect.unwrap_or((0, 0, 0, 0));
        LayerRecord {
            top,
            left,
            bottom,
            right,
            name: name.to_string(),
            layer_id: None,
            cmls_payload: None,
            channels: Vec::new(),
            blend: *b"norm",
            opacity: 255,
            fill_opacity: None,
            clipping: 0,
            flags: if hidden { 2 } else { 0 },
            mask_size: 0,
            mask: None,
            real_mask: None,
            vector_mask: None,
            is_section_divider: section_type.is_some(),
            section_type,
        }
    }

    fn layer(name: &str, rect: (i32, i32, i32, i32), hidden: bool) -> LayerRecord {
        test_record(name, Some(rect), hidden, None)
    }

    fn folder(name: &str, hidden: bool) -> LayerRecord {
        test_record(name, None, hidden, Some(SECTION_TYPE_OPEN_FOLDER))
    }

    fn divider() -> LayerRecord {
        test_record(
            "</Layer group>",
            None,
            false,
            Some(SECTION_TYPE_BOUNDING_DIVIDER),
        )
    }

    #[test]
    fn rank_max_bbox_top_level_orders_by_area_and_limits() {
        let records = vec![
            layer("tiny", (0, 0, 2, 2), true),
            layer("large", (0, 0, 20, 20), true),
            layer("medium", (0, 0, 10, 10), true),
        ];
        let ranked = rank_max_bbox_top_level(&records, P25B_MAX_CANDIDATES);
        assert_eq!(ranked.len(), 3);
        assert_eq!(ranked[0].root_name, "large");
        assert_eq!(ranked[1].root_name, "medium");
        assert_eq!(ranked[2].root_name, "tiny");

        let top1 = rank_max_bbox_top_level(&records, 1);
        assert_eq!(top1.len(), 1);
        assert_eq!(top1[0].root_name, "large");
    }

    #[test]
    fn select_max_bbox_top_level_picks_larger_group() {
        // Bottom-to-top PSD record order:
        //   [small group divider, small child, small folder,
        //    large group divider, large child, large folder]
        let records = vec![
            divider(),
            layer("small child", (0, 0, 10, 10), false),
            folder("small group", true),
            divider(),
            layer("large child", (-5, 2, 15, 12), false),
            folder("large group", true),
        ];

        let selection = select_max_bbox_top_level(&records).expect("selection");

        assert_eq!(selection.root_index, 5);
        assert_eq!(selection.root_name, "large group");
        assert_eq!(selection.member_indices, vec![3, 4, 5]);
    }

    #[test]
    fn select_max_bbox_tie_breaks_to_bottommost() {
        let records = vec![
            layer("bottom", (0, 0, 10, 10), true),
            layer("top", (20, 20, 30, 30), false),
        ];

        let selection = select_max_bbox_top_level(&records).expect("selection");

        assert_eq!(selection.root_index, 0);
        assert_eq!(selection.root_name, "bottom");
        assert_eq!(selection.member_indices, vec![0]);
    }

    #[test]
    fn build_visibility_respect_then_force_open() {
        let records = vec![
            divider(),
            layer("hidden child", (0, 0, 8, 8), true),
            layer("visible child", (10, 0, 18, 8), false),
            folder("visible group", false),
            layer("outside", (0, 10, 8, 18), false),
        ];
        let members = vec![0, 1, 2, 3];

        let respected = visibility_respect_subtree(&records, &members);

        assert_eq!(respected, vec![true, false, true, true, false]);

        let all_hidden_records = vec![
            divider(),
            layer("hidden child a", (0, 0, 8, 8), true),
            layer("hidden child b", (10, 0, 18, 8), true),
            folder("hidden group", true),
            layer("outside", (0, 10, 8, 18), false),
        ];
        let all_hidden_respected = visibility_respect_subtree(&all_hidden_records, &members);
        assert!(
            all_hidden_respected.iter().all(|visible| !visible),
            "Pass1 should respect hidden leaves and hidden ancestor groups"
        );

        let forced = visibility_force_open_subtree(&all_hidden_records, &members);

        assert_eq!(forced, vec![false, true, true, false, false]);

        let force_all = visibility_force_open_all(&all_hidden_records);
        assert_eq!(
            force_all,
            vec![false, true, true, false, true],
            "force-open-all must ignore visibility and include every drawable leaf"
        );
    }

    #[test]
    fn force_open_also_opens_clip_base() {
        // Base at index 0 (hidden), clip at index 1 (hidden). Force-open must
        // enable both so the compositor does not orphan the clip.
        let mut base = layer("base", (0, 0, 8, 8), true);
        base.clipping = 0;
        let mut clip = layer("clip", (0, 0, 8, 8), true);
        clip.clipping = 1;
        let records = vec![base, clip];

        let forced = visibility_force_open_all(&records);
        assert!(forced[0], "base must be force-opened with its clip");
        assert!(forced[1], "drawable clip must remain open");

        // Opacity-0 base cannot be opened; clip must be cleared.
        let mut dead_base = layer("dead base", (0, 0, 8, 8), true);
        dead_base.opacity = 0;
        dead_base.clipping = 0;
        let mut orphan_clip = layer("orphan clip", (0, 0, 8, 8), true);
        orphan_clip.clipping = 1;
        let records = vec![dead_base, orphan_clip];
        let forced = visibility_force_open_all(&records);
        assert!(!forced[0]);
        assert!(!forced[1], "clip without drawable base must be skipped");
    }
}
