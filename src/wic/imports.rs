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

pub use crate::formats::{FormatGroup, ImageFormat, get_registry};
pub use crate::loader::TiledImageSource;
pub use std::cell::RefCell;
pub use std::sync::atomic::Ordering;
pub use std::thread;
pub use windows::Win32::Foundation::GENERIC_READ;
pub use windows::Win32::Graphics::Imaging::*;
pub use windows::Win32::System::Com::*;
pub use windows::core::*;

thread_local! {
    static WIC_FACTORY: RefCell<Option<IWICImagingFactory>> = const { RefCell::new(None) };
}

pub(crate) fn get_wic_factory() -> windows::core::Result<IWICImagingFactory> {
    WIC_FACTORY.with(|f| {
        let mut factory = f.borrow_mut();
        if factory.is_none() {
            let instance =
                unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)? };
            *factory = Some(instance);
        }
        factory
            .as_ref()
            .cloned()
            .ok_or_else(windows::core::Error::from_win32)
    })
}
