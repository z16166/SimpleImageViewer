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

use crate::loader::{DecodedImage, ImageData, TiledImageSource};
use memmap2::Mmap;
use parking_lot::Mutex;
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tiff::decoder::{Decoder, DecodingResult};
use tiff::tags::Tag;

use core_foundation::array::CFArray;
use core_foundation::base::{CFTypeRef, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};
use core_graphics::color_space::CGColorSpace;
use core_graphics::context::CGContext;
use core_graphics::image::CGImage;
use foreign_types::ForeignType;

// External link to ImageIO and CoreServices
#[link(name = "ImageIO", kind = "framework")]
#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    fn CGImageSourceCreateImageAtIndex(
        source: *const std::ffi::c_void,
        index: usize,
        options: core_foundation::dictionary::CFDictionaryRef,
    ) -> core_graphics::sys::CGImageRef;
    fn CGImageSourceCreateThumbnailAtIndex(
        source: *const std::ffi::c_void,
        index: usize,
        options: core_foundation::dictionary::CFDictionaryRef,
    ) -> core_graphics::sys::CGImageRef;
    fn CGImageSourceCreateWithData(
        data: *const std::ffi::c_void,
        options: core_foundation::dictionary::CFDictionaryRef,
    ) -> *const std::ffi::c_void;
    fn CGImageSourceCopyPropertiesAtIndex(
        source: *const std::ffi::c_void,
        index: usize,
        options: core_foundation::dictionary::CFDictionaryRef,
    ) -> core_foundation::dictionary::CFDictionaryRef;
    fn CFRelease(obj: *const std::ffi::c_void);
    fn CFDictionaryGetValue(
        theDict: core_foundation::dictionary::CFDictionaryRef,
        key: *const std::ffi::c_void,
    ) -> *const std::ffi::c_void;
    fn CGImageSourceGetCount(source: *const std::ffi::c_void) -> usize;
    #[allow(dead_code)]
    fn CGImageCreateWithImageInRect(
        image: core_graphics::sys::CGImageRef,
        rect: core_graphics::geometry::CGRect,
    ) -> core_graphics::sys::CGImageRef;

    fn CFDataCreateWithBytesNoCopy(
        allocator: *const std::ffi::c_void,
        bytes: *const u8,
        length: isize,
        bytesDeallocator: *const std::ffi::c_void,
    ) -> *const std::ffi::c_void;

    // CoreFoundation Constants
    static kCFAllocatorDefault: *const std::ffi::c_void;
    static kCFAllocatorNull: *const std::ffi::c_void;

    // Discovery APIs
    fn CGImageSourceCopyTypeIdentifiers() -> core_foundation::array::CFArrayRef;

    // Property Keys
    static kCGImageSourceShouldCache: core_foundation::string::CFStringRef;
    static kCGImagePropertyPixelWidth: core_foundation::string::CFStringRef;
    static kCGImagePropertyPixelHeight: core_foundation::string::CFStringRef;

    // Thumbnail Keys
    static kCGImageSourceCreateThumbnailWithTransform: core_foundation::string::CFStringRef;
    static kCGImageSourceCreateThumbnailFromImageAlways: core_foundation::string::CFStringRef;
    static kCGImageSourceCreateThumbnailFromImageIfAbsent: core_foundation::string::CFStringRef;
    static kCGImageSourceThumbnailMaxPixelSize: core_foundation::string::CFStringRef;

    // Color Space Keys
    static kCGColorSpaceSRGB: core_foundation::string::CFStringRef;

    fn CGImageSourceGetTypeID() -> core_foundation::base::CFTypeID;
    fn CFRetain(obj: *const std::ffi::c_void) -> *const std::ffi::c_void;
}

pub struct CGImageSource(core_foundation::base::CFTypeRef);

impl TCFType for CGImageSource {
    type Ref = core_foundation::base::CFTypeRef;
    fn as_concrete_TypeRef(&self) -> Self::Ref {
        self.0
    }
    unsafe fn wrap_under_get_rule(reference: Self::Ref) -> Self {
        unsafe {
            CFRetain(reference);
        }
        CGImageSource(reference)
    }
    unsafe fn wrap_under_create_rule(reference: Self::Ref) -> Self {
        CGImageSource(reference)
    }
    fn as_CFTypeRef(&self) -> core_foundation::base::CFTypeRef {
        self.0
    }
    fn type_id() -> core_foundation::base::CFTypeID {
        unsafe { CGImageSourceGetTypeID() }
    }
}

impl Drop for CGImageSource {
    fn drop(&mut self) {
        unsafe {
            CFRelease(self.0);
        }
    }
}

impl Clone for CGImageSource {
    fn clone(&self) -> Self {
        unsafe { CGImageSource::wrap_under_get_rule(self.0) }
    }
}

unsafe impl Send for CGImageSource {}
unsafe impl Sync for CGImageSource {}

unsafe fn get_cf_number_u32(
    dict: &CFDictionary<CFString, CFTypeRef>,
    key: CFTypeRef,
) -> Option<u32> {
    let val_ptr = unsafe { CFDictionaryGetValue(dict.as_concrete_TypeRef(), key as *const _) };
    if !val_ptr.is_null() {
        let type_id = unsafe { core_foundation::base::CFGetTypeID(val_ptr) };
        if type_id == CFNumber::type_id() {
            let cf_num = unsafe { CFNumber::wrap_under_get_rule(val_ptr as _) };
            if let Some(val) = cf_num.to_i64() {
                return Some(val as u32);
            }
        }
    }
    None
}

fn apply_orientation_ctm(
    context: &mut CGContext,
    orientation: u32,
    log_full_w: f64,
    log_full_h: f64,
) {
    match orientation {
        2 => {
            context.translate(log_full_w, 0.0);
            context.scale(-1.0, 1.0);
        }
        3 => {
            context.translate(log_full_w, log_full_h);
            context.rotate(std::f64::consts::PI);
        }
        4 => {
            context.translate(0.0, log_full_h);
            context.scale(1.0, -1.0);
        }
        5 => {
            context.scale(1.0, -1.0);
            context.rotate(-std::f64::consts::FRAC_PI_2);
        }
        6 => {
            context.translate(0.0, log_full_h);
            context.rotate(-std::f64::consts::FRAC_PI_2);
        }
        7 => {
            context.translate(log_full_w, log_full_h);
            context.scale(-1.0, 1.0);
            context.rotate(-std::f64::consts::FRAC_PI_2);
        }
        8 => {
            context.translate(log_full_w, 0.0);
            context.rotate(std::f64::consts::FRAC_PI_2);
        }
        _ => {}
    }
}

