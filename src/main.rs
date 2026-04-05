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

    pub fn ensure_windows_registration() {
        thread::spawn(|| {
            let exe_path = match env::current_exe() {
                Ok(p) => p.to_string_lossy().to_string(),
                Err(_) => return,
            };

            let hkcu = RegKey::predef(HKEY_CURRENT_USER);
            let path = r"Software\Classes\Applications\SimpleImageViewer.exe";
            
            // 1. Create/Open the base application key
            let (key, _) = match hkcu.create_subkey(path) {
                Ok(res) => res,
                Err(_) => return,
            };

            // 2. Set FriendlyAppName (fallback display name in Open With)
            let _: () = key.set_value("FriendlyAppName", &"Simple Image Viewer").unwrap_or(());

            // 3. Set Open Command
            if let Ok((cmd_key, _)) = key.create_subkey(r"shell\open\command") {
                let current_cmd: String = cmd_key.get_value("").unwrap_or_default();
                let desired_cmd = format!("\"{}\" \"%1\"", exe_path);
                
                // Only write if path changed or is missing
                if current_cmd != desired_cmd {
                    let _: () = cmd_key.set_value("", &desired_cmd).unwrap_or(());
                }
            }

            // 4. Register Supported Types (to show up in 'Recommended' list)
            if let Ok((types_key, _)) = key.create_subkey("SupportedTypes") {
                let extensions = [
                    ".jpg", ".jpeg", ".png", ".gif", ".webp", ".apng", 
                    ".bmp", ".tiff", ".tga", ".ico", ".pnm", ".hdr",
                    ".avif", ".qoi", ".exr"
                ];
                for ext in extensions {
                    let _: () = types_key.set_value(ext, &"").unwrap_or(());
                }
            }
        });
    }
}

/// Load the application icon from the embedded JPEG bytes.
/// Returns an `egui::IconData` at 256×256 RGBA for the taskbar/titlebar icon.
fn load_icon() -> egui::IconData {
    // Embed the source image at compile time — works from any working directory.
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

    // Perform Windows-specific registration in a background thread
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
            // Opened via file association / Explorer double-click:
            // - Disable auto-advance so the user sees the specific image they clicked.
            // - Disable recursive scan to avoid scanning huge directory trees.
            // Both are persisted to disk so the next no-arg launch inherits these settings.
            settings.auto_switch = false;
            settings.recursive = false;
            initial_image = Some(pic_path);
        }
    }

    let (ipc_tx, ipc_rx) = crossbeam_channel::unbounded();
    // no_recursive=true when launched via CLI (double-click from Explorer):
    // prevents accidentally recursive-scanning huge directory trees.
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
