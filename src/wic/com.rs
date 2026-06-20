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

use super::imports::*;
use windows::Win32::Foundation::S_FALSE;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::core::HRESULT;

const RPC_E_CHANGED_MODE: HRESULT = HRESULT(0x8001_0106_u32 as i32);

pub struct ComGuard {
    should_uninitialize: bool,
}

impl ComGuard {
    /// Initialize COM as MTA on worker threads.
    ///
    /// On the egui/UI thread COM is usually already initialized as STA
    /// (`RPC_E_CHANGED_MODE` / 0x80010106). In that case we reuse the existing apartment
    /// and do not call [`CoUninitialize`] on drop. Callers that need WIC from the UI
    /// thread should run work on [`crate::loader::preview_caps::REFINEMENT_POOL`] instead.
    pub fn new() -> windows::core::Result<Self> {
        unsafe {
            let hr = CoInitializeEx(None, COINIT_MULTITHREADED);
            if hr.is_ok() {
                return Ok(Self {
                    should_uninitialize: hr != S_FALSE,
                });
            }
            if hr == RPC_E_CHANGED_MODE {
                return Ok(Self {
                    should_uninitialize: false,
                });
            }
            Err(hr.into())
        }
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.should_uninitialize {
            unsafe {
                CoUninitialize();
            }
        }
    }
}

pub fn init_rayon_with_com() {
    rayon::ThreadPoolBuilder::new()
        .spawn_handler(|rayon_thread| {
            let mut builder = thread::Builder::new();
            if let Some(name) = rayon_thread.name() {
                builder = builder.name(name.to_owned());
            }
            if let Some(stack_size) = rayon_thread.stack_size() {
                builder = builder.stack_size(stack_size);
            }

            builder.spawn(move || {
                let _com = ComGuard::new().expect("Failed to initialize COM on WIC worker");
                rayon_thread.run()
            })?;
            Ok(())
        })
        .build_global()
        .unwrap_or(());
}
