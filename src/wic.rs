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

use std::sync::{Arc, RwLock};
use std::collections::HashSet;
use std::thread;

#[cfg(target_os = "windows")]
use windows::Win32::Graphics::Imaging::*;
#[cfg(target_os = "windows")]
use windows::Win32::System::Com::*;
#[cfg(target_os = "windows")]
use windows::core::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FormatGroup {
    Standard,
    Pro,
    WicSystem,
    WicRaw,
    Others,
}

#[derive(Debug, Clone)]
pub struct ImageFormat {
    pub extension: String,
    pub group: FormatGroup,
    pub description: String,
    pub clsid: Option<windows::core::GUID>,
}

pub struct FormatRegistry {
    pub formats: Vec<ImageFormat>,
    pub extensions: HashSet<String>,
    pub discovery_finished: bool,
}

impl FormatRegistry {
    fn new() -> Self {
        let mut formats = Vec::new();
        let mut extensions = HashSet::new();

        let builtin_standard = [
            ("png", "PNG Image"),
            ("jpg", "JPEG Image"),
            ("jpeg", "JPEG Image"),
            ("gif", "GIF Image"),
            ("bmp", "Bitmap Image"),
            ("webp", "WebP Image"),
            ("ico", "Icon Image"),
            ("avif", "AVIF Image"),
        ];

        let builtin_pro = [
            ("tiff", "TIFF Image"),
            ("tif", "TIFF Image"),
            ("tga", "TGA Image"),
            ("psd", "Photoshop Image"),
            ("psb", "Photoshop Large Image"),
            ("exr", "OpenEXR Image"),
            ("hdr", "High Dynamic Range Image"),
            ("qoi", "QOI Image"),
            ("ppm", "PPM Image"),
            ("pbm", "PBM Image"),
            ("pgm", "PGM Image"),
            ("pnm", "PNM Image"),
            ("heif", "HEIF Image"),
            ("heic", "HEIC Image"),
        ];

        for (ext, desc) in builtin_standard {
            formats.push(ImageFormat {
                extension: ext.to_string(),
                group: FormatGroup::Standard,
                description: desc.to_string(),
                clsid: None,
            });
            extensions.insert(ext.to_string());
        }

        for (ext, desc) in builtin_pro {
            formats.push(ImageFormat {
                extension: ext.to_string(),
                group: FormatGroup::Pro,
                description: desc.to_string(),
                clsid: None,
            });
            extensions.insert(ext.to_string());
        }

        Self {
            formats,
            extensions,
            discovery_finished: false,
        }
    }

    fn add_format(&mut self, format: ImageFormat) {
        if !self.extensions.contains(&format.extension) {
            self.extensions.insert(format.extension.clone());
            self.formats.push(format);
        }
    }
}

pub static REGISTRY: std::sync::OnceLock<Arc<RwLock<FormatRegistry>>> = std::sync::OnceLock::new();

pub fn get_registry() -> Arc<RwLock<FormatRegistry>> {
    REGISTRY.get_or_init(|| Arc::new(RwLock::new(FormatRegistry::new()))).clone()
}

pub struct ComGuard;

impl ComGuard {
    pub fn new() -> windows::core::Result<Self> {
        #[cfg(target_os = "windows")]
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED)?;
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
                let _com = ComGuard::new().expect("Failed to initialize COM on Rayon worker");
                rayon_thread.run()
            })?;
            Ok(())
        })
        .build_global()
        .expect("Failed to build global Rayon thread pool");
}

pub fn spawn_wic_discovery() {
    thread::spawn(|| {
        #[cfg(target_os = "windows")]
        {
            let _com = ComGuard::new().expect("Failed to initialize COM for WIC discovery");
            if let Err(e) = discover_wic_formats() {
                log::error!("WIC discovery failed: {:?}", e);
            }
        }
        
        if let Ok(mut reg) = get_registry().write() {
            reg.discovery_finished = true;
        }
    });
}

#[cfg(target_os = "windows")]
fn discover_wic_formats() -> windows::core::Result<()> {
    unsafe {
        let factory: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        
        let enumerator = match factory.CreateComponentEnumerator(WICDecoder.0 as u32, WICComponentEnumerateDefault.0 as u32) {
            Ok(e) => e,
            Err(_) => {
                // Fallback to Refresh if Default fails (unlikely, but helps stability)
                factory.CreateComponentEnumerator(WICDecoder.0 as u32, WICComponentEnumerateRefresh.0 as u32)?
            }
        };

        let mut components = [None; 1];
        let mut fetched = 0;
        let mut new_codecs = 0;

        loop {
            match enumerator.Next(&mut components, Some(&mut fetched)) {
                Ok(_) if fetched > 0 => {
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
                                                    clsid: codec_info.GetCLSID().ok(),
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
                }
                Ok(_) => break,
                Err(e) => {
                    log::error!("WIC discovery encountered error during enumeration: {:?}", e);
                    return Err(e);
                }
            }
        }
        log::info!("WIC discovery finished: identified {} additional system codecs.", new_codecs);
    }
    Ok(())
}

/// Decodes an image file using Windows Imaging Component.
pub fn load_via_wic(path: &std::path::Path) -> std::result::Result<crate::loader::ImageData, String> {
    #[cfg(not(target_os = "windows"))]
    {
        let _ = path;
        return Err("WIC is only available on Windows".to_string());
    }

    #[cfg(target_os = "windows")]
    unsafe {
        let _com = ComGuard::new().map_err(|e| format!("COM Init failed: {:?}", e))?;

        let factory: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)
            .map_err(|e| format!("Factory creation failed: {:?}", e))?;

        let path_os = path.as_os_str();
        use std::os::windows::ffi::OsStrExt;
        let mut path_wide: Vec<u16> = path_os.encode_wide().collect();
        path_wide.push(0);
        let path_ptr = PCWSTR(path_wide.as_ptr());

        // Using explicit module paths for all WIC/Win32 symbols
        use windows::Win32::Foundation::GENERIC_READ;
        use windows::Win32::Graphics::Imaging::*;

        // Attempt 1: Standard sniffing using CreateDecoderFromFilename
        let mut decoder_res = factory.CreateDecoderFromFilename(
            path_ptr,
            None,
            GENERIC_READ,
            WICDecodeMetadataCacheOnLoad,
        );

        // Attempt 2: Explicit decoder creation if sniffer failed to find a component (0x88982F50)
        if let Err(ref e) = decoder_res {
            if e.code() == windows::core::HRESULT(0x88982F50u32 as i32) {
                if let Some(ext) = path.extension().and_then(|e| e.to_str()).map(|e| e.to_lowercase()) {
                    let clsid_opt = if let Ok(reg) = get_registry().read() {
                        reg.formats.iter()
                            .find(|f| f.extension == ext)
                            .and_then(|f| f.clsid)
                    } else {
                        None
                    };

                    if let Some(clsid) = clsid_opt {
                        log::info!("WIC Sniffer failed for {:?} (COMPONENTNOTFOUND), trying explicit decoder instantiation for CLSID {:?}", path, clsid);
                        
                        // Create the specific decoder instance
                        let specific_decoder: windows::core::Result<IWICBitmapDecoder> = CoCreateInstance(&clsid, None, CLSCTX_INPROC_SERVER);
                        match specific_decoder {
                            Ok(sd) => {
                                // Must initialize the decoder with a stream
                                match (|| -> windows::core::Result<IWICBitmapDecoder> {
                                    let stream = factory.CreateStream()?;
                                    stream.InitializeFromFilename(path_ptr, GENERIC_READ.0)?;
                                    sd.Initialize(&stream, WICDecodeMetadataCacheOnLoad)?;
                                    Ok(sd)
                                })() {
                                    Ok(sd) => {
                                        log::info!("Explicit WIC decoder initialization successful for {:?}", path);
                                        decoder_res = Ok(sd);
                                    }
                                    Err(ie) => {
                                        log::warn!("Failed to initialize explicit WIC decoder for {:?}: {:?}", path, ie);
                                    }
                                }
                            }
                            Err(ce) => {
                                log::warn!("Failed to explicitly instantiate WIC decoder {:?}: {:?}", clsid, ce);
                            }
                        }
                    }
                }
            }
        }

        let decoder = decoder_res.map_err(|e| {
            let err_msg = format!("Failed to create WIC decoder for {:?}: {:?}", path, e);
            log::error!("{}", err_msg);
            err_msg
        })?;

        let frame = decoder.GetFrame(0)
            .map_err(|e| format!("Failed to get WIC frame: {:?}", e))?;

        let converter = factory.CreateFormatConverter()
            .map_err(|e| format!("Failed to create WIC converter: {:?}", e))?;

        converter.Initialize(
            &frame,
            &GUID_WICPixelFormat32bppRGBA,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeCustom,
        ).map_err(|e| format!("WIC converter initialization failed: {:?}", e))?;

        let mut width = 0;
        let mut height = 0;
        converter.GetSize(&mut width, &mut height)
            .map_err(|e| format!("Failed to get WIC image size: {:?}", e))?;

        let stride = width * 4;
        let mut pixels = vec![0u8; (stride * height) as usize];
        converter.CopyPixels(
            std::ptr::null(),
            stride,
            &mut pixels,
        ).map_err(|e| format!("Failed to copy WIC pixels: {:?}", e))?;

        Ok(crate::loader::ImageData::Static(crate::loader::DecodedImage {
            width,
            height,
            pixels,
        }))
    }
}
