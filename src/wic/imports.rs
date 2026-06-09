// Simple Image Viewer - A high-performance, cross-platform image viewer
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
    static WIC_FACTORY: RefCell<Option<IWICImagingFactory>> = RefCell::new(None);
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
            .ok_or_else(|| windows::core::Error::from_win32())
    })
}
