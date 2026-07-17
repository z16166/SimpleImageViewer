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

//! Install / activate Simple Image Viewer as a Windows system screensaver.
//!
//! Also provides multi-monitor covering helpers for `/s` run hosts: winit
//! borderless fullscreen only targets a single monitor, so display policy
//! `All` must size the window to the virtual screen explicitly.

use std::path::{Path, PathBuf};

const SCR_FILE_NAME: &str = "SimpleImageViewer.scr";

/// Physical-pixel screen rectangle in Windows virtual-desktop coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl ScreenRect {
    #[inline]
    pub fn is_valid(self) -> bool {
        self.width > 0 && self.height > 0
    }
}

fn current_exe_path() -> Result<PathBuf, String> {
    std::env::current_exe().map_err(|e| format!("current_exe: {e}"))
}

fn system_scr_path() -> Result<PathBuf, String> {
    let base = std::env::var_os("WINDIR")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    Ok(base.join("System32").join(SCR_FILE_NAME))
}

fn user_scr_path() -> Result<PathBuf, String> {
    let local = dirs::data_local_dir().ok_or_else(|| "local data dir not found".to_string())?;
    let dir = local.join("SimpleImageViewer");
    std::fs::create_dir_all(&dir).map_err(|e| format!("create install dir: {e}"))?;
    Ok(dir.join(SCR_FILE_NAME))
}

fn copy_exe_as_scr(dest: &Path) -> Result<(), String> {
    let src = current_exe_path()?;
    if dest.exists() {
        let _ = std::fs::remove_file(dest);
    }
    std::fs::copy(&src, dest)
        .map_err(|e| format!("copy {} -> {}: {e}", src.display(), dest.display()))?;
    Ok(())
}

/// Copy the running executable as `SimpleImageViewer.scr` into a user-writable location.
pub fn install_system_screensaver() -> Result<String, String> {
    let dest = user_scr_path().or_else(|_| system_scr_path())?;
    copy_exe_as_scr(&dest)?;
    set_screensaver_registry(&dest)?;
    Ok(rust_i18n::t!("screensaver.install_ok", path = dest.display().to_string()).to_string())
}

/// Set the installed SCR as the active Windows screensaver (registry Desktop keys).
pub fn set_as_active_screensaver() -> Result<String, String> {
    let candidates = [
        user_scr_path().ok(),
        system_scr_path().ok(),
        current_exe_path().ok().map(|p| p.with_extension("scr")),
    ];
    let scr = candidates
        .into_iter()
        .flatten()
        .find(|p| p.is_file())
        .ok_or_else(|| rust_i18n::t!("screensaver.not_installed").to_string())?;
    set_screensaver_registry(&scr)?;
    Ok(rust_i18n::t!(
        "screensaver.set_active_ok",
        path = scr.display().to_string()
    )
    .to_string())
}

fn set_screensaver_registry(scr: &Path) -> Result<(), String> {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (desktop, _) = hkcu
        .create_subkey(r"Control Panel\Desktop")
        .map_err(|e| format!("open Desktop key: {e}"))?;
    let path_str = scr
        .to_str()
        .ok_or_else(|| "screensaver path is not valid UTF-8".to_string())?;
    desktop
        .set_value("SCRNSAVE.EXE", &path_str)
        .map_err(|e| format!("set SCRNSAVE.EXE: {e}"))?;
    let _ = desktop.set_value("ScreenSaveActive", &"1");
    Ok(())
}

/// Reparent the current process main window under a preview HWND (`/p`).
/// Best-effort: failures are logged by the caller.
pub fn try_embed_preview_window(child: isize, parent: isize) -> Result<(), String> {
    use winapi::shared::windef::{HWND, RECT};
    use winapi::um::winuser::{
        GWL_STYLE, GetClientRect, GetWindowLongW, MoveWindow, SWP_FRAMECHANGED, SWP_NOACTIVATE,
        SWP_NOZORDER, SetParent, SetWindowLongW, SetWindowPos, WS_CHILD, WS_POPUP, WS_VISIBLE,
    };

    if child == 0 || parent == 0 {
        return Err("invalid hwnd".to_string());
    }
    let child_hwnd = child as HWND;
    let parent_hwnd = parent as HWND;

    unsafe {
        let mut style = GetWindowLongW(child_hwnd, GWL_STYLE);
        style &= !(WS_POPUP as i32);
        style |= WS_CHILD as i32 | WS_VISIBLE as i32;
        SetWindowLongW(child_hwnd, GWL_STYLE, style);

        if SetParent(child_hwnd, parent_hwnd).is_null() {
            return Err("SetParent failed".to_string());
        }

        let mut rect = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        if GetClientRect(parent_hwnd, &mut rect) == 0 {
            return Err("GetClientRect failed".to_string());
        }
        let w = (rect.right - rect.left).max(1);
        let h = (rect.bottom - rect.top).max(1);
        let _ = MoveWindow(child_hwnd, 0, 0, w, h, 1);
        let _ = SetWindowPos(
            child_hwnd,
            std::ptr::null_mut(),
            0,
            0,
            w,
            h,
            SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
    }
    Ok(())
}

/// Bounding rectangle of the Windows virtual screen (all monitors).
pub fn virtual_screen_rect() -> Option<ScreenRect> {
    use winapi::um::winuser::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN,
    };

    unsafe {
        let x = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let y = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let width = GetSystemMetrics(SM_CXVIRTUALSCREEN);
        let height = GetSystemMetrics(SM_CYVIRTUALSCREEN);
        if width <= 0 || height <= 0 {
            return None;
        }
        Some(ScreenRect {
            x,
            y,
            width: width as u32,
            height: height as u32,
        })
    }
}

/// Full rectangle of the Windows primary monitor in virtual-desktop coordinates.
pub fn primary_monitor_rect() -> Option<ScreenRect> {
    use winapi::shared::windef::RECT;
    use winapi::um::winuser::{
        GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromWindow,
    };

    unsafe {
        // NULL hwnd + DEFAULTTOPRIMARY resolves the primary monitor regardless of layout.
        let mon = MonitorFromWindow(std::ptr::null_mut(), MONITOR_DEFAULTTOPRIMARY);
        if mon.is_null() {
            return None;
        }
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            rcMonitor: RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
            rcWork: RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
            dwFlags: 0,
        };
        if GetMonitorInfoW(mon, &mut info) == 0 {
            return None;
        }
        let width = info.rcMonitor.right.saturating_sub(info.rcMonitor.left);
        let height = info.rcMonitor.bottom.saturating_sub(info.rcMonitor.top);
        if width <= 0 || height <= 0 {
            return None;
        }
        Some(ScreenRect {
            x: info.rcMonitor.left,
            y: info.rcMonitor.top,
            width: width as u32,
            height: height as u32,
        })
    }
}

/// Cover `hwnd` with a borderless topmost window over `rect` (physical pixels).
///
/// Used by screensaver `/s` instead of winit `Fullscreen::Borderless(None)`,
/// which only covers the current single monitor and cannot span dual displays.
pub fn cover_window_to_rect(hwnd: isize, rect: ScreenRect) -> Result<(), String> {
    use winapi::shared::windef::HWND;
    use winapi::um::winuser::{
        GWL_EXSTYLE, GWL_STYLE, GetWindowLongW, HWND_TOPMOST, SWP_FRAMECHANGED, SWP_SHOWWINDOW,
        SetWindowLongW, SetWindowPos, WS_BORDER, WS_CAPTION, WS_DLGFRAME, WS_EX_APPWINDOW,
        WS_EX_TOOLWINDOW, WS_EX_WINDOWEDGE, WS_MAXIMIZEBOX, WS_MINIMIZEBOX, WS_POPUP, WS_SIZEBOX,
        WS_SYSMENU, WS_THICKFRAME, WS_VISIBLE,
    };

    if hwnd == 0 {
        return Err("invalid hwnd".to_string());
    }
    if !rect.is_valid() {
        return Err(format!(
            "invalid cover rect {}x{} at ({}, {})",
            rect.width, rect.height, rect.x, rect.y
        ));
    }

    let hwnd = hwnd as HWND;
    // Strip all chrome that would leave a framed window floating across monitors.
    let chrome = (WS_CAPTION
        | WS_THICKFRAME
        | WS_MINIMIZEBOX
        | WS_MAXIMIZEBOX
        | WS_SYSMENU
        | WS_BORDER
        | WS_DLGFRAME
        | WS_SIZEBOX) as i32;

    unsafe {
        let mut style = GetWindowLongW(hwnd, GWL_STYLE);
        style &= !chrome;
        style |= WS_POPUP as i32 | WS_VISIBLE as i32;
        SetWindowLongW(hwnd, GWL_STYLE, style);

        let mut ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE);
        // Prefer toolwindow so the saver is less visible on the taskbar.
        ex_style |= WS_EX_TOOLWINDOW as i32;
        ex_style &= !((WS_EX_APPWINDOW | WS_EX_WINDOWEDGE) as i32);
        SetWindowLongW(hwnd, GWL_EXSTYLE, ex_style);

        let ok = SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            rect.x,
            rect.y,
            rect.width as i32,
            rect.height as i32,
            SWP_SHOWWINDOW | SWP_FRAMECHANGED,
        );
        if ok == 0 {
            return Err("SetWindowPos failed".to_string());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtual_screen_rect_is_positive() {
        let rect = virtual_screen_rect().expect("virtual screen metrics");
        assert!(rect.is_valid(), "{rect:?}");
    }

    #[test]
    fn primary_monitor_rect_is_positive() {
        let rect = primary_monitor_rect().expect("primary monitor");
        assert!(rect.is_valid(), "{rect:?}");
    }

    #[test]
    fn cover_rejects_invalid_inputs() {
        assert!(
            cover_window_to_rect(
                0,
                ScreenRect {
                    x: 0,
                    y: 0,
                    width: 100,
                    height: 100
                }
            )
            .is_err()
        );
        assert!(
            cover_window_to_rect(
                1,
                ScreenRect {
                    x: 0,
                    y: 0,
                    width: 0,
                    height: 100
                }
            )
            .is_err()
        );
    }
}
