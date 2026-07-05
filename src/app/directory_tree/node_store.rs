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

//! Path-keyed arena for directory tree nodes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::DirectoryTreeNode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InsertNodeError {
    CapReached,
    IdOverflow,
}

pub(crate) struct DirectoryTreeNodeArena {
    entries: Vec<DirectoryTreeNode>,
    path_index: HashMap<PathBuf, u32>,
}

impl Default for DirectoryTreeNodeArena {
    fn default() -> Self {
        Self::new()
    }
}

impl DirectoryTreeNodeArena {
    pub(crate) fn new() -> Self {
        Self {
            entries: Vec::new(),
            path_index: HashMap::new(),
        }
    }

    #[allow(dead_code)] // retained for tests and future arena resets
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.path_index.clear();
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    #[allow(dead_code)] // used by directory_tree integration tests
    pub(crate) fn contains_key(&self, path: &Path) -> bool {
        self.path_index.contains_key(path)
    }

    pub(crate) fn get(&self, path: &Path) -> Option<&DirectoryTreeNode> {
        self.path_index
            .get(path)
            .map(|&id| &self.entries[id as usize])
    }

    pub(crate) fn get_mut(&mut self, path: &Path) -> Option<&mut DirectoryTreeNode> {
        self.path_index
            .get(path)
            .map(|&id| &mut self.entries[id as usize])
    }

    pub(crate) fn insert(
        &mut self,
        path: PathBuf,
        node: DirectoryTreeNode,
        max_nodes: usize,
    ) -> Result<(), InsertNodeError> {
        if let Some(&id) = self.path_index.get(&path) {
            self.entries[id as usize] = node;
            return Ok(());
        }
        if self.entries.len() >= max_nodes {
            return Err(InsertNodeError::CapReached);
        }
        let id = u32::try_from(self.entries.len()).map_err(|_| InsertNodeError::IdOverflow)?;
        self.entries.push(node);
        self.path_index.insert(path, id);
        Ok(())
    }

    pub(crate) fn or_insert_with<F: FnOnce() -> DirectoryTreeNode>(
        &mut self,
        path: PathBuf,
        max_nodes: usize,
        f: F,
    ) -> Result<&mut DirectoryTreeNode, InsertNodeError> {
        if let Some(&id) = self.path_index.get(&path) {
            return Ok(&mut self.entries[id as usize]);
        }
        if self.entries.len() >= max_nodes {
            return Err(InsertNodeError::CapReached);
        }
        let id = u32::try_from(self.entries.len()).map_err(|_| InsertNodeError::IdOverflow)?;
        self.entries.push(f());
        self.path_index.insert(path, id);
        Ok(&mut self.entries[id as usize])
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&PathBuf, &DirectoryTreeNode)> {
        self.path_index
            .iter()
            .map(|(path, &id)| (path, &self.entries[id as usize]))
    }

    pub(crate) fn retain<F>(&mut self, mut keep: F)
    where
        F: FnMut(&PathBuf) -> bool,
    {
        let kept: Vec<(PathBuf, DirectoryTreeNode)> = self
            .path_index
            .iter()
            .filter_map(|(path, &id)| {
                if keep(path) {
                    Some((path.clone(), self.entries[id as usize].clone()))
                } else {
                    None
                }
            })
            .collect();
        self.entries.clear();
        self.path_index.clear();
        for (path, node) in kept {
            let id = u32::try_from(self.entries.len()).expect("retain cannot overflow arena ids");
            self.entries.push(node);
            self.path_index.insert(path, id);
        }
    }
}
