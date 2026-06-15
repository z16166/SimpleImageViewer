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

//! Process termination helpers for shutdown paths.

/// Terminate the process immediately without running global destructors.
///
/// On Unix this uses `_exit(2)` so mimalloc / `atexit` handlers do not run while
/// LibRaw OpenMP worker threads may still be active (closing the window during
/// in-flight RAW decode otherwise races `mi_process_done` with `__kmp_*`).
///
/// On Windows we keep `std::process::exit` so existing COM / audio teardown
/// behavior is unchanged.
pub fn force_process_exit(code: i32) -> ! {
    #[cfg(unix)]
    {
        unsafe extern "C" {
            fn _exit(status: i32) -> !;
        }
        unsafe { _exit(code) }
    }
    #[cfg(not(unix))]
    {
        std::process::exit(code)
    }
}
