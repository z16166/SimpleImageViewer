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

use winreg::RegKey;
use winreg::enums::*;

/// Resource id assigned by `winresource::WindowsResource::set_icon` in `build.rs`.
const PE_APP_ICON_ID: u16 = 1;

/// Application icon loaded from the PE resource table, with GDI bitmap handles owned by
/// [`GetIconInfo`](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-geticoninfo).
struct OwnedAppIcon {
    handle: winapi::shared::windef::HICON,
    color_bitmap: winapi::shared::windef::HBITMAP,
    mask_bitmap: winapi::shared::windef::HBITMAP,
}

impl OwnedAppIcon {
    unsafe fn load(resource_id: u16) -> Option<Self> {
        use winapi::shared::windef::HICON;
        use winapi::um::libloaderapi::GetModuleHandleW;
        use winapi::um::winuser::{
            DestroyIcon, GetIconInfo, LoadImageW, MAKEINTRESOURCEW, ICONINFO, IMAGE_ICON,
            LR_DEFAULTSIZE,
        };

        unsafe {
            let instance = GetModuleHandleW(std::ptr::null());
            if instance.is_null() {
                return None;
            }

            let mut handle: HICON = std::ptr::null_mut();
            for size in [256i32, 128, 64, 48, 32, 16] {
                let loaded = LoadImageW(
                    instance,
                    MAKEINTRESOURCEW(resource_id),
                    IMAGE_ICON,
                    size,
                    size,
                    0,
                );
                if !loaded.is_null() {
                    handle = loaded as HICON;
                    break;
                }
            }
            if handle.is_null() {
                let loaded = LoadImageW(
                    instance,
                    MAKEINTRESOURCEW(resource_id),
                    IMAGE_ICON,
                    0,
                    0,
                    LR_DEFAULTSIZE,
                );
                if loaded.is_null() {
                    return None;
                }
                handle = loaded as HICON;
            }

            let mut icon_info: ICONINFO = std::mem::zeroed();
            if GetIconInfo(handle, &mut icon_info) == 0 {
                DestroyIcon(handle);
                return None;
            }

            Some(Self {
                handle,
                color_bitmap: icon_info.hbmColor,
                mask_bitmap: icon_info.hbmMask,
            })
        }
    }

    unsafe fn color_dimensions(&self) -> Option<(u32, u32)> {
        use winapi::shared::windef::HGDIOBJ;
        use winapi::um::wingdi::{BITMAP, GetObjectW};

        unsafe {
            let mut bitmap: BITMAP = std::mem::zeroed();
            if GetObjectW(
                self.color_bitmap as HGDIOBJ,
                std::mem::size_of::<BITMAP>() as i32,
                &mut bitmap as *mut BITMAP as *mut _,
            ) == 0
            {
                return None;
            }

            let width = bitmap.bmWidth.unsigned_abs();
            let height = bitmap.bmHeight.unsigned_abs();
            (width > 0 && height > 0).then_some((width, height))
        }
    }
}

impl Drop for OwnedAppIcon {
    fn drop(&mut self) {
        use winapi::shared::windef::HGDIOBJ;
        use winapi::um::wingdi::DeleteObject;
        use winapi::um::winuser::DestroyIcon;

        unsafe {
            if !self.color_bitmap.is_null() {
                DeleteObject(self.color_bitmap as HGDIOBJ);
            }
            if !self.mask_bitmap.is_null() {
                DeleteObject(self.mask_bitmap as HGDIOBJ);
            }
            if !self.handle.is_null() {
                DestroyIcon(self.handle);
            }
        }
    }
}

struct ScreenDc(winapi::shared::windef::HDC);

impl ScreenDc {
    unsafe fn acquire() -> Option<Self> {
        use winapi::um::winuser::GetDC;

        unsafe {
            let dc = GetDC(std::ptr::null_mut());
            (!dc.is_null()).then_some(Self(dc))
        }
    }
}

impl Drop for ScreenDc {
    fn drop(&mut self) {
        use winapi::um::winuser::ReleaseDC;

        unsafe {
            if !self.0.is_null() {
                ReleaseDC(std::ptr::null_mut(), self.0);
            }
        }
    }
}

struct MemDc(winapi::shared::windef::HDC);

impl MemDc {
    unsafe fn compatible_with(screen: winapi::shared::windef::HDC) -> Option<Self> {
        use winapi::um::wingdi::CreateCompatibleDC;

        unsafe {
            let dc = CreateCompatibleDC(screen);
            (!dc.is_null()).then_some(Self(dc))
        }
    }
}

impl Drop for MemDc {
    fn drop(&mut self) {
        use winapi::um::wingdi::DeleteDC;

        unsafe {
            if !self.0.is_null() {
                DeleteDC(self.0);
            }
        }
    }
}

struct DcBitmapSelection {
    dc: winapi::shared::windef::HDC,
    previous: winapi::shared::windef::HGDIOBJ,
}

impl DcBitmapSelection {
    unsafe fn select(
        dc: winapi::shared::windef::HDC,
        bitmap: winapi::shared::windef::HBITMAP,
    ) -> Option<Self> {
        use winapi::shared::windef::HGDIOBJ;
        use winapi::um::wingdi::{SelectObject, HGDI_ERROR};

        unsafe {
            let previous = SelectObject(dc, bitmap as HGDIOBJ);
            (!previous.is_null() && previous != HGDI_ERROR).then_some(Self { dc, previous })
        }
    }
}

impl Drop for DcBitmapSelection {
    fn drop(&mut self) {
        use winapi::um::wingdi::SelectObject;

        unsafe {
            SelectObject(self.dc, self.previous);
        }
    }
}

/// Decode the application icon embedded in the PE (same `.ico` as the taskbar) into RGBA8.
pub fn load_icon_rgba_from_pe() -> Option<(Vec<u8>, u32, u32)> {
    use winapi::um::wingdi::{
        BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, GetDIBits,
    };

    unsafe {
        let icon = OwnedAppIcon::load(PE_APP_ICON_ID)?;
        let (width, height) = icon.color_dimensions()?;

        let screen_dc = ScreenDc::acquire()?;
        let mem_dc = MemDc::compatible_with(screen_dc.0)?;
        let _bitmap_selected = DcBitmapSelection::select(mem_dc.0, icon.color_bitmap)?;

        let mut bmi: BITMAPINFO = std::mem::zeroed();
        bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = width as i32;
        bmi.bmiHeader.biHeight = -(height as i32);
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB;

        let byte_len = (width as usize)
            .checked_mul(height as usize)?
            .checked_mul(4)?;
        let mut bgra = vec![0u8; byte_len];
        if GetDIBits(
            mem_dc.0,
            icon.color_bitmap,
            0,
            height,
            bgra.as_mut_ptr() as *mut _,
            &mut bmi,
            DIB_RGB_COLORS,
        ) == 0
        {
            return None;
        }

        for px in bgra.chunks_exact_mut(4) {
            px.swap(0, 2);
        }

        Some((bgra, width, height))
    }
}

const APP_ID: &str = "SimpleImageViewer.Viewer";
const FRIENDLY_NAME: &str = "Simple Image Viewer";
const CAP_PATH: &str = r"Software\SimpleImageViewer\Capabilities";
const LEGACY_PATH: &str = r"Software\Classes\Applications\SimpleImageViewer.exe";

/// Register the application as a handler for the given image extensions.
/// This writes ProgID, Application Capabilities, RegisteredApplications,
/// OpenWithProgids entries, and the legacy Applications key.
pub fn register_file_associations(extensions: &[&str]) {
    let exe_path = match std::env::current_exe() {
        Ok(p) => p.to_string_lossy().to_string(),
        Err(_) => return,
    };

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);

    // 1. ProgID
    let prog_id_path = format!(r"Software\Classes\{}", APP_ID);
    if let Ok((prog_key, _)) = hkcu.create_subkey(&prog_id_path) {
        let _: () = prog_key.set_value("", &FRIENDLY_NAME).unwrap_or(());
        if let Ok((icon_key, _)) = prog_key.create_subkey("DefaultIcon") {
            let _: () = icon_key
                .set_value("", &format!("\"{}\",0", exe_path))
                .unwrap_or(());
        }
        if let Ok((cmd_key, _)) = prog_key.create_subkey(r"shell\open\command") {
            let _: () = cmd_key
                .set_value("", &format!("\"{}\" \"%1\"", exe_path))
                .unwrap_or(());
        }
    }

    // 2. Application Capabilities
    if let Ok((cap_key, _)) = hkcu.create_subkey(CAP_PATH) {
        let _: () = cap_key
            .set_value("ApplicationName", &FRIENDLY_NAME)
            .unwrap_or(());
        let _: () = cap_key
            .set_value(
                "ApplicationDescription",
                &"A high-performance image viewer.",
            )
            .unwrap_or(());
        if let Ok((assoc_key, _)) = cap_key.create_subkey("FileAssociations") {
            for ext in extensions {
                let dot_ext = if ext.starts_with('.') {
                    ext.to_string()
                } else {
                    format!(".{}", ext)
                };
                let _: () = assoc_key.set_value(&dot_ext, &APP_ID).unwrap_or(());
            }
        }
    }

    // 3. RegisteredApplications
    if let Ok((reg_apps, _)) = hkcu.create_subkey(r"Software\RegisteredApplications") {
        let _: () = reg_apps.set_value(FRIENDLY_NAME, &CAP_PATH).unwrap_or(());
    }

    // 4. OpenWithProgids for each extension
    for ext in extensions {
        let ext_clean = ext.trim_start_matches('.');
        let progid_list_path = format!(r"Software\Classes\.{}\OpenWithProgids", ext_clean);
        if let Ok((list_key, _)) = hkcu.create_subkey(&progid_list_path) {
            let _: () = list_key.set_value(APP_ID, &"").unwrap_or(());
        }
    }

    // 5. Legacy Applications key
    if let Ok((leg_key, _)) = hkcu.create_subkey(LEGACY_PATH) {
        let _: () = leg_key
            .set_value("FriendlyAppName", &FRIENDLY_NAME)
            .unwrap_or(());
        if let Ok((cmd_key, _)) = leg_key.create_subkey(r"shell\open\command") {
            let _: () = cmd_key
                .set_value("", &format!("\"{}\" \"%1\"", exe_path))
                .unwrap_or(());
        }
    }
}

/// Remove all registry entries created by `register_file_associations`.
/// Only removes our own entries — does not touch other applications.
pub fn unregister_file_associations() {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);

    // 1. Remove ProgID
    let _ = hkcu.delete_subkey_all(format!(r"Software\Classes\{}", APP_ID));

    // 2. Remove Application Capabilities (entire SimpleImageViewer tree)
    let _ = hkcu.delete_subkey_all(r"Software\SimpleImageViewer");

    // 3. Remove from RegisteredApplications
    if let Ok(reg_apps) = hkcu.open_subkey_with_flags(r"Software\RegisteredApplications", KEY_WRITE)
    {
        let _ = reg_apps.delete_value(FRIENDLY_NAME);
    }

    // 4. Remove our ProgID from each extension's OpenWithProgids
    if let Ok(reg) = crate::formats::get_registry().read() {
        for fmt in &reg.formats {
            let ext = &fmt.extension;
            let progid_list_path = format!(r"Software\Classes\.{}\OpenWithProgids", ext);
            if let Ok(list_key) = hkcu.open_subkey_with_flags(&progid_list_path, KEY_WRITE) {
                let _ = list_key.delete_value(APP_ID);
            }
        }
    }

    // 5. Remove legacy Applications key
    let _ = hkcu.delete_subkey_all(LEGACY_PATH);
}
