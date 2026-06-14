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
use std::time::{Duration, Instant};

use eframe::egui;

use super::effective::active_monitor_hdr_status;
use super::types::{HDR_MONITOR_PROBE_INTERVAL, HdrMonitorSelection, HdrMonitorSignature};
#[derive(Debug)]
pub struct HdrMonitorState {
    #[cfg(test)]
    pub(crate) last_signature: Option<HdrMonitorSignature>,
    #[cfg(not(test))]
    last_signature: Option<HdrMonitorSignature>,
    #[cfg(test)]
    pub(crate) last_probe_at: Option<Instant>,
    #[cfg(not(test))]
    last_probe_at: Option<Instant>,
    #[cfg(test)]
    pub(crate) selection: Option<HdrMonitorSelection>,
    #[cfg(not(test))]
    selection: Option<HdrMonitorSelection>,
    /// True after the first HWND-based [`active_monitor_hdr_status`] attempt (success or failure).
    /// Startup preload deferral waits for this so spawn-time DXGI (often the wrong monitor / SDR)
    /// does not release loads before the runtime probe settles decode capacity.
    #[cfg(test)]
    pub(crate) runtime_probe_completed: bool,
    #[cfg(not(test))]
    runtime_probe_completed: bool,
    /// Sticky flag so we only `warn!` on the first failure in a streak of
    /// consecutive failures (avoid log spam at 1.33 Hz). Cleared the moment
    /// any probe succeeds.
    last_probe_failed: bool,
}

impl Default for HdrMonitorState {
    fn default() -> Self {
        Self {
            last_signature: None,
            last_probe_at: None,
            selection: None,
            last_probe_failed: false,
            runtime_probe_completed: false,
        }
    }
}

impl HdrMonitorState {
    /// Same as [`Default::default`], but starts with a known DXGI spawn outcome so
    /// [`crate::hdr::monitor::effective_capability_output_mode`] matches the swap chain
    /// from frame zero (see [`crate::hdr::surface::initial_monitor_selection_from_environment_probe`]).
    pub fn with_initial_selection(selection: Option<HdrMonitorSelection>) -> Self {
        Self {
            last_signature: None,
            last_probe_at: None,
            selection,
            last_probe_failed: false,
            runtime_probe_completed: false,
        }
    }

    pub fn selection(&self) -> Option<&HdrMonitorSelection> {
        self.selection.as_ref()
    }

    pub(crate) fn runtime_probe_completed(&self) -> bool {
        self.runtime_probe_completed
    }

    pub fn refresh_from_viewport(
        &mut self,
        ctx: &egui::Context,
        now: Instant,
        hdr_content_visible: bool,
    ) -> Option<&HdrMonitorSelection> {
        let signature = ctx.input(|input| HdrMonitorSignature::from_viewport(input.viewport()));

        // When a spawn-time DXGI probe already seeded a valid selection (via
        // `with_initial_selection`), record the first HWND signature without
        // re-probing.  The runtime probe relies on the HWND rect, which the OS
        // may not have placed yet on the first frame — `MonitorFromPoint` at a
        // tiny default-rect centre would land on the wrong display and overwrite
        // the correct seed.
        if self.last_signature.is_none() && self.selection.is_some() {
            self.last_signature = Some(signature);
            return self.selection.as_ref();
        }

        if !self.should_probe(signature, now, hdr_content_visible) {
            return self.selection.as_ref();
        }

        self.last_signature = Some(signature);
        self.last_probe_at = Some(now);
        match active_monitor_hdr_status(signature.outer_rect) {
            Ok(selection) => {
                if self.selection.as_ref() != Some(&selection) {
                    log::info!(
                        "[HDR] active_monitor={} hdr_supported={} max_luminance_nits={:?} max_full_frame_luminance_nits={:?} max_hdr_capacity={:?} hdr_capacity_source={:?}",
                        selection.label,
                        selection.hdr_supported,
                        selection.max_luminance_nits,
                        selection.max_full_frame_luminance_nits,
                        selection.max_hdr_capacity,
                        selection.hdr_capacity_source
                    );
                }
                self.selection = Some(selection);
            }
            Err(err) => {
                // Promoted from debug → warn so it's visible at the default
                // log level: when the runtime active-monitor probe never
                // succeeds, the cross-monitor swap-chain hot-swap chain is
                // dead in the water (we have no `selection`, so
                // `desired_target_format_for_active_monitor` always returns
                // `Bgra8Unorm` and mismatches never fire). The first failure
                // is what we need to see in user logs to diagnose.
                if !self.last_probe_failed {
                    log::warn!(
                        "[HDR] active monitor HDR probe FAILED: {err} \
                         (will retry on next viewport change / 750ms; \
                         dynamic HDR↔SDR swap-chain switching is disabled \
                         until probe succeeds)"
                    );
                    self.last_probe_failed = true;
                } else {
                    log::debug!("[HDR] active monitor HDR probe still failing: {err}");
                }
            }
        }
        self.runtime_probe_completed = true;
        if self.selection.is_some() {
            self.last_probe_failed = false;
        }
        self.selection.as_ref()
    }

    pub(crate) fn should_probe(
        &self,
        signature: HdrMonitorSignature,
        now: Instant,
        hdr_content_visible: bool,
    ) -> bool {
        self.should_probe_for_platform(
            signature,
            now,
            hdr_content_visible,
            cfg!(target_os = "macos"),
        )
    }

    pub(crate) fn should_probe_for_platform(
        &self,
        signature: HdrMonitorSignature,
        now: Instant,
        hdr_content_visible: bool,
        supports_current_edr_reprobe: bool,
    ) -> bool {
        let interval_elapsed = match self.last_probe_at {
            Some(last_probe_at) => now.duration_since(last_probe_at) >= HDR_MONITOR_PROBE_INTERVAL,
            None => true,
        };
        if self.last_signature == Some(signature) {
            if supports_current_edr_reprobe {
                return hdr_content_visible
                    && self.should_reprobe_current_edr_capacity(supports_current_edr_reprobe)
                    && interval_elapsed;
            }
            // Windows (and other non-macOS): `HdrMonitorSignature` can stay identical for
            // many frames while the native frame is dragged between monitors because
            // `egui::ViewportInfo::outer_rect` may not update until the move ends. The DXGI
            // monitor bound to the main HWND still changes; without a timer reprobe,
            // `active_monitor_hdr_status` never runs again and HDR↔SDR swap-chain switching
            // appears permanently stuck.
            //
            // Windows uses a shorter poll than `HDR_MONITOR_PROBE_INTERVAL` so cross-monitor
            // drags do not wait ~750ms after the outer rect stops changing.
            if cfg!(target_os = "windows") {
                return match self.last_probe_at {
                    Some(last_probe_at) => {
                        now.duration_since(last_probe_at) >= Duration::from_millis(200)
                    }
                    None => true,
                };
            }
            return interval_elapsed;
        }

        // Viewport signature changed (outer rect, reported monitor size, or PPP). This almost
        // always means a cross-monitor move or resize that can change DXGI `ColorSpace` / EDR
        // while the swap-chain format is being hot-swapped. We must **not** gate on the generic
        // 750 ms interval here: until `selection` catches up, `effective_render_output_mode` can
        // pair `Rgba16Float` with stale `hdr_supported = false`, which runs `encode_sdr` (γ
        // encoded for 8-bit) into a linear scRGB buffer — lifted blacks on SDR and visible color
        // skew when switching to HDR. Always probe immediately on signature change.
        true
    }

    fn should_reprobe_current_edr_capacity(&self, supports_current_edr_reprobe: bool) -> bool {
        if !supports_current_edr_reprobe {
            return false;
        }
        self.selection.as_ref().is_some_and(|selection| {
            selection.hdr_supported && selection.max_hdr_capacity.is_none()
        })
    }
}
