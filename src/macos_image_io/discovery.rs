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
pub fn discover_imageio_codecs() -> Vec<String> {
    unsafe {
        let array_ref = CGImageSourceCopyTypeIdentifiers();
        if !array_ref.is_null() {
            let array: CFArray<CFTypeRef> = CFArray::wrap_under_create_rule(array_ref);
            for uti_ptr in array.iter() {
                let _uti_str_ref = *uti_ptr as CFStringRef;
            }
        }
    }
    vec![
        "tif".to_string(),
        "tiff".to_string(),
        "jpg".to_string(),
        "png".to_string(),
        "heic".to_string(),
    ]
}

#[cfg(all(test, target_os = "macos"))]
