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
use std::sync::Arc;
use std::time::{Duration, Instant};

const LOADER_EXIT_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

impl ImageLoader {
    /// True when any generation is in-flight for `index` (including CPU fallback reloads).
    pub fn is_loading_any(&self, index: usize) -> bool {
        self.loading.lock().contains_key(&index)
    }

    pub(crate) fn active_load_count(&self) -> usize {
        self.loading.lock().len()
    }

    /// Profile / window based pending output filter (generation-plan Phase A).
    pub fn discard_pending_stale_outputs_profile(
        &mut self,
        keep: impl Fn(&LoaderOutput, &std::collections::HashMap<usize, crate::loader::InFlightLoad>) -> bool,
    ) {
        let loading_snapshot = self.loading.lock().clone();
        let keep_output = |output: &LoaderOutput| -> bool {
            match output {
                LoaderOutput::HdrSdrFallback(_) => true,
                _ => keep(output, &loading_snapshot),
            }
        };

        let mut retained = std::collections::VecDeque::new();
        for output in self.local_queue.drain(..) {
            if keep_output(&output) {
                retained.push_back(output);
            } else if let LoaderOutput::Image(ref r) = output {
                let mut loading = self.loading.lock();
                if loading
                    .get(&r.index)
                    .is_some_and(|e| e.profile == r.decode_profile)
                {
                    loading.remove(&r.index);
                }
            }
        }
        self.local_queue = retained;

        while let Ok(output) = self.rx.try_recv() {
            if keep_output(&output) {
                self.local_queue.push_back(output);
            } else if let LoaderOutput::Image(ref r) = output {
                let mut loading = self.loading.lock();
                if loading
                    .get(&r.index)
                    .is_some_and(|e| e.profile == r.decode_profile)
                {
                    loading.remove(&r.index);
                }
            }
        }
    }

    pub fn has_pending_outputs(&self) -> bool {
        !self.local_queue.is_empty() || !self.rx.is_empty()
    }

    pub(crate) fn set_root_redraw_wake(&self, wake: Arc<dyn Fn() + Send + Sync>) {
        self.tx.set_root_wake(wake);
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

    pub fn finish_image_request(&self, index: usize) {
        self.loading.lock().remove(&index);
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

    /// Best-effort shutdown before process exit: invalidate queued work and wait briefly
    /// for the rayon decode pool. In-flight LibRaw OpenMP work cannot be cancelled;
    /// callers must terminate via [`crate::startup::force_process_exit`] afterward on Unix.
    ///
    /// Sets `current_gen` to `u64::MAX` so any late decode worker treats queued work as stale.
    /// This loader must not be reused after this call — it is only valid on the process-exit path.
    pub fn prepare_for_process_exit(&mut self) {
        self.cancel_all();
        self.raw_open_prefetch.clear_all();
        drain_rayon_pool_for_exit(&self.pool, LOADER_EXIT_DRAIN_TIMEOUT);
    }

    #[cfg(test)]
    pub(crate) fn test_register_inflight(&self, index: usize) {
        self.loading.lock().insert(
            index,
            crate::loader::InFlightLoad {
                profile: crate::loader::decode_profile_stub(),
            },
        );
    }

    #[cfg(test)]
    pub(crate) fn test_send_loader_output(&self, output: LoaderOutput) {
        self.tx.send(output).expect("test loader channel send");
    }
}

fn drain_rayon_pool_for_exit(pool: &rayon::ThreadPool, timeout: Duration) -> bool {
    let n = pool.current_num_threads();
    if n == 0 {
        return true;
    }
    let (tx, rx) = crossbeam_channel::bounded(n);
    for _ in 0..n {
        let tx = tx.clone();
        pool.spawn(move || {
            let _ = tx.send(());
        });
    }
    drop(tx);

    let deadline = Instant::now() + timeout;
    for _ in 0..n {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if rx.recv_timeout(remaining).is_err() {
            log::warn!(
                "[Loader] Timed out after {:?} waiting for {n} img-loader thread(s) during shutdown",
                timeout
            );
            return false;
        }
    }
    true
}
