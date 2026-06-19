// Image list sort order and comparison helpers.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

use rust_i18n::t;

use super::{DirectoryTreeState, ImageListSortColumn};

pub(super) fn image_list_sort_order(
    len: usize,
    column: ImageListSortColumn,
    ascending: bool,
    paths: &[PathBuf],
    sizes: &[u64],
    modified: &[Option<i64>],
) -> Vec<usize> {
    let mut order: Vec<usize> = (0..len).collect();
    order.sort_by(|&left, &right| {
        let ordering = compare_image_list_sort_keys(left, right, column, paths, sizes, modified);
        let primary = if ascending {
            ordering
        } else {
            ordering.reverse()
        };
        primary.then_with(|| {
            if ascending {
                left.cmp(&right)
            } else {
                right.cmp(&left)
            }
        })
    });
    order
}

pub(super) fn compare_image_list_sort_keys(
    left: usize,
    right: usize,
    column: ImageListSortColumn,
    paths: &[PathBuf],
    sizes: &[u64],
    modified: &[Option<i64>],
) -> Ordering {
    match column {
        ImageListSortColumn::Name => {
            file_name_sort_key(&paths[left]).cmp(&file_name_sort_key(&paths[right]))
        }
        ImageListSortColumn::Size => sizes
            .get(left)
            .copied()
            .unwrap_or(0)
            .cmp(&sizes.get(right).copied().unwrap_or(0)),
        ImageListSortColumn::Modified => compare_optional_unix_time(
            modified.get(left).copied().flatten(),
            modified.get(right).copied().flatten(),
        ),
    }
}

pub(super) fn compare_optional_unix_time(left: Option<i64>, right: Option<i64>) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => left.cmp(&right),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

pub(super) fn file_name_sort_key(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

pub(super) fn image_list_sort_indicator(
    column: ImageListSortColumn,
    state: &DirectoryTreeState,
) -> String {
    if !state.image_list_sort_active || state.image_list_sort_column != column {
        return String::new();
    }
    if state.image_list_sort_ascending {
        t!("directory_tree.sort_asc").to_string()
    } else {
        t!("directory_tree.sort_desc").to_string()
    }
}
