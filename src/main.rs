#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod audio;
mod loader;
mod scanner;
mod settings;

/// Load the application icon from the embedded JPEG bytes.
/// Returns an `egui::IconData` at 256×256 RGBA for the taskbar/titlebar icon.
fn load_icon() -> egui::IconData {
    // Embed the source image at compile time — works from any working directory.
    let bytes = include_bytes!("../assets/icon.jpg");
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

    let settings = settings::Settings::load();
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
        Box::new(|cc| Ok(Box::new(app::ImageViewerApp::new(cc, settings)))),
    )
}
