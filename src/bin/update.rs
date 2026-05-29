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
//
// This helper intentionally avoids egui/wgpu and uses only std plus Win32 APIs.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(target_os = "windows")]
const MAIN_PROCESS_WAIT_TIMEOUT_MS: u32 = 30_000;
#[cfg(target_os = "windows")]
const MAX_RENAME_RETRIES: u32 = 40;
#[cfg(target_os = "windows")]
const RETRY_SLEEP_MS: u64 = 250;

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("update helper is only supported on Windows");
}

#[cfg(target_os = "windows")]
fn main() {
    let parsed = parse_args(std::env::args().skip(1));
    let locale = parsed
        .as_ref()
        .ok()
        .map(|args| args.locale.clone())
        .unwrap_or_else(|| "en".to_string());
    let text = ui_text(&locale);

    if let Err(err) = parsed.and_then(|args| run(&args, &text, &locale)) {
        show_message(text.failed_title, &err);
        std::process::exit(1);
    }
}

#[cfg(target_os = "windows")]
#[derive(Debug)]
struct Args {
    pid: u32,
    old_exe: std::path::PathBuf,
    new_exe: std::path::PathBuf,
    backup_exe: std::path::PathBuf,
    log: std::path::PathBuf,
    success_marker: std::path::PathBuf,
    version: String,
    locale: String,
    restart: bool,
}

#[cfg(target_os = "windows")]
struct UiText {
    failed_title: &'static str,
    backup_old_exe: &'static str,
    copy_new_exe: &'static str,
    write_success_marker_failed: &'static str,
    restart_app_failed: &'static str,
}

#[cfg(target_os = "windows")]
fn ui_text(locale: &str) -> UiText {
    match locale {
        "zh-CN" => UiText {
            failed_title: "Simple Image Viewer 更新失败",
            backup_old_exe: "备份旧程序",
            copy_new_exe: "复制新程序",
            write_success_marker_failed: "写入更新成功标记失败",
            restart_app_failed: "重启程序失败",
        },
        "zh-HK" => UiText {
            failed_title: "Simple Image Viewer 更新失敗",
            backup_old_exe: "備份舊程式",
            copy_new_exe: "複製新程式",
            write_success_marker_failed: "寫入更新成功標記失敗",
            restart_app_failed: "重新啟動程式失敗",
        },
        "zh-TW" => UiText {
            failed_title: "Simple Image Viewer 更新失敗",
            backup_old_exe: "備份舊程式",
            copy_new_exe: "複製新程式",
            write_success_marker_failed: "寫入更新成功標記失敗",
            restart_app_failed: "重新啟動程式失敗",
        },
        _ => UiText {
            failed_title: "Simple Image Viewer Update Failed",
            backup_old_exe: "Backup old executable",
            copy_new_exe: "Copy new executable",
            write_success_marker_failed: "Failed to write success marker",
            restart_app_failed: "Failed to restart app",
        },
    }
}

#[cfg(target_os = "windows")]
fn operation_failed(locale: &str, operation: &str, detail: &str) -> String {
    match locale {
        "zh-CN" => format!("{operation}失败：{detail}"),
        "zh-HK" | "zh-TW" => format!("{operation}失敗：{detail}"),
        _ => format!("{operation} failed: {detail}"),
    }
}

#[cfg(target_os = "windows")]
fn run(args: &Args, text: &UiText, locale: &str) -> Result<(), String> {
    log_line(&args.log, "update helper started");
    wait_for_process_exit(args.pid, &args.log);
    retry(
        || rename_or_replace(&args.old_exe, &args.backup_exe),
        &args.log,
        "backup old exe",
        text.backup_old_exe,
        locale,
    )?;
    retry(
        || copy_new_exe(&args.new_exe, &args.old_exe),
        &args.log,
        "copy new exe",
        text.copy_new_exe,
        locale,
    )?;
    std::fs::write(&args.success_marker, args.version.as_bytes()).map_err(|err| {
        operation_failed(locale, text.write_success_marker_failed, &err.to_string())
    })?;
    if args.restart {
        std::process::Command::new(&args.old_exe)
            .spawn()
            .map_err(|err| operation_failed(locale, text.restart_app_failed, &err.to_string()))?;
    }
    log_line(&args.log, "update helper completed");
    Ok(())
}

#[cfg(target_os = "windows")]
fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut pid = None;
    let mut old_exe = None;
    let mut new_exe = None;
    let mut backup_exe = None;
    let mut log = None;
    let mut success_marker = None;
    let mut version = None;
    let mut locale = None;
    let mut restart = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--pid" => pid = args.next().and_then(|s| s.parse::<u32>().ok()),
            "--old-exe" => old_exe = args.next().map(Into::into),
            "--new-exe" => new_exe = args.next().map(Into::into),
            "--backup-exe" => backup_exe = args.next().map(Into::into),
            "--log" => log = args.next().map(Into::into),
            "--success-marker" => success_marker = args.next().map(Into::into),
            "--version" => version = args.next(),
            "--locale" => locale = args.next(),
            "--restart" => restart = true,
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(Args {
        pid: pid.ok_or_else(|| "missing --pid".to_string())?,
        old_exe: old_exe.ok_or_else(|| "missing --old-exe".to_string())?,
        new_exe: new_exe.ok_or_else(|| "missing --new-exe".to_string())?,
        backup_exe: backup_exe.ok_or_else(|| "missing --backup-exe".to_string())?,
        log: log.ok_or_else(|| "missing --log".to_string())?,
        success_marker: success_marker.ok_or_else(|| "missing --success-marker".to_string())?,
        version: version.ok_or_else(|| "missing --version".to_string())?,
        locale: locale.unwrap_or_else(|| "en".to_string()),
        restart,
    })
}

#[cfg(target_os = "windows")]
fn wait_for_process_exit(pid: u32, log: &std::path::Path) {
    use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
    };

    unsafe {
        match OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, false, pid) {
            Ok(handle) => {
                let handle = ScopedHandle(handle);
                let result = WaitForSingleObject(handle.0, MAIN_PROCESS_WAIT_TIMEOUT_MS);
                if result == WAIT_TIMEOUT {
                    log_line(
                        log,
                        "main process wait timed out; terminating main process before replacement",
                    );
                    if TerminateProcess(handle.0, 0).is_ok() {
                        let _ = WaitForSingleObject(handle.0, 5_000);
                        log_line(log, "main process termination requested");
                    } else {
                        log_line(
                            log,
                            "failed to terminate main process; continuing with retry loop",
                        );
                    }
                } else if result != WAIT_OBJECT_0 {
                    log_line(log, "main process wait failed; continuing with retry loop");
                }
            }
            Err(_) => log_line(log, "main process already exited or could not be opened"),
        }
    }

    struct ScopedHandle(HANDLE);

    impl Drop for ScopedHandle {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

#[cfg(target_os = "windows")]
fn retry(
    mut op: impl FnMut() -> Result<(), String>,
    log: &std::path::Path,
    log_label: &str,
    user_label: &str,
    locale: &str,
) -> Result<(), String> {
    let mut last_err = String::new();
    for attempt in 1..=MAX_RENAME_RETRIES {
        match op() {
            Ok(()) => return Ok(()),
            Err(err) => {
                last_err = err;
                log_line(
                    log,
                    &format!("{log_label} attempt {attempt} failed: {last_err}"),
                );
                std::thread::sleep(std::time::Duration::from_millis(RETRY_SLEEP_MS));
            }
        }
    }
    Err(operation_failed(locale, user_label, &last_err))
}

#[cfg(target_os = "windows")]
fn rename_or_replace(
    old_exe: &std::path::Path,
    backup_exe: &std::path::Path,
) -> Result<(), String> {
    if backup_exe.exists() {
        std::fs::remove_file(backup_exe).map_err(|err| err.to_string())?;
    }
    std::fs::rename(old_exe, backup_exe).map_err(|err| err.to_string())
}

#[cfg(target_os = "windows")]
fn copy_new_exe(new_exe: &std::path::Path, old_exe: &std::path::Path) -> Result<(), String> {
    std::fs::copy(new_exe, old_exe)
        .map(|_| ())
        .map_err(|err| err.to_string())
}

#[cfg(target_os = "windows")]
fn log_line(path: &std::path::Path, message: &str) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let line = format!("[{ts}] {message}\n");
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| std::io::Write::write_all(&mut file, line.as_bytes()));
}

#[cfg(target_os = "windows")]
fn show_message(title: &str, message: &str) {
    use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
    use windows::core::PCWSTR;

    let title = wide(title);
    let message = wide(message);
    unsafe {
        let _ = MessageBoxW(
            None,
            PCWSTR(message.as_ptr()),
            PCWSTR(title.as_ptr()),
            MB_OK | MB_ICONERROR,
        );
    }
}

#[cfg(target_os = "windows")]
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}
