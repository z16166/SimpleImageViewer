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
    LayerRecord, compute_effective_visibility, dimensions_within_limit,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaxBboxSelection {
    pub root_index: usize,
    pub root_name: String,
    pub member_indices: Vec<usize>,
}

pub fn select_max_bbox_top_level(records: &[LayerRecord]) -> Option<MaxBboxSelection> {
    let mut best: Option<(u64, MaxBboxSelection)> = None;
    let mut open_group_starts = Vec::new();

    for (index, record) in records.iter().enumerate() {
        match record.section_type.filter(|_| record.is_section_divider) {
            Some(3) => open_group_starts.push(index),
            Some(1) | Some(2) => {
                let Some(start) = open_group_starts.pop() else {
                    continue;
                };
                if open_group_starts.is_empty() {
                    let members: Vec<usize> = (start..=index).collect();
                    consider_candidate(records, index, members, &mut best);
                }
            }
            _ if open_group_starts.is_empty() => {
                let members = vec![index];
                consider_candidate(records, index, members, &mut best);
            }
            _ => {}
        }
    }

    best.map(|(_, selection)| selection)
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
    visible
}

fn consider_candidate(
    records: &[LayerRecord],
    root_index: usize,
    member_indices: Vec<usize>,
    best: &mut Option<(u64, MaxBboxSelection)>,
) {
    let Some(area) = bbox_area(records, &member_indices) else {
        return;
    };
    if area == 0 {
        return;
    }
    let should_replace = best.as_ref().is_none_or(|(best_area, best_selection)| {
        area > *best_area || (area == *best_area && root_index < best_selection.root_index)
    });
    if should_replace {
        let root_name = records
            .get(root_index)
            .map(|record| record.name.clone())
            .unwrap_or_default();
        *best = Some((
            area,
            MaxBboxSelection {
                root_index,
                root_name,
                member_indices,
            },
        ));
    }
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

#[cfg(test)]
mod tests {
    use super::{
        select_max_bbox_top_level, visibility_force_open_subtree, visibility_respect_subtree,
    };
    use crate::psb_layer_composite::LayerRecord;

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
            clipping: 0,
            flags: if hidden { 2 } else { 0 },
            mask_size: 0,
            mask: None,
            real_mask: None,
            is_section_divider: section_type.is_some(),
            section_type,
        }
    }

    fn layer(name: &str, rect: (i32, i32, i32, i32), hidden: bool) -> LayerRecord {
        test_record(name, Some(rect), hidden, None)
    }

    fn folder(name: &str, hidden: bool) -> LayerRecord {
        test_record(name, None, hidden, Some(1))
    }

    fn divider() -> LayerRecord {
        test_record("</Layer group>", None, false, Some(3))
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
    }
}
