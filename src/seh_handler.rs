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

//! Windows SEH (Structured Exception Handling) crash capture.
//!
//! This module installs both:
//! 1. A top-level unhandled exception filter for final crash capture.
//! 2. A vectored exception handler probe that runs earlier in the SEH chain.
//!
//! The vectored handler is intentionally lightweight and only emits a small
//! breadcrumb file for fatal native exception codes. This helps distinguish
//! "the process never raised a native exception for us" from
//! "a native exception happened but the top-level unhandled filter was replaced
//! or could not finish".

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use windows::Win32::Foundation::{
    BOOL, CloseHandle, EXCEPTION_ACCESS_VIOLATION, EXCEPTION_ARRAY_BOUNDS_EXCEEDED,
    EXCEPTION_BREAKPOINT, EXCEPTION_DATATYPE_MISALIGNMENT, EXCEPTION_FLT_DENORMAL_OPERAND,
    EXCEPTION_FLT_DIVIDE_BY_ZERO, EXCEPTION_FLT_INEXACT_RESULT, EXCEPTION_FLT_INVALID_OPERATION,
    EXCEPTION_FLT_OVERFLOW, EXCEPTION_FLT_STACK_CHECK, EXCEPTION_FLT_UNDERFLOW,
    EXCEPTION_GUARD_PAGE, EXCEPTION_ILLEGAL_INSTRUCTION, EXCEPTION_IN_PAGE_ERROR,
    EXCEPTION_INT_DIVIDE_BY_ZERO, EXCEPTION_INT_OVERFLOW, EXCEPTION_INVALID_DISPOSITION,
    EXCEPTION_INVALID_HANDLE, EXCEPTION_NONCONTINUABLE_EXCEPTION, EXCEPTION_PRIV_INSTRUCTION,
    EXCEPTION_SINGLE_STEP, EXCEPTION_STACK_OVERFLOW, HANDLE, NTSTATUS,
};
use windows::Win32::Storage::FileSystem::{
    CREATE_ALWAYS, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, WriteFile,
};
use windows::Win32::System::Diagnostics::Debug::{
    AddVectoredExceptionHandler, EXCEPTION_POINTERS, EXCEPTION_RECORD,
    MINIDUMP_EXCEPTION_INFORMATION, MINIDUMP_TYPE, MiniDumpWriteDump, SetUnhandledExceptionFilter,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, GetCurrentThreadId,
};
use windows::core::PCWSTR;

const CRASH_PATH_WIDE_CAPACITY: usize = 512;
const PROBE_REPORT_BUFFER_SIZE: usize = 1024;
const TEXT_REPORT_BUFFER_SIZE: usize = 4096;

/// Guard to prevent reentrant invocations of the unhandled exception filter.
static SEH_ENTERED: AtomicBool = AtomicBool::new(false);
/// Ensure the vectored handler probe writes at most once per process.
static PROBE_WRITTEN: AtomicBool = AtomicBool::new(false);
/// Cache whether the unhandled exception filter ever actually fired.
static TOP_LEVEL_FILTER_ENTERED: AtomicBool = AtomicBool::new(false);
/// Snapshot from the earliest fatal native exception seen by the vectored probe.
static LAST_FATAL_EXCEPTION_CODE: AtomicU32 = AtomicU32::new(0);
static LAST_FATAL_EXCEPTION_ADDRESS: AtomicU64 = AtomicU64::new(0);
static LAST_FATAL_EXCEPTION_THREAD: AtomicU32 = AtomicU32::new(0);
static CRASH_OUTPUT_PATHS: OnceLock<Option<CrashOutputPaths>> = OnceLock::new();

struct CrashOutputPaths {
    report_path: [u16; CRASH_PATH_WIDE_CAPACITY],
    dump_path: [u16; CRASH_PATH_WIDE_CAPACITY],
    probe_path: [u16; CRASH_PATH_WIDE_CAPACITY],
}

/// Minimal stack-only RAII wrapper for Win32 handles used inside the crash
/// path. No allocation, no indirection: it only guarantees `CloseHandle` on
/// every return path so helper functions cannot accidentally leak handles.
struct ScopedHandle(HANDLE);

impl ScopedHandle {
    fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for ScopedHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

impl CrashOutputPaths {
    fn new() -> Option<Self> {
        let base_dir = crate::settings::settings_path();
        let base_dir = base_dir.parent().unwrap_or(std::path::Path::new("."));

        let report = base_dir.join(crate::constants::CRASH_REPORT_FILENAME);
        let dump = base_dir.join(crate::constants::CRASH_DUMP_FILENAME);
        let probe = base_dir.join(crate::constants::CRASH_PROBE_FILENAME);

        let mut paths = Self {
            report_path: [0u16; CRASH_PATH_WIDE_CAPACITY],
            dump_path: [0u16; CRASH_PATH_WIDE_CAPACITY],
            probe_path: [0u16; CRASH_PATH_WIDE_CAPACITY],
        };
        if path_to_wide(&report, &mut paths.report_path) == 0
            || path_to_wide(&dump, &mut paths.dump_path) == 0
            || path_to_wide(&probe, &mut paths.probe_path) == 0
        {
            return None;
        }
        Some(paths)
    }
}

/// Install the Windows crash handlers. Call as early as possible in `main()`.
pub fn install() {
    let _ = CRASH_OUTPUT_PATHS.get_or_init(CrashOutputPaths::new);
    unsafe {
        let _ = AddVectoredExceptionHandler(1, Some(vectored_exception_handler));
        SetUnhandledExceptionFilter(Some(unhandled_exception_filter));
    }
}

/// Re-assert our top-level unhandled exception filter after library init that
/// might have replaced it.
pub fn reinstall_top_level_filter() {
    unsafe {
        SetUnhandledExceptionFilter(Some(unhandled_exception_filter));
    }
}

unsafe extern "system" fn vectored_exception_handler(
    exception_info: *mut EXCEPTION_POINTERS,
) -> i32 {
    const EXCEPTION_CONTINUE_SEARCH: i32 = 0;

    let Some(paths) = CRASH_OUTPUT_PATHS.get().and_then(Option::as_ref) else {
        return EXCEPTION_CONTINUE_SEARCH;
    };
    if PROBE_WRITTEN.load(Ordering::Relaxed) {
        return EXCEPTION_CONTINUE_SEARCH;
    }
    if exception_info.is_null() {
        return EXCEPTION_CONTINUE_SEARCH;
    }

    let ei = unsafe { &*exception_info };
    if ei.ExceptionRecord.is_null() {
        return EXCEPTION_CONTINUE_SEARCH;
    }
    let rec = unsafe { &*ei.ExceptionRecord };
    let code = rec.ExceptionCode;
    if !is_fatal_native_exception(code) {
        return EXCEPTION_CONTINUE_SEARCH;
    }

    LAST_FATAL_EXCEPTION_CODE.store(code.0 as u32, Ordering::Relaxed);
    LAST_FATAL_EXCEPTION_ADDRESS.store(rec.ExceptionAddress as u64, Ordering::Relaxed);
    LAST_FATAL_EXCEPTION_THREAD.store(unsafe { GetCurrentThreadId() }, Ordering::Relaxed);

    if PROBE_WRITTEN
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
    {
        unsafe {
            write_probe_report(PCWSTR(paths.probe_path.as_ptr()), exception_info);
        }
    }

    EXCEPTION_CONTINUE_SEARCH
}

unsafe extern "system" fn unhandled_exception_filter(
    exception_info: *const EXCEPTION_POINTERS,
) -> i32 {
    const EXCEPTION_EXECUTE_HANDLER: i32 = 1;

    TOP_LEVEL_FILTER_ENTERED.store(true, Ordering::SeqCst);
    if SEH_ENTERED.swap(true, Ordering::SeqCst) {
        return EXCEPTION_EXECUTE_HANDLER;
    }

    let Some(paths) = CRASH_OUTPUT_PATHS.get().and_then(Option::as_ref) else {
        return EXCEPTION_EXECUTE_HANDLER;
    };

    unsafe {
        write_text_report(PCWSTR(paths.report_path.as_ptr()), exception_info);
        write_minidump(PCWSTR(paths.dump_path.as_ptr()), exception_info);
    }

    EXCEPTION_EXECUTE_HANDLER
}

unsafe fn write_probe_report(path: PCWSTR, exception_info: *const EXCEPTION_POINTERS) {
    let Some(handle) = (unsafe { open_output_file(path) }) else {
        return;
    };

    let mut buf = [0u8; PROBE_REPORT_BUFFER_SIZE];
    let mut pos = 0usize;
    pos = append_str(
        &mut buf,
        pos,
        "--- Simple Image Viewer Native Exception Probe ---\r\n",
    );
    pos = append_str(
        &mut buf,
        pos,
        "This file is written by the vectored exception handler before the top-level unhandled exception filter.\r\n",
    );
    pos = append_str(&mut buf, pos, "Version: v");
    pos = append_str(&mut buf, pos, env!("CARGO_PKG_VERSION"));
    pos = append_str(&mut buf, pos, "\r\n");
    pos = append_str(&mut buf, pos, "Top-level filter entered: ");
    pos = append_str(
        &mut buf,
        pos,
        if TOP_LEVEL_FILTER_ENTERED.load(Ordering::Relaxed) {
            "yes"
        } else {
            "no"
        },
    );
    pos = append_str(&mut buf, pos, "\r\n");

    if !exception_info.is_null() {
        let ei = unsafe { &*exception_info };
        if !ei.ExceptionRecord.is_null() {
            let rec = unsafe { &*ei.ExceptionRecord };
            pos = append_str(&mut buf, pos, "Exception Code: 0x");
            pos = append_hex32(&mut buf, pos, rec.ExceptionCode.0 as u32);
            pos = append_str(&mut buf, pos, " (");
            pos = append_str(&mut buf, pos, exception_code_name(rec.ExceptionCode));
            pos = append_str(&mut buf, pos, ")\r\n");
            pos = append_str(&mut buf, pos, "Exception Address: 0x");
            pos = append_hex64(&mut buf, pos, rec.ExceptionAddress as u64);
            pos = append_str(&mut buf, pos, "\r\nThread ID: 0x");
            pos = append_hex32(&mut buf, pos, unsafe { GetCurrentThreadId() });
            pos = append_str(&mut buf, pos, "\r\n");
        }
    }

    write_buffer_to_handle(&handle, &buf[..pos]);
}

unsafe fn write_text_report(path: PCWSTR, exception_info: *const EXCEPTION_POINTERS) {
    let Some(handle) = (unsafe { open_output_file(path) }) else {
        return;
    };

    let mut buf = [0u8; TEXT_REPORT_BUFFER_SIZE];
    let mut pos = 0usize;

    pos = append_str(
        &mut buf,
        pos,
        "--- Simple Image Viewer SEH Crash Report ---\r\n",
    );
    pos = append_str(&mut buf, pos, "Version: v");
    pos = append_str(&mut buf, pos, env!("CARGO_PKG_VERSION"));
    pos = append_str(&mut buf, pos, "\r\n");

    if !exception_info.is_null() {
        let ei = unsafe { &*exception_info };
        if !ei.ExceptionRecord.is_null() {
            let rec: &EXCEPTION_RECORD = unsafe { &*ei.ExceptionRecord };
            let code = rec.ExceptionCode;

            pos = append_str(&mut buf, pos, "Exception Code: 0x");
            pos = append_hex32(&mut buf, pos, code.0 as u32);
            pos = append_str(&mut buf, pos, " (");
            pos = append_str(&mut buf, pos, exception_code_name(code));
            pos = append_str(&mut buf, pos, ")\r\n");

            pos = append_str(&mut buf, pos, "Exception Address: 0x");
            pos = append_hex64(&mut buf, pos, rec.ExceptionAddress as u64);
            pos = append_str(&mut buf, pos, "\r\n");

            if code == EXCEPTION_ACCESS_VIOLATION && rec.NumberParameters >= 2 {
                let rw = rec.ExceptionInformation[0];
                let addr = rec.ExceptionInformation[1];
                pos = append_str(&mut buf, pos, "Access Type: ");
                pos = append_str(
                    &mut buf,
                    pos,
                    if rw == 0 {
                        "READ"
                    } else if rw == 1 {
                        "WRITE"
                    } else {
                        "EXECUTE"
                    },
                );
                pos = append_str(&mut buf, pos, "\r\nFaulting Address: 0x");
                pos = append_hex64(&mut buf, pos, addr as u64);
                pos = append_str(&mut buf, pos, "\r\n");
            }
        }

        #[cfg(target_arch = "x86_64")]
        if !ei.ContextRecord.is_null() {
            let ctx = unsafe { &*ei.ContextRecord };
            pos = append_str(&mut buf, pos, "\r\nRegisters (x64):\r\n");
            pos = append_reg(&mut buf, pos, "RIP", ctx.Rip);
            pos = append_reg(&mut buf, pos, "RSP", ctx.Rsp);
            pos = append_reg(&mut buf, pos, "RBP", ctx.Rbp);
            pos = append_reg(&mut buf, pos, "RAX", ctx.Rax);
            pos = append_reg(&mut buf, pos, "RBX", ctx.Rbx);
            pos = append_reg(&mut buf, pos, "RCX", ctx.Rcx);
            pos = append_reg(&mut buf, pos, "RDX", ctx.Rdx);
            pos = append_reg(&mut buf, pos, "RSI", ctx.Rsi);
            pos = append_reg(&mut buf, pos, "RDI", ctx.Rdi);
            pos = append_reg(&mut buf, pos, "R8 ", ctx.R8);
            pos = append_reg(&mut buf, pos, "R9 ", ctx.R9);
            pos = append_reg(&mut buf, pos, "R10", ctx.R10);
            pos = append_reg(&mut buf, pos, "R11", ctx.R11);
            pos = append_reg(&mut buf, pos, "R12", ctx.R12);
            pos = append_reg(&mut buf, pos, "R13", ctx.R13);
            pos = append_reg(&mut buf, pos, "R14", ctx.R14);
            pos = append_reg(&mut buf, pos, "R15", ctx.R15);
        }

        #[cfg(target_arch = "aarch64")]
        if !ei.ContextRecord.is_null() {
            let ctx = unsafe { &*ei.ContextRecord };
            pos = append_str(&mut buf, pos, "\r\nRegisters (ARM64):\r\n");
            pos = append_reg(&mut buf, pos, "PC ", ctx.Pc);
            pos = append_reg(&mut buf, pos, "SP ", ctx.Sp);
            pos = append_reg(&mut buf, pos, "FP ", ctx.Anonymous.Anonymous.Fp);
            pos = append_reg(&mut buf, pos, "LR ", ctx.Anonymous.Anonymous.Lr);
            for i in 0..8u32 {
                let mut name = [b'X', b'0', b' '];
                name[1] = b'0' + i as u8;
                pos = append_str(&mut buf, pos, unsafe {
                    core::str::from_utf8_unchecked(&name)
                });
                pos = append_str(&mut buf, pos, " = 0x");
                pos = append_hex64(&mut buf, pos, ctx.Anonymous.X[i as usize]);
                pos = append_str(&mut buf, pos, "\r\n");
            }
        }
    } else {
        pos = append_str(
            &mut buf,
            pos,
            "No EXCEPTION_POINTERS provided to the unhandled exception filter.\r\n",
        );
    }

    let probe_code = LAST_FATAL_EXCEPTION_CODE.load(Ordering::Relaxed);
    if probe_code != 0 {
        pos = append_str(&mut buf, pos, "\r\nVectored Probe Snapshot:\r\n");
        pos = append_str(&mut buf, pos, "Probe Exception Code: 0x");
        pos = append_hex32(&mut buf, pos, probe_code);
        pos = append_str(&mut buf, pos, "\r\nProbe Exception Address: 0x");
        pos = append_hex64(
            &mut buf,
            pos,
            LAST_FATAL_EXCEPTION_ADDRESS.load(Ordering::Relaxed),
        );
        pos = append_str(&mut buf, pos, "\r\nProbe Thread ID: 0x");
        pos = append_hex32(
            &mut buf,
            pos,
            LAST_FATAL_EXCEPTION_THREAD.load(Ordering::Relaxed),
        );
        pos = append_str(&mut buf, pos, "\r\n");
    }

    pos = append_str(
        &mut buf,
        pos,
        "\r\nA minidump (.dmp) file has also been generated in the same directory.\r\n",
    );
    pos = append_str(
        &mut buf,
        pos,
        "If crash_probe.txt exists but this report is missing or incomplete, another component may have replaced the top-level unhandled exception filter.\r\n",
    );
    pos = append_str(
        &mut buf,
        pos,
        "Please send both files to the developer for analysis.\r\n",
    );
    pos = append_str(
        &mut buf,
        pos,
        "--------------------------------------------\r\n",
    );

    write_buffer_to_handle(&handle, &buf[..pos]);
}

unsafe fn write_minidump(path: PCWSTR, exception_info: *const EXCEPTION_POINTERS) {
    let Some(handle) = (unsafe { open_output_file(path) }) else {
        return;
    };

    let process = unsafe { GetCurrentProcess() };
    let pid = unsafe { GetCurrentProcessId() };
    let tid = unsafe { GetCurrentThreadId() };

    // `MiniDumpWithDataSegs` includes globals that are often critical for
    // state-corruption debugging. `MiniDumpWithHandleData` keeps Win32 handle
    // context, and `MiniDumpWithThreadInfo` preserves thread start addresses /
    // timings with modest dump growth.
    let dump_type = MINIDUMP_TYPE(
        0x00000001 | // MiniDumpWithDataSegs
        0x00000004 | // MiniDumpWithHandleData
        0x00001000, // MiniDumpWithThreadInfo
    );

    let exception_param = MINIDUMP_EXCEPTION_INFORMATION {
        ThreadId: tid,
        ExceptionPointers: exception_info as *mut EXCEPTION_POINTERS,
        ClientPointers: BOOL(0),
    };

    unsafe {
        let _ = MiniDumpWriteDump(
            process,
            pid,
            handle.raw(),
            dump_type,
            Some(&exception_param),
            None,
            None,
        );
    }
}

/// Open a crash-output file using plain Win32 APIs only. The returned handle
/// is wrapped in stack-only RAII so callers do not need to remember a separate
/// `CloseHandle` path while already executing in crash context.
unsafe fn open_output_file(path: PCWSTR) -> Option<ScopedHandle> {
    let handle = unsafe {
        CreateFileW(
            path,
            0x40000000,
            FILE_SHARE_READ,
            None,
            CREATE_ALWAYS,
            FILE_ATTRIBUTE_NORMAL,
            HANDLE::default(),
        )
    };
    match handle {
        Ok(h) if !h.is_invalid() => Some(ScopedHandle(h)),
        _ => None,
    }
}

/// Best-effort stack-buffer write used by the crash-report text emitters.
/// The handle lifetime is owned by [`ScopedHandle`], so this helper writes only
/// and does not silently close resources.
fn write_buffer_to_handle(handle: &ScopedHandle, buf: &[u8]) {
    let mut written = 0u32;
    unsafe {
        let _ = WriteFile(handle.raw(), Some(buf), Some(&mut written), None);
    }
}

/// Native exceptions worth probing before the top-level unhandled filter has a
/// chance to run. We intentionally exclude noisy first-chance events such as
/// breakpoints and single-step traps.
fn is_fatal_native_exception(code: NTSTATUS) -> bool {
    matches!(
        code,
        EXCEPTION_ACCESS_VIOLATION
            | EXCEPTION_ARRAY_BOUNDS_EXCEEDED
            | EXCEPTION_DATATYPE_MISALIGNMENT
            | EXCEPTION_FLT_DENORMAL_OPERAND
            | EXCEPTION_FLT_DIVIDE_BY_ZERO
            | EXCEPTION_FLT_INEXACT_RESULT
            | EXCEPTION_FLT_INVALID_OPERATION
            | EXCEPTION_FLT_OVERFLOW
            | EXCEPTION_FLT_STACK_CHECK
            | EXCEPTION_FLT_UNDERFLOW
            | EXCEPTION_GUARD_PAGE
            | EXCEPTION_ILLEGAL_INSTRUCTION
            | EXCEPTION_IN_PAGE_ERROR
            | EXCEPTION_INT_DIVIDE_BY_ZERO
            | EXCEPTION_INT_OVERFLOW
            | EXCEPTION_INVALID_DISPOSITION
            | EXCEPTION_INVALID_HANDLE
            | EXCEPTION_NONCONTINUABLE_EXCEPTION
            | EXCEPTION_PRIV_INSTRUCTION
            | EXCEPTION_STACK_OVERFLOW
    )
}

/// Convert a Rust `Path` to a null-terminated UTF-16 stack buffer for Win32
/// file APIs. Returns the number of UTF-16 code units written, excluding the
/// trailing NUL, or `0` if the destination buffer is too small.
fn path_to_wide(path: &std::path::Path, buf: &mut [u16]) -> usize {
    use std::os::windows::ffi::OsStrExt;
    let mut i = 0;
    for c in path.as_os_str().encode_wide() {
        if i >= buf.len() - 1 {
            return 0;
        }
        buf[i] = c;
        i += 1;
    }
    buf[i] = 0;
    i
}

/// Append a UTF-8 string slice into a fixed-size byte buffer and return the
/// next write position, truncating when the remaining capacity is exhausted.
fn append_str(buf: &mut [u8], pos: usize, s: &str) -> usize {
    let bytes = s.as_bytes();
    let avail = buf.len().saturating_sub(pos);
    let n = bytes.len().min(avail);
    buf[pos..pos + n].copy_from_slice(&bytes[..n]);
    pos + n
}

/// Append a 32-bit value as uppercase hexadecimal without heap allocation.
fn append_hex32(buf: &mut [u8], pos: usize, val: u32) -> usize {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut tmp = [0u8; 8];
    for i in 0..8 {
        tmp[7 - i] = HEX[((val >> (i * 4)) & 0xF) as usize];
    }
    append_str(buf, pos, unsafe { core::str::from_utf8_unchecked(&tmp) })
}

/// Append a 64-bit value as uppercase hexadecimal without heap allocation.
fn append_hex64(buf: &mut [u8], pos: usize, val: u64) -> usize {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut tmp = [0u8; 16];
    for i in 0..16 {
        tmp[15 - i] = HEX[((val >> (i * 4)) & 0xF) as usize];
    }
    append_str(buf, pos, unsafe { core::str::from_utf8_unchecked(&tmp) })
}

/// Append a named register line in the form `RAX = 0x...`.
fn append_reg(buf: &mut [u8], pos: usize, name: &str, val: u64) -> usize {
    let mut pos = append_str(buf, pos, name);
    pos = append_str(buf, pos, " = 0x");
    pos = append_hex64(buf, pos, val);
    append_str(buf, pos, "\r\n")
}

/// Map common SEH exception codes to stable text for crash reports.
fn exception_code_name(code: NTSTATUS) -> &'static str {
    match code {
        EXCEPTION_ACCESS_VIOLATION => "ACCESS_VIOLATION",
        EXCEPTION_ARRAY_BOUNDS_EXCEEDED => "ARRAY_BOUNDS_EXCEEDED",
        EXCEPTION_BREAKPOINT => "BREAKPOINT",
        EXCEPTION_DATATYPE_MISALIGNMENT => "DATATYPE_MISALIGNMENT",
        EXCEPTION_FLT_DENORMAL_OPERAND => "FLT_DENORMAL_OPERAND",
        EXCEPTION_FLT_DIVIDE_BY_ZERO => "FLT_DIVIDE_BY_ZERO",
        EXCEPTION_FLT_INEXACT_RESULT => "FLT_INEXACT_RESULT",
        EXCEPTION_FLT_INVALID_OPERATION => "FLT_INVALID_OPERATION",
        EXCEPTION_FLT_OVERFLOW => "FLT_OVERFLOW",
        EXCEPTION_FLT_STACK_CHECK => "FLT_STACK_CHECK",
        EXCEPTION_FLT_UNDERFLOW => "FLT_UNDERFLOW",
        EXCEPTION_GUARD_PAGE => "GUARD_PAGE",
        EXCEPTION_ILLEGAL_INSTRUCTION => "ILLEGAL_INSTRUCTION",
        EXCEPTION_IN_PAGE_ERROR => "IN_PAGE_ERROR",
        EXCEPTION_INT_DIVIDE_BY_ZERO => "INT_DIVIDE_BY_ZERO",
        EXCEPTION_INT_OVERFLOW => "INT_OVERFLOW",
        EXCEPTION_INVALID_DISPOSITION => "INVALID_DISPOSITION",
        EXCEPTION_INVALID_HANDLE => "INVALID_HANDLE",
        EXCEPTION_NONCONTINUABLE_EXCEPTION => "NONCONTINUABLE_EXCEPTION",
        EXCEPTION_PRIV_INSTRUCTION => "PRIV_INSTRUCTION",
        EXCEPTION_SINGLE_STEP => "SINGLE_STEP",
        EXCEPTION_STACK_OVERFLOW => "STACK_OVERFLOW",
        _ => "UNKNOWN_EXCEPTION",
    }
}
