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

use crate::loader::DecodedImage;
use crate::loader::decode::open_raw_processor_with_preview;
use crate::raw_processor::RawProcessor;
use parking_lot::{Condvar, Mutex};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::types::WORKER_SHUTDOWN_POLL;

#[derive(Debug, Clone, Copy)]
pub(crate) struct RawOpenPhaseTimings {
    pub open_ms: u32,
    pub thumb_ms: u32,
}

pub(crate) struct RawPrefetchedOpen {
    pub processor: RawProcessor,
    pub preview: Option<DecodedImage>,
    pub timings: RawOpenPhaseTimings,
    pub final_lr_flip: i32,
}

enum RawPrefetchEntry {
    InProgress,
    Ready(RawPrefetchedOpen),
}

#[derive(Default)]
struct RawOpenPrefetchInner {
    entries: HashMap<PathBuf, RawPrefetchEntry>,
}

pub(crate) struct RawOpenPrefetch {
    state: Arc<(Mutex<RawOpenPrefetchInner>, Condvar)>,
    shutdown: Arc<AtomicBool>,
}

impl RawOpenPrefetch {
    pub(crate) fn new(shutdown: Arc<AtomicBool>) -> Self {
        Self {
            state: Arc::new((Mutex::new(RawOpenPrefetchInner::default()), Condvar::new())),
            shutdown,
        }
    }

    pub(crate) fn wake_waiters(&self) {
        let (_, cvar) = &*self.state;
        cvar.notify_all();
    }

    pub(crate) fn request(&self, pool: &rayon::ThreadPool, path: PathBuf) {
        {
            let (lock, _) = &*self.state;
            let mut inner = lock.lock();
            match inner.entries.get(&path) {
                Some(RawPrefetchEntry::InProgress | RawPrefetchEntry::Ready(_)) => return,
                None => {}
            }
            inner
                .entries
                .insert(path.clone(), RawPrefetchEntry::InProgress);
        }

        let state = Arc::clone(&self.state);
        pool.spawn(move || {
            let result = open_raw_processor_with_preview(&path);
            let (lock, cvar) = &*state;
            let mut inner = lock.lock();
            match result {
                Ok((processor, preview, timings, final_lr_flip)) => {
                    inner.entries.insert(
                        path,
                        RawPrefetchEntry::Ready(RawPrefetchedOpen {
                            processor,
                            preview,
                            timings,
                            final_lr_flip,
                        }),
                    );
                }
                Err(err) => {
                    log::debug!(
                        "raw open prefetch failed path={:?}: {err}",
                        path.file_name().unwrap_or_default()
                    );
                    inner.entries.remove(&path);
                }
            }
            cvar.notify_all();
        });
    }

    /// Drop cached / in-flight prefetch entries during process shutdown.
    pub(crate) fn clear_all(&self) {
        let (lock, cvar) = &*self.state;
        lock.lock().entries.clear();
        cvar.notify_all();
    }

    pub(crate) fn take_or_wait(&self, path: &Path) -> Option<RawPrefetchedOpen> {
        let (lock, cvar) = &*self.state;
        loop {
            if self.shutdown.load(Ordering::Acquire) {
                return None;
            }
            let mut inner = lock.lock();
            match inner.entries.get(path) {
                Some(RawPrefetchEntry::Ready(_)) => {
                    if let Some(RawPrefetchEntry::Ready(session)) = inner.entries.remove(path) {
                        return Some(session);
                    }
                }
                Some(RawPrefetchEntry::InProgress) => {
                    cvar.wait_for(&mut inner, WORKER_SHUTDOWN_POLL);
                }
                None => return None,
            }
        }
    }
}

pub(crate) fn should_prefetch_raw_gpu_open(
    settings: &crate::settings::Settings,
    path: &Path,
    gpu_demosaic_failed: bool,
) -> bool {
    !gpu_demosaic_failed
        && settings.raw_high_quality
        && settings.raw_demosaic_mode == crate::settings::RawDemosaicMode::Gpu
        && crate::loader::GPU_DEMOSAIC_SUPPORTED.load(std::sync::atomic::Ordering::Relaxed)
        && path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(crate::raw_processor::is_raw_extension)
}
