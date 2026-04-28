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

use crate::constants::*;
use interprocess::local_socket::{GenericNamespaced, ListenerOptions, Stream, prelude::*, ConnectOptions};
use interprocess::ConnectWaitMode;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::Duration;

pub enum IpcMessage {
    /// Open an image, using the current recursive scan setting.
    OpenImage(PathBuf),
    /// Open an image with recursive scan forcibly disabled for this operation.
    /// Used when a file is opened via CLI (e.g. double-click from Explorer) to
    /// prevent accidentally scanning huge directory trees.
    OpenImageNoRecursive(PathBuf),
    Focus,
}

/// Attempts to setup IPC.
/// Returns `true` if this instance should immediately exit (because it successfully forwarded args to the primary instance).
/// Returns `false` if this instance should continue as the primary server.
pub fn setup_or_forward_args(
    tx: crossbeam_channel::Sender<IpcMessage>,
    initial_image: Option<&PathBuf>,
    no_recursive: bool,
) -> bool {
    let sock_name = IPC_SOCKET_NAME.to_ns_name::<GenericNamespaced>().unwrap();

    let payload = if let Some(path) = initial_image {
        if let Some(p) = path.to_str() {
            // Use OPEN_NR (No Recursive) prefix when launched from Explorer
            if no_recursive {
                format!("OPEN_NR:{}", p)
            } else {
                format!("OPEN:{}", p)
            }
        } else {
            "FOCUS".to_string()
        }
    } else {
        "FOCUS".to_string()
    };

    // Try to connect as a client first.
    // To guarantee absolute responsiveness and avoid ANY potential OS-level blocking 
    // during connect or write (especially on Windows), we isolate the entire 
    // client-side logic in a separate thread.
    enum ClientOp {
        Success,
        ConnectFailed(std::io::Error),
        WriteFailed(std::io::Error),
    }

    let (done_tx, done_rx) = crossbeam_channel::bounded(1);
    let payload_clone = payload.clone();
    let sock_name_clone = sock_name.clone();

    std::thread::spawn(move || {
        let options = ConnectOptions::new()
            .name(sock_name_clone)
            .wait_mode(ConnectWaitMode::Timeout(IPC_CONNECT_TIMEOUT));

        let mut conn = match options.connect_sync() {
            Ok(c) => c,
            Err(e) => {
                let _ = done_tx.send(ClientOp::ConnectFailed(e));
                return;
            }
        };

        #[cfg(windows)]
        {
            const ASFW_ANY: u32 = u32::MAX;
            unsafe extern "system" { fn AllowSetForegroundWindow(dwProcessId: u32) -> i32; }
            unsafe { let _ = AllowSetForegroundWindow(ASFW_ANY); }
        }

        match conn.write_all(payload_clone.as_bytes()) {
            Ok(_) => { let _ = done_tx.send(ClientOp::Success); }
            Err(e) => { let _ = done_tx.send(ClientOp::WriteFailed(e)); }
        }
    });

    // Handle the flattened result
    match done_rx.recv_timeout(IPC_CLIENT_TIMEOUT) {
        Ok(ClientOp::Success) => {
            log::info!("Message forwarded successfully. Exiting secondary instance.");
            return true;
        }
        Ok(ClientOp::WriteFailed(e)) => {
            log::error!("IPC primary detected but write failed: {}. Possible zombie.", e);
            return true;
        }
        Ok(ClientOp::ConnectFailed(e)) => {
            use std::io::ErrorKind;
            let kind = e.kind();
            if kind == ErrorKind::NotFound || kind == ErrorKind::ConnectionRefused {
                log::info!("No existing instance detected ({}). Becoming primary server.", e);
            } else {
                log::error!("IPC primary appears to exist but is unreachable ({}). Exiting to avoid conflicts.", e);
                return true;
            }
        }
        Err(_) => {
            log::error!("IPC client operation timed out ({:?}). Primary instance is likely frozen.", IPC_CLIENT_TIMEOUT);
            return true; 
        }
    }

    match ListenerOptions::new().name(sock_name).create_sync() {
        Ok(listener) => {
            let res = std::thread::Builder::new()
                .name("siv-ipc-server".to_string())
                .spawn(move || {
                    ipc_server_loop(listener, tx);
                });
            if let Err(e) = res {
                log::error!("[IPC] Failed to spawn IPC listener thread: {}", e);
            }
        }
        Err(e) => {
            // On macOS/Linux with filesystem sockets, a stale socket file from a
            // previous crash can cause bind to fail. Attempt cleanup and retry.
            log::warn!(
                "Failed to bind IPC socket ({}), attempting stale cleanup...",
                e
            );
            cleanup_stale_socket();
            let sock_name_retry = IPC_SOCKET_NAME.to_ns_name::<GenericNamespaced>().unwrap();
            match ListenerOptions::new().name(sock_name_retry).create_sync() {
                Ok(listener) => {
                    log::info!("Successfully bound IPC socket after stale cleanup.");
                    let res = std::thread::Builder::new()
                        .name("siv-ipc-server".to_string())
                        .spawn(move || {
                            ipc_server_loop(listener, tx);
                        });
                    if let Err(e) = res {
                        log::error!("[IPC] Failed to spawn IPC listener thread (retry): {}", e);
                    }
                }
                Err(e2) => {
                    log::warn!(
                        "Failed to bind IPC socket after retry, single-instance mode disabled: {}",
                        e2
                    );
                }
            }
        }
    }

    false // Do not exit
}

/// The IPC server loop running on its own thread.
/// Accepts connections, reads messages with a timeout, and forwards them to the UI.
fn ipc_server_loop(
    listener: interprocess::local_socket::Listener,
    tx: crossbeam_channel::Sender<IpcMessage>,
) {
    for conn in listener.incoming().filter_map(Result::ok) {
        // Set a read timeout to prevent a single bad connection from blocking the listener forever
        if let Err(e) = set_stream_timeouts(&conn, Some(Duration::from_secs(2))) {
            log::warn!("Failed to set read timeout on IPC connection: {}", e);
        }

        let mut s = String::new();
        let mut conn = conn;
        
        // Use .take() to enforce a hard limit on read size
        if std::io::Read::by_ref(&mut conn).take(MAX_IPC_PAYLOAD_SIZE).read_to_string(&mut s).is_ok() {
            // Trim whitespace and validate minimal length
            let s = s.trim();
            if s.is_empty() || s.len() > (MAX_IPC_PAYLOAD_SIZE as usize) {
                continue;
            }

            if s.starts_with("OPEN_NR:") {
                let path_str = s.trim_start_matches("OPEN_NR:").trim();
                if !path_str.is_empty() {
                    let _ = tx.send(IpcMessage::OpenImageNoRecursive(PathBuf::from(path_str)));
                }
            } else if s.starts_with("OPEN:") {
                let path_str = s.trim_start_matches("OPEN:").trim();
                if !path_str.is_empty() {
                    let _ = tx.send(IpcMessage::OpenImage(PathBuf::from(path_str)));
                }
            } else if s == "FOCUS" {
                let _ = tx.send(IpcMessage::Focus);
            }
        }
        // Connection dropped here; continue accepting next connection
    }
}

/// Best-effort timeout hint for local socket streams.
/// In practice, client connections always close (triggering EOF for read_to_string)
/// when the client drops the stream or crashes, so socket-level timeouts are not
/// strictly necessary for a local desktop IPC channel.
fn set_stream_timeouts(stream: &Stream, timeout: Option<Duration>) -> std::io::Result<()> {
    stream.set_recv_timeout(timeout)?;
    stream.set_send_timeout(timeout)?;
    Ok(())
}

/// Attempt to remove a stale Unix domain socket file left by a crashed process.
/// This is a no-op on Windows (Named Pipes are kernel objects, auto-cleaned).
fn cleanup_stale_socket() {
    #[cfg(unix)]
    {
        let candidates = [
            format!("/tmp/siv_ipc_sock_v1"),
            format!("/tmp/siv_ipc_sock_v1.sock"),
        ];
        for path in &candidates {
            if std::path::Path::new(path).exists() {
                log::info!("Removing stale socket file: {}", path);
                let _ = std::fs::remove_file(path);
            }
        }
    }
    #[cfg(windows)]
    {
        // Named Pipes on Windows are kernel objects — nothing to clean up on disk.
    }
}

/// Aggressively bring our window to the foreground on Windows.
/// Uses the AttachThreadInput trick to bypass the OS foreground-lock restriction.
/// On non-Windows platforms, this is a no-op (egui's Focus command suffices).
#[cfg(windows)]
pub fn force_foreground() {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    type HWND = isize;
    type BOOL = i32;
    type DWORD = u32;
    const SW_RESTORE: i32 = 9;
    const SW_SHOW: i32 = 5;

    unsafe extern "system" {
        fn FindWindowW(lpClassName: *const u16, lpWindowName: *const u16) -> HWND;
        fn GetForegroundWindow() -> HWND;
        fn SetForegroundWindow(hWnd: HWND) -> BOOL;
        fn ShowWindow(hWnd: HWND, nCmdShow: i32) -> BOOL;
        fn IsIconic(hWnd: HWND) -> BOOL;
        fn GetWindowThreadProcessId(hWnd: HWND, lpdwProcessId: *mut DWORD) -> DWORD;
        fn GetCurrentThreadId() -> DWORD;
        fn AttachThreadInput(idAttach: DWORD, idAttachTo: DWORD, fAttach: BOOL) -> BOOL;
        fn BringWindowToTop(hWnd: HWND) -> BOOL;
    }

    unsafe {
        // Find window by Class Name (which is our App ID "Simple Image Viewer")
        // rather than by Title, because the title changes with i18n or current file name.
        let class_name: Vec<u16> = OsStr::new("Simple Image Viewer")
            .encode_wide()
            .chain(Some(0))
            .collect();
        let hwnd = FindWindowW(class_name.as_ptr(), std::ptr::null());

        if hwnd == 0 {
            log::warn!("force_foreground: could not find window by class name");
            return;
        }

        // If minimized, restore it first
        if IsIconic(hwnd) != 0 {
            ShowWindow(hwnd, SW_RESTORE);
        } else {
            ShowWindow(hwnd, SW_SHOW);
        }

        // Attach to the foreground thread to gain permission
        let fg_hwnd = GetForegroundWindow();
        let fg_thread = GetWindowThreadProcessId(fg_hwnd, std::ptr::null_mut());
        let our_thread = GetCurrentThreadId();

        if fg_thread != our_thread && fg_thread != 0 {
            AttachThreadInput(our_thread, fg_thread, 1);
            BringWindowToTop(hwnd);
            SetForegroundWindow(hwnd);
            AttachThreadInput(our_thread, fg_thread, 0);
        } else {
            BringWindowToTop(hwnd);
            SetForegroundWindow(hwnd);
        }
    }
}

#[cfg(not(windows))]
pub fn force_foreground() {
    // On non-Windows, egui's ViewportCommand::Focus is sufficient.
}

