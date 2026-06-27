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

//! macOS EDR headroom change notifications (event-driven, not timer polling).
//!
//! Apple documents that when [`maximumExtendedDynamicRangeColorComponentValue`](https://developer.apple.com/documentation/appkit/nsscreen/maximumextendeddynamicrangecolorcomponentvalue)
//! changes, the system posts
//! [`NSApplication.didChangeScreenParametersNotification`](https://developer.apple.com/documentation/appkit/nsapplication/didchangescreenparametersnotification).
//! We register for that notification once and re-probe NSScreen only when it fires (plus viewport
//! signature changes and the first potential-headroom probe). Policy: `macos.rs`.

#[cfg(target_os = "macos")]
use std::ffi::CString;
#[cfg(target_os = "macos")]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(target_os = "macos")]
use std::sync::Once;

#[cfg(target_os = "macos")]
static INSTALL_ONCE: Once = Once::new();
#[cfg(target_os = "macos")]
static HEADROOM_DIRTY: AtomicBool = AtomicBool::new(false);

#[cfg(not(target_os = "macos"))]
pub(crate) fn ensure_observer_installed() {}

#[cfg(target_os = "macos")]
pub(crate) fn ensure_observer_installed() {
    INSTALL_ONCE.call_once(|| {
        if let Err(err) = unsafe { install_observer() } {
            log::warn!(
                "[HDR] macOS didChangeScreenParametersNotification observer failed: {err} \
                 (EDR headroom updates fall back to viewport-change probes only)"
            );
        } else {
            log::debug!(
                "[HDR] macOS listening for NSApplication.didChangeScreenParametersNotification"
            );
        }
    });
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn take_headroom_refresh_pending() -> bool {
    false
}

#[cfg(target_os = "macos")]
pub(crate) fn take_headroom_refresh_pending() -> bool {
    HEADROOM_DIRTY.swap(false, Ordering::AcqRel)
}

#[cfg(all(test, target_os = "macos"))]
pub(crate) fn test_set_headroom_refresh_pending() {
    HEADROOM_DIRTY.store(true, Ordering::Release);
}

#[cfg(target_os = "macos")]
type ObjcId = *mut std::ffi::c_void;

#[cfg(target_os = "macos")]
type ObjcSel = *mut std::ffi::c_void;

#[cfg(target_os = "macos")]
const OBSERVER_CLASS: &str = "SIVScreenParametersObserver";

#[cfg(target_os = "macos")]
const NOTIFICATION_NAME: &str = "NSApplicationDidChangeScreenParametersNotification";

#[cfg(target_os = "macos")]
extern "C" fn screen_parameters_changed(
    _this: ObjcId,
    _sel: ObjcSel,
    _notification: ObjcId,
) {
    HEADROOM_DIRTY.store(true, Ordering::Release);
}

#[cfg(target_os = "macos")]
unsafe fn install_observer() -> Result<(), String> {
    #[link(name = "objc")]
    unsafe extern "C" {
        fn objc_allocateClassPair(
            superclass: ObjcId,
            name: *const std::ffi::c_char,
            extra_bytes: usize,
        ) -> ObjcId;
        fn objc_registerClassPair(cls: ObjcId);
        fn class_addMethod(
            cls: ObjcId,
            name: ObjcSel,
            imp: *const std::ffi::c_void,
            types: *const std::ffi::c_char,
        ) -> bool;
        #[link_name = "objc_msgSend"]
        fn objc_msg_send_id(receiver: ObjcId, selector: ObjcSel) -> ObjcId;
    }

    unsafe {
        let observer_class = {
            let class_name = CString::new(OBSERVER_CLASS).map_err(|err| err.to_string())?;
            let class = objc_allocateClassPair(
                objc_get_class("NSObject")?,
                class_name.as_ptr(),
                0,
            );
            if class.is_null() {
                objc_get_class(OBSERVER_CLASS)?
            } else {
                let changed_sel = objc_sel("screenParametersChanged:")?;
                let types = CString::new("v@:@").map_err(|err| err.to_string())?;
                if !class_addMethod(
                    class,
                    changed_sel,
                    screen_parameters_changed as *const () as *const std::ffi::c_void,
                    types.as_ptr(),
                ) {
                    return Err("class_addMethod(screenParametersChanged:) failed".into());
                }
                // Class pairs cannot be deallocated; `INSTALL_ONCE` registers at most once.
                objc_registerClassPair(class);
                class
            }
        };

        // Observer lives for the process lifetime; NSNotificationCenter retains it (MRC).
        // Intentional no-release — `ensure_observer_installed` is Once-guarded.
        let observer = {
            let allocated = objc_msg_send_id(observer_class, objc_sel("alloc")?);
            objc_msg_send_id(allocated, objc_sel("init")?)
        };
        if observer.is_null() {
            return Err("SIVScreenParametersObserver init returned null".into());
        }

        let center = objc_msg_send_id(
            objc_get_class("NSNotificationCenter")?,
            objc_sel("defaultCenter")?,
        );
        let name = nsstring(NOTIFICATION_NAME)?;
        let changed_sel = objc_sel("screenParametersChanged:")?;
        let add_observer: unsafe extern "C" fn(ObjcId, ObjcSel, ObjcId, ObjcSel, ObjcId, ObjcId) =
            std::mem::transmute(objc_msg_send_id as *const ());
        add_observer(
            center,
            objc_sel("addObserver:selector:name:object:")?,
            observer,
            changed_sel,
            name,
            std::ptr::null_mut(),
        );
        Ok(())
    }
}

#[cfg(target_os = "macos")]
fn objc_sel(name: &str) -> Result<ObjcSel, String> {
    unsafe extern "C" {
        fn sel_registerName(name: *const std::ffi::c_char) -> ObjcSel;
    }
    let name = CString::new(name).map_err(|err| err.to_string())?;
    let selector = unsafe { sel_registerName(name.as_ptr()) };
    if selector.is_null() {
        Err(format!("Objective-C selector was not found: {name:?}"))
    } else {
        Ok(selector)
    }
}

#[cfg(target_os = "macos")]
unsafe fn objc_get_class(name: &str) -> Result<ObjcId, String> {
    unsafe extern "C" {
        fn objc_getClass(name: *const std::ffi::c_char) -> ObjcId;
    }
    let name = CString::new(name).map_err(|err| err.to_string())?;
    let class = unsafe { objc_getClass(name.as_ptr()) };
    if class.is_null() {
        Err(format!("Objective-C class was not found: {name:?}"))
    } else {
        Ok(class)
    }
}

#[cfg(target_os = "macos")]
unsafe fn nsstring(text: &str) -> Result<ObjcId, String> {
    unsafe extern "C" {
        #[link_name = "objc_msgSend"]
        fn objc_msg_send_id(receiver: ObjcId, selector: ObjcSel) -> ObjcId;
    }
    let utf8 = CString::new(text).map_err(|err| err.to_string())?;
    let string_with_utf8: unsafe extern "C" fn(ObjcId, ObjcSel, *const std::ffi::c_char) -> ObjcId =
        unsafe { std::mem::transmute(objc_msg_send_id as *const ()) };
    let value = unsafe {
        string_with_utf8(
            objc_get_class("NSString")?,
            objc_sel("stringWithUTF8String:")?,
            utf8.as_ptr(),
        )
    };
    if value.is_null() {
        Err(format!("NSString allocation failed for {text}"))
    } else {
        Ok(value)
    }
}
