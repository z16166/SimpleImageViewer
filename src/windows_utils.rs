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

use winreg::enums::*;
use winreg::RegKey;

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
            let _: () = icon_key.set_value("", &format!("\"{}\",0", exe_path)).unwrap_or(());
        }
        if let Ok((cmd_key, _)) = prog_key.create_subkey(r"shell\open\command") {
            let _: () = cmd_key.set_value("", &format!("\"{}\" \"%1\"", exe_path)).unwrap_or(());
        }
    }

    // 2. Application Capabilities
    if let Ok((cap_key, _)) = hkcu.create_subkey(CAP_PATH) {
        let _: () = cap_key.set_value("ApplicationName", &FRIENDLY_NAME).unwrap_or(());
        let _: () = cap_key.set_value("ApplicationDescription", &"A high-performance image viewer.").unwrap_or(());
        if let Ok((assoc_key, _)) = cap_key.create_subkey("FileAssociations") {
            for ext in extensions {
                let dot_ext = if ext.starts_with('.') { ext.to_string() } else { format!(".{}", ext) };
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
        let _: () = leg_key.set_value("FriendlyAppName", &FRIENDLY_NAME).unwrap_or(());
        if let Ok((cmd_key, _)) = leg_key.create_subkey(r"shell\open\command") {
            let _: () = cmd_key.set_value("", &format!("\"{}\" \"%1\"", exe_path)).unwrap_or(());
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
    if let Ok(reg_apps) = hkcu.open_subkey_with_flags(r"Software\RegisteredApplications", KEY_WRITE) {
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
