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

pub use crate::formats::{FormatGroup, ImageFormat, get_registry};
use std::thread;
use std::cell::RefCell;

#[cfg(target_os = "windows")]
thread_local! {
    static WIC_FACTORY: RefCell<Option<IWICImagingFactory>> = RefCell::new(None);
}

#[cfg(target_os = "windows")]
fn get_wic_factory() -> windows::core::Result<IWICImagingFactory> {
    WIC_FACTORY.with(|f| {
        let mut factory = f.borrow_mut();
        if factory.is_none() {
            *factory = Some(unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)? });
        }
        Ok(factory.as_ref().unwrap().clone())
    })
}

#[cfg(target_os = "windows")]
use windows::Win32::Graphics::Imaging::*;
#[cfg(target_os = "windows")]
use windows::Win32::System::Com::*;
#[cfg(target_os = "windows")]
use windows::Win32::Foundation::GENERIC_READ;
#[cfg(target_os = "windows")]
use windows::core::*;

pub struct ComGuard;

impl ComGuard {
    pub fn new() -> windows::core::Result<Self> {
        #[cfg(target_os = "windows")]
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        }
        Ok(Self)
    }
}

#[cfg(target_os = "windows")]
impl Drop for ComGuard {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}

pub fn init_rayon_with_com() {
    rayon::ThreadPoolBuilder::new()
        .spawn_handler(|rayon_thread| {
            let mut builder = thread::Builder::new();
            if let Some(name) = rayon_thread.name() {
                builder = builder.name(name.to_owned());
            }
            if let Some(stack_size) = rayon_thread.stack_size() {
                builder = builder.stack_size(stack_size);
            }

            builder.spawn(move || {
                let _com = ComGuard::new().expect("Failed to initialize COM on WIC worker");
                rayon_thread.run()
            })?;
            Ok(())
        })
        .build_global()
        .unwrap_or(());
}

pub fn spawn_wic_discovery() {
    thread::spawn(|| {
        #[cfg(target_os = "windows")]
        if let Err(e) = discover_wic_codecs() {
            log::error!("WIC codec discovery failed: {:?}", e);
        }
        
        #[cfg(not(target_os = "windows"))]
        if let Ok(mut reg) = get_registry().write() {
            reg.discovery_finished = true;
        }
    });
}

#[cfg(target_os = "windows")]
fn discover_wic_codecs() -> windows::core::Result<()> {
    let _com = ComGuard::new()?;
    unsafe {
        let factory = get_wic_factory()?;
        
        let enumerator = factory.CreateComponentEnumerator(WICDecoder.0 as u32, WICComponentEnumerateDefault.0 as u32)?;
        
        let mut components = [None; 1];
        let mut _fetched = 0;
        let mut new_codecs = 0;

        loop {
            let hr = enumerator.Next(&mut components, Some(&mut _fetched));
            if hr.is_ok() && _fetched > 0 {
                if let Some(unknown) = components[0].take() {
                    if let Ok(codec_info) = unknown.cast::<IWICBitmapCodecInfo>() {
                        let mut ext_buf = [0u16; 512];
                        let mut ext_len = 0;
                        if codec_info.GetFileExtensions(&mut ext_buf, &mut ext_len).is_ok() && ext_len > 1 {
                            let extensions_str = String::from_utf16_lossy(&ext_buf[..ext_len as usize - 1]);

                            let mut name_buf = [0u16; 512];
                            let mut name_len = 0;
                            let friendly_name = if codec_info.GetFriendlyName(&mut name_buf, &mut name_len).is_ok() && name_len > 1 {
                                String::from_utf16_lossy(&name_buf[..name_len as usize - 1])
                            } else {
                                "Unknown WIC Codec".to_string()
                            };

                            let group = if friendly_name.contains("RAW") || friendly_name.contains("Camera") {
                                FormatGroup::WicRaw
                            } else if friendly_name.contains("Microsoft") || friendly_name.contains("Windows") {
                                FormatGroup::WicSystem
                            } else {
                                FormatGroup::Others
                            };

                            let clsid = codec_info.GetCLSID()?;
                            let mut clsid_bytes = [0u8; 16];
                            std::ptr::copy_nonoverlapping(&clsid as *const GUID as *const u8, clsid_bytes.as_mut_ptr(), 16);

                            if let Ok(mut reg) = get_registry().write() {
                                let mut added_for_codec = false;
                                for ext in extensions_str.split(|c| c == ',' || c == ';') {
                                    let normalized_ext = ext.trim()
                                        .trim_start_matches('*')
                                        .trim_start_matches('.')
                                        .to_lowercase();
                                    if !normalized_ext.is_empty() {
                                        if !reg.extensions.contains(&normalized_ext) {
                                            reg.add_format(ImageFormat {
                                                extension: normalized_ext,
                                                group,
                                                description: friendly_name.clone(),
                                                wic_clsid: Some(clsid_bytes),
                                            });
                                            added_for_codec = true;
                                        }
                                    }
                                }
                                if added_for_codec {
                                    new_codecs += 1;
                                }
                            }
                        }
                    }
                }
            } else {
                break;
            }
        }
        log::info!("WIC discovery finished: identified {} additional system codecs.", new_codecs);
    }
    
    if let Ok(mut reg) = get_registry().write() {
        reg.discovery_finished = true;
    }
    Ok(())
}

/// A tiled source for Windows Imaging Component (WIC) decoders.
/// Allows on-demand decoding of any WIC-supported format without full memory allocation.
pub struct WicTiledSource {
    #[allow(dead_code)]
    path: std::path::PathBuf,
    width: u32,
    height: u32,
    #[allow(dead_code)]
    frame: IWICBitmapFrameDecode,
    converter: IWICFormatConverter,
}

// WIC interfaces are thread-safe for reading if COM was initialized as COINIT_MULTITHREADED.
unsafe impl Send for WicTiledSource {}
unsafe impl Sync for WicTiledSource {}

impl crate::loader::TiledImageSource for WicTiledSource {
    fn width(&self) -> u32 { self.width }
    fn height(&self) -> u32 { self.height }

    fn extract_tile(&self, x: u32, y: u32, w: u32, h: u32) -> Vec<u8> {
        let stride = w * 4;
        let mut pixels = vec![0u8; (stride * h) as usize];
        
        let rect = WICRect {
            X: x as i32,
            Y: y as i32,
            Width: w as i32,
            Height: h as i32,
        };

        unsafe {
            // WIC's CopyPixels is highly efficient and only decodes the required region
            // if the underlying codec supports it (e.g., tiled TIFF).
            let _ = self.converter.CopyPixels(&rect, stride, &mut pixels);
        }
        
        pixels
    }

    fn generate_preview(&self, max_w: u32, max_h: u32) -> (u32, u32, Vec<u8>) {
        let scale = (max_w as f64 / self.width as f64)
            .min(max_h as f64 / self.height as f64)
            .min(1.0);
        let out_w = (self.width as f64 * scale).round().max(1.0) as u32;
        let out_h = (self.height as f64 * scale).round().max(1.0) as u32;

        // For general preview, we still use WIC but since it doesn't have a high-performance 
        // downscaler built into CopyPixels, we just sample it or use a scaler if available.
        // For now, to keep it simple and responsive, we extract at intervals or just decode 
        // a thumbnail if WIC supports it.
        
        // Simple implementation: Use the scaler if possible (not implemented here for brevity, 
        // fall back to interval sampling or just scaling the result of a larger CopyPixels).
        // Actual implementation uses the same logic as PsbTiledSource for consistency.
        let mut out = vec![0u8; (out_w * out_h * 4) as usize];
        for y in 0..out_h {
            let src_y = ((y as f64 / scale).min((self.height - 1) as f64)) as u32;
            let rect = WICRect { X: 0, Y: src_y as i32, Width: self.width as i32, Height: 1 };
            let mut line = vec![0u8; (self.width * 4) as usize];
            unsafe {
                let _ = self.converter.CopyPixels(&rect, self.width * 4, &mut line);
            }
            for x in 0..out_w {
                let src_x = ((x as f64 / scale).min((self.width - 1) as f64)) as usize;
                let src_off = src_x * 4;
                let dst_off = (y as usize * out_w as usize + x as usize) * 4;
                out[dst_off..dst_off+4].copy_from_slice(&line[src_off..src_off+4]);
            }
        }

        (out_w, out_h, out)
    }

    fn full_pixels(&self) -> Option<std::sync::Arc<Vec<u8>>> {
        None
    }
}

pub fn load_via_wic(path: &std::path::Path) -> std::result::Result<crate::loader::ImageData, String> {
    #[cfg(not(target_os = "windows"))]
    {
        let _ = path;
        return Err("WIC is only available on Windows".to_string());
    }

    #[cfg(target_os = "windows")]
    unsafe {
        let _com = ComGuard::new().map_err(|e| format!("COM Init failed: {:?}", e))?;

        let factory = get_wic_factory().map_err(|e| format!("Factory access failed: {:?}", e))?;

        let path_os = path.as_os_str();
        use std::os::windows::ffi::OsStrExt;
        let mut path_wide: Vec<u16> = path_os.encode_wide().collect();
        path_wide.push(0);
        let path_ptr = PCWSTR(path_wide.as_ptr());

        // Attempt 1: Standard sniffing
        let mut decoder_res = factory.CreateDecoderFromFilename(
            path_ptr,
            None,
            GENERIC_READ,
            WICDecodeMetadataCacheOnLoad,
        );

        // Attempt 2: Explicit decoder creation
        if let Err(ref e) = decoder_res {
            if e.code() == windows::core::HRESULT(0x88982F50u32 as i32) {
                if let Some(ext) = path.extension().and_then(|e| e.to_str()).map(|e| e.to_lowercase()) {
                    let clsid_bytes_opt = if let Ok(reg) = get_registry().read() {
                        reg.formats.iter()
                            .find(|f| f.extension == ext)
                            .and_then(|f| f.wic_clsid.clone())
                    } else {
                        None
                    };

                    if let Some(clsid_bytes) = clsid_bytes_opt {
                        let mut clsid = GUID::default();
                        std::ptr::copy_nonoverlapping(clsid_bytes.as_ptr(), &mut clsid as *mut GUID as *mut u8, 16);

                        log::info!("WIC Sniffer failed for {:?} (COMPONENTNOTFOUND), trying explicit decoder instantiation for CLSID {:?}", path, clsid);

                        let specific_decoder: windows::core::Result<IWICBitmapDecoder> = CoCreateInstance(&clsid, None, CLSCTX_INPROC_SERVER);
                        if let Ok(sd) = specific_decoder {
                            if let Ok(stream) = factory.CreateStream() {
                                if stream.InitializeFromFilename(path_ptr, GENERIC_READ.0).is_ok() {
                                    if sd.Initialize(&stream, WICDecodeMetadataCacheOnLoad).is_ok() {
                                        decoder_res = Ok(sd);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        let decoder = decoder_res.map_err(|e| format!("WIC decoder creation failed: {:?}", e))?;
        let frame = decoder.GetFrame(0).map_err(|e| format!("failed to get frame: {:?}", e))?;
        let converter = factory.CreateFormatConverter().map_err(|e| format!("converter creation failed: {:?}", e))?;

        converter.Initialize(
            &frame,
            &GUID_WICPixelFormat32bppRGBA,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeCustom,
        ).map_err(|e| format!("converter init failed: {:?}", e))?;

        let mut width = 0;
        let mut height = 0;
        converter.GetSize(&mut width, &mut height).map_err(|e| format!("get size failed: {:?}", e))?;

        let pixel_count = width as u64 * height as u64;
        let limit = crate::tile_cache::get_max_texture_side();

        if pixel_count >= crate::tile_cache::TILED_THRESHOLD || width > limit || height > limit {
            // Virtualized path: return the Tiled source instead of decoding everything now
            return Ok(crate::loader::ImageData::Tiled(std::sync::Arc::new(WicTiledSource {
                path: path.to_path_buf(),
                width,
                height,
                frame,
                converter,
            })));
        }

        let stride = width * 4;
        let mut pixels = vec![0u8; (stride * height) as usize];
        converter.CopyPixels(std::ptr::null(), stride, &mut pixels)
            .map_err(|e| format!("copy pixels failed: {:?}", e))?;

        Ok(crate::loader::ImageData::Static(crate::loader::DecodedImage {
            width,
            height,
            pixels,
        }))
    }
}
