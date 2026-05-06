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

//! HQ preview sizing, monitor-based caps, and the bounded refinement thread pool.

use crate::constants::MAX_QUALITY_PREVIEW_SIZE;
use std::sync::LazyLock;

/// Hardware-tier cap for HQ preview / refine (written at startup from
/// [`crate::app::HardwareTier::max_preview_size`]).
///
/// **Display cap:** do not use the window's **client size**; the user may fullscreen at any time.
/// **Multi-monitor (policy):** use the monitor for the **current** root viewport (eframe/winit:
/// the monitor that contains the window, aligned with centering/fullscreen on that display).
///
/// **`k_zoom`:** [`crate::constants::HQ_PREVIEW_MONITOR_HEADROOM`] (**1.1**).
pub static PREVIEW_LIMIT: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(MAX_QUALITY_PREVIEW_SIZE / 2);

/// Max preview side derived from the current monitor's **physical** long edge × headroom
/// (see [`refresh_hq_preview_monitor_cap`]). Capped at [`MAX_QUALITY_PREVIEW_SIZE`]; combined with
/// [`PREVIEW_LIMIT`] in [`hq_preview_max_side`].
pub static MONITOR_PREVIEW_CAP: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(MAX_QUALITY_PREVIEW_SIZE);

/// Recompute [`MONITOR_PREVIEW_CAP`] from egui viewport info (physical pixels). Call each frame
/// from the UI thread (cheap). If monitor size is unknown, the atomic is left unchanged.
pub fn refresh_hq_preview_monitor_cap(ctx: &eframe::egui::Context) {
    let cap = ctx.input(|i| {
        let vp = i.viewport();
        let (Some(ms), Some(npp)) = (vp.monitor_size, vp.native_pixels_per_point) else {
            return None;
        };
        if ms.x < 1.0 || ms.y < 1.0 || !npp.is_finite() || npp <= 0.0 {
            return None;
        }
        // `monitor_size` is in UI points; scale by OS native pixels-per-point → physical pixels.
        let phys_w = (ms.x * npp).round().clamp(1.0, u32::MAX as f32) as u32;
        let phys_h = (ms.y * npp).round().clamp(1.0, u32::MAX as f32) as u32;
        let long = phys_w.max(phys_h);
        let scaled = (long as f32) * crate::constants::HQ_PREVIEW_MONITOR_HEADROOM;
        let cap = scaled.ceil().max(256.0) as u32;
        Some(cap.min(MAX_QUALITY_PREVIEW_SIZE))
    });
    if let Some(c) = cap {
        MONITOR_PREVIEW_CAP.store(c, std::sync::atomic::Ordering::Relaxed);
    }
}

/// HQ preview / refine max side: `min` of hardware tier ([`PREVIEW_LIMIT`]), monitor-based cap
/// ([`MONITOR_PREVIEW_CAP`]), and [`MAX_QUALITY_PREVIEW_SIZE`].
#[inline]
pub fn hq_preview_max_side() -> u32 {
    let tier = PREVIEW_LIMIT.load(std::sync::atomic::Ordering::Relaxed);
    let tier_v = if tier == 0 {
        MAX_QUALITY_PREVIEW_SIZE
    } else {
        tier.min(MAX_QUALITY_PREVIEW_SIZE)
    };
    let mon = MONITOR_PREVIEW_CAP.load(std::sync::atomic::Ordering::Relaxed);
    let mon_v = if mon == 0 {
        MAX_QUALITY_PREVIEW_SIZE
    } else {
        mon.min(MAX_QUALITY_PREVIEW_SIZE)
    };
    tier_v.min(mon_v)
}

/// Upper bound for [`REFINEMENT_POOL`] workers. Each task can hold large HDR/SDR preview buffers;
/// too many concurrent refinements spikes RSS and contends with the main loader / tile workers.
const REFINEMENT_POOL_MAX_THREADS: usize = 4;
/// Minimum workers: keep some overlap for I/O vs CPU without spawning a thread per logical core.
const REFINEMENT_POOL_MIN_THREADS: usize = 2;

/// Dedicated pool for heavy high-quality preview generation (refinement).
/// Crate-visible for the loader orchestration paths in this module.
pub(crate) static REFINEMENT_POOL: LazyLock<rayon::ThreadPool> = LazyLock::new(|| {
    match rayon::ThreadPoolBuilder::new()
        .num_threads(
            std::thread::available_parallelism()
                .map(|n| {
                    n.get()
                        .div_ceil(4)
                        .clamp(REFINEMENT_POOL_MIN_THREADS, REFINEMENT_POOL_MAX_THREADS)
                })
                .unwrap_or(REFINEMENT_POOL_MIN_THREADS),
        )
        .thread_name(|i| format!("refinement-worker-{i}"))
        .build()
    {
        Ok(p) => p,
        Err(e) => {
            log::error!(
                "[Loader] Failed to create refinement pool: {}. Falling back to default pool.",
                e
            );
            rayon::ThreadPoolBuilder::new()
                .num_threads(1)
                .build()
                .unwrap()
        }
    }
});
