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
pub fn load_via_image_io(
    path: &Path,
    high_quality: bool,
    orientation_override: Option<u16>,
) -> Result<ImageData, String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default();
    let mmap = Arc::new(crate::mmap_util::map_file(path)?);
    unsafe {
        let cf_data = CFDataCreateWithBytesNoCopy(
            kCFAllocatorDefault,
            mmap.as_ptr(),
            mmap.len() as isize,
            kCFAllocatorNull,
        );

        if cf_data.is_null() {
            return Err("Failed to create CFData from mmap".to_string());
        }

        let options = CFDictionary::from_CFType_pairs(&[(
            CFString::wrap_under_get_rule(kCGImageSourceShouldCache).as_CFType(),
            CFBoolean::false_value().as_CFType(),
        )]);

        let source_ref = CGImageSourceCreateWithData(cf_data, options.as_CFTypeRef() as _);
        CFRelease(cf_data);

        if source_ref.is_null() {
            return Err("Failed to create CGImageSource from data (mmap)".to_string());
        }

        let source = CGImageSource::wrap_under_create_rule(source_ref as _);
        let mut physical_width = 0u32;
        let mut physical_height = 0u32;

        let props_ref =
            CGImageSourceCopyPropertiesAtIndex(source.as_concrete_TypeRef(), 0, std::ptr::null());
        if !props_ref.is_null() {
            let props = CFDictionary::<CFString, CFTypeRef>::wrap_under_create_rule(props_ref as _);

            // Diagnostics: Check if TIFF is tiled or stripped
            let tiff_key = CFString::from_static_string("{TIFF}");
            if let Some(tiff_props_ref) = props.find(&tiff_key) {
                let tiff_props =
                    CFDictionary::<CFString, CFTypeRef>::wrap_under_get_rule(*tiff_props_ref as _);
                let tw_key = CFString::from_static_string("TileWidth");
                let th_key = CFString::from_static_string("TileHeight");

                if tiff_props.contains_key(&tw_key) && tiff_props.contains_key(&th_key) {
                    log::info!("TIFF Diagnostics: [{}] is TILED", path.display());
                } else {
                    log::info!(
                        "TIFF Diagnostics: [{}] is STRIPPED (Potentially slower random access)",
                        path.display()
                    );
                }
            }

            physical_width =
                get_cf_number_u32(&props, kCGImagePropertyPixelWidth as _).unwrap_or(0);
            physical_height =
                get_cf_number_u32(&props, kCGImagePropertyPixelHeight as _).unwrap_or(0);
        }

        let orientation = orientation_override
            .unwrap_or_else(|| crate::metadata_utils::get_exif_orientation(path))
            as u32;

        if physical_width == 0 || physical_height == 0 {
            return Err(
                "Failed to read image dimensions from metadata. File might be corrupted.".into(),
            );
        }

        let (logical_width, logical_height) = if orientation >= 5 && orientation <= 8 {
            (physical_height, physical_width)
        } else {
            (physical_width, physical_height)
        };

        let tiled_threshold = crate::tile_cache::TILED_THRESHOLD.load(Ordering::Relaxed);
        if (logical_width as u64 * logical_height as u64) < tiled_threshold {
            let options_decode = CFDictionary::from_CFType_pairs(&[(
                CFString::wrap_under_get_rule(kCGImageSourceShouldCache).as_CFType(),
                CFBoolean::false_value().as_CFType(),
            )]);
            let cg_image_ref = CGImageSourceCreateImageAtIndex(
                source.as_concrete_TypeRef(),
                0,
                options_decode.as_CFTypeRef() as _,
            );
            if cg_image_ref.is_null() {
                return Err("Failed to decode".to_string());
            }
            let decoded = render_cgimage_to_rgba_sync(
                &CGImage::from_ptr(cg_image_ref),
                orientation,
                logical_width,
                logical_height,
            );
            return Ok(ImageData::Static(decoded));
        }

        // --- Tiled Path Selection ---
        // Optimization: For giant STRIPPED TIFFs, use our custom caching loader to avoid CoreGraphics re-decoding overhead.
        let is_tiff = ext == "tif" || ext == "tiff";
        let mut is_stripped = false;
        if is_tiff {
            let props_ref = CGImageSourceCopyPropertiesAtIndex(
                source.as_concrete_TypeRef(),
                0,
                std::ptr::null(),
            );
            if !props_ref.is_null() {
                let props =
                    CFDictionary::<CFString, CFTypeRef>::wrap_under_create_rule(props_ref as _);
                let tiff_key = CFString::from_static_string("{TIFF}");
                if let Some(tiff_props_ref) = props.find(&tiff_key) {
                    let tiff_props = CFDictionary::<CFString, CFTypeRef>::wrap_under_get_rule(
                        *tiff_props_ref as _,
                    );
                    let tw_key = CFString::from_static_string("TileWidth");
                    let th_key = CFString::from_static_string("TileHeight");
                    if !tiff_props.contains_key(&tw_key) || !tiff_props.contains_key(&th_key) {
                        is_stripped = true;
                    }
                }
            }
        }

        if is_tiff && is_stripped {
            log::info!(
                "MacOS ImageIO: Giant STRIPPED TIFF detected ({}x{}, orientation {}). Using TiffStripCachingSource.",
                physical_width,
                physical_height,
                orientation
            );

            // Get RowsPerStrip or fallback to a reasonable chunk size (e.g., 256 for optimal scrolling)
            let mut chunk_h = 256;
            let cursor = Cursor::new(&mmap[..]);
            if let Ok(mut decoder) = Decoder::new(cursor) {
                chunk_h = decoder
                    .get_tag_u32(Tag::RowsPerStrip)
                    .unwrap_or(256)
                    .min(physical_height);
            }

            let options_decode = CFDictionary::from_CFType_pairs(&[(
                CFString::wrap_under_get_rule(kCGImageSourceShouldCache).as_CFType(),
                CFBoolean::true_value().as_CFType(),
            )]);
            let cg_image_ref = CGImageSourceCreateImageAtIndex(
                source.as_concrete_TypeRef(),
                0,
                options_decode.as_CFTypeRef() as _,
            );
            if cg_image_ref.is_null() {
                return Err("Failed to create cached CGImage for optimized path".to_string());
            }
            let cached_image = CGImage::from_ptr(cg_image_ref);

            let color_space = CGColorSpace::create_with_name(
                CFString::wrap_under_get_rule(kCGColorSpaceSRGB).as_concrete_TypeRef(),
            )
            .unwrap_or_else(|| CGColorSpace::create_device_rgb());

            return Ok(ImageData::Tiled(Arc::new(TiffStripCachingSource {
                path: path.to_path_buf(),
                _mmap: Arc::clone(&mmap),
                cached_image,
                color_space,
                physical_width,
                physical_height,
                logical_width,
                logical_height,
                chunk_w: physical_width,
                chunk_h,
                orientation,
                strip_cache: Mutex::new(HashMap::new()),
                cache_order: Mutex::new(Vec::new()),
            })));
        }

        let options_decode = CFDictionary::from_CFType_pairs(&[(
            CFString::wrap_under_get_rule(kCGImageSourceShouldCache).as_CFType(),
            CFBoolean::true_value().as_CFType(),
        )]);
        let cg_image_ref = CGImageSourceCreateImageAtIndex(
            source.as_concrete_TypeRef(),
            0,
            options_decode.as_CFTypeRef() as _,
        );
        if cg_image_ref.is_null() {
            return Err("Failed to create cached CGImage".to_string());
        }
        let cached_image = CGImage::from_ptr(cg_image_ref);

        let color_space = CGColorSpace::create_with_name(
            CFString::wrap_under_get_rule(kCGColorSpaceSRGB).as_concrete_TypeRef(),
        )
        .unwrap_or_else(|| CGColorSpace::create_device_rgb());

        let is_raw = crate::raw_processor::is_raw_extension(&ext);

        // PERFORMANCE OPTIMIZATION: If we are in performance mode (!high_quality)
        // for a RAW file, we prioritize returning a static preview immediately.
        if is_raw && !high_quality {
            let temp_source = ImageIoTiledSource {
                _path: path.to_path_buf(),
                physical_width,
                physical_height,
                logical_width,
                logical_height,
                orientation,
                source: source.clone(),
                cached_image: cached_image.clone(),
                color_space: color_space.clone(),
                _mmap: mmap.clone(),
            };
            let lim = crate::loader::hq_preview_max_side();
            let (pw, ph, p) = temp_source.generate_preview(lim, lim);
            if pw > 0 && ph > 0 {
                log::debug!(
                    "ImageIO [Performance Mode]: Using static preview for RAW {:?}",
                    path
                );
                return Ok(ImageData::Static(DecodedImage::new(pw, ph, p)));
            }
        }

        Ok(ImageData::Tiled(Arc::new(ImageIoTiledSource {
            _path: path.to_path_buf(),
            physical_width,
            physical_height,
            logical_width,
            logical_height,
            orientation,
            source,
            cached_image,
            color_space,
            _mmap: mmap,
        })))
    }
}

