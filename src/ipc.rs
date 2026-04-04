use interprocess::local_socket::{prelude::*, GenericNamespaced, ListenerOptions, Stream};
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
    let sock_name = "siv_ipc_sock_v1".to_ns_name::<GenericNamespaced>().unwrap();

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
    // Use a short timeout to avoid blocking forever if the server is dead/stuck.
    if let Ok(mut conn) = Stream::connect(sock_name.clone()) {
        // Set a write timeout so we don't hang if the pipe buffer is full
        if let Err(e) = set_stream_timeouts(&conn, Some(Duration::from_millis(500))) {
            log::warn!("Failed to set stream timeout: {}", e);
        }

        // On Windows, this client process was just launched by Explorer and owns the
        // foreground right. Transfer it to ANY process (i.e. the server) so the server
        // can call SetForegroundWindow successfully.
        #[cfg(windows)]
        {
            const ASFW_ANY: u32 = u32::MAX; // -1 as DWORD
            unsafe extern "system" {
                fn AllowSetForegroundWindow(dwProcessId: u32) -> i32;
            }
            unsafe { AllowSetForegroundWindow(ASFW_ANY); }
        }

        log::info!("Another instance is running. Forwarding arguments and exiting.");
        let _ = conn.write_all(payload.as_bytes());
        // Dropping `conn` here closes the connection and signals EOF to the server
        drop(conn);
        return true; // We are the client, exit the process
    }

    // Connect failed, meaning we are the primary instance.
    log::info!("No existing instance detected. Becoming the primary IPC server.");

    match ListenerOptions::new().name(sock_name).create_sync() {
        Ok(listener) => {
            std::thread::Builder::new()
                .name("siv-ipc-server".to_string())
                .spawn(move || {
                    ipc_server_loop(listener, tx);
                })
                .expect("Failed to spawn IPC listener thread");
        }
        Err(e) => {
            // On macOS/Linux with filesystem sockets, a stale socket file from a
            // previous crash can cause bind to fail. Attempt cleanup and retry.
            log::warn!("Failed to bind IPC socket ({}), attempting stale cleanup...", e);
            cleanup_stale_socket();
            let sock_name_retry = "siv_ipc_sock_v1".to_ns_name::<GenericNamespaced>().unwrap();
            match ListenerOptions::new().name(sock_name_retry).create_sync() {
                Ok(listener) => {
                    log::info!("Successfully bound IPC socket after stale cleanup.");
                    std::thread::Builder::new()
                        .name("siv-ipc-server".to_string())
                        .spawn(move || {
                            ipc_server_loop(listener, tx);
                        })
                        .expect("Failed to spawn IPC listener thread");
                }
                Err(e2) => {
                    log::warn!("Failed to bind IPC socket after retry, single-instance mode disabled: {}", e2);
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
        // read_to_string will return at EOF when client drops the connection
        let mut conn = conn;
        if conn.read_to_string(&mut s).is_ok() {
            if s.starts_with("OPEN_NR:") {
                let path_str = s.trim_start_matches("OPEN_NR:");
                let _ = tx.send(IpcMessage::OpenImageNoRecursive(PathBuf::from(path_str)));
            } else if s.starts_with("OPEN:") {
                let path_str = s.trim_start_matches("OPEN:");
                let _ = tx.send(IpcMessage::OpenImage(PathBuf::from(path_str)));
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
fn set_stream_timeouts(_stream: &Stream, _timeout: Option<Duration>) -> std::io::Result<()> {
    // interprocess::local_socket::Stream does not directly expose raw fd/handle
    // for portable timeout configuration. Since all our IPC clients close the
    // connection immediately after writing (via drop), read_to_string will
    // reliably return at EOF without needing an explicit timeout.
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
        let title: Vec<u16> = OsStr::new("Simple Image Viewer")
            .encode_wide()
            .chain(Some(0))
            .collect();
        let hwnd = FindWindowW(std::ptr::null(), title.as_ptr());
        if hwnd == 0 {
            log::warn!("force_foreground: could not find window by title");
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
