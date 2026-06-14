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
use super::types::ImageLoader;

use crate::loader::LoaderOutput;
use crossbeam_channel::TryRecvError;

impl ImageLoader {
    /// True when any generation is in-flight for `index` (including CPU fallback reloads).
    pub fn is_loading_any(&self, index: usize) -> bool {
        self.loading.lock().contains_key(&index)
    }

    /// Drop queued decode results from a previous `generation` so rapid navigation
    /// cannot retain hundreds of megabytes in the unbounded channel / defer queue.
    ///
    /// `also_keep_preview` — when `Some((index, gen))`, Preview results for that
    /// specific (index, generation) are also preserved even though they don't match
    /// `keep_generation`. Used when a prefetched TileManager is promoted to current:
    /// the prefetch-phase HQ preview task carries the old generation and must not be
    /// discarded merely because the generation counter was bumped on promotion.
    pub fn discard_pending_stale_outputs(
        &mut self,
        keep_generation: u64,
        also_keep_preview: Option<(usize, u64)>,
        also_keep_image: impl Fn(usize, u64) -> bool,
    ) {
        let keep = |output: &LoaderOutput| -> bool {
            match output {
                LoaderOutput::Image(r) => {
                    r.generation == keep_generation || also_keep_image(r.index, r.generation)
                }
                LoaderOutput::Preview(p) => {
                    p.generation == keep_generation
                        || also_keep_preview
                            .is_some_and(|(idx, old_gen)| p.index == idx && p.generation == old_gen)
                }
                LoaderOutput::HdrSdrFallback(_) => true,
                LoaderOutput::Refined(_, g) => *g == keep_generation,
                LoaderOutput::Tile(t) => t.generation == keep_generation,
            }
        };

        let mut retained = std::collections::VecDeque::new();
        for output in self.local_queue.drain(..) {
            if keep(&output) {
                retained.push_back(output);
            } else if let LoaderOutput::Image(ref r) = output {
                let mut loading = self.loading.lock();
                if loading.get(&r.index) == Some(&r.generation) {
                    loading.remove(&r.index);
                }
            }
        }
        self.local_queue = retained;

        while let Ok(output) = self.rx.try_recv() {
            if keep(&output) {
                self.local_queue.push_back(output);
            } else if let LoaderOutput::Image(ref r) = output {
                let mut loading = self.loading.lock();
                if loading.get(&r.index) == Some(&r.generation) {
                    loading.remove(&r.index);
                }
            }
        }
    }

    pub fn poll(&mut self) -> Option<LoaderOutput> {
        // Priority: drain deferred items from previous frames first.
        if let Some(output) = self.local_queue.pop_front() {
            return Some(output);
        }

        match self.rx.try_recv() {
            Ok(output) => Some(output),
            Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => None,
        }
    }

    pub fn finish_image_request(&self, index: usize, generation: u64) {
        let mut loading = self.loading.lock();
        if let Some(&g) = loading.get(&index) {
            if g <= generation {
                loading.remove(&index);
            }
        }
    }

    /// Push a result back so it is retried on the next frame.
    /// Used by the UI thread when the per-frame upload quota is reached.
    /// Items are pushed to the FRONT so order is preserved across frames.
    pub fn repush(&mut self, output: LoaderOutput) {
        self.local_queue.push_front(output);
    }

    /// Push a deferred result behind already-queued items.
    /// Use when work should yield to older results instead of preserving current-frame order.
    pub fn repush_back(&mut self, output: LoaderOutput) {
        self.local_queue.push_back(output);
    }

    /// Clear all pending tile requests from the queue.
    /// Called on zoom change to discard tiles from stale zoom levels.
    pub fn flush_tile_queue(&self) {
        let (lock, _) = &*self.tile_queue;
        lock.lock().clear();
    }

    pub fn cancel_all(&mut self) {
        self.loading.lock().clear();
        self.local_queue.clear();
        {
            let (lock, cvar) = &*self.delayed_fallback;
            let mut slot = lock.lock();
            *slot = None;
            cvar.notify_one();
        }
        {
            let (lock, _) = &*self.tile_queue;
            lock.lock().clear();
        }
        while self.rx.try_recv().is_ok() {}
    }

    #[cfg(test)]
    pub(crate) fn test_register_inflight(&self, index: usize, generation: u64) {
        self.loading.lock().insert(index, generation);
    }

    #[cfg(test)]
    pub(crate) fn test_send_loader_output(&self, output: LoaderOutput) {
        self.tx.send(output).expect("test loader channel send");
    }
}
