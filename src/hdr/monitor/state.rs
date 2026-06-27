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
    #[cfg(test)]
    pub(crate) runtime_probe_completed_at: Option<Instant>,
    #[cfg(not(test))]
    runtime_probe_completed_at: Option<Instant>,
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
            runtime_probe_completed_at: None,
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
            runtime_probe_completed_at: None,
        }
    }

    pub fn selection(&self) -> Option<&HdrMonitorSelection> {
        self.selection.as_ref()
    }

    pub(crate) fn runtime_probe_completed(&self) -> bool {
        self.runtime_probe_completed
    }

    pub(crate) fn runtime_probe_completed_at(&self) -> Option<Instant> {
        self.runtime_probe_completed_at
    }

    pub fn refresh_from_viewport(
        &mut self,
        ctx: &egui::Context,
        now: Instant,
        hdr_content_visible: bool,
        main_window_outer_top_left: Option<[i32; 2]>,
        settings_spawn_top_left: Option<[i32; 2]>,
    ) -> Option<&HdrMonitorSelection> {
        #[cfg(target_os = "macos")]
        super::macos_screen_parameters::ensure_observer_installed();

        let signature = HdrMonitorSignature::from_main_viewport(ctx);

        // When a spawn-time DXGI probe already seeded a valid selection, record the first
        // viewport signature without re-probing. Runtime probing uses the cached ROOT
        // outer top-left (+20 px, same as spawn); running too early can mis-classify while
        // deferred child viewports are still appearing.
        if self.last_signature.is_none() && self.selection.is_some() {
            self.last_signature = Some(signature);
            return self.selection.as_ref();
        }

        if !self.should_probe(signature, now, hdr_content_visible) {
            return self.selection.as_ref();
        }

        #[cfg(target_os = "windows")]
        {
            let has_probe_anchor = main_window_outer_top_left.is_some()
                || signature
                    .outer_rect
                    .is_some_and(|[left, top, right, bottom]| {
                        i64::from(right.saturating_sub(left)).max(0)
                            * i64::from(bottom.saturating_sub(top)).max(0)
                            >= 64 * 64
                    });
            if !has_probe_anchor && self.selection.is_some() {
                return self.selection.as_ref();
            }
        }

        self.last_signature = Some(signature);
        self.last_probe_at = Some(now);
        match active_monitor_hdr_status(
            signature.outer_rect,
            main_window_outer_top_left,
            signature.native_pixels_per_point(),
            settings_spawn_top_left,
        ) {
            Ok(selection) => {
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
                         (will retry on next viewport change{}; \
                         dynamic HDR↔SDR swap-chain switching is disabled \
                         until probe succeeds)",
                        if cfg!(target_os = "macos") {
                            " or didChangeScreenParametersNotification"
                        } else if cfg!(target_os = "windows") {
                            " / ~200ms timer"
                        } else {
                            " / 750ms timer"
                        }
                    );
                    self.last_probe_failed = true;
                } else {
                    log::debug!("[HDR] active monitor HDR probe still failing: {err}");
                }
            }
        }
        self.runtime_probe_completed = true;
        if self.runtime_probe_completed_at.is_none() {
            self.runtime_probe_completed_at = Some(now);
        }
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
        _hdr_content_visible: bool,
        supports_current_edr_reprobe: bool,
    ) -> bool {
        let interval_elapsed = match self.last_probe_at {
            Some(last_probe_at) => now.duration_since(last_probe_at) >= HDR_MONITOR_PROBE_INTERVAL,
            None => true,
        };
        if self.last_signature == Some(signature) {
            if supports_current_edr_reprobe {
                return self.should_probe_macos_edr_headroom(interval_elapsed);
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

    /// macOS live EDR headroom refresh — notification-driven per Apple docs (no timer poll).
    ///
    /// **Side effect:** consumes a pending `didChangeScreenParametersNotification` via
    /// [`take_headroom_refresh_pending`](super::macos_screen_parameters::take_headroom_refresh_pending).
    /// Callers must run a full NSScreen probe when this returns `true` (today only
    /// `should_probe_for_platform` when the viewport signature is unchanged).
    ///
    /// [`NSApplication.didChangeScreenParametersNotification`](https://developer.apple.com/documentation/appkit/nsapplication/didchangescreenparametersnotification)
    /// when [`maximumExtendedDynamicRangeColorComponentValue`](https://developer.apple.com/documentation/appkit/nsscreen/maximumextendeddynamicrangecolorcomponentvalue)
    /// changes; plus the first probe until **potential** headroom is known. Viewport signature
    /// changes are handled by the caller (`should_probe_for_platform` returns `true` earlier).
    /// See `macos_screen_parameters.rs` and `macos.rs`.
    fn should_probe_macos_edr_headroom(&self, interval_elapsed: bool) -> bool {
        if super::macos_screen_parameters::take_headroom_refresh_pending() {
            return true;
        }
        // Potential headroom unknown: retry on the standard probe interval, not every frame.
        interval_elapsed
            && self.selection.as_ref().is_some_and(|selection| {
                selection.hdr_supported && selection.max_hdr_capacity.is_none()
            })
    }
}
