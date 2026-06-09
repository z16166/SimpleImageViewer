use super::logging::shutdown_logger;

#[cfg(target_os = "windows")]
fn show_crash_dialog(title: &str, message: &str) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MB_TASKMODAL, MB_TOPMOST, MessageBoxW,
    };
    use windows::core::HSTRING;

    let title = HSTRING::from(title);
    let message = HSTRING::from(message);
    // The panic hook can run while the egui window is unresponsive or tearing
    // down, so use a task-modal topmost box without relying on a parent HWND.
    let _ = unsafe {
        MessageBoxW(
            HWND::default(),
            &message,
            &title,
            MB_OK | MB_ICONERROR | MB_TOPMOST | MB_SETFOREGROUND | MB_TASKMODAL,
        )
    };
}

#[cfg(not(target_os = "windows"))]
fn show_crash_dialog(title: &str, message: &str) {
    rfd::MessageDialog::new()
        .set_title(title)
        .set_description(message)
        .set_level(rfd::MessageLevel::Error)
        .show();
}

/// Set up a global panic hook to capture and report crashes across all threads.
/// Decoder paths that use `catch_exr_panic` increment a thread-local so this hook skips
/// dialog/exit — otherwise `process::exit(1)` would run before `catch_unwind` can handle the panic.
pub fn setup_panic_hook() {
    std::panic::set_hook(Box::new(|panic_info| {
        if crate::hdr::exr_tiled::is_exr_panic_hook_suppressed() {
            return;
        }

        let location = panic_info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "unknown location".to_string());
        let payload = panic_info.payload();
        let message = if let Some(s) = payload.downcast_ref::<&str>() {
            s.to_string()
        } else if let Some(s) = payload.downcast_ref::<String>() {
            s.clone()
        } else {
            "no message".to_string()
        };

        let app_ver = env!("CARGO_PKG_VERSION");

        // Capture a full backtrace
        let backtrace = std::backtrace::Backtrace::force_capture();

        // Re-detect basic env info for the report
        let os_name = sysinfo::System::name().unwrap_or_else(|| "Unknown OS".to_string());
        let os_ver = sysinfo::System::os_version().unwrap_or_else(|| "Unknown Version".to_string());

        let report = format!(
            "--- Simple Image Viewer Crash Report ---\n\
            Version: v{}\n\
            OS: {} [{}]\n\
            Location: {}\n\
            Error: {}\n\n\
            STACK BACKTRACE:\n\
            {:?}\n\
            ----------------------------------------\n",
            app_ver, os_name, os_ver, location, message, backtrace
        );

        // 1. Log to stderr (for console users) and file system
        eprintln!("{}", report);
        log::error!("{}", report);

        let log_path = crate::settings::settings_path()
            .with_file_name(crate::constants::CRASH_REPORT_FILENAME);
        let _ = std::fs::write(&log_path, &report);

        // 2. Try to copy to clipboard
        use clipboard_rs::{Clipboard, ClipboardContext};
        if let Ok(ctx) = ClipboardContext::new() {
            let _ = ctx.set_text(report.clone());
        }

        // 3. Show localized error dialog (if i18n is available, else fallback to English)
        let mut title = rust_i18n::t!("dialog.crash_title").to_string();
        if title.contains("dialog.crash_title") {
            title = crate::constants::CRASH_DIALOG_FALLBACK_TITLE.to_string();
        }

        let mut msg = format!(
            "{}\n\n{}\n\n{}",
            rust_i18n::t!("dialog.crash_msg"),
            format!("Location: {}", location),
            format!("Error: {}", message)
        );
        if msg.contains("dialog.crash_msg") {
            msg = format!(
                "{}\n\nLocation: {}\nError: {}\n\nDiagnostic info copied to clipboard.",
                crate::constants::CRASH_DIALOG_FALLBACK_MSG,
                location,
                message
            );
        }

        show_crash_dialog(&title, &msg);

        // Critical: After showing the crash dialog, the application must terminate.
        // Otherwise, the window may hang or enter an unstable state.
        shutdown_logger();
        std::process::exit(1);
    }));
}
