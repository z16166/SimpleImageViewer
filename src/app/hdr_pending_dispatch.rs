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

//! Shared drain-partition-spawn-requeue helpers for HDR pending work queues.

use crate::hdr::renderer::HdrPendingWorkQueues;
use crate::loader::REFINEMENT_POOL;
use eframe::egui_wgpu::RenderState;
use std::sync::Arc;

/// Run up to `max_per_logic` GPU upload jobs now; return the remainder for requeue.
pub(crate) fn dispatch_hdr_gpu_upload_batch<T: Send + 'static>(
    requests: Vec<T>,
    wgpu_state: Option<&RenderState>,
    wgpu_is_opengl: bool,
    device_id: u64,
    completed: Arc<HdrPendingWorkQueues>,
    max_per_logic: usize,
    finish_fn: fn(&Arc<HdrPendingWorkQueues>, &wgpu::Device, T, u64),
) -> Vec<T> {
    let Some(wgpu_state) = wgpu_state else {
        return requests;
    };

    let device = wgpu_state.device.clone();
    let (run_now, requeue): (Vec<_>, Vec<_>) = requests
        .into_iter()
        .enumerate()
        .partition(|(idx, _)| *idx < max_per_logic);

    if wgpu_is_opengl {
        for (_, request) in run_now {
            finish_fn(&completed, &device, request, device_id);
        }
    } else {
        for (_, request) in run_now {
            let completed = Arc::clone(&completed);
            let device = device.clone();
            REFINEMENT_POOL.spawn(move || {
                finish_fn(&completed, &device, request, device_id);
            });
        }
    }

    requeue.into_iter().map(|(_, request)| request).collect()
}

/// Start up to the remaining CPU compose budget; return requests that did not run.
pub(crate) fn dispatch_hdr_cpu_compose_batch<T: Send + 'static>(
    requests: Vec<T>,
    started: &mut usize,
    max_total: usize,
    spawn_fn: impl Fn(T),
) -> Vec<T> {
    let mut requeue = Vec::new();
    for request in requests {
        if *started >= max_total {
            requeue.push(request);
            continue;
        }
        *started += 1;
        spawn_fn(request);
    }
    requeue
}

/// Result of attempting to register one completed HDR work item.
pub(crate) enum HdrCompletedRegisterOutcome<T> {
    Applied,
    Skipped,
    Deferred(T),
}

/// Apply completed HDR work items, deferring those that cannot register yet.
pub(crate) fn apply_hdr_completed_batch<T>(
    wgpu_state: Option<&RenderState>,
    completed: Vec<T>,
    restore: impl FnOnce(Vec<T>),
    extend_deferred: impl FnOnce(Vec<T>),
    mut register_one: impl FnMut(&RenderState, T) -> HdrCompletedRegisterOutcome<T>,
) -> bool {
    let Some(wgpu_state) = wgpu_state else {
        restore(completed);
        return false;
    };

    let mut changed = false;
    let mut defer = Vec::new();
    for item in completed {
        match register_one(wgpu_state, item) {
            HdrCompletedRegisterOutcome::Applied => changed = true,
            HdrCompletedRegisterOutcome::Skipped => {}
            HdrCompletedRegisterOutcome::Deferred(deferred) => defer.push(deferred),
        }
    }
    if !defer.is_empty() {
        extend_deferred(defer);
    }
    changed
}
