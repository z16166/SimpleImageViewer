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

use super::resources::{HdrCallbackResources, create_callback_resources};
use eframe::egui_wgpu::CallbackResources;
use std::sync::{Arc, Mutex};

/// Stored in [`CallbackResources`] so HDR callbacks can defer prepare while prewarm runs.
pub(crate) struct HdrCallbackResourcesPrewarmSlot(pub Arc<HdrCallbackResourcesPrewarm>);

enum PrewarmState {
    Idle,
    Running {
        format: wgpu::TextureFormat,
    },
    Ready {
        format: wgpu::TextureFormat,
        resources: HdrCallbackResources,
    },
    /// Resources were injected into the live renderer; do not compile again.
    Installed {
        format: wgpu::TextureFormat,
    },
}

/// Background compilation of [`HdrCallbackResources`] (HDR render + RAW demosaic pipelines).
pub(crate) struct HdrCallbackResourcesPrewarm {
    state: Mutex<PrewarmState>,
}

impl HdrCallbackResourcesPrewarm {
    pub(crate) fn new_shared() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(PrewarmState::Idle),
        })
    }

    pub(crate) fn ensure_started(
        self: &Arc<Self>,
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        pipeline_cache: Option<&wgpu::PipelineCache>,
    ) {
        let mut guard = self
            .state
            .lock()
            .expect("HdrCallbackResourcesPrewarm mutex poisoned");
        match &*guard {
            PrewarmState::Installed { format: installed } if *installed == format => return,
            PrewarmState::Ready { format: ready, .. } if *ready == format => return,
            PrewarmState::Running { format: running } if *running == format => return,
            _ => {}
        }

        *guard = PrewarmState::Running { format };
        drop(guard);

        let this = Arc::clone(self);
        let device = device.clone();
        let pipeline_cache = pipeline_cache.cloned();
        let spawn_result = std::thread::Builder::new()
            .name("hdr-callback-prewarm".into())
            .spawn(move || {
                let resources = create_callback_resources(&device, format, pipeline_cache.as_ref());
                let mut guard = this
                    .state
                    .lock()
                    .expect("HdrCallbackResourcesPrewarm mutex poisoned");
                match &*guard {
                    PrewarmState::Running { format: wanted } if *wanted == format => {
                        *guard = PrewarmState::Ready { format, resources };
                    }
                    _ => {
                        log::debug!(
                            "[HDR] discarded stale callback resources prewarm for format={:?}",
                            format
                        );
                    }
                }
            });
        if let Err(error) = spawn_result {
            log::warn!(
                "[HDR] failed to spawn callback resources prewarm thread: {error}; \
                 first prepare will compile synchronously"
            );
            let mut guard = self
                .state
                .lock()
                .expect("HdrCallbackResourcesPrewarm mutex poisoned");
            *guard = PrewarmState::Idle;
        }
    }

    pub(crate) fn try_take_ready(
        &self,
        format: wgpu::TextureFormat,
    ) -> Option<HdrCallbackResources> {
        let mut guard = self
            .state
            .lock()
            .expect("HdrCallbackResourcesPrewarm mutex poisoned");
        let PrewarmState::Ready {
            format: ready,
            resources: _,
        } = &*guard
        else {
            return None;
        };
        if *ready != format {
            return None;
        }
        let installed_format = *ready;
        match std::mem::replace(
            &mut *guard,
            PrewarmState::Installed {
                format: installed_format,
            },
        ) {
            PrewarmState::Ready {
                format: _,
                resources,
            } => Some(resources),
            _ => None,
        }
    }

    pub(crate) fn is_running(&self, format: wgpu::TextureFormat) -> bool {
        matches!(
            *self
                .state
                .lock()
                .expect("HdrCallbackResourcesPrewarm mutex poisoned"),
            PrewarmState::Running { format: running } if running == format
        )
    }

    pub(crate) fn inject_ready_into_callback_resources(
        &self,
        format: wgpu::TextureFormat,
        callback_resources: &mut CallbackResources,
    ) -> bool {
        if callback_resources
            .get::<HdrCallbackResources>()
            .is_some_and(|resources| resources.target_format == format)
        {
            return false;
        }
        if let Some(resources) = self.try_take_ready(format) {
            callback_resources.insert(resources);
            return true;
        }
        false
    }

    pub(crate) fn ensure_prewarm_slot(
        callback_resources: &mut CallbackResources,
        slot: &Arc<Self>,
    ) {
        if !callback_resources.contains::<HdrCallbackResourcesPrewarmSlot>() {
            callback_resources.insert(HdrCallbackResourcesPrewarmSlot(Arc::clone(slot)));
        }
    }
}

/// Target format for early prewarm before the swap-chain hot-swap completes.
///
/// Prefer the spawn-time HDR candidate (`candidate_texture_format`, usually
/// `Rgba16Float` on DXGI) so compilation overlaps the initial SDR swap-chain
/// frames instead of wasting a compile on transient `Bgra8Unorm`.
pub(crate) fn predicted_hdr_callback_target_format(
    hdr_native_surface_enabled: bool,
    hdr_monitor_hdr_supported: bool,
    candidate_texture_format: Option<wgpu::TextureFormat>,
    live_target_format: Option<wgpu::TextureFormat>,
) -> Option<wgpu::TextureFormat> {
    if hdr_native_surface_enabled {
        if let Some(candidate) = candidate_texture_format {
            return Some(candidate);
        }
        if hdr_monitor_hdr_supported {
            return Some(wgpu::TextureFormat::Rgba16Float);
        }
    }
    live_target_format
}

/// Ensures [`HdrCallbackResources`] exist for `target_format`.
///
/// Returns `false` when a background prewarm is still running; callers should defer
/// GPU upload/demosaic work and repaint on the next frame instead of blocking the UI
/// thread on synchronous shader compilation.
pub(crate) fn ensure_hdr_callback_resources(
    device: &wgpu::Device,
    target_format: wgpu::TextureFormat,
    callback_resources: &mut CallbackResources,
) -> bool {
    if callback_resources
        .get::<HdrCallbackResources>()
        .is_some_and(|resources| resources.target_format == target_format)
    {
        return true;
    }

    if let Some(slot) = callback_resources.get::<HdrCallbackResourcesPrewarmSlot>() {
        if let Some(resources) = slot.0.try_take_ready(target_format) {
            callback_resources.insert(resources);
            return true;
        }
        if slot.0.is_running(target_format) {
            return false;
        }
    }

    log::warn!(
        "[HDR] prepare sync compile HDR callback resources format={:?} (prewarm missed)",
        target_format
    );
    callback_resources.insert(create_callback_resources(device, target_format, None));
    true
}
