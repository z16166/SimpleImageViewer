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

//! Flat arena storage for directory-tree nodes (Vec + path index).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::DirectoryTreeNode;

#[derive(Default)]
pub(crate) struct DirectoryTreeNodeArena {
    entries: Vec<DirectoryTreeNode>,
    path_index: HashMap<PathBuf, u32>,
}

impl DirectoryTreeNodeArena {
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.path_index.clear();
    }

    pub(crate) fn len(&self) -> usize {
        self.path_index.len()
    }

    pub(crate) fn contains_key(&self, path: &Path) -> bool {
        self.path_index.contains_key(path)
    }

    pub(crate) fn get(&self, path: &Path) -> Option<&DirectoryTreeNode> {
        self.path_index
            .get(path)
            .map(|&id| &self.entries[id as usize])
    }

    pub(crate) fn get_mut(&mut self, path: &Path) -> Option<&mut DirectoryTreeNode> {
        let id = *self.path_index.get(path)?;
        Some(&mut self.entries[id as usize])
    }

    pub(crate) fn insert(&mut self, path: PathBuf, node: DirectoryTreeNode) {
        if let Some(&id) = self.path_index.get(&path) {
            self.entries[id as usize] = node;
            return;
        }
        let id = u32::try_from(self.entries.len()).expect("directory tree node arena overflow");
        self.entries.push(node);
        self.path_index.insert(path, id);
    }

    pub(crate) fn or_insert_with<F: FnOnce() -> DirectoryTreeNode>(
        &mut self,
        path: PathBuf,
        f: F,
    ) -> &mut DirectoryTreeNode {
        if let Some(&id) = self.path_index.get(&path) {
            return &mut self.entries[id as usize];
        }
        let id = u32::try_from(self.entries.len()).expect("directory tree node arena overflow");
        self.entries.push(f());
        self.path_index.insert(path, id);
        &mut self.entries[id as usize]
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&PathBuf, &DirectoryTreeNode)> {
        self.path_index
            .iter()
            .map(|(path, &id)| (path, &self.entries[id as usize]))
    }
}
