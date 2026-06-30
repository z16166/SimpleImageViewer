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

//! macOS Extended Dynamic Range (EDR) headroom — policy and Apple references.
//!
//! Apple exposes **three different** `NSScreen` scalars. They must not be conflated:
//!
//! | Property | Role | Stability |
//! |----------|------|-----------|
//! | [`maximumPotentialExtendedDynamicRangeColorComponentValue`](https://developer.apple.com/documentation/appkit/nsscreen/maximumpotentialextendeddynamicrangecolorcomponentvalue) | Upper bound when the screen is in EDR mode | Fixed when the `NSScreen` object is created |
//! | [`maximumExtendedDynamicRangeColorComponentValue`](https://developer.apple.com/documentation/appkit/nsscreen/maximumextendeddynamicrangecolorcomponentvalue) | **Current** available headroom (`1.0` = no EDR) | Dynamic (ambient, brightness, on-screen EDR content) |
//! | [`maximumReferenceExtendedDynamicRangeColorComponentValue`](https://developer.apple.com/documentation/appkit/nsscreen/maximumreferenceextendeddynamicrangecolorcomponentvalue) | Reference-monitor ceiling | `0` on non-reference hardware |
//!
//! Apple documentation states that the **actual** maximum may be lower than potential and
//! **can change dynamically**; when current headroom changes the system posts
//! [`NSApplication.didChangeScreenParametersNotification`](https://developer.apple.com/documentation/appkit/nsapplication/didchangescreenparametersnotification).
//! Apple does **not** specify any percentage tolerance or hysteresis — apps must treat current
//! headroom as live display state, not a stable decode cache key.
//!
//! **WWDC22 — Display EDR content with Core Image, Metal, and SwiftUI** ([video 10114](https://developer.apple.com/videos/play/wwdc2022/10114/)):
//! enable EDR on the layer ([`CAMetalLayer.wantsExtendedDynamicRangeContent`](https://developer.apple.com/documentation/quartzcore/cametallayer/wantsextendeddynamicrangecontent)),
//! use a float pixel format, then **read current headroom before every draw** and tone-map to it.
//!
//! ## How Simple Image Viewer maps this
//!
//! - [`HdrMonitorSelection::max_hdr_capacity`] ← **potential** (decode / loader / cache invalidation).
//!   Scene-linear HQ RAW tone-maps at display time; potential is a stable conservative ceiling.
//! - [`HdrMonitorSelection::current_edr_headroom`] ← **current** (per-frame tone-map via
//!   [`Settings::hdr_tone_map_settings_for_monitor`](crate::settings::Settings::hdr_tone_map_settings_for_monitor)).
//! - [`HdrMonitorState`](super::state::HdrMonitorState) listens for
//!   [`NSApplication.didChangeScreenParametersNotification`](https://developer.apple.com/documentation/appkit/nsapplication/didchangescreenparametersnotification)
//!   (`macos_screen_parameters.rs`) to refresh `current_edr_headroom` without a timer poll.
//!   Viewport signature changes still trigger an immediate full probe (monitor move / resize).
//!
//! Related: [`crate::app::preload::ultra_hdr_decode_capacity_for_output_mode`],
//! [`crate::app::image_management::monitor_hdr_decode_capacity_is_known`],
//! [`crate::app::image_management::startup_preload_defer_can_release`],
//! `src/app/image_management/hdr_state.rs` (`refresh_ultra_hdr_decode_capacity`).

use super::types::{HdrMonitorSelection, HdrNativeSurfaceEncoding};
#[cfg_attr(not(target_os = "macos"), allow(dead_code, unused_variables))]
pub(crate) fn macos_edr_selection_from_values(
    label: String,
    current_edr_capacity: f32,
    potential_edr_capacity: f32,
    reference_edr_capacity: f32,
) -> HdrMonitorSelection {
    // Map probe results into HdrMonitorSelection — see module docs above for Apple sources.
    let potential_cap =
        finite_positive_capacity(potential_edr_capacity).filter(|value| *value > 1.0);
    #[cfg(target_os = "macos")]
    let current_headroom = finite_positive_capacity(current_edr_capacity);
    // Decode path: potential only (Apple: "determined when you create the NSScreen object,
    // and doesn't change afterwards").
    // https://developer.apple.com/documentation/appkit/nsscreen/maximumpotentialextendeddynamicrangecolorcomponentvalue
    let (capacity, source) = if let Some(value) = potential_cap {
        (
            Some(value),
            Some("macOS maximumPotentialExtendedDynamicRangeColorComponentValue"),
        )
    } else {
        (None, None)
    };
    // hdr_supported: potential > 1.0 and/or reference > 1.0 per Apple property semantics.
    let hdr_supported = potential_cap.is_some()
        || finite_positive_capacity(reference_edr_capacity).is_some_and(|value| value > 1.0);
    HdrMonitorSelection {
        hdr_supported,
        label,
        max_luminance_nits: None,
        max_full_frame_luminance_nits: None,
        max_hdr_capacity: capacity,
        hdr_capacity_source: source,
        // Display path: current headroom (Apple: "current maximum"; query before each draw —
        // WWDC22 10114). Stored here; refreshed via didChangeScreenParametersNotification
        // (`macos_screen_parameters.rs`) without decode invalidation.
        // https://developer.apple.com/documentation/appkit/nsscreen/maximumextendeddynamicrangecolorcomponentvalue
        #[cfg(target_os = "macos")]
        current_edr_headroom: current_headroom,
        native_surface_encoding: hdr_supported.then_some(HdrNativeSurfaceEncoding::LinearScRgb),
        reference_luminance_nits: None,
        linux_wp_transfer: None,
        linux_wp_primaries: None,
        linux_explicit_hdr_state: None,
        linux_explicit_hdr_state_source: None,
    }
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn finite_positive_capacity(value: f32) -> Option<f32> {
    (value.is_finite() && value > 0.0).then_some(value)
}

#[cfg(target_os = "macos")]
pub(crate) fn macos_active_monitor_hdr_status() -> Result<HdrMonitorSelection, String> {
    use super::objc_util;

    let screen = unsafe {
        let app_class = objc_util::objc_class("NSApplication")?;
        let app = objc_util::objc_msg_send_id(app_class, objc_util::objc_sel("sharedApplication")?);
        let mut window = if app.is_null() {
            std::ptr::null_mut()
        } else {
            objc_util::objc_msg_send_id(app, objc_util::objc_sel("keyWindow")?)
        };
        if window.is_null() && !app.is_null() {
            window = objc_util::objc_msg_send_id(app, objc_util::objc_sel("mainWindow")?);
        }

        let mut screen = if window.is_null() {
            std::ptr::null_mut()
        } else {
            objc_util::objc_msg_send_id(window, objc_util::objc_sel("screen")?)
        };
        if screen.is_null() {
            let screen_class = objc_util::objc_class("NSScreen")?;
            screen = objc_util::objc_msg_send_id(screen_class, objc_util::objc_sel("mainScreen")?);
        }
        screen
    };
    if screen.is_null() {
        return Err("active NSScreen was not found".to_string());
    }

    let label = unsafe {
        let localized_name =
            objc_util::objc_msg_send_id(screen, objc_util::objc_sel("localizedName")?);
        objc_util::ns_string_to_string(localized_name).unwrap_or_else(|| "macOS screen".to_string())
    };
    // NSScreen EDR probes — property semantics documented in the module header above.
    let current = unsafe {
        objc_util::objc_msg_send_f64(
            screen,
            objc_util::objc_sel("maximumExtendedDynamicRangeColorComponentValue")?,
        ) as f32
    };
    let potential = unsafe {
        objc_util::objc_msg_send_f64(
            screen,
            objc_util::objc_sel("maximumPotentialExtendedDynamicRangeColorComponentValue")?,
        ) as f32
    };
    let reference = unsafe {
        objc_util::objc_msg_send_f64(
            screen,
            objc_util::objc_sel("maximumReferenceExtendedDynamicRangeColorComponentValue")?,
        ) as f32
    };

    Ok(macos_edr_selection_from_values(
        label, current, potential, reference,
    ))
}
