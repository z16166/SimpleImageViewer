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
            let instance = unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)? };
            *factory = Some(instance);
        }
        factory.as_ref().cloned().ok_or_else(|| windows::core::Error::from_win32())
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
    path: std::path::PathBuf,
    width: u32,
    height: u32,
    factory: IWICImagingFactory,
    decoder: IWICBitmapDecoder, 
    frame: IWICBitmapFrameDecode,
    source: IWICBitmapSource,
    physical_width: u32,
    physical_height: u32,
    transform_options: WICBitmapTransformOptions,
    // Explicitly keep the stream alive to prevent data source invalidation
    #[allow(dead_code)]
    stream: Option<IWICStream>,
    // Lazy initialized converter to avoid overhead during initial load
    converter: std::sync::Mutex<Option<IWICFormatConverter>>,
    // _mmap MUST be at the end to ensure it is dropped AFTER the WIC objects
    _mmap: Option<std::sync::Arc<memmap2::Mmap>>,
}

impl WicTiledSource {
    fn ensure_converter(&self) -> windows::core::Result<IWICFormatConverter> {
        // Ensure COM is initialized on the current worker thread
        let _com = ComGuard::new();
        
        let mut lock = self.converter.lock().unwrap();
        if let Some(c) = &*lock {
            return Ok(c.clone());
        }

        unsafe {
            let converter = self.factory.CreateFormatConverter()?;
            converter.Initialize(
                &self.source,
                &GUID_WICPixelFormat32bppRGBA,
                WICBitmapDitherTypeNone,
                None,
                0.0,
                WICBitmapPaletteTypeCustom,
            )?;
            *lock = Some(converter.clone());
            Ok(converter)
        }
    }
}

// WIC interfaces are thread-safe for reading if COM was initialized as COINIT_MULTITHREADED.
unsafe impl Send for WicTiledSource {}
unsafe impl Sync for WicTiledSource {}

impl crate::loader::TiledImageSource for WicTiledSource {
    fn width(&self) -> u32 { self.width }
    fn height(&self) -> u32 { self.height }

    fn extract_tile(&self, x: u32, y: u32, w: u32, h: u32) -> Vec<u8> {
        let mut pixels = vec![0u8; (w * h * 4) as usize];
        let stride = w * 4;
        
        // Use a persistent converter to avoid re-allocating it for every tile
        let _lock = match self.converter.lock() {
            Ok(l) => l,
            Err(_) => return pixels, // Poisoned mutex, return empty pixels
        };
        
        let converter = match self.ensure_converter() {
            Ok(c) => c,
            Err(e) => {
                log::error!("[{}] WIC: Failed to ensure converter for tile: {:?}", self.path.display(), e);
                return pixels;
            }
        };

        let rect = WICRect {
            X: x as i32,
            Y: y as i32,
            Width: w as i32,
            Height: h as i32,
        };

        unsafe {
            let _ = converter.CopyPixels(&rect, stride, &mut pixels);
        }
        
        pixels
    }

    fn generate_preview(&self, max_w: u32, max_h: u32) -> (u32, u32, Vec<u8>) {
        let scale = (max_w as f64 / self.width as f64)
            .min(max_h as f64 / self.height as f64)
            .min(1.0);
        let out_w = (self.width as f64 * scale).round().max(1.0) as u32;
        let out_h = (self.height as f64 * scale).round().max(1.0) as u32;

        unsafe {
            let factory = match get_wic_factory() {
                Ok(f) => f,
                Err(e) => {
                    log::error!("[{}] WIC: Failed to get factory for preview: {:?}", self.path.display(), e);
                    return (0, 0, Vec::new());
                }
            };

            // Path 1: Extract embedded thumbnail if large enough
            if let Ok(thumbnail) = self.frame.GetThumbnail() {
                let mut fw = 0;
                let mut fh = 0;
                if thumbnail.GetSize(&mut fw, &mut fh).is_ok() && fw >= out_w && fh >= out_h {
                    log::info!("WIC: Using embedded thumbnail as preview");
                    if let Ok(thumb_src) = thumbnail.cast::<IWICBitmapSource>() {
                        let mut thumb_final = thumb_src.clone();
                        if self.transform_options != WICBitmapTransformOptions(0) {
                            if let Ok(rotator) = factory.CreateBitmapFlipRotator() {
                                if rotator.Initialize(&thumb_src, self.transform_options).is_ok() {
                                    if let Ok(src) = rotator.cast::<IWICBitmapSource>() {
                                        thumb_final = src;
                                    }
                                }
                            }
                        }
                        if let Some(res) = render_source_to_pixels(&thumb_final, &factory) {
                            return res;
                        }
                    }
                }
            }

            // Path 2: Check for secondary downscaled frames (e.g. Pyramid TIFFs)
            if let Ok(count) = self.decoder.GetFrameCount() {
                if count > 1 {
                    for i in 1..count {
                        if let Ok(f) = self.decoder.GetFrame(i) {
                            let mut fw = 0;
                            let mut fh = 0;
                            if f.GetSize(&mut fw, &mut fh).is_ok() {
                                // If it's a good intermediate size, use it
                                if fw > 512 && fw < self.physical_width / 2 {
                                    log::info!("WIC: Using secondary frame {} as preview ({}x{})", i, fw, fh);
                                    if let Ok(f_src) = f.cast::<IWICBitmapSource>() {
                                        let mut f_final = f_src.clone();
                                        if self.transform_options != WICBitmapTransformOptions(0) {
                                            if let Ok(rotator) = factory.CreateBitmapFlipRotator() {
                                                if rotator.Initialize(&f_src, self.transform_options).is_ok() {
                                                    if let Ok(src) = rotator.cast::<IWICBitmapSource>() {
                                                        f_final = src;
                                                    }
                                                }
                                            }
                                        }
                                        if let Some(res) = render_source_to_pixels(&f_final, &factory) {
                                            return res;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Path 3: Try Native Decoder Source Transform (Fastest if supported)
            if let Ok(transform) = self.frame.cast::<IWICBitmapSourceTransform>() {
                let swap = self.width != self.physical_width;
                let mut closest_phys_w = if swap { out_h } else { out_w };
                let mut closest_phys_h = if swap { out_w } else { out_h };
                if transform.GetClosestSize(&mut closest_phys_w, &mut closest_phys_h).is_ok() {
                    // Significant speedup if we are scaling down by at least 2x
                    if closest_phys_w < self.physical_width / 2 || closest_phys_h < self.physical_height / 2 {
                        let log_final_w = if swap { closest_phys_h } else { closest_phys_w };
                        let log_final_h = if swap { closest_phys_w } else { closest_phys_h };
                        log::info!("WIC: Using Native Source Transform to decode directly to {}x{} (logical: {}x{})", closest_phys_w, closest_phys_h, log_final_w, log_final_h);
                        let stride = log_final_w * 4;
                        let mut out = vec![0u8; (stride * log_final_h) as usize];
                        let rect = WICRect { X: 0, Y: 0, Width: closest_phys_w as i32, Height: closest_phys_h as i32 };
                        
                        if transform.CopyPixels(
                            &rect, 
                            closest_phys_w, 
                            closest_phys_h, 
                            &GUID_WICPixelFormat32bppRGBA as *const _, 
                            self.transform_options, 
                            stride, 
                            &mut out
                        ).is_ok() {
                            return (log_final_w, log_final_h, out);
                        }
                    }
                }
            }

            // Path 4: Sub-sampling scaler (High-speed fallback)
            // Using NearestNeighbor is MUCH faster as it allows sub-sampling at the decoder level.
            log::info!("WIC: No embedded thumbnail found for {:?}, falling back to high-speed NearestNeighbor scaler", self.path.file_name().unwrap_or_default());

            let scaler: IWICBitmapScaler = match factory.CreateBitmapScaler() {
                Ok(s) => s,
                Err(_) => return (0, 0, Vec::new()),
            };

            if scaler.Initialize(
                &self.source,
                out_w,
                out_h,
                WICBitmapInterpolationModeNearestNeighbor,
            ).is_err() {
                return (0, 0, Vec::new());
            }

            let sc_converter: IWICFormatConverter = match factory.CreateFormatConverter() {
                Ok(c) => c,
                Err(_) => return (0, 0, Vec::new()),
            };

            if sc_converter.Initialize(
                &scaler,
                &GUID_WICPixelFormat32bppRGBA,
                WICBitmapDitherTypeNone,
                None,
                0.0,
                WICBitmapPaletteTypeCustom,
            ).is_err() {
                return (0, 0, Vec::new());
            }

            let stride = out_w * 4;
            let mut out = vec![0u8; (stride * out_h) as usize];
            let rect = WICRect { X: 0, Y: 0, Width: out_w as i32, Height: out_h as i32 };
            
            if sc_converter.CopyPixels(&rect, stride, &mut out).is_ok() {
                (out_w, out_h, out)
            } else {
                (0, 0, Vec::new())
            }
        }
    }

    fn full_pixels(&self) -> Option<std::sync::Arc<Vec<u8>>> {
        None
    }
}

fn render_source_to_pixels(source: &impl Interface, factory: &IWICImagingFactory) -> Option<(u32, u32, Vec<u8>)> {
    unsafe {
        let mut w = 0;
        let mut h = 0;
        // IWICBitmapSource is the base for frames and thumbnails
        let b_source = source.cast::<IWICBitmapSource>().ok()?;
        b_source.GetSize(&mut w, &mut h).ok()?;

        let sc_converter: IWICFormatConverter = factory.CreateFormatConverter().ok()?;
        sc_converter.Initialize(
            &b_source,
            &GUID_WICPixelFormat32bppRGBA,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeCustom,
        ).ok()?;

        let stride = w * 4;
        let mut out = vec![0u8; (stride * h) as usize];
        let rect = WICRect { X: 0, Y: 0, Width: w as i32, Height: h as i32 };
        if sc_converter.CopyPixels(&rect, stride, &mut out).is_ok() {
            Some((w, h, out))
        } else {
            None
        }
    }
}

fn get_exif_orientation(path: &std::path::Path) -> u32 {
    if let Ok(file) = std::fs::File::open(path) {
        let mut reader = std::io::BufReader::new(file);
        let exifreader = exif::Reader::new();
        if let Ok(exif_data) = exifreader.read_from_container(&mut reader) {
            if let Some(field) = exif_data.get_field(exif::Tag::Orientation, exif::In::PRIMARY) {
                if let exif::Value::Short(ref v) = field.value {
                    if let Some(&o) = v.first() {
                        return o as u32;
                    }
                }
            }
        }
    }
    1
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

        let ext = path.extension().and_then(|e| e.to_str()).map(|e| e.to_lowercase()).unwrap_or_default();
        
        let mut decoder_res: Result<IWICBitmapDecoder> = Err(windows::core::Error::from_win32());
        let mut stream_out: Option<IWICStream> = None;
        let mut mmap_out: Option<std::sync::Arc<memmap2::Mmap>> = None;

        // --- Fast Path: Generic direct instantiation via registry CLSID (No sniffing) ---
        let clsid_opt = if let Ok(reg) = get_registry().read() {
            reg.formats.iter()
                .find(|f| f.extension == ext)
                .and_then(|f| f.wic_clsid)
        } else {
            None
        };

        if let Some(clsid_bytes) = clsid_opt {
            let mut clsid = GUID::default();
            std::ptr::copy_nonoverlapping(clsid_bytes.as_ptr(), &mut clsid as *mut GUID as *mut u8, 16);

            let specific_decoder: Result<IWICBitmapDecoder> = CoCreateInstance(&clsid, None, CLSCTX_INPROC_SERVER);
            if let Ok(sd) = specific_decoder {
                if let Ok(stream) = factory.CreateStream() {
                    // --- Mmap Path ---
                    let file = std::fs::File::open(path).map_err(|e| format!("File open failed: {:?}", e))?;
                    if let Ok(mmap) = memmap2::Mmap::map(&file) {
                        let m_arc = std::sync::Arc::new(mmap);
                        if stream.InitializeFromMemory(&m_arc[..]).is_ok() {
                            if sd.Initialize(&stream, WICDecodeMetadataCacheOnDemand).is_ok() {
                                decoder_res = Ok(sd);
                                stream_out = Some(stream);
                                mmap_out = Some(m_arc);
                            }
                        }
                    }
                }
            }
        }

        // --- Fallback: Standard Sniffing (If direct fails or not registered) ---
        if decoder_res.is_err() {
            decoder_res = factory.CreateDecoderFromFilename(
                path_ptr,
                None,
                GENERIC_READ,
                WICDecodeMetadataCacheOnDemand,
            );
        }

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
                                    if sd.Initialize(&stream, WICDecodeMetadataCacheOnDemand).is_ok() {
                                        decoder_res = Ok(sd);
                                        stream_out = Some(stream);
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

        // Extract dimensions directly from frame (FASTER than from converter)
        let mut width = 0;
        let mut height = 0;
        frame.GetSize(&mut width, &mut height).map_err(|e| format!("get size failed: {:?}", e))?;

        let orientation = get_exif_orientation(path);
        let transform_options = match orientation {
            2 => WICBitmapTransformOptions(8), // Flip Horizontal
            3 => WICBitmapTransformOptions(2), // Rotate180
            4 => WICBitmapTransformOptions(16),// Flip Vertical
            5 => WICBitmapTransformOptions(3 | 8), // Rotate270 | Flip Horizontal
            6 => WICBitmapTransformOptions(1), // Rotate90
            7 => WICBitmapTransformOptions(1 | 8), // Rotate90 | Flip Horizontal
            8 => WICBitmapTransformOptions(3), // Rotate270
            _ => WICBitmapTransformOptions(0),
        };
        
        let swap_wh = matches!(orientation, 5 | 6 | 7 | 8);
        let logical_width = if swap_wh { height } else { width };
        let logical_height = if swap_wh { width } else { height };

        let base_source: IWICBitmapSource = frame.cast().map_err(|e| format!("cast failed: {:?}", e))?;
        let mut final_source = base_source.clone();

        if transform_options != WICBitmapTransformOptions(0) {
            if let Ok(rotator) = factory.CreateBitmapFlipRotator() {
                if rotator.Initialize(&base_source, transform_options).is_ok() {
                    if let Ok(src) = rotator.cast::<IWICBitmapSource>() {
                        final_source = src;
                    }
                }
            }
        }

        let pixel_count = logical_width as u64 * logical_height as u64;
        let limit = crate::tile_cache::get_max_texture_side();

        if pixel_count >= crate::tile_cache::TILED_THRESHOLD || logical_width > limit || logical_height > limit {
            // Virtualized path: return the Tiled source instead of decoding everything now
            return Ok(crate::loader::ImageData::Tiled(std::sync::Arc::new(WicTiledSource {
                path: path.to_path_buf(),
                width: logical_width,
                height: logical_height,
                physical_width: width,
                physical_height: height,
                transform_options,
                factory: factory.clone(),
                decoder: decoder,
                frame: frame,
                source: final_source,
                stream: stream_out,
                converter: std::sync::Mutex::new(None),
                _mmap: mmap_out,
            })));
        }

        // --- Fallback for regular small images (Direct decode remains unchanged) ---
        let sc_converter = factory.CreateFormatConverter().map_err(|e| format!("converter creation failed: {:?}", e))?;
        sc_converter.Initialize(
            &final_source,
            &GUID_WICPixelFormat32bppRGBA,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeCustom,
        ).map_err(|e| format!("converter init failed: {:?}", e))?;

        let stride = logical_width * 4;
        let mut out = vec![0u8; (stride * logical_height) as usize];
        let rect = WICRect { X: 0, Y: 0, Width: logical_width as i32, Height: logical_height as i32 };
        
        sc_converter.CopyPixels(&rect, stride, &mut out).map_err(|e| format!("Pixel copy failed: {:?}", e))?;
        
        Ok(crate::loader::ImageData::Static(crate::loader::DecodedImage {
            width: logical_width,
            height: logical_height,
            pixels: out,
        }))
    }
}
