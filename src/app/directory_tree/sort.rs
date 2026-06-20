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

//! Image list sort order and comparison helpers.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

use rust_i18n::t;

use super::ImageListSortColumn;

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
        ImageListSortColumn::Name => compare_file_names(&paths[left], &paths[right]),
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

fn compare_file_names(left: &Path, right: &Path) -> Ordering {
    locale_compare_str(&file_name_sort_key(left), &file_name_sort_key(right))
}

fn locale_compare_str(left: &str, right: &str) -> Ordering {
    #[cfg(windows)]
    {
        return windows_locale_compare(left, right);
    }
    #[cfg(target_os = "macos")]
    {
        return macos_locale_compare(left, right);
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        left.to_lowercase().cmp(&right.to_lowercase())
    }
}

#[cfg(windows)]
fn windows_locale_compare(left: &str, right: &str) -> Ordering {
    use windows::Win32::Globalization::CompareStringOrdinal;

    let left_wide: Vec<u16> = left.encode_utf16().collect();
    let right_wide: Vec<u16> = right.encode_utf16().collect();
    let result = unsafe { CompareStringOrdinal(&left_wide, &right_wide, true) };
    match result.0 {
        1 => Ordering::Less,
        2 => Ordering::Equal,
        3 => Ordering::Greater,
        _ => left.to_lowercase().cmp(&right.to_lowercase()),
    }
}

#[cfg(target_os = "macos")]
fn macos_locale_compare(left: &str, right: &str) -> Ordering {
    use core_foundation::base::TCFType;
    use core_foundation::string::{CFString, CFStringCompareFlags};

    let left_cf = CFString::new(left);
    let right_cf = CFString::new(right);
    let flags = CFStringCompareFlags::COMPARE_CASE_INSENSITIVE
        | CFStringCompareFlags::COMPARE_LOCALIZED;
    let result = unsafe {
        core_foundation::string::CFStringCompare(left_cf.as_concrete_TypeRef(), right_cf.as_concrete_TypeRef(), flags)
    };
    match result {
        core_foundation::string::CFComparisonResult::LessThan => Ordering::Less,
        core_foundation::string::CFComparisonResult::EqualTo => Ordering::Equal,
        core_foundation::string::CFComparisonResult::GreaterThan => Ordering::Greater,
    }
}

pub(super) fn file_name_sort_key(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

pub(super) fn image_list_sort_indicator_fields(
    column: ImageListSortColumn,
    sort_active: bool,
    sort_column: ImageListSortColumn,
    sort_ascending: bool,
) -> String {
    if !sort_active || sort_column != column {
        return String::new();
    }
    if sort_ascending {
        t!("directory_tree.sort_asc").to_string()
    } else {
        t!("directory_tree.sort_desc").to_string()
    }
}
