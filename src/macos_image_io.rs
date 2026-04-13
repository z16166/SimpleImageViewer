// Simple Image Viewer - macOS Image I/O Native Decoder
// This module provides robust image decoding using Apple's system libraries.

use crate::loader::ImageData;
#[cfg(target_os = "macos")]
use crate::loader::DecodedImage;

#[cfg(target_os = "macos")]
use core_graphics::image::CGImage;
#[cfg(target_os = "macos")]
use core_graphics::color_space::CGColorSpace;
#[cfg(target_os = "macos")]
use core_graphics::context::CGContext;
#[cfg(target_os = "macos")]
use core_foundation::base::{TCFType, CFTypeRef};
#[cfg(target_os = "macos")]
#[cfg(target_os = "macos")]
use core_foundation::string::{CFString, CFStringRef};
#[cfg(target_os = "macos")]
use core_foundation::array::CFArray;
#[cfg(target_os = "macos")]
use core_foundation::dictionary::CFDictionary;
#[cfg(target_os = "macos")]
use core_foundation::boolean::CFBoolean;
#[cfg(target_os = "macos")]
#[cfg(target_os = "macos")]
use foreign_types::ForeignType;

// External link to ImageIO and CoreServices
#[cfg(target_os = "macos")]
#[link(name = "ImageIO", kind = "framework")]
#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    fn CGImageSourceCreateWithData(data: core_foundation::data::CFDataRef, options: core_foundation::dictionary::CFDictionaryRef) -> *const std::ffi::c_void;
    fn CGImageSourceCreateImageAtIndex(source: *const std::ffi::c_void, index: usize, options: core_foundation::dictionary::CFDictionaryRef) -> core_graphics::sys::CGImageRef;
    fn CGImageSourceCopyPropertiesAtIndex(source: *const std::ffi::c_void, index: usize, options: core_foundation::dictionary::CFDictionaryRef) -> core_foundation::dictionary::CFDictionaryRef;
    fn CFRelease(obj: *const std::ffi::c_void);
    fn CFDictionaryGetValue(theDict: core_foundation::dictionary::CFDictionaryRef, key: *const std::ffi::c_void) -> *const std::ffi::c_void;
    
    // CoreFoundation Zero-copy
    static kCFAllocatorNull: *const std::ffi::c_void;
    
    fn CFDataCreateWithBytesNoCopy(
        allocator: *const std::ffi::c_void,
        bytes: *const u8,
        length: isize,
        bytesDeallocator: *const std::ffi::c_void
    ) -> core_foundation::data::CFDataRef;
    
    // Discovery APIs
    fn CGImageSourceCopyTypeIdentifiers() -> core_foundation::array::CFArrayRef;
    fn UTTypeCopyPreferredTagWithClass(uti: CFStringRef, tag_class: CFStringRef) -> CFStringRef;

    // Property Keys
    static kCGImageSourceShouldCache: core_foundation::string::CFStringRef;
    static kCGImagePropertyOrientation: core_foundation::string::CFStringRef;
    static kCGImagePropertyPixelWidth: core_foundation::string::CFStringRef;
    static kCGImagePropertyPixelHeight: core_foundation::string::CFStringRef;
}

#[cfg(target_os = "macos")]
static K_UT_TAG_CLASS_FILENAME_EXTENSION: &str = "public.filename-extension";

#[cfg(target_os = "macos")]
unsafe fn get_u32_property(dict: core_foundation::dictionary::CFDictionaryRef, key: core_foundation::string::CFStringRef) -> Option<u32> {
    let key_ptr = key as *const std::ffi::c_void;
    let val_ptr = unsafe { CFDictionaryGetValue(dict, key_ptr) };
    if !val_ptr.is_null() {
        let type_id = unsafe { core_foundation::base::CFGetTypeID(val_ptr) };
        if type_id == core_foundation::number::CFNumber::type_id() {
            let cf_num = unsafe { core_foundation::number::CFNumber::wrap_under_get_rule(val_ptr as _) };
            if let Some(val) = cf_num.to_i64() {
                return Some(val as u32);
            }
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn create_no_cache_options() -> core_foundation::dictionary::CFDictionary<CFString, CFBoolean> {
    unsafe {
        let key = CFString::wrap_under_get_rule(kCGImageSourceShouldCache);
        CFDictionary::from_CFType_pairs(&[(key, CFBoolean::false_value())])
    }
}

// CTM adjustment for extracting logical representation out of physical data
#[cfg(target_os = "macos")]
fn apply_orientation_ctm(context: &mut CGContext, orientation: u32, log_full_w: f64, log_full_h: f64) {
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

#[cfg(target_os = "macos")]
pub struct ImageIoTiledSource {
    #[allow(dead_code)]
    path: std::path::PathBuf,
    physical_width: u32,
    physical_height: u32,
    logical_width: u32,
    logical_height: u32,
    orientation: u32,
    source: *const std::ffi::c_void, // CGImageSourceRef
    _mmap: std::sync::Arc<memmap2::Mmap>, // Keep data alive
}

#[cfg(target_os = "macos")]
unsafe impl Send for ImageIoTiledSource {}
#[cfg(target_os = "macos")]
unsafe impl Sync for ImageIoTiledSource {}

#[cfg(target_os = "macos")]
impl Drop for ImageIoTiledSource {
    fn drop(&mut self) {
        unsafe { CFRelease(self.source); }
    }
}

#[cfg(target_os = "macos")]
impl crate::loader::TiledImageSource for ImageIoTiledSource {
    fn width(&self) -> u32 { self.logical_width }
    fn height(&self) -> u32 { self.logical_height }

    fn extract_tile(&self, x: u32, y: u32, w: u32, h: u32) -> Vec<u8> {
        let color_space = CGColorSpace::create_device_rgb();
        let context_opt = CGContext::create_bitmap_context(
            None, w as usize, h as usize, 8, w as usize * 4, &color_space,
            core_graphics::base::kCGImageAlphaPremultipliedLast
        );

        if let Some(mut context) = context_opt {
            unsafe {
                let options = create_no_cache_options();
                let cg_image_ref = CGImageSourceCreateImageAtIndex(self.source, 0, options.as_CFTypeRef() as _);
                if !cg_image_ref.is_null() {
                    let cg_image = CGImage::from_ptr(cg_image_ref);
                    
                    context.translate(-(x as f64), -(self.logical_height as f64 - (y + h) as f64));
                    apply_orientation_ctm(&mut context, self.orientation, self.logical_width as f64, self.logical_height as f64);
                    
                    let rect = core_graphics::geometry::CGRect::new(
                        &core_graphics::geometry::CGPoint::new(0.0, 0.0),
                        &core_graphics::geometry::CGSize::new(self.physical_width as f64, self.physical_height as f64)
                    );
                    context.draw_image(rect, &cg_image);
                } else {
                    log::error!("[{}] MacOS ImageIO: Failed to create CGImage for tile at ({}, {})", self.path.display(), x, y);
                }
            }
            context.data().to_vec()
        } else {
            log::error!("[{}] MacOS ImageIO: Failed to create bitmap context for tile at ({}, {})", self.path.display(), x, y);
            vec![0u8; (w * h * 4) as usize]
        }
    }

    fn generate_preview(&self, max_w: u32, max_h: u32) -> (u32, u32, Vec<u8>) {
        let scale = (max_w as f64 / self.logical_width as f64)
            .min(max_h as f64 / self.logical_height as f64)
            .min(1.0);
        let pw = (self.logical_width as f64 * scale).round().max(1.0) as u32;
        let ph = (self.logical_height as f64 * scale).round().max(1.0) as u32;

        let color_space = CGColorSpace::create_device_rgb();
        let context_opt = CGContext::create_bitmap_context(
            None, pw as usize, ph as usize, 8, pw as usize * 4, &color_space,
            core_graphics::base::kCGImageAlphaPremultipliedLast
        );

        if let Some(mut context) = context_opt {
            unsafe {
                let options = create_no_cache_options();
                let cg_image_ref = CGImageSourceCreateImageAtIndex(self.source, 0, options.as_CFTypeRef() as _);
                if !cg_image_ref.is_null() {
                    let cg_image = CGImage::from_ptr(cg_image_ref);
                    
                    context.scale(scale, scale);
                    apply_orientation_ctm(&mut context, self.orientation, self.logical_width as f64, self.logical_height as f64);
                    
                    let rect = core_graphics::geometry::CGRect::new(
                        &core_graphics::geometry::CGPoint::new(0.0, 0.0),
                        &core_graphics::geometry::CGSize::new(self.physical_width as f64, self.physical_height as f64)
                    );
                    context.draw_image(rect, &cg_image);
                } else {
                    log::error!("[{}] MacOS ImageIO: Failed to create CGImage for preview", self.path.display());
                }
            }
            (pw, ph, context.data().to_vec())
        } else {
            log::error!("[{}] MacOS ImageIO: Failed to create bitmap context for preview", self.path.display());
            (pw, ph, vec![0u8; (pw * ph * 4) as usize])
        }
    }

    fn full_pixels(&self) -> Option<std::sync::Arc<Vec<u8>>> { None }
}

#[cfg(target_os = "macos")]
pub fn load_via_image_io(path: &std::path::PathBuf) -> Result<ImageData, String> {
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("unknown");
    
    let file = std::fs::File::open(path).map_err(|e| format!("Failed to open file: {}", e))?;
    let mmap = unsafe { memmap2::Mmap::map(&file).map_err(|e| format!("Failed to mmap file: {}", e))? };
    let mmap_arc = std::sync::Arc::new(mmap);

    unsafe {
        let cf_data = CFDataCreateWithBytesNoCopy(
            std::ptr::null(),
            mmap_arc.as_ptr(),
            mmap_arc.len() as isize,
            kCFAllocatorNull
        );
        
        if cf_data.is_null() {
            return Err("Failed to create CFData from mmap".to_string());
        }

        let options = create_no_cache_options();
        let source = CGImageSourceCreateWithData(cf_data, options.as_CFTypeRef() as _);
        CFRelease(cf_data as *const _); 
        
        if source.is_null() {
            return Err("Failed to create CGImageSource from data".to_string());
        }

        let mut physical_width = 0;
        let mut physical_height = 0;
        let mut orientation = 1;

        let props_options = create_no_cache_options();
        let props_ref = CGImageSourceCopyPropertiesAtIndex(source, 0, props_options.as_CFTypeRef() as _);
        if !props_ref.is_null() {
            if let Some(w) = get_u32_property(props_ref, kCGImagePropertyPixelWidth) { physical_width = w; }
            if let Some(h) = get_u32_property(props_ref, kCGImagePropertyPixelHeight) { physical_height = h; }
            if let Some(o) = get_u32_property(props_ref, kCGImagePropertyOrientation) { orientation = o; }
            CFRelease(props_ref as *const _);
        }

        if physical_width == 0 || physical_height == 0 {
            let options2 = create_no_cache_options();
            let cg_image_ref = CGImageSourceCreateImageAtIndex(source, 0, options2.as_CFTypeRef() as _);
            if !cg_image_ref.is_null() {
                let cg_image = CGImage::from_ptr(cg_image_ref);
                physical_width = cg_image.width() as u32;
                physical_height = cg_image.height() as u32;
            } else {
                CFRelease(source);
                return Err("Failed to create CGImage from source (fallback)".to_string());
            }
        }

        let (logical_width, logical_height) = match orientation {
            5 | 6 | 7 | 8 => (physical_height, physical_width),
            _ => (physical_width, physical_height),
        };
        
        let pixel_count = logical_width as u64 * logical_height as u64;
        let limit = crate::tile_cache::get_max_texture_side();

        if pixel_count >= crate::tile_cache::TILED_THRESHOLD || logical_width > limit || logical_height > limit {
            return Ok(ImageData::Tiled(std::sync::Arc::new(ImageIoTiledSource {
                path: path.to_path_buf(),
                physical_width,
                physical_height,
                logical_width,
                logical_height,
                orientation,
                source,
                _mmap: mmap_arc,
            })));
        }

        let options3 = create_no_cache_options();
        let cg_image_ref = CGImageSourceCreateImageAtIndex(source, 0, options3.as_CFTypeRef() as _);
        if cg_image_ref.is_null() {
            CFRelease(source);
            return Err("Failed to create CGImage from source".to_string());
        }
        let cg_image = CGImage::from_ptr(cg_image_ref);

        let color_space = CGColorSpace::create_device_rgb();
        let mut context = CGContext::create_bitmap_context(
            None,
            logical_width as usize,
            logical_height as usize,
            8,
            logical_width as usize * 4,
            &color_space,
            core_graphics::base::kCGImageAlphaPremultipliedLast
        );
        
        apply_orientation_ctm(&mut context, orientation, logical_width as f64, logical_height as f64);
        
        let rect = core_graphics::geometry::CGRect::new(
            &core_graphics::geometry::CGPoint::new(0.0, 0.0),
            &core_graphics::geometry::CGSize::new(physical_width as f64, physical_height as f64)
        );
        context.draw_image(rect, &cg_image);
        
        let pixel_data = context.data().to_vec();
        
        CFRelease(source);
        
        log::info!("[{}] Decoded via MacOS ImageIO (Static/Mmap): {}x{} (Orientation: {})", file_name, logical_width, logical_height, orientation);

        Ok(ImageData::Static(DecodedImage {
            width: logical_width,
            height: logical_height,
            pixels: pixel_data,
        }))
    }
}

// Fallback for non-macos platforms so it compiles
#[cfg(not(target_os = "macos"))]
#[allow(dead_code)]
pub fn load_via_image_io(_path: &std::path::PathBuf) -> Result<ImageData, String> {
    Err("ImageIO is only supported on macOS".to_string())
}

#[cfg(target_os = "macos")]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn discover_imageio_codecs() -> Vec<String> {
    use std::collections::HashSet;

    let mut extensions = HashSet::new();
    let tag_class = CFString::from_static_string(K_UT_TAG_CLASS_FILENAME_EXTENSION);

    unsafe {
        let array_ref = CGImageSourceCopyTypeIdentifiers();
        if !array_ref.is_null() {
            let array: CFArray<CFTypeRef> = CFArray::wrap_under_create_rule(array_ref);
            for uti_ptr in array.iter() {
                let uti_str_ref = *uti_ptr as CFStringRef;
                let ext_ref = UTTypeCopyPreferredTagWithClass(uti_str_ref, tag_class.as_concrete_TypeRef());
                
                if !ext_ref.is_null() {
                    let ext_cfstring: CFString = CFString::wrap_under_create_rule(ext_ref);
                    let ext = ext_cfstring.to_string().to_lowercase();
                    if !ext.is_empty() {
                        extensions.insert(ext);
                    }
                }
            }
        }
    }

    let mut result: Vec<String> = extensions.into_iter().collect();
    result.sort();
    result
}

#[cfg(not(target_os = "macos"))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn discover_imageio_codecs() -> Vec<String> {
    vec![]
}
