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
pub struct ImageIoTiledSource {
    _path: PathBuf,
    physical_width: u32,
    physical_height: u32,
    logical_width: u32,
    logical_height: u32,
    orientation: u32,
    source: CGImageSource,
    cached_image: CGImage,
    color_space: CGColorSpace,
    _mmap: Arc<Mmap>,
}

unsafe impl Send for ImageIoTiledSource {}
unsafe impl Sync for ImageIoTiledSource {}

impl crate::loader::TiledImageSource for ImageIoTiledSource {
    fn width(&self) -> u32 {
        self.logical_width
    }
    fn height(&self) -> u32 {
        self.logical_height
    }

    fn extract_tile(&self, x: u32, y: u32, w: u32, h: u32) -> Arc<Vec<u8>> {
        let mut context = CGContext::create_bitmap_context(
            None,
            w as usize,
            h as usize,
            8,
            w as usize * 4,
            &self.color_space,
            core_graphics::base::kCGImageAlphaPremultipliedLast,
        );

        context.translate(-(x as f64), -(self.logical_height as f64 - (y + h) as f64));
        apply_orientation_ctm(
            &mut context,
            self.orientation,
            self.logical_width as f64,
            self.logical_height as f64,
        );

        let rect = core_graphics::geometry::CGRect::new(
            &core_graphics::geometry::CGPoint::new(0.0, 0.0),
            &core_graphics::geometry::CGSize::new(
                self.physical_width as f64,
                self.physical_height as f64,
            ),
        );
        context.draw_image(rect, &self.cached_image);
        std::sync::Arc::new(context.data().to_vec())
    }

    fn generate_preview(&self, max_w: u32, max_h: u32) -> (u32, u32, Vec<u8>) {
        let max_size = max_w.max(max_h);

        unsafe {
            use core_foundation::base::TCFType;
            use core_foundation::number::CFNumber;

            let k_max_size = CFString::wrap_under_get_rule(kCGImageSourceThumbnailMaxPixelSize);
            let k_always =
                CFString::wrap_under_get_rule(kCGImageSourceCreateThumbnailFromImageAlways);
            let k_if_absent =
                CFString::wrap_under_get_rule(kCGImageSourceCreateThumbnailFromImageIfAbsent);
            let k_transform =
                CFString::wrap_under_get_rule(kCGImageSourceCreateThumbnailWithTransform);

            // Step 1: Pyramid Discovery
            let count = CGImageSourceGetCount(self.source.as_concrete_TypeRef());
            let mut best_index = 0;
            let mut smallest_fit_dim = u32::MAX;

            if count > 1 {
                let k_width = CFString::wrap_under_get_rule(kCGImagePropertyPixelWidth);
                let k_height = CFString::wrap_under_get_rule(kCGImagePropertyPixelHeight);

                for i in 0..count {
                    let props_ref = CGImageSourceCopyPropertiesAtIndex(
                        self.source.as_concrete_TypeRef(),
                        i,
                        std::ptr::null(),
                    );
                    if !props_ref.is_null() {
                        let props = CFDictionary::wrap_under_create_rule(props_ref);
                        let w = get_cf_number_u32(&props, k_width.as_CFTypeRef() as _).unwrap_or(0);
                        let h =
                            get_cf_number_u32(&props, k_height.as_CFTypeRef() as _).unwrap_or(0);
                        let dim = w.max(h);

                        if dim >= max_size && dim < smallest_fit_dim {
                            smallest_fit_dim = dim;
                            best_index = i;
                        }
                    }
                }
            }

            let main_start = std::time::Instant::now();

            // ========================================================
            // Path 1 (Fast): Explicit Embedded Thumb Check
            // FIX: k_if_absent = false prevents the 2.5s blocking behavior!
            // ========================================================
            let options_fast = CFDictionary::from_CFType_pairs(&[
                (
                    k_max_size.as_CFType(),
                    CFNumber::from(max_size as i32).as_CFType(),
                ),
                (
                    k_if_absent.as_CFType(),
                    CFBoolean::false_value().as_CFType(),
                ), // <-- Fixed the 2.5s spike
                (k_always.as_CFType(), CFBoolean::false_value().as_CFType()),
                (k_transform.as_CFType(), CFBoolean::true_value().as_CFType()),
            ]);

            let cg_image_ref = CGImageSourceCreateThumbnailAtIndex(
                self.source.as_concrete_TypeRef(),
                best_index,
                options_fast.as_CFTypeRef() as _,
            );
            if !cg_image_ref.is_null() {
                let cg_image = CGImage::from_ptr(cg_image_ref);
                let pw = cg_image.width() as u32;

                if max_size <= 512 || pw >= max_size || pw >= 2048 {
                    let res = self.render_cgimage_to_rgba(&cg_image);
                    log::info!(
                        "MacOS ImageIO: Path 1 (Existing Thumb/Pyramid) took {}ms",
                        main_start.elapsed().as_millis()
                    );
                    return res;
                }
            }

            // ========================================================
            // Path 2 (Fast): The 'Ultima' Path - Parallel Rust Stride-Reading
            // ========================================================
            let is_giant = self.physical_width >= crate::constants::MAX_QUALITY_PREVIEW_SIZE
                || self.physical_height >= crate::constants::MAX_QUALITY_PREVIEW_SIZE;
            if is_giant && best_index == 0 {
                log::info!(
                    "MacOS ImageIO: Giant single-layer detected. Prioritizing Path 2 (Rayon Stride-Reader)..."
                );
                let rust_start = std::time::Instant::now();
                match HugeTiffStrideDecoder::decode_preview(&self._path, max_size, self.orientation)
                {
                    Ok(res) => {
                        log::info!(
                            "MacOS ImageIO: Path 2 (Rayon Stride-Reader) took {}ms",
                            rust_start.elapsed().as_millis()
                        );
                        return res;
                    }
                    Err(e) => {
                        log::warn!("Rust Stride-Reader fallback: {}. Passing to Path 3...", e)
                    }
                }
            }

            // ========================================================
            // Path 3 (Fallback): ImageIO Generation (Only for normal images)
            // ========================================================
            let path3_start = std::time::Instant::now();
            let options_gen = CFDictionary::from_CFType_pairs(&[
                (
                    k_max_size.as_CFType(),
                    CFNumber::from(max_size as i32).as_CFType(),
                ),
                (k_always.as_CFType(), CFBoolean::true_value().as_CFType()),
                (k_transform.as_CFType(), CFBoolean::true_value().as_CFType()),
            ]);

            let cg_image_ref_gen = CGImageSourceCreateThumbnailAtIndex(
                self.source.as_concrete_TypeRef(),
                best_index,
                options_gen.as_CFTypeRef() as _,
            );
            if !cg_image_ref_gen.is_null() {
                let cg_image = CGImage::from_ptr(cg_image_ref_gen);
                let res = self.render_cgimage_to_rgba(&cg_image);
                log::info!(
                    "MacOS ImageIO: Path 3 (Forced ImageIO) took {}ms",
                    path3_start.elapsed().as_millis()
                );
                return res;
            }
        }
        (0, 0, vec![])
    }

    fn full_pixels(&self) -> Option<std::sync::Arc<Vec<u8>>> {
        None
    }
}

impl ImageIoTiledSource {
    fn render_cgimage_to_rgba(&self, cg_image: &CGImage) -> (u32, u32, Vec<u8>) {
        let pw = cg_image.width() as u32;
        let ph = cg_image.height() as u32;
        let color_space = unsafe {
            CGColorSpace::create_with_name(
                CFString::wrap_under_get_rule(kCGColorSpaceSRGB).as_concrete_TypeRef(),
            )
            .unwrap_or_else(|| CGColorSpace::create_device_rgb())
        };
        let mut context = CGContext::create_bitmap_context(
            None,
            pw as usize,
            ph as usize,
            8,
            pw as usize * 4,
            &color_space,
            core_graphics::base::kCGImageAlphaPremultipliedLast,
        );
        let rect = core_graphics::geometry::CGRect::new(
            &core_graphics::geometry::CGPoint::new(0.0, 0.0),
            &core_graphics::geometry::CGSize::new(pw as f64, ph as f64),
        );
        context.draw_image(rect, &cg_image);
        (pw, ph, context.data().to_vec())
    }
}

