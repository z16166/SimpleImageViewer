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

//! Shared Objective-C runtime helpers for macOS HDR monitor probes.
//!
//! All `objc_msgSend` variants live in one `extern "C"` block. `objc_msgSend` is one C symbol
//! with per-selector calling conventions; multiple typed wrappers are the standard pattern.

#![allow(clashing_extern_declarations)]

pub(crate) type ObjcId = *mut std::ffi::c_void;
pub(crate) type ObjcSel = *mut std::ffi::c_void;

#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {}

#[link(name = "objc")]
unsafe extern "C" {
    fn objc_getClass(name: *const std::ffi::c_char) -> ObjcId;
    fn sel_registerName(name: *const std::ffi::c_char) -> ObjcSel;
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
    fn objc_msg_send_id_raw(receiver: ObjcId, selector: ObjcSel) -> ObjcId;
    #[link_name = "objc_msgSend"]
    fn objc_msg_send_f64_raw(receiver: ObjcId, selector: ObjcSel) -> f64;
    #[link_name = "objc_msgSend"]
    fn objc_msg_send_add_observer_raw(
        receiver: ObjcId,
        selector: ObjcSel,
        observer: ObjcId,
        observer_sel: ObjcSel,
        name: ObjcId,
        object: ObjcId,
    );
    #[link_name = "objc_msgSend"]
    fn objc_msg_send_string_with_utf8(
        receiver: ObjcId,
        selector: ObjcSel,
        utf8: *const std::ffi::c_char,
    ) -> ObjcId;
}

pub(crate) fn objc_class(name: &str) -> Result<ObjcId, String> {
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

pub(crate) fn objc_sel(name: &str) -> Result<ObjcSel, String> {
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

pub(crate) unsafe fn objc_msg_send_f64(receiver: ObjcId, selector: ObjcSel) -> f64 {
    unsafe { objc_msg_send_f64_raw(receiver, selector) }
}

pub(crate) unsafe fn nsstring(text: &str) -> Result<ObjcId, String> {
    let utf8 = std::ffi::CString::new(text).map_err(|err| err.to_string())?;
    let value = unsafe {
        objc_msg_send_string_with_utf8(
            objc_class("NSString")?,
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

pub(crate) unsafe fn ns_string_to_string(value: ObjcId) -> Option<String> {
    if value.is_null() {
        return None;
    }
    let ptr = unsafe { objc_msg_send_id_raw(value, objc_sel("UTF8String").ok()?) };
    if ptr.is_null() {
        return None;
    }
    let text = unsafe { std::ffi::CStr::from_ptr(ptr.cast()).to_string_lossy() };
    Some(text.into_owned())
}

pub(crate) unsafe fn objc_allocate_class_pair(
    superclass: ObjcId,
    name: *const std::ffi::c_char,
    extra_bytes: usize,
) -> ObjcId {
    unsafe { objc_allocateClassPair(superclass, name, extra_bytes) }
}

pub(crate) unsafe fn objc_register_class_pair(cls: ObjcId) {
    unsafe { objc_registerClassPair(cls) }
}

pub(crate) unsafe fn class_add_method(
    cls: ObjcId,
    name: ObjcSel,
    imp: *const std::ffi::c_void,
    types: *const std::ffi::c_char,
) -> bool {
    unsafe { class_addMethod(cls, name, imp, types) }
}

pub(crate) unsafe fn objc_msg_send_id(receiver: ObjcId, selector: ObjcSel) -> ObjcId {
    unsafe { objc_msg_send_id_raw(receiver, selector) }
}

pub(crate) unsafe fn objc_msg_send_add_observer(
    receiver: ObjcId,
    selector: ObjcSel,
    observer: ObjcId,
    observer_sel: ObjcSel,
    name: ObjcId,
    object: ObjcId,
) {
    unsafe {
        objc_msg_send_add_observer_raw(receiver, selector, observer, observer_sel, name, object);
    }
}
