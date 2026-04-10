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

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

rust_i18n::i18n!("locales");

mod app;
mod audio;
mod ipc;
mod loader;
mod psb_reader;
mod scanner;
mod settings;
pub mod theme;
pub mod print;
mod tile_cache;

#[cfg(target_os = "windows")]
pub mod windows_utils {
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
        for ext in crate::scanner::SUPPORTED_EXTENSIONS {
            let progid_list_path = format!(r"Software\Classes\.{}\OpenWithProgids", ext);
            if let Ok(list_key) = hkcu.open_subkey_with_flags(&progid_list_path, KEY_WRITE) {
                let _ = list_key.delete_value(APP_ID);
            }
        }

        // 5. Remove legacy Applications key
        let _ = hkcu.delete_subkey_all(LEGACY_PATH);
    }
}

/// Load the application icon from the embedded PNG bytes.
fn load_icon() -> egui::IconData {
    let bytes = include_bytes!("../assets/icon.png");
    match image::load_from_memory(bytes) {
        Ok(img) => {
            use image::imageops::FilterType;
            use image::GenericImageView;
            let img = img.resize_exact(256, 256, FilterType::Lanczos3);
            let (w, h) = img.dimensions();
            egui::IconData {
                rgba: img.to_rgba8().into_raw(),
                width: w,
                height: h,
            }
        }
        Err(e) => {
            eprintln!("Failed to load app icon: {e}");
            egui::IconData::default()
        }
    }
}

fn main() -> eframe::Result {
    env_logger::init();

    let mut settings = settings::Settings::load();

    // Initialize locale — detect from OS if not yet configured
    if settings.language.is_empty() {
        settings.language = settings::detect_system_language();
    }
    rust_i18n::set_locale(&settings.language);

    let mut initial_image = None;

    if let Some(arg) = std::env::args_os().nth(1) {
        let pic_path = std::path::PathBuf::from(arg);
        if pic_path.is_file() {
            if let Some(parent) = pic_path.parent() {
                settings.last_image_dir = Some(parent.to_path_buf());
            }
            settings.auto_switch = false;
            settings.recursive = false;
            initial_image = Some(pic_path);
        }
    }

    let (ipc_tx, ipc_rx) = crossbeam_channel::unbounded();
    let no_recursive = initial_image.is_some();
    if ipc::setup_or_forward_args(ipc_tx, initial_image.as_ref(), no_recursive) {
        std::process::exit(0);
    }

    let fullscreen = settings.fullscreen;

    let viewport = egui::ViewportBuilder::default()
        .with_title(rust_i18n::t!("app.title").to_string())
        .with_inner_size([1280.0, 800.0])
        .with_min_inner_size([400.0, 300.0])
        .with_decorations(true)
        .with_fullscreen(fullscreen)
        .with_icon(load_icon());

    let native_options = eframe::NativeOptions {
        viewport,
        centered: true,
        ..Default::default()
    };

    eframe::run_native(
        "Simple Image Viewer", // Eframe specific, can stay in English as a unique identifier for egui
        native_options,
        Box::new(move |cc| Ok(Box::new(app::ImageViewerApp::new(cc, settings, initial_image, ipc_rx)) as Box<dyn eframe::App>)),
    )
}
