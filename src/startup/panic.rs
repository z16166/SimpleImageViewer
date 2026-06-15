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

use super::logging::shutdown_logger;

#[cfg(target_os = "windows")]
fn show_crash_dialog(title: &str, message: &str) {
    use windows::Win32::UI::WindowsAndMessaging::{
        MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MB_SYSTEMMODAL, MB_TOPMOST, MessageBoxW,
    };
    use windows::core::HSTRING;

    // Never call MessageBoxW on the thread that is inside winit's WndProc. During
    // cross-monitor drags the panic hook runs nested in `DialogBox2`/`DispatchMessage`
    // while the main frame straddles two displays; an owned/task-modal box tied to that
    // HWND is often created but not painted (beep-only modal). A system-modal box on a
    // dedicated thread avoids reentrancy and lands on the foreground desktop.
    let title = title.to_string();
    let message = message.to_string();
    let show_inline = |title: &str, message: &str| {
        let title = HSTRING::from(title);
        let message = HSTRING::from(message);
        let flags = MB_OK | MB_ICONERROR | MB_SETFOREGROUND | MB_TOPMOST;
        unsafe { MessageBoxW(None, &message, &title, flags) };
    };
    let title_for_thread = title.clone();
    let message_for_thread = message.clone();
    match std::thread::Builder::new()
        .name("crash-dialog".into())
        .spawn(move || {
            let title = HSTRING::from(title_for_thread);
            let message = HSTRING::from(message_for_thread);
            let flags = MB_OK | MB_ICONERROR | MB_SETFOREGROUND | MB_SYSTEMMODAL | MB_TOPMOST;
            let _ = unsafe { MessageBoxW(None, &message, &title, flags) };
        }) {
        Ok(handle) => {
            if handle.join().is_err() {
                show_inline(&title, &message);
            }
        }
        Err(_) => show_inline(&title, &message),
    }
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
