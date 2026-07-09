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

//! Tracks short-lived background threads so `on_exit` can join them.

use std::collections::VecDeque;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::constants::BACKGROUND_THREAD_SOFT_LIMIT;

pub(crate) const BACKGROUND_THREAD_JOIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Poll interval while waiting for unfinished background threads during shutdown.
const JOIN_POLL_INTERVAL: Duration = Duration::from_millis(50);

pub(crate) struct BackgroundThreadJoiner {
    handles: Mutex<VecDeque<JoinHandle<()>>>,
}

impl BackgroundThreadJoiner {
    pub(crate) fn new() -> Self {
        Self {
            handles: Mutex::new(VecDeque::new()),
        }
    }

    pub(crate) fn spawn<F>(&self, name: impl Into<String>, f: F) -> bool
    where
        F: FnOnce() + Send + 'static,
    {
        let name = name.into();
        let over_limit = {
            let handles = self.handles.lock();
            handles.len() >= BACKGROUND_THREAD_SOFT_LIMIT
        };
        if over_limit {
            // Soft cap: avoid unbounded OS threads; fold into the shared rayon pool.
            log::warn!(
                "[BackgroundThreads] Soft limit ({BACKGROUND_THREAD_SOFT_LIMIT}) reached; \
                 running `{name}` on rayon instead of a new OS thread"
            );
            rayon::spawn(f);
            return true;
        }
        match std::thread::Builder::new().name(name).spawn(f) {
            Ok(handle) => {
                self.handles.lock().push_back(handle);
                true
            }
            Err(err) => {
                log::error!("[BackgroundThreads] Failed to spawn thread: {err}");
                false
            }
        }
    }

    pub(crate) fn join_all(&mut self, timeout: Duration) {
        let mut handles = std::mem::take(&mut *self.handles.lock());
        if handles.is_empty() {
            log::debug!("[BackgroundThreads] join_all: no background threads");
            return;
        }
        log::debug!(
            "[BackgroundThreads] join_all: waiting up to {:?} for {} thread(s)",
            timeout,
            handles.len()
        );
        let total = handles.len();
        let deadline = Instant::now() + timeout;
        let mut joined = 0usize;
        while !handles.is_empty() {
            let now = Instant::now();
            if now >= deadline {
                let remaining = handles.len();
                log::warn!(
                    "[BackgroundThreads] Join timed out after {:?}; detaching {remaining} of {total} thread(s)",
                    timeout
                );
                for handle in handles.drain(..) {
                    match handle.thread().name() {
                        Some(name) => {
                            log::warn!("[BackgroundThreads] Detaching unfinished thread {name}");
                        }
                        None => {
                            log::warn!("[BackgroundThreads] Detaching unnamed background thread");
                        }
                    }
                }
                return;
            }

            // Only join finished threads so one stuck worker cannot block the rest
            // of the shutdown budget (join itself has no timeout).
            if let Some(idx) = handles.iter().position(|h| h.is_finished()) {
                let handle = handles
                    .remove(idx)
                    .expect("index from position must be valid");
                if handle.join().is_err() {
                    log::warn!("[BackgroundThreads] Background thread panicked on join");
                }
                joined += 1;
                continue;
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                continue;
            }
            std::thread::sleep(remaining.min(JOIN_POLL_INTERVAL));
        }
        log::debug!("[BackgroundThreads] join_all: joined {joined} background thread(s)");
    }
}
