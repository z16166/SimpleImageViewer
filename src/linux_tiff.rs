use std::path::Path;
use std::ffi::{CString, c_void, c_char, c_int};
use libloading::{Library, Symbol};
use crate::loader::{ImageData, DecodedImage};

// libtiff types
type TIFF = c_void;
#[allow(non_camel_case_types)]
type uint32 = u32;

// TIFF tags
const TIFFTAG_IMAGEWIDTH: uint32 = 256;
const TIFFTAG_IMAGELENGTH: uint32 = 257;

// Function signatures
type TIFFOpenFn = unsafe extern "C" fn(path: *const c_char, mode: *const c_char) -> *mut TIFF;
type TIFFCloseFn = unsafe extern "C" fn(tif: *mut TIFF);
type TIFFGetFieldFn = unsafe extern "C" fn(tif: *mut TIFF, tag: uint32, ...) -> c_int;
type TIFFReadRGBAImageOrientedFn = unsafe extern "C" fn(
    tif: *mut TIFF,
    width: uint32,
    height: uint32,
    raster: *mut uint32,
    orientation: c_int,
    stop_on_error: c_int,
) -> c_int;

struct LibTiff {
    _lib: &'static Library,
    open: Symbol<'static, TIFFOpenFn>,
    close: Symbol<'static, TIFFCloseFn>,
    get_field: Symbol<'static, TIFFGetFieldFn>,
    read_rgba_image_oriented: Symbol<'static, TIFFReadRGBAImageOrientedFn>,
}

impl LibTiff {
    fn load() -> Result<Self, String> {
        let lib_names = ["libtiff.so.6", "libtiff.so.5", "libtiff.so"];
        for name in lib_names {
            if let Ok(lib) = unsafe { Library::new(name) } {
                // Leak the library to keep it alive for the duration of the program
                let lib: &'static Library = Box::leak(Box::new(lib));
                unsafe {
                    let open = lib.get(b"TIFFOpen").map_err(|e| e.to_string())?;
                    let close = lib.get(b"TIFFClose").map_err(|e| e.to_string())?;
                    let get_field = lib.get(b"TIFFGetField").map_err(|e| e.to_string())?;
                    let read_rgba_image_oriented = lib.get(b"TIFFReadRGBAImageOriented").map_err(|e| e.to_string())?;

                    return Ok(Self {
                        _lib: lib,
                        open,
                        close,
                        get_field,
                        read_rgba_image_oriented,
                    });
                }
            }
        }
        Err("Could not find libtiff.so.6 or libtiff.so.5".to_string())
    }
}

// Global or lazy-loaded library handle
thread_local! {
    static LIB: Result<LibTiff, String> = LibTiff::load();
}

pub fn load_via_libtiff(path: &Path) -> Result<ImageData, String> {
    LIB.with(|lib_res| {
        let lib = lib_res.as_ref().map_err(|e| e.clone())?;

        let c_path = CString::new(path.to_str().ok_or("Invalid path")?)
            .map_err(|e| e.to_string())?;
        let c_mode = CString::new("r").unwrap();

        unsafe {
            let tif = (lib.open)(c_path.as_ptr(), c_mode.as_ptr());
            if tif.is_null() {
                return Err("TIFFOpen failed".to_string());
            }

            let mut width: uint32 = 0;
            let mut height: uint32 = 0;

            if (lib.get_field)(tif, TIFFTAG_IMAGEWIDTH, &mut width) == 0 {
                (lib.close)(tif);
                return Err("Failed to get TIFF width".to_string());
            }
            if (lib.get_field)(tif, TIFFTAG_IMAGELENGTH, &mut height) == 0 {
                (lib.close)(tif);
                return Err("Failed to get TIFF height".to_string());
            }

            let total_pixels = (width as usize) * (height as usize);
            // TIFFReadRGBAImageOriented writes pixels as uint32 (0xAABBGGRR on little endian = RGBA bytes)
            let mut raster: Vec<uint32> = vec![0; total_pixels];

            // orientation 1 = top-left
            if (lib.read_rgba_image_oriented)(tif, width, height, raster.as_mut_ptr(), 1, 0) == 0 {
                (lib.close)(tif);
                return Err("TIFFReadRGBAImageOriented failed".to_string());
            }

            (lib.close)(tif);

            // Convert Vec<uint32> to Vec<u8> (RGBA8)
            // Safety: raster contains total_pixels * 4 bytes. We can transmute or just copy.
            // Transmuting Vec is tricky due to alignment and capacity, so we'll just convert.
            let mut pixels = Vec::with_capacity(total_pixels * 4);
            for p in raster {
                let bytes = p.to_ne_bytes();
                pixels.extend_from_slice(&bytes);
            }

            Ok(ImageData::Static(DecodedImage { width, height, pixels }))
        }
    })
}
