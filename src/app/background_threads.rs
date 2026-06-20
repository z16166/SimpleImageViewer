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

use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

pub(crate) const BACKGROUND_THREAD_JOIN_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) struct BackgroundThreadJoiner {
    handles: Mutex<Vec<JoinHandle<()>>>,
}

impl BackgroundThreadJoiner {
    pub(crate) fn new() -> Self {
        Self {
            handles: Mutex::new(Vec::new()),
        }
    }

    pub(crate) fn spawn<F>(&self, name: impl Into<String>, f: F) -> bool
    where
        F: FnOnce() + Send + 'static,
    {
        match std::thread::Builder::new().name(name.into()).spawn(f) {
            Ok(handle) => {
                self.handles.lock().push(handle);
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
            if Instant::now() >= deadline {
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
            let handle = handles.remove(0);
            if handle.join().is_err() {
                log::warn!("[BackgroundThreads] Background thread panicked on join");
            }
            joined += 1;
        }
        log::debug!(
            "[BackgroundThreads] join_all: joined {joined} background thread(s)"
        );
    }
}
