// Simple Image Viewer - macOS Image I/O Native Decoder
// This module provides robust image decoding using Apple's system libraries.

use std::path::PathBuf;
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
use core_foundation::base::TCFType;
#[cfg(target_os = "macos")]
use core_foundation::data::CFData;
#[cfg(target_os = "macos")]
use std::fs;

// External link to ImageIO which is not always fully covered by core-graphics crate
#[cfg(target_os = "macos")]
#[link(name = "ImageIO", kind = "framework")]
extern "C" {
    fn CGImageSourceCreateWithData(data: core_foundation::data::CFDataRef, options: core_foundation::dictionary::CFDictionaryRef) -> *const std::ffi::c_void;
    fn CGImageSourceCreateImageAtIndex(source: *const std::ffi::c_void, index: usize, options: core_foundation::dictionary::CFDictionaryRef) -> core_graphics::sys::CGImageRef;
}

#[cfg(target_os = "macos")]
pub fn load_via_image_io(path: &PathBuf) -> Result<ImageData, String> {
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("unknown");
    
    // 1. Load file into CFData
    let data = fs::read(path).map_err(|e| format!("Failed to read file: {}", e))?;
    let cf_data = CFData::from_buffer(&data);
    
    unsafe {
        // 2. Create Image Source
        let source = CGImageSourceCreateWithData(cf_data.as_concrete_TypeRef(), std::ptr::null());
        if source.is_null() {
            return Err("Failed to create CGImageSource".to_string());
        }
        
        // 3. Create CGImage
        let cg_image_ref = CGImageSourceCreateImageAtIndex(source, 0, std::ptr::null());
        if cg_image_ref.is_null() {
            return Err("Failed to create CGImage from source".to_string());
        }
        let cg_image = CGImage::from_referenced_ptr(cg_image_ref);
        
        // 4. Create Bitmap Context to force RGBA8 format
        let width = cg_image.width();
        let height = cg_image.height();
        let color_space = CGColorSpace::create_device_rgb();
        
        let mut context = CGContext::create_bitmap_context(
            None,
            width,
            height,
            8,
            width * 4,
            &color_space,
            core_graphics::base::kCGImageAlphaPremultipliedLast // This yields RGBA
        );
        
        // 5. Draw image into context
        let rect = core_graphics::geometry::CGRect::new(
            &core_graphics::geometry::CGPoint::new(0.0, 0.0),
            &core_graphics::geometry::CGSize::new(width as f64, height as f64)
        );
        context.draw_image(&rect, &cg_image);
        
        // 6. Extract data
        let pixel_data = context.data().to_vec();
        
        log::info!("[{}] Decoded via MacOS ImageIO: {}x{}", file_name, width, height);

        Ok(ImageData::Static(DecodedImage {
            width: width as u32,
            height: height as u32,
            pixels: pixel_data,
        }))
    }
}

// Fallback for non-macos platforms so it compiles
#[cfg(not(target_os = "macos"))]
#[allow(dead_code)]
pub fn load_via_image_io(_path: &PathBuf) -> Result<ImageData, String> {
    Err("ImageIO is only supported on macOS".to_string())
}
