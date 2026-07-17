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

use std::path::{Path, PathBuf};

const SCR_FILE_NAME: &str = "SimpleImageViewer.scr";

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

/// Origin (top-left) of the Windows primary monitor in screen coordinates.
pub fn primary_monitor_origin() -> Option<eframe::egui::Pos2> {
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
        Some(eframe::egui::pos2(
            info.rcMonitor.left as f32,
            info.rcMonitor.top as f32,
        ))
    }
}
