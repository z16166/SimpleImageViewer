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

use std::collections::{HashMap, HashSet};

pub(crate) fn permute_usize_hashmap<T>(map: &mut HashMap<usize, T>, old_to_new: &[usize]) {
    let taken = std::mem::take(map);
    for (old_idx, value) in taken {
        if old_idx < old_to_new.len() {
            map.insert(old_to_new[old_idx], value);
        }
    }
}

pub(crate) fn permute_usize_set(set: &mut HashSet<usize>, old_to_new: &[usize]) {
    let taken = std::mem::take(set);
    for old_idx in taken {
        if old_idx < old_to_new.len() {
            set.insert(old_to_new[old_idx]);
        }
    }
}
