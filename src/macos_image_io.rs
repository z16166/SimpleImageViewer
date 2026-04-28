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
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
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

    fn extract_tile(&self, x: u32, y: u32, w: u32, h: u32) -> Vec<u8> {
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
        context.data().to_vec()
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

pub struct TiffStripCachingSource {
    path: PathBuf,
    _mmap: Arc<Mmap>,
    cached_image: CGImage,
    color_space: CGColorSpace,
    physical_width: u32,
    physical_height: u32,
    logical_width: u32,
    logical_height: u32,
    chunk_w: u32,
    chunk_h: u32,
    orientation: u32,
    // Key: chunk_idx, Value: Normalized RGBA8 buffer for that chunk
    strip_cache: Mutex<HashMap<u32, Arc<Vec<u8>>>>,
    cache_order: Mutex<Vec<u32>>,
}

unsafe impl Send for TiffStripCachingSource {}
unsafe impl Sync for TiffStripCachingSource {}

impl crate::loader::TiledImageSource for TiffStripCachingSource {
    fn width(&self) -> u32 {
        self.logical_width
    }
    fn height(&self) -> u32 {
        self.logical_height
    }

    fn extract_tile(&self, x: u32, y: u32, w: u32, h: u32) -> Vec<u8> {
        let mut rgba = vec![255u8; (w * h * 4) as usize];

        // 1. Group processing by chunk to minimize Mutex locking overhead (CRITICAL PERF FIX)
        let tiles_across = (self.physical_width + self.chunk_w - 1) / self.chunk_w;

        // Find which chunks intersect with this tile
        let start_chunk_row = y / self.chunk_h;
        let end_chunk_row = (y + h - 1) / self.chunk_h;
        let start_chunk_col = x / self.chunk_w;
        let end_chunk_col = (x + w - 1) / self.chunk_w;

        for crow in start_chunk_row..=end_chunk_row {
            for ccol in start_chunk_col..=end_chunk_col {
                let chunk_idx = crow * tiles_across + ccol;

                // Get or decode the strip/tile (one lock per chunk instead of per pixel!)
                if let Some(data) = self.get_or_decode_chunk(chunk_idx) {
                    let chunk_y_start = crow * self.chunk_h;
                    let chunk_x_start = ccol * self.chunk_w;

                    // Calculate intersection between tile and this chunk
                    let intersect_y_start = y.max(chunk_y_start);
                    let intersect_y_end = (y + h)
                        .min(chunk_y_start + self.chunk_h)
                        .min(self.logical_height);
                    let intersect_x_start = x.max(chunk_x_start);
                    let intersect_x_end = (x + w)
                        .min(chunk_x_start + self.chunk_w)
                        .min(self.logical_width);

                    if intersect_y_start >= intersect_y_end || intersect_x_start >= intersect_x_end
                    {
                        continue;
                    }

                    for py in intersect_y_start..intersect_y_end {
                        let y_in_chunk = py - chunk_y_start;
                        let ty = py - y;

                        let src_row_start = (y_in_chunk * self.chunk_w
                            + (intersect_x_start - chunk_x_start))
                            as usize
                            * 4;
                        let dst_row_start = (ty * w + (intersect_x_start - x)) as usize * 4;
                        let copy_px_count = intersect_x_end - intersect_x_start;
                        let copy_bytes = copy_px_count as usize * 4;

                        if src_row_start + copy_bytes <= data.len()
                            && dst_row_start + copy_bytes <= rgba.len()
                        {
                            rgba[dst_row_start..dst_row_start + copy_bytes]
                                .copy_from_slice(&data[src_row_start..src_row_start + copy_bytes]);
                        }
                    }
                } else {
                    log::warn!(
                        "TiffStripCachingSource: Failed to decode chunk {}. Returning partial white tile.",
                        chunk_idx
                    );
                }
            }
        }

        // If orientation is not 1, we need to transform the whole tile
        if self.orientation > 1 {
            let (_ow, _oh, opixels) = apply_orientation_buffer(rgba, w, h, self.orientation);
            // Note: apply_orientation_buffer might change dimensions if 90deg rotation is involved.
            // But here extract_tile expects w, h. This needs careful handling.
            // For now, if orientation > 1, we might just want to use ImageIO which handles it via CTM.
            return opixels;
        }

        rgba
    }

    fn generate_preview(&self, max_w: u32, max_h: u32) -> (u32, u32, Vec<u8>) {
        HugeTiffStrideDecoder::decode_preview(&self.path, max_w.max(max_h), self.orientation)
            .unwrap_or((0, 0, vec![]))
    }

    fn full_pixels(&self) -> Option<Arc<Vec<u8>>> {
        None
    }
}

impl TiffStripCachingSource {
    fn get_or_decode_chunk(&self, chunk_idx: u32) -> Option<Arc<Vec<u8>>> {
        {
            let cache = self.strip_cache.lock().unwrap();
            if let Some(chunk) = cache.get(&chunk_idx) {
                return Some(Arc::clone(chunk));
            }
        }

        // Decode
        let data = self.decode_chunk_to_rgba8(chunk_idx)?;
        let data_arc = Arc::new(data);

        {
            let mut cache = self.strip_cache.lock().unwrap();
            let mut order = self.cache_order.lock().unwrap();

            cache.insert(chunk_idx, Arc::clone(&data_arc));
            order.push(chunk_idx);

            // Evict if too many strips (Max 32 strips ~ 1.5GB for very giant images)
            if order.len() > 32 {
                let to_remove = order.remove(0);
                cache.remove(&to_remove);
            }
        }

        Some(data_arc)
    }

    fn decode_chunk_to_rgba8(&self, chunk_idx: u32) -> Option<Vec<u8>> {
        let pw = self.physical_width;
        let ph = self.physical_height;
        let start_y = chunk_idx * self.chunk_h;
        if start_y >= ph {
            return None;
        }

        let h = self.chunk_h.min(ph - start_y);

        // Render the strip by drawing the full CGImage into a small bitmap context
        // with a translation offset. This is the same proven approach used by
        // ImageIoTiledSource::extract_tile, which correctly handles CoreGraphics'
        // bottom-up coordinate system.
        let mut context = CGContext::create_bitmap_context(
            None,
            pw as usize,
            h as usize,
            8,
            pw as usize * 4,
            &self.color_space,
            core_graphics::base::kCGImageAlphaPremultipliedLast,
        );

        // CoreGraphics has Y=0 at bottom. To extract strip at top-down row `start_y`,
        // we need to translate so that the correct portion of the image lands in
        // our h-pixel-tall bitmap context.
        //
        // The full image drawn at origin would place its bottom-left at (0,0).
        // We want the rows [start_y .. start_y+h] (top-down) to appear in our context.
        // In CG coords, those rows are at y = (ph - start_y - h) .. (ph - start_y).
        // We translate by -(ph - start_y - h) to shift them into view.
        let cg_y_offset = ph as f64 - start_y as f64 - h as f64;
        context.translate(0.0, -cg_y_offset);

        let full_rect = core_graphics::geometry::CGRect::new(
            &core_graphics::geometry::CGPoint::new(0.0, 0.0),
            &core_graphics::geometry::CGSize::new(pw as f64, ph as f64),
        );
        context.draw_image(full_rect, &self.cached_image);
        Some(context.data().to_vec())
    }
}

struct HugeTiffStrideDecoder;

impl HugeTiffStrideDecoder {
    /// Multi-threaded, Zero-Allocation Stride-Reader using Rayon + Mmap.
    fn decode_preview(
        path: &Path,
        max_size: u32,
        orientation: u32,
    ) -> Result<(u32, u32, Vec<u8>), String> {
        let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
        let mmap = unsafe { Mmap::map(&file).map_err(|e| e.to_string())? };

        let cursor = Cursor::new(&mmap[..]);
        let mut decoder = Decoder::new(cursor).map_err(|e| e.to_string())?;

        let (width, height) = decoder.dimensions().map_err(|e| e.to_string())?;
        let max_dim = width.max(height);
        let stride = (max_dim / max_size).max(1);

        let target_w = width / stride;
        let target_h = height / stride;

        let chunk_w = decoder.get_tag_u32(Tag::TileWidth).unwrap_or(width);
        let chunk_h = decoder
            .get_tag_u32(Tag::TileLength)
            .unwrap_or_else(|_| decoder.get_tag_u32(Tag::RowsPerStrip).unwrap_or(height));
        let is_tiled = decoder.get_tag_u32(Tag::TileWidth).is_ok();

        let color_type = decoder.colortype().map_err(|e| e.to_string())?;
        let samples = match color_type {
            tiff::ColorType::RGB(_) => 3,
            tiff::ColorType::RGBA(_) | tiff::ColorType::CMYK(_) => 4,
            tiff::ColorType::Gray(_) => 1,
            _ => return Err("Unsupported color type".into()),
        };

        let mut preview_pixels = vec![0u8; (target_w * target_h * 4) as usize];

        let tiles_across = (width + chunk_w - 1) / chunk_w;
        let tiles_down = (height + chunk_h - 1) / chunk_h;
        let total_chunks = tiles_across * tiles_down;

        let comp = decoder.get_tag_u32(Tag::Compression).unwrap_or(1);
        let planar = decoder.get_tag_u32(Tag::PlanarConfiguration).unwrap_or(1);
        let is_8bit = matches!(
            color_type,
            tiff::ColorType::RGB(8)
                | tiff::ColorType::RGBA(8)
                | tiff::ColorType::CMYK(8)
                | tiff::ColorType::Gray(8)
        );
        let is_cmyk = matches!(color_type, tiff::ColorType::CMYK(_));

        let offsets_tag = if is_tiled {
            Tag::TileOffsets
        } else {
            Tag::StripOffsets
        };
        let offsets = if comp == 1 && planar == 1 && is_8bit {
            decoder.get_tag_u64_vec(offsets_tag).ok().or_else(|| {
                decoder
                    .get_tag_u32_vec(offsets_tag)
                    .ok()
                    .map(|v| v.into_iter().map(|x| x as u64).collect())
            })
        } else {
            None
        };

        if let Some(offsets) = offsets {
            // Engine A: Fast Zero-Copy Mmap
            log::info!("MacOS ImageIO: Engine A - Unified Zero-Copy Mmap Stride-Reader");
            for ty in 0..target_h {
                let y = ty * stride;
                let chunk_row = y / chunk_h;
                let y_in_chunk = y % chunk_h;
                let dst_y_offset = (ty * target_w) as usize * 4;

                for tx in 0..target_w {
                    let x = tx * stride;
                    let chunk_col = x / chunk_w;
                    let chunk_idx = (chunk_row * tiles_across + chunk_col) as usize;

                    if chunk_idx >= offsets.len() {
                        continue;
                    }

                    let offset_in_chunk = (y_in_chunk * chunk_w + (x % chunk_w)) * samples;
                    let src_offset = (offsets[chunk_idx] + offset_in_chunk as u64) as usize;
                    let dst_offset = dst_y_offset + (tx as usize) * 4;

                    if src_offset + (samples as usize) > mmap.len() {
                        continue;
                    }

                    if samples == 3 {
                        preview_pixels[dst_offset] = mmap[src_offset];
                        preview_pixels[dst_offset + 1] = mmap[src_offset + 1];
                        preview_pixels[dst_offset + 2] = mmap[src_offset + 2];
                        preview_pixels[dst_offset + 3] = 255;
                    } else if samples == 4 {
                        if is_cmyk {
                            let c = mmap[src_offset] as f32 / 255.0;
                            let m = mmap[src_offset + 1] as f32 / 255.0;
                            let y = mmap[src_offset + 2] as f32 / 255.0;
                            let k = mmap[src_offset + 3] as f32 / 255.0;
                            preview_pixels[dst_offset] = (255.0 * (1.0 - c) * (1.0 - k)) as u8;
                            preview_pixels[dst_offset + 1] = (255.0 * (1.0 - m) * (1.0 - k)) as u8;
                            preview_pixels[dst_offset + 2] = (255.0 * (1.0 - y) * (1.0 - k)) as u8;
                            preview_pixels[dst_offset + 3] = 255;
                        } else {
                            preview_pixels[dst_offset..dst_offset + 4]
                                .copy_from_slice(&mmap[src_offset..src_offset + 4]);
                        }
                    } else if samples == 1 {
                        let g = mmap[src_offset];
                        preview_pixels[dst_offset] = g;
                        preview_pixels[dst_offset + 1] = g;
                        preview_pixels[dst_offset + 2] = g;
                        preview_pixels[dst_offset + 3] = 255;
                    }
                }
            }
        } else {
            // ========================================================
            // Engine B: Rayon Multi-threaded Decompression + Zero Allocation
            // ========================================================
            log::info!("MacOS ImageIO: Engine B - Rayon Parallel Decompression (Zero Alloc)");

            // 1. Identify which chunks are ACTUALLY required
            let mut required_chunks = HashSet::new();
            for ty in 0..target_h {
                let y = ty * stride;
                for tx in 0..target_w {
                    let x = tx * stride;
                    let chunk_row = y / chunk_h;
                    let chunk_col = x / chunk_w;
                    let chunk_idx = chunk_row * tiles_across + chunk_col;
                    if chunk_idx < total_chunks {
                        required_chunks.insert(chunk_idx);
                    }
                }
            }

            let required_chunks: Vec<u32> = required_chunks.into_iter().collect();
            let mmap_slice = &mmap[..];

            // 2. Parallel decode across all cores!
            // Each thread builds a lightweight Decoder pointing to the same mmap memory.
            let chunk_cache: std::collections::HashMap<u32, DecodingResult> = required_chunks
                .into_par_iter()
                .filter_map(|chunk_idx| {
                    let local_cursor = Cursor::new(mmap_slice);
                    if let Ok(mut local_decoder) = Decoder::new(local_cursor) {
                        if let Ok(res) = local_decoder.read_chunk(chunk_idx) {
                            return Some((chunk_idx, res));
                        }
                    }
                    None
                })
                .collect();

            // 3. Extract pixels with On-The-Fly 16-bit shift (NO intermediate memory allocations)
            for ty in 0..target_h {
                let y = ty * stride;
                let dst_y_offset = (ty * target_w) as usize * 4;

                for tx in 0..target_w {
                    let x = tx * stride;
                    let chunk_row = y / chunk_h;
                    let chunk_col = x / chunk_w;
                    let chunk_idx = chunk_row * tiles_across + chunk_col;

                    let y_in_chunk = y % chunk_h;
                    let x_in_chunk = x % chunk_w;
                    let src_offset = ((y_in_chunk * chunk_w + x_in_chunk) * samples) as usize;
                    let dst_offset = dst_y_offset + (tx as usize) * 4;

                    if let Some(chunk_data) = chunk_cache.get(&chunk_idx) {
                        match chunk_data {
                            DecodingResult::U8(v) => {
                                if src_offset + (samples as usize) <= v.len() {
                                    if samples == 3 {
                                        preview_pixels[dst_offset] = v[src_offset];
                                        preview_pixels[dst_offset + 1] = v[src_offset + 1];
                                        preview_pixels[dst_offset + 2] = v[src_offset + 2];
                                        preview_pixels[dst_offset + 3] = 255;
                                    } else if samples == 4 {
                                        if is_cmyk {
                                            let c = v[src_offset] as f32 / 255.0;
                                            let m = v[src_offset + 1] as f32 / 255.0;
                                            let y = v[src_offset + 2] as f32 / 255.0;
                                            let k = v[src_offset + 3] as f32 / 255.0;
                                            preview_pixels[dst_offset] =
                                                (255.0 * (1.0 - c) * (1.0 - k)) as u8;
                                            preview_pixels[dst_offset + 1] =
                                                (255.0 * (1.0 - m) * (1.0 - k)) as u8;
                                            preview_pixels[dst_offset + 2] =
                                                (255.0 * (1.0 - y) * (1.0 - k)) as u8;
                                            preview_pixels[dst_offset + 3] = 255;
                                        } else {
                                            preview_pixels[dst_offset..dst_offset + 4]
                                                .copy_from_slice(&v[src_offset..src_offset + 4]);
                                        }
                                    } else if samples == 1 {
                                        let g = v[src_offset];
                                        preview_pixels[dst_offset] = g;
                                        preview_pixels[dst_offset + 1] = g;
                                        preview_pixels[dst_offset + 2] = g;
                                        preview_pixels[dst_offset + 3] = 255;
                                    }
                                }
                            }
                            DecodingResult::U16(v) => {
                                // ZERO ALLOCATION: We shift bits exactly when writing to the canvas.
                                if src_offset + (samples as usize) <= v.len() {
                                    if samples == 3 {
                                        preview_pixels[dst_offset] = (v[src_offset] >> 8) as u8;
                                        preview_pixels[dst_offset + 1] =
                                            (v[src_offset + 1] >> 8) as u8;
                                        preview_pixels[dst_offset + 2] =
                                            (v[src_offset + 2] >> 8) as u8;
                                        preview_pixels[dst_offset + 3] = 255;
                                    } else if samples == 4 {
                                        if is_cmyk {
                                            let c = (v[src_offset] >> 8) as f32 / 255.0;
                                            let m = (v[src_offset + 1] >> 8) as f32 / 255.0;
                                            let y = (v[src_offset + 2] >> 8) as f32 / 255.0;
                                            let k = (v[src_offset + 3] >> 8) as f32 / 255.0;
                                            preview_pixels[dst_offset] =
                                                (255.0 * (1.0 - c) * (1.0 - k)) as u8;
                                            preview_pixels[dst_offset + 1] =
                                                (255.0 * (1.0 - m) * (1.0 - k)) as u8;
                                            preview_pixels[dst_offset + 2] =
                                                (255.0 * (1.0 - y) * (1.0 - k)) as u8;
                                            preview_pixels[dst_offset + 3] = 255;
                                        } else {
                                            preview_pixels[dst_offset] = (v[src_offset] >> 8) as u8;
                                            preview_pixels[dst_offset + 1] =
                                                (v[src_offset + 1] >> 8) as u8;
                                            preview_pixels[dst_offset + 2] =
                                                (v[src_offset + 2] >> 8) as u8;
                                            preview_pixels[dst_offset + 3] =
                                                (v[src_offset + 3] >> 8) as u8;
                                        }
                                    } else if samples == 1 {
                                        let g = (v[src_offset] >> 8) as u8;
                                        preview_pixels[dst_offset] = g;
                                        preview_pixels[dst_offset + 1] = g;
                                        preview_pixels[dst_offset + 2] = g;
                                        preview_pixels[dst_offset + 3] = 255;
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        Ok(apply_orientation_buffer(
            preview_pixels,
            target_w,
            target_h,
            orientation,
        ))
    }
}

fn apply_orientation_buffer(
    pixels: Vec<u8>,
    w: u32,
    h: u32,
    orientation: u32,
) -> (u32, u32, Vec<u8>) {
    if orientation <= 1 {
        return (w, h, pixels);
    }

    let (out_w, out_h) = if orientation >= 5 && orientation <= 8 {
        (h, w)
    } else {
        (w, h)
    };
    let mut out = vec![0u8; (out_w * out_h * 4) as usize];

    for y in 0..h {
        for x in 0..w {
            let (nx, ny) = match orientation {
                2 => (w - 1 - x, y),
                3 => (w - 1 - x, h - 1 - y),
                4 => (x, h - 1 - y),
                5 => (y, x),
                6 => (h - 1 - y, x),
                7 => (h - 1 - y, w - 1 - x),
                8 => (y, w - 1 - x),
                _ => (x, y),
            };
            let src_idx = (y * w + x) as usize * 4;
            let dst_idx = (ny * out_w + nx) as usize * 4;
            if dst_idx + 4 <= out.len() {
                out[dst_idx..dst_idx + 4].copy_from_slice(&pixels[src_idx..src_idx + 4]);
            }
        }
    }
    (out_w, out_h, out)
}

unsafe fn render_cgimage_to_rgba_sync(
    cg_image: &CGImage,
    orientation: u32,
    lw: u32,
    lh: u32,
) -> DecodedImage {
    unsafe {
        let pw = cg_image.width() as u32;
        let ph = cg_image.height() as u32;
        let color_space = CGColorSpace::create_with_name(
            CFString::wrap_under_get_rule(kCGColorSpaceSRGB).as_concrete_TypeRef(),
        )
        .unwrap_or_else(|| CGColorSpace::create_device_rgb());

        let mut context = CGContext::create_bitmap_context(
            None,
            lw as usize,
            lh as usize,
            8,
            lw as usize * 4,
            &color_space,
            core_graphics::base::kCGImageAlphaPremultipliedLast,
        );

        apply_orientation_ctm(&mut context, orientation, lw as f64, lh as f64);

        let rect = core_graphics::geometry::CGRect::new(
            &core_graphics::geometry::CGPoint::new(0.0, 0.0),
            &core_graphics::geometry::CGSize::new(pw as f64, ph as f64),
        );
        context.draw_image(rect, &cg_image);

        DecodedImage::new(lw, lh, context.data().to_vec())
    }
}

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
    unsafe {
        let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
        let mmap = Arc::new(Mmap::map(&file).map_err(|e| e.to_string())?);

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
        let mut orientation = 1u32;

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

        orientation = orientation_override
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

        if is_tiff && is_stripped && orientation == 1 {
            log::info!(
                "MacOS ImageIO: Giant STRIPPED TIFF detected ({}x{}). Using TiffStripCachingSource.",
                physical_width,
                physical_height
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
            let (pw, ph, p) = temp_source.generate_preview(
                crate::constants::MAX_QUALITY_PREVIEW_SIZE,
                crate::constants::MAX_QUALITY_PREVIEW_SIZE,
            );
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
