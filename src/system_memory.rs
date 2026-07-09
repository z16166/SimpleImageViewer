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

//! Throttled sysinfo RAM snapshot for decode-time budgeting and preload planning.
//!
//! Hot paths (e.g. PSD memory guard) read cached MB values from atomics without
//! triggering sysinfo refresh. Call [`refresh_if_stale`] from the logic thread,
//! monitor-cap updates, or startup.

use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Minimum interval between sysinfo RAM refreshes (shared by preload planning and decode guards).
/// Kept deliberately long so logic-root passes never block the UI thread on sysconf /
/// GlobalMemoryStatusEx more than once every half-minute.
pub const MEMORY_REFRESH_MIN_INTERVAL: Duration = Duration::from_secs(30);
const BYTES_PER_MB: u64 = 1024 * 1024;

/// Cached available RAM in megabytes; updated by [`refresh_if_stale`].
pub static AVAILABLE_MEMORY_MB: AtomicU64 = AtomicU64::new(0);

/// Cached total RAM in megabytes; updated by [`refresh_if_stale`].
pub static TOTAL_MEMORY_MB: AtomicU64 = AtomicU64::new(0);

struct SystemMemorySnapshot {
    sys: sysinfo::System,
    refreshed_at: Option<Instant>,
}

static SNAPSHOT: LazyLock<parking_lot::Mutex<SystemMemorySnapshot>> = LazyLock::new(|| {
    parking_lot::Mutex::new(SystemMemorySnapshot {
        sys: sysinfo::System::new_with_specifics(
            sysinfo::RefreshKind::nothing()
                .with_memory(sysinfo::MemoryRefreshKind::nothing().with_ram()),
        ),
        refreshed_at: None,
    })
});

fn publish_from_sys(sys: &sysinfo::System) {
    AVAILABLE_MEMORY_MB.store(sys.available_memory() / BYTES_PER_MB, Ordering::Relaxed);
    TOTAL_MEMORY_MB.store(sys.total_memory() / BYTES_PER_MB, Ordering::Relaxed);
}

/// Refresh RAM stats when the cached snapshot is stale or unset.
pub fn refresh_if_stale() {
    let mut snapshot = SNAPSHOT.lock();
    let now = Instant::now();
    if snapshot
        .refreshed_at
        .is_some_and(|at| now.duration_since(at) < MEMORY_REFRESH_MIN_INTERVAL)
    {
        return;
    }
    snapshot
        .sys
        .refresh_memory_specifics(sysinfo::MemoryRefreshKind::nothing().with_ram());
    publish_from_sys(&snapshot.sys);
    snapshot.refreshed_at = Some(now);
}

/// Available RAM in megabytes. Refreshes once when the cache is still empty (tests / early load).
pub fn available_memory_mb() -> u64 {
    let cached = AVAILABLE_MEMORY_MB.load(Ordering::Relaxed);
    if cached > 0 {
        return cached;
    }
    refresh_if_stale();
    AVAILABLE_MEMORY_MB.load(Ordering::Relaxed)
}

/// Total RAM in megabytes. Refreshes once when the cache is still empty (tests / early load).
pub fn total_memory_mb() -> u64 {
    let cached = TOTAL_MEMORY_MB.load(Ordering::Relaxed);
    if cached > 0 {
        return cached;
    }
    refresh_if_stale();
    TOTAL_MEMORY_MB.load(Ordering::Relaxed)
}

/// Seed the cache from a startup probe (before the logic thread runs).
///
/// Also marks the snapshot as freshly refreshed so the first logic-root
/// [`refresh_if_stale`] call reuses these values instead of re-querying the OS.
pub fn publish_startup_memory(available_mb: u64, total_mb: u64) {
    AVAILABLE_MEMORY_MB.store(available_mb, Ordering::Relaxed);
    TOTAL_MEMORY_MB.store(total_mb, Ordering::Relaxed);
    let mut snapshot = SNAPSHOT.lock();
    snapshot.refreshed_at = Some(Instant::now());
}
