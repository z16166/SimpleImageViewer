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
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;

/// Stored in [`CallbackResources`] so HDR callbacks can defer prepare while prewarm runs.
pub(crate) struct HdrCallbackResourcesPrewarmSlot(pub Arc<HdrCallbackResourcesPrewarm>);

/// One compiled pipeline bundle per swap-chain target format (avoids format ping-pong).
#[derive(Default)]
pub(crate) struct HdrCallbackResourcesSet {
    by_format: HashMap<wgpu::TextureFormat, HdrCallbackResources>,
}


impl HdrCallbackResourcesSet {
    pub(crate) fn get_for(&self, format: wgpu::TextureFormat) -> Option<&HdrCallbackResources> {
        self.by_format.get(&format)
    }

    pub(crate) fn get_for_mut(
        &mut self,
        format: wgpu::TextureFormat,
    ) -> Option<&mut HdrCallbackResources> {
        self.by_format.get_mut(&format)
    }

    pub(crate) fn contains_format(&self, format: wgpu::TextureFormat) -> bool {
        self.by_format.contains_key(&format)
    }

    pub(crate) fn insert_format(&mut self, resources: HdrCallbackResources) {
        self.by_format.insert(resources.target_format, resources);
    }
}

enum FormatPrewarmState {
    Running,
    Ready {
        resources: Box<HdrCallbackResources>,
    },
    Installed,
}

/// Background compilation of [`HdrCallbackResources`] (HDR render + RAW demosaic pipelines).
pub(crate) struct HdrCallbackResourcesPrewarm {
    states: Mutex<HashMap<wgpu::TextureFormat, FormatPrewarmState>>,
}

impl HdrCallbackResourcesPrewarm {
    pub(crate) fn new_shared() -> Arc<Self> {
        Arc::new(Self {
            states: Mutex::new(HashMap::new()),
        })
    }

    pub(crate) fn ensure_started(
        self: &Arc<Self>,
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        pipeline_cache: Option<&wgpu::PipelineCache>,
    ) {
        let mut states = self.states.lock();
        match states.get(&format) {
            Some(FormatPrewarmState::Installed)
            | Some(FormatPrewarmState::Ready { .. })
            | Some(FormatPrewarmState::Running) => return,
            _ => {}
        }
        states.insert(format, FormatPrewarmState::Running);
        drop(states);

        let this = Arc::clone(self);
        let device = device.clone();
        let pipeline_cache = pipeline_cache.cloned();
        let spawn_result = std::thread::Builder::new()
            .name(format!("hdr-callback-prewarm-{format:?}"))
            .spawn(move || {
                let resources = create_callback_resources(&device, format, pipeline_cache.as_ref());
                let mut states = this.states.lock();
                match states.get(&format) {
                    Some(FormatPrewarmState::Running) => {
                        states.insert(
                            format,
                            FormatPrewarmState::Ready {
                                resources: Box::new(resources),
                            },
                        );
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
                "[HDR] failed to spawn callback resources prewarm thread for format={format:?}: {error}; \
                 first prepare will compile synchronously"
            );
            self.states.lock().remove(&format);
        }
    }

    pub(crate) fn try_take_ready(
        &self,
        format: wgpu::TextureFormat,
    ) -> Option<HdrCallbackResources> {
        let mut states = self.states.lock();
        let Some(FormatPrewarmState::Ready { .. }) = states.get(&format) else {
            return None;
        };
        match states.remove(&format) {
            Some(FormatPrewarmState::Ready { resources }) => {
                states.insert(format, FormatPrewarmState::Installed);
                Some(*resources)
            }
            _ => None,
        }
    }

    pub(crate) fn is_running(&self, format: wgpu::TextureFormat) -> bool {
        matches!(
            self.states.lock().get(&format),
            Some(FormatPrewarmState::Running)
        )
    }

    pub(crate) fn inject_ready_into_callback_resources(
        &self,
        format: wgpu::TextureFormat,
        callback_resources: &mut CallbackResources,
    ) -> bool {
        let set = ensure_callback_resources_set(callback_resources);
        if set.contains_format(format) {
            return false;
        }
        if let Some(resources) = self.try_take_ready(format) {
            set.insert_format(resources);
            return true;
        }
        false
    }

    pub(crate) fn ensure_prewarm_slot(
        callback_resources: &mut CallbackResources,
        slot: &Arc<Self>,
    ) {
        ensure_callback_resources_set(callback_resources);
        if !callback_resources.contains::<HdrCallbackResourcesPrewarmSlot>() {
            callback_resources.insert(HdrCallbackResourcesPrewarmSlot(Arc::clone(slot)));
        }
    }
}

fn ensure_callback_resources_set(
    callback_resources: &mut CallbackResources,
) -> &mut HdrCallbackResourcesSet {
    if !callback_resources.contains::<HdrCallbackResourcesSet>() {
        let mut set = HdrCallbackResourcesSet::default();
        if let Some(legacy) = callback_resources.remove::<HdrCallbackResources>() {
            set.insert_format(legacy);
        }
        callback_resources.insert(set);
    }
    callback_resources
        .get_mut::<HdrCallbackResourcesSet>()
        .expect("HdrCallbackResourcesSet just inserted")
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

/// All swap-chain formats that may need HDR callback pipelines this session.
pub(crate) fn hdr_callback_formats_to_prewarm(
    hdr_native_surface_enabled: bool,
    candidate_texture_format: Option<wgpu::TextureFormat>,
    live_target_format: Option<wgpu::TextureFormat>,
) -> Vec<wgpu::TextureFormat> {
    let mut formats = Vec::new();
    if let Some(live) = live_target_format {
        formats.push(live);
    }
    if hdr_native_surface_enabled
        && let Some(candidate) = candidate_texture_format
            && !formats.contains(&candidate)
        {
            formats.push(candidate);
        }
    formats
}

/// Read-only snapshot of whether HDR callback resources can be used this frame.
pub(crate) enum HdrCallbackResourcesReadiness {
    Ready,
    PrewarmRunning,
    NeedsEnsure,
}

/// Inspect callback resources under a read lock before attempting registration.
pub(crate) fn hdr_callback_resources_readiness(
    callback_resources: &CallbackResources,
    target_format: wgpu::TextureFormat,
) -> HdrCallbackResourcesReadiness {
    if callback_resources
        .get::<HdrCallbackResourcesSet>()
        .is_some_and(|set| set.contains_format(target_format))
    {
        return HdrCallbackResourcesReadiness::Ready;
    }

    if let Some(slot) = callback_resources.get::<HdrCallbackResourcesPrewarmSlot>()
        && slot.0.is_running(target_format)
    {
        return HdrCallbackResourcesReadiness::PrewarmRunning;
    }

    HdrCallbackResourcesReadiness::NeedsEnsure
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
        .get::<HdrCallbackResourcesSet>()
        .is_some_and(|set| set.contains_format(target_format))
    {
        return true;
    }

    if let Some(slot) = callback_resources.get::<HdrCallbackResourcesPrewarmSlot>() {
        if slot.0.is_running(target_format) {
            return false;
        }
        if let Some(resources) = slot.0.try_take_ready(target_format) {
            ensure_callback_resources_set(callback_resources).insert_format(resources);
            return true;
        }
    }

    log::warn!(
        "[HDR] prepare sync compile HDR callback resources format={:?} (prewarm missed)",
        target_format
    );
    ensure_callback_resources_set(callback_resources).insert_format(create_callback_resources(
        device,
        target_format,
        None,
    ));
    true
}
