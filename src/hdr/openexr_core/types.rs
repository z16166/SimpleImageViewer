// Simple Image Viewer - A high-performance, cross-platform image viewer
// Copyright (C) 2024-2026 Simple Image Viewer Contributors
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

#![allow(dead_code)]

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

pub(crate) struct OpenExrCorePartInfo {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) data_window_min: (i32, i32),
    pub(crate) data_window_max: (i32, i32),
    pub(crate) storage: i32,
    pub(crate) chunk_count: u32,
    pub(crate) channels: Vec<OpenExrCoreChannelInfo>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OpenExrCoreChannelInfo {
    pub(crate) name: String,
    pub(crate) pixel_type: i32,
    pub(crate) x_sampling: i32,
    pub(crate) y_sampling: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OpenExrCoreRgbaTile {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) rgba: Vec<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChannelRole {
    Red,
    Green,
    Blue,
    Luma,
    Alpha,
    Ry,
    By,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub(crate) struct OpenExrCoreDecodedChunkKey {
    pub(crate) part_index: i32,
    pub(crate) chunk_index: i32,
    pub(crate) origin: (u32, u32),
    pub(crate) size: (u32, u32),
}

#[derive(Debug)]
pub(crate) struct OpenExrCoreDecodedChunk {
    pub(crate) origin: (u32, u32),
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) rgba: Arc<Vec<f32>>,
    pub(crate) byte_size: usize,
}

#[derive(Debug)]
pub(crate) struct OpenExrCoreDecodedChunkCache {
    max_bytes: usize,
    current_bytes: usize,
    entries: HashMap<OpenExrCoreDecodedChunkKey, Arc<OpenExrCoreDecodedChunk>>,
    in_flight: HashSet<OpenExrCoreDecodedChunkKey>,
    lru: VecDeque<OpenExrCoreDecodedChunkKey>,
    #[cfg(test)]
    hits: usize,
    #[cfg(test)]
    misses: usize,
}

impl OpenExrCoreDecodedChunkCache {
    pub(crate) fn new(max_bytes: usize) -> Self {
        Self {
            max_bytes,
            current_bytes: 0,
            entries: HashMap::new(),
            in_flight: HashSet::new(),
            lru: VecDeque::new(),
            #[cfg(test)]
            hits: 0,
            #[cfg(test)]
            misses: 0,
        }
    }

    pub(crate) fn get(
        &mut self,
        key: &OpenExrCoreDecodedChunkKey,
    ) -> Option<Arc<OpenExrCoreDecodedChunk>> {
        let chunk = self.entries.get(key).cloned();
        if chunk.is_some() {
            #[cfg(test)]
            {
                self.hits += 1;
            }
            self.touch(*key);
        } else {
            #[cfg(test)]
            {
                self.misses += 1;
            }
        }
        chunk
    }

    pub(crate) fn begin_decode(&mut self, key: OpenExrCoreDecodedChunkKey) -> bool {
        self.in_flight.insert(key)
    }

    pub(crate) fn finish_decode(&mut self, key: &OpenExrCoreDecodedChunkKey) {
        self.in_flight.remove(key);
    }

    pub(crate) fn insert(
        &mut self,
        key: OpenExrCoreDecodedChunkKey,
        chunk: Arc<OpenExrCoreDecodedChunk>,
    ) {
        if chunk.byte_size > self.max_bytes {
            return;
        }

        if let Some(old) = self.entries.insert(key, Arc::clone(&chunk)) {
            self.current_bytes = self.current_bytes.saturating_sub(old.byte_size);
        }
        self.current_bytes = self.current_bytes.saturating_add(chunk.byte_size);
        self.touch(key);
        self.evict_over_budget();
    }

    #[cfg(test)]
    pub(crate) fn hit_count(&self) -> usize {
        self.hits
    }

    #[cfg(test)]
    pub(crate) fn miss_count(&self) -> usize {
        self.misses
    }

    fn touch(&mut self, key: OpenExrCoreDecodedChunkKey) {
        self.lru.retain(|candidate| candidate != &key);
        self.lru.push_back(key);
    }

    fn evict_over_budget(&mut self) {
        while self.current_bytes > self.max_bytes {
            let Some(key) = self.lru.pop_front() else {
                break;
            };
            if let Some(chunk) = self.entries.remove(&key) {
                self.current_bytes = self.current_bytes.saturating_sub(chunk.byte_size);
            }
        }
    }
}
