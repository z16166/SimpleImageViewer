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

#[cfg(feature = "jpegxl")]
pub(crate) struct JxlResizableRunnerPtr(*mut std::ffi::c_void);

#[cfg(feature = "jpegxl")]
impl JxlResizableRunnerPtr {
    pub(crate) fn try_new() -> Option<Self> {
        let ptr = unsafe { libjxl_sys::JxlResizableParallelRunnerCreate(std::ptr::null()) };
        if ptr.is_null() { None } else { Some(Self(ptr)) }
    }

    pub(crate) fn as_ptr(&self) -> *mut std::ffi::c_void {
        self.0
    }
}

#[cfg(feature = "jpegxl")]
impl Drop for JxlResizableRunnerPtr {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { libjxl_sys::JxlResizableParallelRunnerDestroy(self.0) };
            self.0 = std::ptr::null_mut();
        }
    }
}
