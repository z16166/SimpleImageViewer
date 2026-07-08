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
use interprocess::ConnectWaitMode;
use interprocess::local_socket::traits::Listener as ListenerTrait;
use interprocess::local_socket::{
    ConnectOptions, GenericNamespaced, Listener, ListenerOptions, Stream, prelude::*,
};
use parking_lot::Mutex;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;
use std::time::Duration;

use eframe::egui;

static IPC_WAKE_CTX: OnceLock<Mutex<Option<egui::Context>>> = OnceLock::new();

struct IpcServerHandle {
    shutdown: Arc<AtomicBool>,
    join: Mutex<Option<JoinHandle<()>>>,
}

static IPC_SERVER: OnceLock<Mutex<Option<IpcServerHandle>>> = OnceLock::new();

fn ipc_wake_slot() -> &'static Mutex<Option<egui::Context>> {
    IPC_WAKE_CTX.get_or_init(|| Mutex::new(None))
}

/// Register the live egui context so the IPC server thread can wake the event loop.
pub fn register_ipc_wake_context(ctx: egui::Context) {
    *ipc_wake_slot().lock() = Some(ctx);
}

fn wake_ui_from_ipc() {
    if let Some(ctx) = ipc_wake_slot().lock().as_ref() {
        ctx.request_repaint();
    }
}

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
    let sock_name = match IPC_SOCKET_NAME.to_ns_name::<GenericNamespaced>() {
        Ok(name) => name,
        Err(e) => {
            log::error!(
                "Failed to resolve IPC socket name: {}. Single-instance mode disabled.",
                e
            );
            return false;
        }
    };

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
            unsafe extern "system" {
                fn AllowSetForegroundWindow(dwProcessId: u32) -> i32;
            }
            unsafe {
                let _ = AllowSetForegroundWindow(ASFW_ANY);
            }
        }

        match conn.write_all(payload_clone.as_bytes()) {
            Ok(_) => {
                let _ = done_tx.send(ClientOp::Success);
            }
            Err(e) => {
                let _ = done_tx.send(ClientOp::WriteFailed(e));
            }
        }
    });

    // Handle the flattened result
    match done_rx.recv_timeout(IPC_CLIENT_TIMEOUT) {
        Ok(ClientOp::Success) => {
            log::info!("Message forwarded successfully. Exiting secondary instance.");
            return true;
        }
        Ok(ClientOp::WriteFailed(e)) => {
            log::error!(
                "IPC primary detected but write failed: {}. Possible zombie.",
                e
            );
            return true;
        }
        Ok(ClientOp::ConnectFailed(e)) => {
            use std::io::ErrorKind;
            let kind = e.kind();
            if kind == ErrorKind::NotFound || kind == ErrorKind::ConnectionRefused {
                log::info!(
                    "No existing instance detected ({}). Becoming primary server.",
                    e
                );
            } else {
                log::error!(
                    "IPC primary appears to exist but is unreachable ({}). Exiting to avoid conflicts.",
                    e
                );
                return true;
            }
        }
        Err(_) => {
            log::error!(
                "IPC client operation timed out ({:?}). Primary instance is likely frozen.",
                IPC_CLIENT_TIMEOUT
            );
            return true;
        }
    }

    match ListenerOptions::new().name(sock_name).create_sync() {
        Ok(listener) => {
            spawn_ipc_server(listener, tx);
        }
        Err(e) => {
            // On macOS/Linux with filesystem sockets, a stale socket file from a
            // previous crash can cause bind to fail. Attempt cleanup and retry.
            log::warn!(
                "Failed to bind IPC socket ({}), attempting stale cleanup...",
                e
            );
            cleanup_stale_socket();
            let sock_name_retry = match IPC_SOCKET_NAME.to_ns_name::<GenericNamespaced>() {
                Ok(name) => name,
                Err(err) => {
                    log::error!(
                        "Failed to resolve IPC socket name on retry: {}. Single-instance mode disabled.",
                        err
                    );
                    return false;
                }
            };
            match ListenerOptions::new().name(sock_name_retry).create_sync() {
                Ok(listener) => {
                    log::info!("Successfully bound IPC socket after stale cleanup.");
                    spawn_ipc_server(listener, tx);
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

fn spawn_ipc_server(listener: Listener, tx: crossbeam_channel::Sender<IpcMessage>) {
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_for_thread = Arc::clone(&shutdown);
    let res = std::thread::Builder::new()
        .name("siv-ipc-server".to_string())
        .spawn(move || ipc_server_loop(listener, tx, shutdown_for_thread));
    match res {
        Ok(join) => {
            *IPC_SERVER.get_or_init(|| Mutex::new(None)).lock() = Some(IpcServerHandle {
                shutdown,
                join: Mutex::new(Some(join)),
            });
        }
        Err(e) => {
            log::error!("[IPC] Failed to spawn IPC listener thread: {}", e);
        }
    }
}

/// Unblock a blocking `listener.accept()` so the IPC server thread can observe shutdown.
fn wake_ipc_listener_for_shutdown() {
    let sock_name = match IPC_SOCKET_NAME.to_ns_name::<GenericNamespaced>() {
        Ok(name) => name,
        Err(e) => {
            log::warn!(
                "[IPC] Failed to resolve socket name for shutdown wake: {}",
                e
            );
            return;
        }
    };
    std::thread::spawn(move || {
        let options = ConnectOptions::new()
            .name(sock_name)
            .wait_mode(ConnectWaitMode::Timeout(Duration::from_millis(200)));
        let _ = options.connect_sync();
    });
}

/// Stop the single-instance IPC accept loop before process exit.
pub fn shutdown_ipc_server() {
    let Some(server) = IPC_SERVER.get().and_then(|slot| slot.lock().take()) else {
        return;
    };
    server.shutdown.store(true, Ordering::Release);
    wake_ipc_listener_for_shutdown();
    let Some(join) = server.join.lock().take() else {
        return;
    };
    if let Err(e) = join.join() {
        log::warn!("[IPC] Server thread panicked on join: {:?}", e);
    }
}

/// The IPC server loop running on its own thread.
/// Accepts connections (blocking) and forwards messages to the UI.
fn ipc_server_loop(
    listener: Listener,
    tx: crossbeam_channel::Sender<IpcMessage>,
    shutdown: Arc<AtomicBool>,
) {
    while !shutdown.load(Ordering::Acquire) {
        match listener.accept() {
            Ok(conn) => {
                if shutdown.load(Ordering::Acquire) {
                    break;
                }
                handle_ipc_connection(conn, &tx);
            }
            Err(e) if shutdown.load(Ordering::Acquire) => {
                let _ = e;
                break;
            }
            Err(e) => {
                log::warn!("[IPC] Accept failed: {}", e);
                break;
            }
        }
    }
    log::debug!("[IPC] Server loop exiting");
}

fn handle_ipc_connection(conn: Stream, tx: &crossbeam_channel::Sender<IpcMessage>) {
    // Set a read timeout to prevent a single bad connection from blocking the listener forever
    if let Err(e) = set_stream_timeouts(&conn, Some(Duration::from_secs(2))) {
        log::warn!("Failed to set read timeout on IPC connection: {}", e);
    }

    let mut s = String::new();
    let mut conn = conn;

    // Use .take(MAX + 1) to detect overflow. If we read more than MAX, the payload is invalid.
    if std::io::Read::by_ref(&mut conn)
        .take(MAX_IPC_PAYLOAD_SIZE + 1)
        .read_to_string(&mut s)
        .is_ok()
    {
        if s.len() > MAX_IPC_PAYLOAD_SIZE as usize {
            log::warn!(
                "IPC: Rejected oversized payload (limit: {} bytes)",
                MAX_IPC_PAYLOAD_SIZE
            );
            return;
        }

        let s = s.trim();
        if s.is_empty() {
            return;
        }

        let mut delivered = false;
        if s.starts_with("OPEN_NR:") {
            let path_str = s.trim_start_matches("OPEN_NR:").trim();
            if !path_str.is_empty() {
                let _ = tx.send(IpcMessage::OpenImageNoRecursive(PathBuf::from(path_str)));
                delivered = true;
            }
        } else if s.starts_with("OPEN:") {
            let path_str = s.trim_start_matches("OPEN:").trim();
            if !path_str.is_empty() {
                let _ = tx.send(IpcMessage::OpenImage(PathBuf::from(path_str)));
                delivered = true;
            }
        } else if s == "FOCUS" {
            let _ = tx.send(IpcMessage::Focus);
            delivered = true;
        }
        if delivered {
            wake_ui_from_ipc();
        }
    }
    // Connection dropped here; continue accepting next connection
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
            format!("/tmp/{}", IPC_SOCKET_NAME),
            format!("/tmp/{}.sock", IPC_SOCKET_NAME),
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
fn current_process_main_window_with_options(visible_only: bool) -> Option<isize> {
    #[allow(clippy::upper_case_acronyms)]
    type HWND = isize;
    #[allow(clippy::upper_case_acronyms)]
    type BOOL = i32;
    #[allow(clippy::upper_case_acronyms)]
    type DWORD = u32;
    #[allow(clippy::upper_case_acronyms)]
    type LPARAM = isize;

    const GA_ROOT: u32 = 2;
    const GW_OWNER: u32 = 4;

    #[repr(C)]
    #[derive(Default)]
    #[allow(clippy::upper_case_acronyms)]
    struct RECT {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }

    struct CollectState {
        process_id: DWORD,
        best_hwnd: HWND,
        best_area: i64,
        visible_only: bool,
    }

    unsafe extern "system" fn enum_collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let state = unsafe { &mut *(lparam as *mut CollectState) };
        let mut process_id = 0;
        unsafe {
            GetWindowThreadProcessId(hwnd, &mut process_id);
        }
        if process_id != state.process_id {
            return 1;
        }

        let is_root = unsafe { GetAncestor(hwnd, GA_ROOT) } == hwnd;
        let is_unowned = unsafe { GetWindow(hwnd, GW_OWNER) } == 0;
        if !is_root || !is_unowned {
            return 1;
        }

        if state.visible_only && unsafe { IsWindowVisible(hwnd) } == 0 {
            return 1;
        }

        let mut rect = RECT::default();
        if unsafe { GetWindowRect(hwnd, &mut rect) } == 0 {
            return 1;
        }

        let width = i64::from(rect.right.saturating_sub(rect.left));
        let height = i64::from(rect.bottom.saturating_sub(rect.top));
        let area = width.saturating_mul(height);
        if area > state.best_area {
            state.best_hwnd = hwnd;
            state.best_area = area;
        }

        1
    }

    unsafe extern "system" {
        fn EnumWindows(
            lpEnumFunc: unsafe extern "system" fn(HWND, LPARAM) -> BOOL,
            lParam: LPARAM,
        ) -> BOOL;
        fn GetCurrentProcessId() -> DWORD;
        fn GetWindowThreadProcessId(hWnd: HWND, lpdwProcessId: *mut DWORD) -> DWORD;
        fn GetAncestor(hWnd: HWND, gaFlags: u32) -> HWND;
        fn GetWindow(hWnd: HWND, uCmd: u32) -> HWND;
        fn GetWindowRect(hWnd: HWND, lpRect: *mut RECT) -> BOOL;
        fn IsWindowVisible(hWnd: HWND) -> BOOL;
    }

    let mut state = CollectState {
        process_id: unsafe { GetCurrentProcessId() },
        best_hwnd: 0,
        best_area: 0,
        visible_only,
    };

    unsafe {
        EnumWindows(enum_collect, &mut state as *mut CollectState as LPARAM);
    }

    (state.best_hwnd != 0).then_some(state.best_hwnd)
}

#[cfg(windows)]
fn current_process_main_window() -> Option<isize> {
    current_process_main_window_with_options(false)
}

#[cfg(windows)]
fn current_process_visible_main_window() -> Option<isize> {
    current_process_main_window_with_options(true)
}

/// Undo [`hide_main_window`] before egui applies `ViewportCommand::Visible(true)`.
#[cfg(windows)]
pub fn unhide_main_window() {
    #[allow(clippy::upper_case_acronyms)]
    type HWND = isize;
    #[allow(clippy::upper_case_acronyms)]
    type BOOL = i32;
    const SW_RESTORE: i32 = 9;
    const SW_SHOW: i32 = 5;

    unsafe extern "system" {
        fn ShowWindow(hWnd: HWND, nCmdShow: i32) -> BOOL;
        fn IsIconic(hWnd: HWND) -> BOOL;
    }

    let Some(hwnd) = current_process_main_window() else {
        log::warn!("unhide_main_window: could not find current process main window");
        return;
    };

    unsafe {
        if IsIconic(hwnd) != 0 {
            ShowWindow(hwnd, SW_RESTORE);
        } else {
            ShowWindow(hwnd, SW_SHOW);
        }
    }
}

#[cfg(not(windows))]
pub fn unhide_main_window() {}

#[cfg(windows)]
pub fn hide_main_window() {
    #[allow(clippy::upper_case_acronyms)]
    type HWND = isize;
    #[allow(clippy::upper_case_acronyms)]
    type BOOL = i32;
    const SW_HIDE: i32 = 0;

    unsafe extern "system" {
        fn ShowWindow(hWnd: HWND, nCmdShow: i32) -> BOOL;
    }

    let Some(hwnd) = current_process_main_window() else {
        log::warn!("hide_main_window: could not find current process main window");
        return;
    };

    unsafe {
        ShowWindow(hwnd, SW_HIDE);
    }
}

#[cfg(not(windows))]
pub fn hide_main_window() {
    // On non-Windows, egui's ViewportCommand::Visible(false) is sufficient.
}

/// Bring an already-visible main window to the foreground. No-op when hidden to tray.
#[cfg(windows)]
pub fn force_foreground_if_visible() {
    let Some(hwnd) = current_process_visible_main_window() else {
        return;
    };
    force_foreground_hwnd(hwnd);
}

#[cfg(not(windows))]
pub fn force_foreground_if_visible() {}

#[cfg(windows)]
fn force_foreground_hwnd(hwnd: isize) {
    #[allow(clippy::upper_case_acronyms)]
    type HWND = isize;
    #[allow(clippy::upper_case_acronyms)]
    type BOOL = i32;
    #[allow(clippy::upper_case_acronyms)]
    type DWORD = u32;
    #[allow(clippy::upper_case_acronyms)]
    type UINT = u32;
    const SW_RESTORE: i32 = 9;
    const SW_SHOW: i32 = 5;
    const SWP_NOMOVE: UINT = 0x0002;
    const SWP_NOSIZE: UINT = 0x0001;
    const SWP_SHOWWINDOW: UINT = 0x0040;
    const HWND_TOP: HWND = 0;

    unsafe extern "system" {
        fn GetForegroundWindow() -> HWND;
        fn SetForegroundWindow(hWnd: HWND) -> BOOL;
        fn SetActiveWindow(hWnd: HWND) -> HWND;
        fn ShowWindow(hWnd: HWND, nCmdShow: i32) -> BOOL;
        fn IsIconic(hWnd: HWND) -> BOOL;
        fn GetWindowThreadProcessId(hWnd: HWND, lpdwProcessId: *mut DWORD) -> DWORD;
        fn AttachThreadInput(idAttach: DWORD, idAttachTo: DWORD, fAttach: BOOL) -> BOOL;
        fn BringWindowToTop(hWnd: HWND) -> BOOL;
        fn SetWindowPos(
            hWnd: HWND,
            hWndInsertAfter: HWND,
            X: i32,
            Y: i32,
            cx: i32,
            cy: i32,
            uFlags: UINT,
        ) -> BOOL;
        fn SwitchToThisWindow(hWnd: HWND, fAltTab: BOOL);
    }

    unsafe {
        if IsIconic(hwnd) != 0 {
            ShowWindow(hwnd, SW_RESTORE);
        } else {
            ShowWindow(hwnd, SW_SHOW);
        }

        SetWindowPos(
            hwnd,
            HWND_TOP,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
        );

        let fg_hwnd = GetForegroundWindow();
        let fg_thread = GetWindowThreadProcessId(fg_hwnd, std::ptr::null_mut());
        let target_thread = GetWindowThreadProcessId(hwnd, std::ptr::null_mut());
        let attached = fg_thread != 0 && fg_thread != target_thread;
        if attached {
            AttachThreadInput(fg_thread, target_thread, 1);
        }

        BringWindowToTop(hwnd);
        SetActiveWindow(hwnd);
        let foreground_set = SetForegroundWindow(hwnd) != 0;

        if attached {
            AttachThreadInput(fg_thread, target_thread, 0);
        }

        if !foreground_set || GetForegroundWindow() != hwnd {
            SwitchToThisWindow(hwnd, 1);
        }
    }
}

/// Aggressively bring our window to the foreground on Windows.
/// Uses the AttachThreadInput trick to bypass the OS foreground-lock restriction.
/// On non-Windows platforms, this is a no-op (egui's Focus command suffices).
#[cfg(windows)]
pub fn force_foreground() {
    let Some(hwnd) = current_process_visible_main_window().or_else(current_process_main_window)
    else {
        log::warn!("force_foreground: could not find current process main window");
        return;
    };
    force_foreground_hwnd(hwnd);
}

#[cfg(not(windows))]
pub fn force_foreground() {
    // On non-Windows, egui's ViewportCommand::Focus is sufficient.
}
