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
//! This module installs a global unhandled exception filter that fires for
//! native-level crashes (ACCESS_VIOLATION, STACK_OVERFLOW, etc.) which bypass
//! Rust's panic machinery.  When triggered it:
//!
//! 1. Writes a human-readable crash report to `crash_report.txt`
//! 2. Generates a minidump (`.dmp`) file for offline WinDbg analysis
//!
//! **Signal-safety**: The handler avoids Rust heap allocations and uses only
//! Win32 stack-based I/O (`WriteFile` with stack buffers) since the process
//! heap may already be corrupted at the point of the exception.

use std::sync::atomic::{AtomicBool, Ordering};

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
    EXCEPTION_POINTERS, EXCEPTION_RECORD, MINIDUMP_EXCEPTION_INFORMATION, MINIDUMP_TYPE,
    MiniDumpWriteDump, SetUnhandledExceptionFilter,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, GetCurrentThreadId,
};
use windows::core::PCWSTR;

/// Guard to prevent reentrant invocations of the exception handler.
static SEH_ENTERED: AtomicBool = AtomicBool::new(false);

/// Install the global SEH handler.  Call as early as possible in `main()`.
pub fn install() {
    unsafe {
        SetUnhandledExceptionFilter(Some(unhandled_exception_filter));
    }
}

// ---------------------------------------------------------------------------
// Exception filter callback
// ---------------------------------------------------------------------------

/// The actual callback invoked by Windows when an unhandled SEH exception
/// occurs.  This function MUST be signal-safe: no Rust heap allocations,
/// no `format!`, no `String`, no `println!`.
unsafe extern "system" fn unhandled_exception_filter(
    exception_info: *const EXCEPTION_POINTERS,
) -> i32 {
    const EXCEPTION_EXECUTE_HANDLER: i32 = 1;

    // Guard against reentrant calls (e.g., if writing the dump itself faults).
    if SEH_ENTERED.swap(true, Ordering::SeqCst) {
        return EXCEPTION_EXECUTE_HANDLER;
    }

    // Resolve the crash report directory (same dir as settings.yaml).
    // We pre-compute this once and keep it on the stack.
    let base_dir = crate::settings::settings_path();
    let base_dir = base_dir.parent().unwrap_or(std::path::Path::new("."));

    // --- 1. Write the text crash report ---
    let report_path = base_dir.join(crate::constants::CRASH_REPORT_FILENAME);
    unsafe {
        write_text_report(&report_path, exception_info);
    }

    // --- 2. Write the minidump ---
    let dump_path = base_dir.join(crate::constants::CRASH_DUMP_FILENAME);
    unsafe {
        write_minidump(&dump_path, exception_info);
    }

    EXCEPTION_EXECUTE_HANDLER
}

// ---------------------------------------------------------------------------
// Text report (signal-safe, stack-only)
// ---------------------------------------------------------------------------

/// Write a human-readable crash report using only Win32 `WriteFile`.
/// On failure this is best-effort; we silently continue.
unsafe fn write_text_report(path: &std::path::Path, exception_info: *const EXCEPTION_POINTERS) {
    // Convert path to wide string on the stack (max ~512 chars is plenty).
    let mut wide_buf = [0u16; 512];
    let wide_len = path_to_wide(path, &mut wide_buf);
    if wide_len == 0 {
        return;
    }

    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide_buf.as_ptr()),
            0x40000000, // GENERIC_WRITE
            FILE_SHARE_READ,
            None,
            CREATE_ALWAYS,
            FILE_ATTRIBUTE_NORMAL,
            HANDLE::default(),
        )
    };
    let handle = match handle {
        Ok(h) if !h.is_invalid() => h,
        _ => return,
    };

    // 4KB stack buffer for composing the report
    let mut buf = [0u8; 4096];
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

            // For ACCESS_VIOLATION, parameter[0] = read/write, parameter[1] = address
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

        // Dump register context
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
            // First 8 general-purpose registers
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
    }

    pos = append_str(
        &mut buf,
        pos,
        "\r\nA minidump (.dmp) file has also been generated in the same directory.\r\n",
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

    // Flush to disk
    let mut written = 0u32;
    unsafe {
        let _ = WriteFile(handle, Some(&buf[..pos]), Some(&mut written), None);
        let _ = CloseHandle(handle);
    }
}

// ---------------------------------------------------------------------------
// Minidump via DbgHelp
// ---------------------------------------------------------------------------

/// Write a minidump using `MiniDumpWriteDump`.  This API is designed by
/// Microsoft to be safe to call from within an exception filter.
unsafe fn write_minidump(path: &std::path::Path, exception_info: *const EXCEPTION_POINTERS) {
    let mut wide_buf = [0u16; 512];
    let wide_len = path_to_wide(path, &mut wide_buf);
    if wide_len == 0 {
        return;
    }

    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide_buf.as_ptr()),
            0x40000000, // GENERIC_WRITE
            FILE_SHARE_READ,
            None,
            CREATE_ALWAYS,
            FILE_ATTRIBUTE_NORMAL,
            HANDLE::default(),
        )
    };
    let handle = match handle {
        Ok(h) if !h.is_invalid() => h,
        _ => return,
    };

    let process = unsafe { GetCurrentProcess() };
    let pid = unsafe { GetCurrentProcessId() };
    let tid = unsafe { GetCurrentThreadId() };

    // MiniDumpWithDataSegs includes global variable segments — very helpful
    // for diagnosing state-related crashes, adds only a few MB.
    // MiniDumpWithThreadInfo includes thread times and start addresses.
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
            handle,
            dump_type,
            Some(&exception_param),
            None,
            None,
        );

        let _ = CloseHandle(handle);
    }
}

// ---------------------------------------------------------------------------
// Signal-safe helper functions (no heap, stack only)
// ---------------------------------------------------------------------------

/// Convert a Rust `Path` to a null-terminated UTF-16 buffer on the stack.
/// Returns the number of u16 characters written (excluding null terminator),
/// or 0 on failure (path too long).
fn path_to_wide(path: &std::path::Path, buf: &mut [u16]) -> usize {
    use std::os::windows::ffi::OsStrExt;
    let mut i = 0;
    for c in path.as_os_str().encode_wide() {
        if i >= buf.len() - 1 {
            return 0; // Path too long for our stack buffer
        }
        buf[i] = c;
        i += 1;
    }
    buf[i] = 0; // Null terminator
    i
}

/// Append a string slice to a fixed-size buffer.  Returns new position.
fn append_str(buf: &mut [u8], pos: usize, s: &str) -> usize {
    let bytes = s.as_bytes();
    let avail = buf.len().saturating_sub(pos);
    let n = bytes.len().min(avail);
    buf[pos..pos + n].copy_from_slice(&bytes[..n]);
    pos + n
}

/// Append a 32-bit value as uppercase hex to the buffer.
fn append_hex32(buf: &mut [u8], pos: usize, val: u32) -> usize {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut tmp = [0u8; 8];
    for i in 0..8 {
        tmp[7 - i] = HEX[((val >> (i * 4)) & 0xF) as usize];
    }
    append_str(buf, pos, unsafe { core::str::from_utf8_unchecked(&tmp) })
}

/// Append a 64-bit value as uppercase hex to the buffer.
fn append_hex64(buf: &mut [u8], pos: usize, val: u64) -> usize {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut tmp = [0u8; 16];
    for i in 0..16 {
        tmp[15 - i] = HEX[((val >> (i * 4)) & 0xF) as usize];
    }
    append_str(buf, pos, unsafe { core::str::from_utf8_unchecked(&tmp) })
}

/// Append a named register value (e.g., "RAX = 0x...\r\n").
fn append_reg(buf: &mut [u8], pos: usize, name: &str, val: u64) -> usize {
    let mut p = append_str(buf, pos, name);
    p = append_str(buf, p, " = 0x");
    p = append_hex64(buf, p, val);
    append_str(buf, p, "\r\n")
}

/// Return a human-readable name for common exception codes.
fn exception_code_name(code: NTSTATUS) -> &'static str {
    match code {
        EXCEPTION_ACCESS_VIOLATION => "ACCESS_VIOLATION",
        EXCEPTION_STACK_OVERFLOW => "STACK_OVERFLOW",
        EXCEPTION_ILLEGAL_INSTRUCTION => "ILLEGAL_INSTRUCTION",
        EXCEPTION_INT_DIVIDE_BY_ZERO => "INT_DIVIDE_BY_ZERO",
        EXCEPTION_INT_OVERFLOW => "INT_OVERFLOW",
        EXCEPTION_FLT_DIVIDE_BY_ZERO => "FLT_DIVIDE_BY_ZERO",
        EXCEPTION_FLT_OVERFLOW => "FLT_OVERFLOW",
        EXCEPTION_FLT_UNDERFLOW => "FLT_UNDERFLOW",
        EXCEPTION_FLT_INEXACT_RESULT => "FLT_INEXACT_RESULT",
        EXCEPTION_FLT_DENORMAL_OPERAND => "FLT_DENORMAL_OPERAND",
        EXCEPTION_FLT_INVALID_OPERATION => "FLT_INVALID_OPERATION",
        EXCEPTION_FLT_STACK_CHECK => "FLT_STACK_CHECK",
        EXCEPTION_BREAKPOINT => "BREAKPOINT",
        EXCEPTION_SINGLE_STEP => "SINGLE_STEP",
        EXCEPTION_DATATYPE_MISALIGNMENT => "DATATYPE_MISALIGNMENT",
        EXCEPTION_ARRAY_BOUNDS_EXCEEDED => "ARRAY_BOUNDS_EXCEEDED",
        EXCEPTION_PRIV_INSTRUCTION => "PRIV_INSTRUCTION",
        EXCEPTION_IN_PAGE_ERROR => "IN_PAGE_ERROR",
        EXCEPTION_NONCONTINUABLE_EXCEPTION => "NONCONTINUABLE_EXCEPTION",
        EXCEPTION_INVALID_DISPOSITION => "INVALID_DISPOSITION",
        EXCEPTION_GUARD_PAGE => "GUARD_PAGE",
        EXCEPTION_INVALID_HANDLE => "INVALID_HANDLE",
        _ => "UNKNOWN_EXCEPTION",
    }
}
