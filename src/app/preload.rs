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

use std::collections::HashSet;

use super::types::{HardwareTier, UltraHdrCapacityRefresh};

// -- Preload configuration --
// Maximum number of images to preload in each direction.
pub(crate) const MAX_PRELOAD_FORWARD: usize = 5;
pub(crate) const MAX_PRELOAD_BACKWARD: usize = 3;
/// Cap simultaneous image decoders so HQ RAW GPU extract is not starved by neighbor preloads.
pub(crate) const MAX_CONCURRENT_DECODER_LOADS: usize =
    crate::loader::MAX_IMG_LOADER_THREADS;
// Texture cache must hold: current + forward + backward + buffer for transitions
pub(crate) const CACHE_SIZE: usize = MAX_PRELOAD_FORWARD + MAX_PRELOAD_BACKWARD + 3;
/// Max CPU-side SDR previews queued for deferred GPU upload (neighbors + HDR fallbacks).
/// Independent of preload direction limits so tuning one does not silently change the other.
pub(crate) const MAX_DEFERRED_SDR_UPLOADS: usize = 12;
/// Avoid refreshing sysinfo RAM stats on every preload schedule during rapid navigation.
pub(crate) const PRELOAD_MEMORY_REFRESH_MIN_INTERVAL: std::time::Duration =
    std::time::Duration::from_secs(1);

/// Decode-time Ultra HDR headroom for loader spawn and cache invalidation.
///
/// **macOS:** uses [`HdrMonitorSelection::max_hdr_capacity`] (NSScreen **potential** —
/// [`maximumPotentialExtendedDynamicRangeColorComponentValue`](https://developer.apple.com/documentation/appkit/nsscreen/maximumpotentialextendeddynamicrangecolorcomponentvalue)).
/// Does **not** use [`HdrMonitorSelection::current_edr_headroom`] (dynamic **current**).
/// Display tone-mapping reads current headroom separately; see
/// [`crate::settings::Settings::hdr_tone_map_settings_for_monitor`] and `src/hdr/monitor/macos.rs`.
pub(crate) fn ultra_hdr_decode_capacity_for_output_mode(
    settings: crate::hdr::types::HdrToneMapSettings,
    output_mode: crate::hdr::types::HdrOutputMode,
    monitor: Option<&crate::hdr::monitor::HdrMonitorSelection>,
) -> f32 {
    if output_mode == crate::hdr::types::HdrOutputMode::SdrToneMapped {
        1.0
    } else if let Some(max_hdr_capacity) = monitor
        .and_then(|selection| selection.max_hdr_capacity)
        .filter(|value| *value > 0.0)
    {
        max_hdr_capacity
    } else if let Some(max_luminance_nits) = monitor
        .and_then(|selection| selection.max_luminance_nits)
        .filter(|value| *value > 0.0)
    {
        max_luminance_nits / settings.sdr_white_nits.max(1.0)
    } else {
        settings.target_hdr_capacity()
    }
}

pub(crate) fn collect_ultra_hdr_capacity_sensitive_indices(
    static_hdr: &HashSet<usize>,
    hdr_tiled: &HashSet<usize>,
    hdr_fallback: &HashSet<usize>,
) -> Vec<usize> {
    let mut indices = std::collections::BTreeSet::new();
    indices.extend(static_hdr.iter().copied());
    indices.extend(hdr_tiled.iter().copied());
    indices.extend(hdr_fallback.iter().copied());
    indices.into_iter().collect()
}
pub(crate) fn plan_ultra_hdr_capacity_refresh(
    current_index: usize,
    static_hdr: &HashSet<usize>,
    hdr_tiled: &HashSet<usize>,
    hdr_fallback: &HashSet<usize>,
    ultra_hdr: &HashSet<usize>,
) -> UltraHdrCapacityRefresh {
    let hdr_indices =
        collect_ultra_hdr_capacity_sensitive_indices(static_hdr, hdr_tiled, hdr_fallback);
    let indices_to_invalidate = hdr_indices
        .into_iter()
        .filter(|index| ultra_hdr.contains(index))
        .collect::<Vec<_>>();
    let reload_current = indices_to_invalidate.binary_search(&current_index).is_ok();
    UltraHdrCapacityRefresh {
        indices_to_invalidate,
        reload_current,
    }
}

pub(crate) fn capacity_refresh_should_reschedule_preloads(
    refresh: &UltraHdrCapacityRefresh,
) -> bool {
    !refresh.indices_to_invalidate.is_empty()
}

/// Compute preload byte budgets based on total system RAM.
/// Forward budget = total_ram / 32, backward = total_ram / 64, both clamped.
pub(crate) fn compute_preload_budgets() -> (u64, u64) {
    const MIN_FORWARD_BUDGET_BYTES: u64 = 64 * 1024 * 1024;
    const MAX_FORWARD_BUDGET_BYTES: u64 = 512 * 1024 * 1024;
    const MIN_BACKWARD_BUDGET_BYTES: u64 = 32 * 1024 * 1024;
    const MAX_BACKWARD_BUDGET_BYTES: u64 = 256 * 1024 * 1024;

    use sysinfo::System;
    let mut sys = System::new();
    sys.refresh_memory();
    let total = sys.total_memory(); // bytes

    let forward = (total / 32).clamp(MIN_FORWARD_BUDGET_BYTES, MAX_FORWARD_BUDGET_BYTES);
    let backward = (total / 64).clamp(MIN_BACKWARD_BUDGET_BYTES, MAX_BACKWARD_BUDGET_BYTES);

    log::info!(
        "Preload budgets: forward={} MB, backward={} MB (system RAM={} MB)",
        forward / (1024 * 1024),
        backward / (1024 * 1024),
        total / (1024 * 1024),
    );
    (forward, backward)
}
pub(crate) fn memory_aware_tile_cache_budgets_mb(
    tier: HardwareTier,
    available_memory_mb: u64,
) -> (usize, usize) {
    const MIN_CPU_CACHE_MB: usize = 256;
    const MIN_HDR_CACHE_MB: usize = 256;

    let desired_cpu = tier.cpu_cache_mb();
    let desired_hdr = tier.hdr_tile_cache_mb();
    let max_combined = (available_memory_mb / 4) as usize;
    if max_combined >= desired_cpu + desired_hdr {
        return (desired_cpu, desired_hdr);
    }

    let available_after_mins = max_combined.saturating_sub(MIN_CPU_CACHE_MB + MIN_HDR_CACHE_MB);
    let desired_extra_cpu = desired_cpu.saturating_sub(MIN_CPU_CACHE_MB);
    let desired_extra_hdr = desired_hdr.saturating_sub(MIN_HDR_CACHE_MB);
    let desired_extra_total = desired_extra_cpu + desired_extra_hdr;
    if desired_extra_total == 0 {
        return (MIN_CPU_CACHE_MB, MIN_HDR_CACHE_MB);
    }

    let cpu_extra = available_after_mins * desired_extra_cpu / desired_extra_total;
    let hdr_extra = available_after_mins.saturating_sub(cpu_extra);
    (
        (MIN_CPU_CACHE_MB + cpu_extra).min(desired_cpu),
        (MIN_HDR_CACHE_MB + hdr_extra).min(desired_hdr),
    )
}
