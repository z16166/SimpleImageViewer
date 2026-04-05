#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod audio;
mod ipc;
mod loader;
mod scanner;
mod settings;

#[cfg(target_os = "windows")]
mod windows_utils {
    use std::env;
    use std::thread;
    use winreg::enums::*;
    use winreg::RegKey;

    /// Perform a deep registration of the application in the Windows Registry (HKCU).
    /// This ensures the app appears in "Recommended Apps", the "Open With" list,
    /// and the "Default Programs" system settings.
    pub fn ensure_windows_registration() {
        thread::spawn(|| {
            let exe_path = match env::current_exe() {
                Ok(p) => p.to_string_lossy().to_string(),
                Err(_) => return,
            };

            let hkcu = RegKey::predef(HKEY_CURRENT_USER);
            let app_id = "SimpleImageViewer.Viewer";
            let friendly_name = "Simple Image Viewer";
            
            // 1. Register the ProgID (The formal application handler)
            let prog_id_path = format!(r"Software\Classes\{}", app_id);
            if let Ok((prog_key, _)) = hkcu.create_subkey(&prog_id_path) {
                let _: () = prog_key.set_value("", &friendly_name).unwrap_or(());
                
                // Icon (use the EXE's embedded icon)
                if let Ok((icon_key, _)) = prog_key.create_subkey("DefaultIcon") {
                    let icon_path = format!("\"{}\",0", exe_path);
                    let _: () = icon_key.set_value("", &icon_path).unwrap_or(());
                }

                // Shell Open Command
                if let Ok((cmd_key, _)) = prog_key.create_subkey(r"shell\open\command") {
                    let desired_cmd = format!("\"{}\" \"%1\"", exe_path);
                    let _: () = cmd_key.set_value("", &desired_cmd).unwrap_or(());
                }
            }

            // 2. Register Application Capabilities (For "Recommended Apps" and Default Programs)
            let cap_path = r"Software\SimpleImageViewer\Capabilities";
            if let Ok((cap_key, _)) = hkcu.create_subkey(cap_path) {
                let _: () = cap_key.set_value("ApplicationName", &friendly_name).unwrap_or(());
                let _: () = cap_key.set_value("ApplicationDescription", &"A high-performance image viewer.").unwrap_or(());
                
                if let Ok((assoc_key, _)) = cap_key.create_subkey("FileAssociations") {
                    for ext in crate::scanner::SUPPORTED_EXTENSIONS {
                        let dot_ext = format!(".{}", ext);
                        let _: () = assoc_key.set_value(&dot_ext, &app_id).unwrap_or(());
                    }
                }
            }

            // Register the capabilities path in RegisteredApplications
            if let Ok((reg_apps, _)) = hkcu.create_subkey(r"Software\RegisteredApplications") {
                let _: () = reg_apps.set_value(friendly_name, &cap_path).unwrap_or(());
            }

            // 3. Inject ProgID into each supported extension's OpenWithProgids
            // This is what puts us in the "Recommended" section of the Open With menu.
            for ext in crate::scanner::SUPPORTED_EXTENSIONS {
                let progid_list_path = format!(r"Software\Classes\.{}\OpenWithProgids", ext);
                if let Ok((list_key, _)) = hkcu.create_subkey(&progid_list_path) {
                    // Setting a value with an empty string name and empty string value 
                    // is how you add a ProgID to the list in Windows.
                    let _: () = list_key.set_value(app_id, &"").unwrap_or(());
                }
            }

            // 4. Legacy "Applications" key (fallback/redundancy)
            let legacy_path = r"Software\Classes\Applications\SimpleImageViewer.exe";
            if let Ok((leg_key, _)) = hkcu.create_subkey(legacy_path) {
                let _: () = leg_key.set_value("FriendlyAppName", &friendly_name).unwrap_or(());
                if let Ok((cmd_key, _)) = leg_key.create_subkey(r"shell\open\command") {
                    let desired_cmd = format!("\"{}\" \"%1\"", exe_path);
                    let _: () = cmd_key.set_value("", &desired_cmd).unwrap_or(());
                }
            }
        });
    }
}

/// Load the application icon from the embedded PNG bytes.
/// Returns an `egui::IconData` at 256×256 RGBA for the taskbar/titlebar icon.
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

    // Perform deep Windows registration in a background thread
    #[cfg(target_os = "windows")]
    windows_utils::ensure_windows_registration();

    let mut settings = settings::Settings::load();
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
        .with_title("Simple Image Viewer")
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
        "Simple Image Viewer",
        native_options,
        Box::new(move |cc| Ok(Box::new(app::ImageViewerApp::new(cc, settings, initial_image, ipc_rx)) as Box<dyn eframe::App>)),
    )
}
