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

//! HDR swap-chain / startup-preload gate diagnostics (`--features preload-debug`).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HdrSwapGateSnapshot {
    native_surface_requests_enabled: bool,
    settings_native_surface_effective: bool,
    settings_hdr_native_surface_enabled: bool,
    backend: Option<eframe::wgpu::Backend>,
    current_target_format: Option<eframe::wgpu::TextureFormat>,
    desired_target_format: Option<eframe::wgpu::TextureFormat>,
    swap_request_outcome: SwapRequestOutcome,
    wsi_probed: bool,
    wsi_hdr10_st2084_rgb10a2: bool,
    wsi_extended_srgb_linear_rgba16f: bool,
    wp_selection_present: bool,
    wp_hdr_supported: Option<bool>,
    effective_selection_present: bool,
    effective_hdr_supported: Option<bool>,
    output_mode: crate::hdr::types::HdrOutputMode,
    native_presentation_enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SwapRequestOutcome {
    Disabled,
    NoMonitorOpinion,
    AlreadyMatched,
    Requested,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PreloadDeferGateSnapshot {
    preload_deferred: bool,
    runtime_probe_completed: bool,
    output_mode: crate::hdr::types::HdrOutputMode,
    effective_hdr_supported: Option<bool>,
    capacity_known: bool,
    can_release: bool,
    wsi_probed: bool,
    current_target_format: Option<eframe::wgpu::TextureFormat>,
}

#[derive(Default)]
pub(crate) struct GateLogState {
    last_swap_gate: Option<HdrSwapGateSnapshot>,
    last_preload_defer_gate: Option<PreloadDeferGateSnapshot>,
}

impl GateLogState {
    pub(crate) fn log_swap_chain_gate(
        &mut self,
        native_surface_requests_enabled: bool,
        settings_native_surface_effective: bool,
        settings_hdr_native_surface_enabled: bool,
        backend: Option<eframe::wgpu::Backend>,
        current_target_format: Option<eframe::wgpu::TextureFormat>,
        desired_target_format: Option<eframe::wgpu::TextureFormat>,
        swap_request_outcome: SwapRequestOutcome,
        wsi: eframe::egui_wgpu::VulkanWsiHdrGates,
        wp_selection: Option<&crate::hdr::monitor::HdrMonitorSelection>,
        effective_selection: Option<&crate::hdr::monitor::HdrMonitorSelection>,
        output_mode: crate::hdr::types::HdrOutputMode,
        native_presentation_enabled: bool,
    ) {
        let snapshot = HdrSwapGateSnapshot {
            native_surface_requests_enabled,
            settings_native_surface_effective,
            settings_hdr_native_surface_enabled,
            backend,
            current_target_format,
            desired_target_format,
            swap_request_outcome,
            wsi_probed: wsi.probed,
            wsi_hdr10_st2084_rgb10a2: wsi.hdr10_st2084_rgb10a2,
            wsi_extended_srgb_linear_rgba16f: wsi.extended_srgb_linear_rgba16f,
            wp_selection_present: wp_selection.is_some(),
            wp_hdr_supported: wp_selection.map(|s| s.hdr_supported),
            effective_selection_present: effective_selection.is_some(),
            effective_hdr_supported: effective_selection.map(|s| s.hdr_supported),
            output_mode,
            native_presentation_enabled,
        };
        if self.last_swap_gate == Some(snapshot) {
            return;
        }
        self.last_swap_gate = Some(snapshot);
        crate::preload_debug!(
            "[PreloadDebug][HDR-Gate] swap_chain: native_requests={} settings_native_effective={} \
             settings_hdr_native_surface_enabled={} backend={:?} current={:?} desired={:?} outcome={:?} \
             wsi={{probed={} hdr10={} scrgb={}}} \
             wp={{present={} hdr_supported={:?}}} effective={{present={} hdr_supported={:?}}} \
             output_mode={:?} native_presentation={}",
            snapshot.native_surface_requests_enabled,
            snapshot.settings_native_surface_effective,
            snapshot.settings_hdr_native_surface_enabled,
            snapshot.backend,
            snapshot.current_target_format,
            snapshot.desired_target_format,
            snapshot.swap_request_outcome,
            snapshot.wsi_probed,
            snapshot.wsi_hdr10_st2084_rgb10a2,
            snapshot.wsi_extended_srgb_linear_rgba16f,
            snapshot.wp_selection_present,
            snapshot.wp_hdr_supported,
            snapshot.effective_selection_present,
            snapshot.effective_hdr_supported,
            snapshot.output_mode,
            snapshot.native_presentation_enabled,
        );
    }

    pub(crate) fn log_preload_defer_gate(
        &mut self,
        preload_deferred: bool,
        runtime_probe_completed: bool,
        output_mode: crate::hdr::types::HdrOutputMode,
        effective_selection: Option<&crate::hdr::monitor::HdrMonitorSelection>,
        can_release: bool,
        wsi_probed: bool,
        current_target_format: Option<eframe::wgpu::TextureFormat>,
    ) {
        if !preload_deferred {
            return;
        }
        let capacity_known =
            super::image_management::monitor_hdr_decode_capacity_is_known(effective_selection);
        let snapshot = PreloadDeferGateSnapshot {
            preload_deferred,
            runtime_probe_completed,
            output_mode,
            effective_hdr_supported: effective_selection.map(|s| s.hdr_supported),
            capacity_known,
            can_release,
            wsi_probed,
            current_target_format,
        };
        if self.last_preload_defer_gate == Some(snapshot) {
            return;
        }
        self.last_preload_defer_gate = Some(snapshot);
        crate::preload_debug!(
            "[PreloadDebug][HDR-Gate] preload_defer: runtime_probe_completed={} \
             output_mode={:?} effective_hdr_supported={:?} capacity_known={} can_release={} \
             wsi_probed={} current_target_format={:?}",
            snapshot.runtime_probe_completed,
            snapshot.output_mode,
            snapshot.effective_hdr_supported,
            snapshot.capacity_known,
            snapshot.can_release,
            snapshot.wsi_probed,
            snapshot.current_target_format,
        );
    }
}
