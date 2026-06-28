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
use std::sync::Once;
#[cfg(target_os = "macos")]
use std::sync::atomic::{AtomicBool, Ordering};

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
use super::objc_util::{self, ObjcId, ObjcSel};

#[cfg(target_os = "macos")]
const OBSERVER_CLASS: &str = "SIVScreenParametersObserver";

#[cfg(target_os = "macos")]
const NOTIFICATION_NAME: &str = "NSApplicationDidChangeScreenParametersNotification";

#[cfg(target_os = "macos")]
extern "C" fn screen_parameters_changed(_this: ObjcId, _sel: ObjcSel, _notification: ObjcId) {
    HEADROOM_DIRTY.store(true, Ordering::Release);
}

#[cfg(target_os = "macos")]
unsafe fn install_observer() -> Result<(), String> {
    unsafe {
        let observer_class = {
            let class_name = CString::new(OBSERVER_CLASS).map_err(|err| err.to_string())?;
            let class = objc_util::objc_allocate_class_pair(
                objc_util::objc_class("NSObject")?,
                class_name.as_ptr(),
                0,
            );
            if class.is_null() {
                objc_util::objc_class(OBSERVER_CLASS)?
            } else {
                let changed_sel = objc_util::objc_sel("screenParametersChanged:")?;
                let types = CString::new("v@:@").map_err(|err| err.to_string())?;
                if !objc_util::class_add_method(
                    class,
                    changed_sel,
                    screen_parameters_changed as *const () as *const std::ffi::c_void,
                    types.as_ptr(),
                ) {
                    return Err("class_addMethod(screenParametersChanged:) failed".into());
                }
                // Class pairs cannot be deallocated; `INSTALL_ONCE` registers at most once.
                objc_util::objc_register_class_pair(class);
                class
            }
        };

        // Observer lives for the process lifetime; NSNotificationCenter retains it (MRC).
        // Intentional no-release — `ensure_observer_installed` is Once-guarded.
        let observer = {
            let allocated =
                objc_util::objc_msg_send_id(observer_class, objc_util::objc_sel("alloc")?);
            objc_util::objc_msg_send_id(allocated, objc_util::objc_sel("init")?)
        };
        if observer.is_null() {
            return Err("SIVScreenParametersObserver init returned null".into());
        }

        let center = objc_util::objc_msg_send_id(
            objc_util::objc_class("NSNotificationCenter")?,
            objc_util::objc_sel("defaultCenter")?,
        );
        let name = objc_util::nsstring(NOTIFICATION_NAME)?;
        let changed_sel = objc_util::objc_sel("screenParametersChanged:")?;
        objc_util::objc_msg_send_add_observer(
            center,
            objc_util::objc_sel("addObserver:selector:name:object:")?,
            observer,
            changed_sel,
            name,
            std::ptr::null_mut(),
        );
        Ok(())
    }
}
