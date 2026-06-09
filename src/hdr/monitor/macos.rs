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

use super::types::{HdrMonitorSelection, HdrNativeSurfaceEncoding};
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn macos_edr_selection_from_values(
    label: String,
    current_edr_capacity: f32,
    potential_edr_capacity: f32,
    reference_edr_capacity: f32,
) -> HdrMonitorSelection {
    let (capacity, source) = if let Some(value) =
        finite_positive_capacity(current_edr_capacity).filter(|value| *value > 1.0)
    {
        (
            Some(value),
            Some("macOS maximumExtendedDynamicRangeColorComponentValue"),
        )
    } else {
        (None, None)
    };
    let hdr_supported = capacity.is_some()
        || finite_positive_capacity(potential_edr_capacity).is_some_and(|value| value > 1.0)
        || finite_positive_capacity(reference_edr_capacity).is_some_and(|value| value > 1.0);
    HdrMonitorSelection {
        hdr_supported,
        label,
        max_luminance_nits: None,
        max_full_frame_luminance_nits: None,
        max_hdr_capacity: capacity,
        hdr_capacity_source: source,
        native_surface_encoding: hdr_supported.then_some(HdrNativeSurfaceEncoding::LinearScRgb),
    }
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn finite_positive_capacity(value: f32) -> Option<f32> {
    (value.is_finite() && value > 0.0).then_some(value)
}

#[cfg(target_os = "macos")]
pub(crate) fn macos_active_monitor_hdr_status() -> Result<HdrMonitorSelection, String> {
    let screen = unsafe {
        let app_class = objc_class("NSApplication")?;
        let app = objc_msg_send_id(app_class, objc_sel("sharedApplication")?);
        let mut window = if app.is_null() {
            std::ptr::null_mut()
        } else {
            objc_msg_send_id(app, objc_sel("keyWindow")?)
        };
        if window.is_null() && !app.is_null() {
            window = objc_msg_send_id(app, objc_sel("mainWindow")?);
        }

        let mut screen = if window.is_null() {
            std::ptr::null_mut()
        } else {
            objc_msg_send_id(window, objc_sel("screen")?)
        };
        if screen.is_null() {
            let screen_class = objc_class("NSScreen")?;
            screen = objc_msg_send_id(screen_class, objc_sel("mainScreen")?);
        }
        screen
    };
    if screen.is_null() {
        return Err("active NSScreen was not found".to_string());
    }

    let label = unsafe {
        let localized_name = objc_msg_send_id(screen, objc_sel("localizedName")?);
        ns_string_to_string(localized_name).unwrap_or_else(|| "macOS screen".to_string())
    };
    let current = unsafe {
        objc_msg_send_f64(
            screen,
            objc_sel("maximumExtendedDynamicRangeColorComponentValue")?,
        ) as f32
    };
    let potential = unsafe {
        objc_msg_send_f64(
            screen,
            objc_sel("maximumPotentialExtendedDynamicRangeColorComponentValue")?,
        ) as f32
    };
    let reference = unsafe {
        objc_msg_send_f64(
            screen,
            objc_sel("maximumReferenceExtendedDynamicRangeColorComponentValue")?,
        ) as f32
    };

    Ok(macos_edr_selection_from_values(
        label, current, potential, reference,
    ))
}

#[cfg(target_os = "macos")]
type ObjcId = *mut std::ffi::c_void;

#[cfg(target_os = "macos")]
type ObjcSel = *mut std::ffi::c_void;

#[cfg(target_os = "macos")]
#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {}

#[cfg(target_os = "macos")]
#[link(name = "objc")]
unsafe extern "C" {
    fn objc_getClass(name: *const std::ffi::c_char) -> ObjcId;
    fn sel_registerName(name: *const std::ffi::c_char) -> ObjcSel;
    #[link_name = "objc_msgSend"]
    fn objc_msg_send_id(receiver: ObjcId, selector: ObjcSel) -> ObjcId;
}

#[cfg(target_os = "macos")]
fn objc_class(name: &str) -> Result<ObjcId, String> {
    let name = std::ffi::CString::new(name).map_err(|err| err.to_string())?;
    let class = unsafe { objc_getClass(name.as_ptr()) };
    if class.is_null() {
        Err(format!(
            "Objective-C class was not found: {}",
            name.to_string_lossy()
        ))
    } else {
        Ok(class)
    }
}

#[cfg(target_os = "macos")]
fn objc_sel(name: &str) -> Result<ObjcSel, String> {
    let name = std::ffi::CString::new(name).map_err(|err| err.to_string())?;
    let selector = unsafe { sel_registerName(name.as_ptr()) };
    if selector.is_null() {
        Err(format!(
            "Objective-C selector was not found: {}",
            name.to_string_lossy()
        ))
    } else {
        Ok(selector)
    }
}

#[cfg(target_os = "macos")]
unsafe fn objc_msg_send_f64(receiver: ObjcId, selector: ObjcSel) -> f64 {
    let send: unsafe extern "C" fn(ObjcId, ObjcSel) -> f64 =
        unsafe { std::mem::transmute(objc_msg_send_id as *const ()) };
    unsafe { send(receiver, selector) }
}

#[cfg(target_os = "macos")]
unsafe fn ns_string_to_string(value: ObjcId) -> Option<String> {
    if value.is_null() {
        return None;
    }
    let ptr = unsafe { objc_msg_send_id(value, objc_sel("UTF8String").ok()?) };
    if ptr.is_null() {
        return None;
    }
    let text = unsafe { std::ffi::CStr::from_ptr(ptr.cast()).to_string_lossy() };
    Some(text.into_owned())
}
