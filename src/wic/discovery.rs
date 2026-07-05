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

use super::imports::get_wic_factory;
use super::imports::*;
use windows::Win32::Graphics::Imaging::*;
use windows::core::*;

use super::com::ComGuard;
pub fn spawn_wic_discovery() {
    thread::spawn(|| {
        if let Err(e) = discover_wic_codecs() {
            log::error!("WIC codec discovery failed: {:?}", e);
        }
    });
}

fn discover_wic_codecs() -> windows::core::Result<()> {
    let _com = ComGuard::new()?;
    unsafe {
        let factory = get_wic_factory()?;

        let enumerator = factory.CreateComponentEnumerator(
            WICDecoder.0 as u32,
            WICComponentEnumerateDefault.0 as u32,
        )?;

        let mut components = [None; 1];
        let mut _fetched = 0;
        let mut new_codecs = 0;

        loop {
            let hr = enumerator.Next(&mut components, Some(&mut _fetched));
            if hr.is_ok() && _fetched > 0 {
                if let Some(unknown) = components[0].take()
                    && let Ok(codec_info) = unknown.cast::<IWICBitmapCodecInfo>()
                {
                    let mut ext_buf = [0u16; 512];
                    let mut ext_len = 0;
                    if codec_info
                        .GetFileExtensions(&mut ext_buf, &mut ext_len)
                        .is_ok()
                        && ext_len > 1
                    {
                        let extensions_str =
                            String::from_utf16_lossy(&ext_buf[..ext_len as usize - 1]);

                        let mut name_buf = [0u16; 512];
                        let mut name_len = 0;
                        let friendly_name = if codec_info
                            .GetFriendlyName(&mut name_buf, &mut name_len)
                            .is_ok()
                            && name_len > 1
                        {
                            String::from_utf16_lossy(&name_buf[..name_len as usize - 1])
                        } else {
                            "Unknown WIC Codec".to_string()
                        };

                        let group =
                            if friendly_name.contains("RAW") || friendly_name.contains("Camera") {
                                FormatGroup::WicRaw
                            } else if friendly_name.contains("Microsoft")
                                || friendly_name.contains("Windows")
                            {
                                FormatGroup::WicSystem
                            } else {
                                FormatGroup::Others
                            };

                        let clsid = codec_info.GetCLSID()?;
                        let mut clsid_bytes = [0u8; 16];
                        std::ptr::copy_nonoverlapping(
                            &clsid as *const GUID as *const u8,
                            clsid_bytes.as_mut_ptr(),
                            16,
                        );

                        let mut reg = get_registry().write();
                        let mut added_for_codec = false;
                        for ext in extensions_str.split([',', ';']) {
                            let normalized_ext = ext
                                .trim()
                                .trim_start_matches('*')
                                .trim_start_matches('.')
                                .to_lowercase();
                            if !normalized_ext.is_empty()
                                && !reg.extensions.contains(&normalized_ext)
                            {
                                reg.add_format(ImageFormat {
                                    extension: normalized_ext,
                                    group,
                                    description: friendly_name.clone(),
                                    wic_clsid: Some(clsid_bytes),
                                });
                                added_for_codec = true;
                            }
                        }
                        if added_for_codec {
                            new_codecs += 1;
                        }
                    }
                }
            } else {
                break;
            }
        }
        log::info!(
            "WIC discovery finished: identified {} additional system codecs.",
            new_codecs
        );
    }

    get_registry().write().discovery_finished = true;
    Ok(())
}
