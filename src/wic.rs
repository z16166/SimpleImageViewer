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
        let factory: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        
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
