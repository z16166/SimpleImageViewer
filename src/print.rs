// Simple Image Viewer - A high-performance, cross-platform image viewer
// Copyright (C) 2024 Simple Image Viewer Contributors
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

use std::path::{Path, PathBuf};
use std::thread;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use image::{Rgb, RgbImage, Rgba, RgbaImage};

const JPEG_QUALITY: u8 = 95;

#[derive(Clone, Copy, PartialEq)]
pub enum PrintMode {
    FullImage,
    VisibleArea,
}

pub struct PrintJob {
    pub mode: PrintMode,
    pub original_path: PathBuf,
    /// [x, y, w, h] in original image pixel coordinates (before tile downscaling)
    pub crop_rect_pixels: Option<[u32; 4]>, 
    pub is_tiled: bool,
    /// For tiled images: Arc-shared raw pixel buffer + dimensions.
    /// The background thread will do the downsampling itself.
    pub tile_pixel_buffer: Option<Arc<Vec<u8>>>,
    pub tile_full_width: u32,
    pub tile_full_height: u32,
}

pub fn spawn_print_job(
    job: PrintJob, 
    is_busy_flag: Arc<AtomicBool>, 
    tx_status: crossbeam_channel::Sender<Option<String>>
) {
    is_busy_flag.store(true, Ordering::Relaxed);

    thread::spawn(move || {
        let result = process_print_job(job);
        
        if let Err(e) = result {
            let _ = tx_status.send(Some(e));
        } else {
            let _ = tx_status.send(None); // Clear status
        }
        is_busy_flag.store(false, Ordering::Relaxed);
    });
}

/// Downsample a raw RGBA pixel buffer (same algorithm as TileManager::generate_preview).
/// Runs on the background thread to avoid blocking the UI.
fn downsample_preview(
    pixel_buffer: &[u8],
    full_width: u32,
    full_height: u32,
    max_w: u32,
    max_h: u32,
) -> (u32, u32, Vec<u8>) {
    let scale = (max_w as f64 / full_width as f64)
        .min(max_h as f64 / full_height as f64)
        .min(1.0);
    let out_w = (full_width as f64 * scale).round().max(1.0) as u32;
    let out_h = (full_height as f64 * scale).round().max(1.0) as u32;

    let mut out = vec![0u8; (out_w * out_h * 4) as usize];
    let src_stride = full_width as usize * 4;

    for y in 0..out_h {
        let src_y = ((y as f64 / scale).min((full_height - 1) as f64)) as usize;
        for x in 0..out_w {
            let src_x = ((x as f64 / scale).min((full_width - 1) as f64)) as usize;
            let src_off = src_y * src_stride + src_x * 4;
            let dst_off = (y as usize * out_w as usize + x as usize) * 4;
            out[dst_off..dst_off + 4].copy_from_slice(&pixel_buffer[src_off..src_off + 4]);
        }
    }
    (out_w, out_h, out)
}

fn process_print_job(job: PrintJob) -> Result<(), String> {
    let is_windows = cfg!(target_os = "windows");

    // 1. If Windows, FullImage, non-tiled, and JPEG (no alpha), we might fast-path it.
    let ext = job.original_path.extension().unwrap_or_default().to_string_lossy().to_lowercase();
    let is_jpeg = ext == "jpg" || ext == "jpeg";
    
    if is_windows && job.mode == PrintMode::FullImage && !job.is_tiled && is_jpeg {
        // Fast path for Windows JPEG
        invoke_system_print(&job.original_path)?;
        return Ok(());
    }

    // 2. Generate the print-ready RGB image.
    let print_rgb: RgbImage = if job.is_tiled {
        // Tiled mode: downsample the full pixel buffer on THIS (background) thread.
        let pixel_buf = job.tile_pixel_buffer.as_ref()
            .ok_or_else(|| rust_i18n::t!("print.err_buffer").to_string())?;
        
        let max_dim = 4000;
        let (pw, ph, preview_rgba) = downsample_preview(
            pixel_buf, job.tile_full_width, job.tile_full_height, max_dim, max_dim,
        );
        let rgba_img = RgbaImage::from_raw(pw, ph, preview_rgba)
            .ok_or_else(|| rust_i18n::t!("print.err_buffer").to_string())?;
        
        if job.mode == PrintMode::VisibleArea && job.crop_rect_pixels.is_some() {
            let [cx, cy, cw, ch] = job.crop_rect_pixels.unwrap();
            // Scale crop rect from original-image coordinates to preview coordinates
            let scale_x = pw as f32 / job.tile_full_width as f32;
            let scale_y = ph as f32 / job.tile_full_height as f32;
            let sx = (cx as f32 * scale_x) as u32;
            let sy = (cy as f32 * scale_y) as u32;
            let sw = (cw as f32 * scale_x).max(1.0) as u32;
            let sh = (ch as f32 * scale_y).max(1.0) as u32;
            // Clamp to preview image bounds to prevent panic
            let sx = sx.min(pw.saturating_sub(1));
            let sy = sy.min(ph.saturating_sub(1));
            let sw = sw.min(pw - sx);
            let sh = sh.min(ph - sy);
            let cropped_img = image::imageops::crop_imm(&rgba_img, sx, sy, sw, sh).to_image();
            flatten_alpha_to_white(&cropped_img)
        } else {
            flatten_alpha_to_white(&rgba_img)
        }
    } else {
        // Standard (non-tiled) load
        let img = image::open(&job.original_path).map_err(|e| e.to_string())?;
        
        let final_img = if job.mode == PrintMode::VisibleArea && job.crop_rect_pixels.is_some() {
            let [x, y, w, h] = job.crop_rect_pixels.unwrap();
            // Clamp to image bounds to prevent panic
            let (iw, ih) = (img.width(), img.height());
            let x = x.min(iw.saturating_sub(1));
            let y = y.min(ih.saturating_sub(1));
            let w = w.min(iw - x);
            let h = h.min(ih - y);
            img.crop_imm(x, y, w, h)
        } else {
            img
        };

        let is_opaque = final_img.color().has_alpha() == false; 
        
        // Fast path for Windows non-alpha non-tiled supported standard format (BMP,PNG,GIF,TIF)
        // Only if it was FullImage (because visible area forces temporary file anyway)
        if is_windows && job.mode == PrintMode::FullImage && is_opaque {
            if ["png", "bmp", "tif", "tiff", "gif"].contains(&ext.as_str()) {
                invoke_system_print(&job.original_path)?;
                return Ok(());
            }
        }

        // If we reach here, we must flatten and save.
        flatten_alpha_to_white(&final_img.to_rgba8())
    };

    // 3. Save to temp and print (with 95% JPEG quality)
    let temp_dir = std::env::temp_dir();
    let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_micros();
    
    #[cfg(target_os = "windows")]
    {
        let temp_jpg = temp_dir.join(format!("siv_print_temp_{}.jpg", ts));
        save_jpeg_with_quality(&print_rgb, &temp_jpg, JPEG_QUALITY)?;
        invoke_system_print(&temp_jpg)?;
    }

    #[cfg(not(target_os = "windows"))]
    {
        // Generate PDF for macOS/Linux
        let temp_pdf = temp_dir.join(format!("siv_print_temp_{}.pdf", ts));
        export_to_pdf_and_print(&print_rgb, &temp_pdf)?;
    }

    Ok(())
}

/// Save an RgbImage as JPEG with explicit quality (0–100).
fn save_jpeg_with_quality(img: &RgbImage, path: &Path, quality: u8) -> Result<(), String> {
    use ::image::codecs::jpeg::JpegEncoder;
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut encoder = JpegEncoder::new_with_quality(std::io::BufWriter::new(file), quality);
    encoder.encode(
        img.as_raw(),
        img.width(),
        img.height(),
        ::image::ExtendedColorType::Rgb8,
    ).map_err(|e| e.to_string())
}

/// Helper to flatten RGBA to white-background RGB
pub fn flatten_alpha_to_white(rgba_img: &RgbaImage) -> RgbImage {
    let (width, height) = rgba_img.dimensions();
    let mut rgb_img = RgbImage::new(width, height);

    for (x, y, pixel) in rgba_img.enumerate_pixels() {
        let Rgba([r, g, b, a]) = *pixel;
        
        if a == 255 {
            rgb_img.put_pixel(x, y, Rgb([r, g, b]));
        } else if a == 0 {
            rgb_img.put_pixel(x, y, Rgb([255, 255, 255]));
        } else {
            let alpha = a as f32 / 255.0;
            let inv_alpha = 1.0 - alpha;
            let new_r = ((r as f32 * alpha) + (255.0 * inv_alpha)) as u8;
            let new_g = ((g as f32 * alpha) + (255.0 * inv_alpha)) as u8;
            let new_b = ((b as f32 * alpha) + (255.0 * inv_alpha)) as u8;
            rgb_img.put_pixel(x, y, Rgb([new_r, new_g, new_b]));
        }
    }
    
    rgb_img
}

#[cfg(target_os = "windows")]
fn invoke_system_print(path: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;
    use winapi::um::shellapi::ShellExecuteW;

    let path_w: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let verb_w: Vec<u16> = std::ffi::OsStr::new("print").encode_wide().chain(std::iter::once(0)).collect();

    let res = unsafe {
        ShellExecuteW(
            ptr::null_mut(),
            verb_w.as_ptr(),
            path_w.as_ptr(),
            ptr::null(),
            ptr::null(),
            winapi::um::winuser::SW_SHOWNORMAL,
        )
    };

    if (res as isize) <= 32 {
        Err(format!("ShellExecuteW failed with code {}", res as isize))
    } else {
        Ok(())
    }
}

#[cfg(not(target_os = "windows"))]
fn invoke_system_print(_path: &Path) -> Result<(), String> {
    // Should not be called on macOS/Linux as we use `export_to_pdf_and_print`
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn export_to_pdf_and_print(img: &RgbImage, out_path: &Path) -> Result<(), String> {
    let (width, height) = img.dimensions();
    
    use printpdf::*;

    // PDF unit: 1pt = 1/72 inch = 0.352778 mm
    let px_to_mm = 0.352778_f32;
    let width_mm = Mm(width as f32 * px_to_mm);
    let height_mm = Mm(height as f32 * px_to_mm);

    // printpdf 0.9.x: create document, add image as RawImage, build page with ops
    let mut doc = PdfDocument::new("Print Image");

    // Build a RawImage from the RGB pixel data
    let raw_image = RawImage {
        pixels: RawImageData::U8(img.as_raw().clone()),
        width: width as usize,
        height: height as usize,
        data_format: RawImageFormat::RGB8,
        tag: Vec::new(),
    };

    // Add image to document resources, get its XObject ID
    let image_id = doc.add_image(&raw_image);

    // Create a page with the image placed at origin, filling the page
    let page = PdfPage::new(width_mm, height_mm, vec![
        Op::UseXobject {
            id: image_id,
            transform: XObjectTransform {
                translate_x: Some(Pt(0.0)),
                translate_y: Some(Pt(0.0)),
                rotate: None,
                scale_x: None,
                scale_y: None,
                dpi: Some(72.0),
            },
        },
    ]);
    doc.with_pages(vec![page]);

    // Save directly to the target file path
    let file = std::fs::File::create(out_path).map_err(|e| e.to_string())?;
    let mut warnings = Vec::new();
    doc.save_writer(
        &mut std::io::BufWriter::new(file),
        &PdfSaveOptions::default(),
        &mut warnings,
    );
    
    // Invoke system open to print
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(out_path)
            .spawn()
            .map_err(|e| format!("Failed to open PDF on macOS: {}", e))?;
    }
    
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(out_path)
            .spawn()
            .map_err(|e| format!("Failed to open PDF on Linux: {}", e))?;
    }

    Ok(())
}
