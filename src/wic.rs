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
use crate::loader::TiledImageSource;
use std::cell::RefCell;
use std::sync::atomic::Ordering;
use std::thread;

thread_local! {
    static WIC_FACTORY: RefCell<Option<IWICImagingFactory>> = RefCell::new(None);
}

fn get_wic_factory() -> windows::core::Result<IWICImagingFactory> {
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

use windows::Win32::Foundation::GENERIC_READ;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::*;
use windows::core::*;

pub struct ComGuard;

impl ComGuard {
    pub fn new() -> windows::core::Result<Self> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        }
        Ok(Self)
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
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

pub fn spawn_wic_discovery() {
    thread::spawn(|| {
        if let Err(e) = discover_wic_codecs() {
            log::error!("WIC codec discovery failed: {:?}", e);
        }
    });
}

fn discover_wic_codecs() -> windows::core::Result<()> {
    let _com = ComGuard::new()?;
    unsafe {
        let factory = get_wic_factory()?;

        let enumerator = factory.CreateComponentEnumerator(
            WICDecoder.0 as u32,
            WICComponentEnumerateDefault.0 as u32,
        )?;

        let mut components = [None; 1];
        let mut _fetched = 0;
        let mut new_codecs = 0;

        loop {
            let hr = enumerator.Next(&mut components, Some(&mut _fetched));
            if hr.is_ok() && _fetched > 0 {
                if let Some(unknown) = components[0].take() {
                    if let Ok(codec_info) = unknown.cast::<IWICBitmapCodecInfo>() {
                        let mut ext_buf = [0u16; 512];
                        let mut ext_len = 0;
                        if codec_info
                            .GetFileExtensions(&mut ext_buf, &mut ext_len)
                            .is_ok()
                            && ext_len > 1
                        {
                            let extensions_str =
                                String::from_utf16_lossy(&ext_buf[..ext_len as usize - 1]);

                            let mut name_buf = [0u16; 512];
                            let mut name_len = 0;
                            let friendly_name = if codec_info
                                .GetFriendlyName(&mut name_buf, &mut name_len)
                                .is_ok()
                                && name_len > 1
                            {
                                String::from_utf16_lossy(&name_buf[..name_len as usize - 1])
                            } else {
                                "Unknown WIC Codec".to_string()
                            };

                            let group = if friendly_name.contains("RAW")
                                || friendly_name.contains("Camera")
                            {
                                FormatGroup::WicRaw
                            } else if friendly_name.contains("Microsoft")
                                || friendly_name.contains("Windows")
                            {
                                FormatGroup::WicSystem
                            } else {
                                FormatGroup::Others
                            };

                            let clsid = codec_info.GetCLSID()?;
                            let mut clsid_bytes = [0u8; 16];
                            std::ptr::copy_nonoverlapping(
                                &clsid as *const GUID as *const u8,
                                clsid_bytes.as_mut_ptr(),
                                16,
                            );

                            if let Ok(mut reg) = get_registry().write() {
                                let mut added_for_codec = false;
                                for ext in extensions_str.split(|c| c == ',' || c == ';') {
                                    let normalized_ext = ext
                                        .trim()
                                        .trim_start_matches('*')
                                        .trim_start_matches('.')
                                        .to_lowercase();
                                    if !normalized_ext.is_empty() {
                                        if !reg.extensions.contains(&normalized_ext) {
                                            reg.add_format(ImageFormat {
                                                extension: normalized_ext,
                                                group,
                                                description: friendly_name.clone(),
                                                wic_clsid: Some(clsid_bytes),
                                            });
                                            added_for_codec = true;
                                        }
                                    }
                                }
                                if added_for_codec {
                                    new_codecs += 1;
                                }
                            }
                        }
                    }
                }
            } else {
                break;
            }
        }
        log::info!(
            "WIC discovery finished: identified {} additional system codecs.",
            new_codecs
        );
    }

    if let Ok(mut reg) = get_registry().write() {
        reg.discovery_finished = true;
    }
    Ok(())
}

/// A tiled source for Windows Imaging Component (WIC) decoders.
/// Allows on-demand decoding of any WIC-supported format without full memory allocation.
pub struct WicTiledSource {
    path: std::path::PathBuf,
    width: u32,
    height: u32,
    factory: IWICImagingFactory,
    decoder: IWICBitmapDecoder,
    frame: IWICBitmapFrameDecode,
    source: IWICBitmapSource,
    physical_width: u32,
    physical_height: u32,
    transform_options: WICBitmapTransformOptions,
    // Removed shared Mutex converter to enable true thread-parallel decoding.
    // Each thread will now create its own local converter in extract_tile.
    // Explicitly keep the stream alive to prevent data source invalidation
    #[allow(dead_code)]
    stream: Option<IWICStream>,
    // Removed shared converter to enable thread-parallel decoding
    // _mmap MUST be at the end to ensure it is dropped AFTER the WIC objects
    _mmap: Option<std::sync::Arc<memmap2::Mmap>>,
}

impl WicTiledSource {
    // extract_tile handles its own converter initialization to avoid deadlocks.

    /// Wraps a source in a WICBitmapCacheOnDemand to prevent decoder thrashing during transforms.
    fn wrap_with_cache(
        factory: &IWICImagingFactory,
        source: &IWICBitmapSource,
    ) -> IWICBitmapSource {
        unsafe {
            match factory.CreateBitmapFromSource(source, WICBitmapCacheOnDemand) {
                Ok(bitmap) => bitmap
                    .cast::<IWICBitmapSource>()
                    .unwrap_or_else(|_| source.clone()),
                Err(_) => source.clone(),
            }
        }
    }
}

// WIC interfaces are thread-safe for reading if COM was initialized as COINIT_MULTITHREADED.
unsafe impl Send for WicTiledSource {}
unsafe impl Sync for WicTiledSource {}

impl crate::loader::TiledImageSource for WicTiledSource {
    fn width(&self) -> u32 {
        self.width
    }
    fn height(&self) -> u32 {
        self.height
    }

    fn extract_tile(&self, x: u32, y: u32, w: u32, h: u32) -> Vec<u8> {
        let mut pixels = vec![0u8; (w * h * 4) as usize];
        let stride = w * 4;

        // Ensure COM is initialized on the current worker thread
        let _com = ComGuard::new();

        unsafe {
            // Create a local converter for this thread to allow parallel decoding.
            // While initialization has a cost, it's dwarfed by the benefit of
            // utilizing all available CPU cores without lock contention.
            if let Ok(converter) = self.factory.CreateFormatConverter() {
                if converter
                    .Initialize(
                        &self.source,
                        &GUID_WICPixelFormat32bppRGBA,
                        WICBitmapDitherTypeNone,
                        None,
                        0.0,
                        WICBitmapPaletteTypeCustom,
                    )
                    .is_ok()
                {
                    let rect = WICRect {
                        X: x as i32,
                        Y: y as i32,
                        Width: w as i32,
                        Height: h as i32,
                    };
                    let _ = converter.CopyPixels(&rect, stride, &mut pixels);
                }
            }
        }

        pixels
    }

    fn generate_preview(&self, max_w: u32, max_h: u32) -> (u32, u32, Vec<u8>) {
        let scale = (max_w as f64 / self.width as f64)
            .min(max_h as f64 / self.height as f64)
            .min(1.0);
        let out_w = (self.width as f64 * scale).round().max(1.0) as u32;
        let out_h = (self.height as f64 * scale).round().max(1.0) as u32;

        unsafe {
            let factory = &self.factory;

            // Path 1: Extract embedded thumbnail if large enough
            if let Ok(thumbnail) = self.frame.GetThumbnail() {
                let mut fw = 0;
                let mut fh = 0;
                if thumbnail.GetSize(&mut fw, &mut fh).is_ok() && fw >= out_w && fh >= out_h {
                    log::info!(
                        "WIC [Idx={}]: Using embedded thumbnail as preview ({}x{})",
                        self.path.display(),
                        fw,
                        fh
                    );

                    if let Ok(thumb_src) = thumbnail.cast::<IWICBitmapSource>() {
                        // Cache the thumbnail before rotation to prevent JPEG decoder
                        // thrashing. Embedded thumbnails can be up to 4096px+ on modern
                        // cameras, making this a real performance concern.
                        let thumb_cached = Self::wrap_with_cache(factory, &thumb_src);
                        let mut thumb_final = thumb_cached.clone();
                        if self.transform_options != WICBitmapTransformOptions(0) {
                            if let Ok(rotator) = factory.CreateBitmapFlipRotator() {
                                if rotator
                                    .Initialize(&thumb_cached, self.transform_options)
                                    .is_ok()
                                {
                                    if let Ok(src) = rotator.cast::<IWICBitmapSource>() {
                                        thumb_final = src;
                                    }
                                }
                            }
                        }
                        if let Some(res) = render_source_to_pixels(&thumb_final, &factory) {
                            log::info!(
                                "WIC [Idx={}]: Successfully rendered embedded thumbnail",
                                self.path.display()
                            );
                            return res;
                        }
                    }
                } else {
                    log::debug!(
                        "WIC [Idx={}]: Embedded thumbnail is too small ({}x{}) for requested {}x{}",
                        self.path.display(),
                        fw,
                        fh,
                        out_w,
                        out_h
                    );
                }
            } else {
                log::debug!(
                    "WIC [Idx={}]: No embedded thumbnail found",
                    self.path.display()
                );
            }

            // Path 2: Check for secondary downscaled frames (e.g. Pyramid TIFFs)
            if let Ok(count) = self.decoder.GetFrameCount() {
                if count > 1 {
                    for i in 1..count {
                        if let Ok(f) = self.decoder.GetFrame(i) {
                            let mut fw = 0;
                            let mut fh = 0;
                            if f.GetSize(&mut fw, &mut fh).is_ok() {
                                // If it's a good intermediate size, use it
                                if fw > 512 && fw < self.physical_width / 2 {
                                    log::info!(
                                        "WIC: Using secondary frame {} as preview ({}x{})",
                                        i,
                                        fw,
                                        fh
                                    );
                                    if let Ok(f_src) = f.cast::<IWICBitmapSource>() {
                                        let f_cached = Self::wrap_with_cache(factory, &f_src);
                                        let mut f_final = f_cached.clone();
                                        if self.transform_options != WICBitmapTransformOptions(0) {
                                            if let Ok(rotator) = factory.CreateBitmapFlipRotator() {
                                                if rotator
                                                    .Initialize(&f_cached, self.transform_options)
                                                    .is_ok()
                                                {
                                                    if let Ok(src) =
                                                        rotator.cast::<IWICBitmapSource>()
                                                    {
                                                        f_final = src;
                                                    }
                                                }
                                            }
                                        }
                                        if let Some(res) =
                                            render_source_to_pixels(&f_final, &factory)
                                        {
                                            return res;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Path 3: Try Native Decoder Source Transform (Fastest if supported)
            if let Ok(transform) = self.frame.cast::<IWICBitmapSourceTransform>() {
                let swap = self.width != self.physical_width;
                let mut closest_phys_w = if swap { out_h } else { out_w };
                let mut closest_phys_h = if swap { out_w } else { out_h };
                if transform
                    .GetClosestSize(&mut closest_phys_w, &mut closest_phys_h)
                    .is_ok()
                {
                    // Significant speedup if we are scaling down by at least 2x
                    if closest_phys_w < self.physical_width / 2
                        || closest_phys_h < self.physical_height / 2
                    {
                        let log_final_w = if swap { closest_phys_h } else { closest_phys_w };
                        let log_final_h = if swap { closest_phys_w } else { closest_phys_h };
                        log::info!(
                            "WIC [Idx={}]: Using Native Source Transform to decode directly to {}x{} (logical: {}x{})",
                            self.path.display(),
                            closest_phys_w,
                            closest_phys_h,
                            log_final_w,
                            log_final_h
                        );
                        let stride = log_final_w * 4;
                        let mut out = vec![0u8; (stride * log_final_h) as usize];
                        let rect = WICRect {
                            X: 0,
                            Y: 0,
                            Width: closest_phys_w as i32,
                            Height: closest_phys_h as i32,
                        };

                        if transform
                            .CopyPixels(
                                &rect,
                                closest_phys_w,
                                closest_phys_h,
                                &GUID_WICPixelFormat32bppRGBA as *const _,
                                self.transform_options,
                                stride,
                                &mut out,
                            )
                            .is_ok()
                        {
                            return (log_final_w, log_final_h, out);
                        } else {
                            log::warn!(
                                "WIC [Idx={}]: Native Source Transform CopyPixels FAILED",
                                self.path.display()
                            );
                        }
                    }
                }
            }

            // Path 4: Sub-sampling scaler (High-speed fallback)
            log::info!(
                "WIC [Idx={}]: No specialized preview source available, using standard Scaler (Target {}x{})",
                self.path.display(),
                out_w,
                out_h
            );

            let scaler: IWICBitmapScaler = match factory.CreateBitmapScaler() {
                Ok(s) => s,
                Err(e) => {
                    log::error!(
                        "WIC [Idx={}]: CreateBitmapScaler failed: {:?}",
                        self.path.display(),
                        e
                    );
                    return (0, 0, Vec::new());
                }
            };

            // IMPORTANT: Use self.source (the cached WIC bitmap) rather than self.raw_source.
            // self.raw_source is the uncached WIC pipeline that re-decodes the image from disk
            // on every access. self.source has WICBitmapCacheOnDemand, so scanlines already
            // decoded during tile extraction are reused, making the scaler dramatically faster.
            if let Err(e) = scaler.Initialize(
                &self.source,
                out_w,
                out_h,
                WICBitmapInterpolationModeNearestNeighbor,
            ) {
                log::error!(
                    "WIC [Idx={}]: Scaler.Initialize FAILED: {:?}",
                    self.path.display(),
                    e
                );
                return (0, 0, Vec::new());
            }

            let sc_converter: IWICFormatConverter = match factory.CreateFormatConverter() {
                Ok(c) => c,
                Err(_) => return (0, 0, Vec::new()),
            };

            if sc_converter
                .Initialize(
                    &scaler,
                    &GUID_WICPixelFormat32bppRGBA,
                    WICBitmapDitherTypeNone,
                    None,
                    0.0,
                    WICBitmapPaletteTypeCustom,
                )
                .is_err()
            {
                return (0, 0, Vec::new());
            }

            let stride = out_w * 4;
            let mut out = vec![0u8; (stride * out_h) as usize];
            let rect = WICRect {
                X: 0,
                Y: 0,
                Width: out_w as i32,
                Height: out_h as i32,
            };

            if sc_converter.CopyPixels(&rect, stride, &mut out).is_ok() {
                (out_w, out_h, out)
            } else {
                (0, 0, Vec::new())
            }
        }
    }

    fn full_pixels(&self) -> Option<std::sync::Arc<Vec<u8>>> {
        None
    }
}

fn render_source_to_pixels(
    source: &impl Interface,
    factory: &IWICImagingFactory,
) -> Option<(u32, u32, Vec<u8>)> {
    unsafe {
        let mut w = 0;
        let mut h = 0;
        // IWICBitmapSource is the base for frames and thumbnails
        let b_source = source.cast::<IWICBitmapSource>().ok()?;
        b_source.GetSize(&mut w, &mut h).ok()?;

        let sc_converter: IWICFormatConverter = factory.CreateFormatConverter().ok()?;
        sc_converter
            .Initialize(
                &b_source,
                &GUID_WICPixelFormat32bppRGBA,
                WICBitmapDitherTypeNone,
                None,
                0.0,
                WICBitmapPaletteTypeCustom,
            )
            .ok()?;

        let stride = w * 4;
        let mut out = vec![0u8; (stride * h) as usize];
        let rect = WICRect {
            X: 0,
            Y: 0,
            Width: w as i32,
            Height: h as i32,
        };
        if sc_converter.CopyPixels(&rect, stride, &mut out).is_ok() {
            Some((w, h, out))
        } else {
            None
        }
    }
}

// Centralized in metadata_utils.rs

pub fn load_via_wic(
    path: &std::path::Path,
    high_quality: bool,
    orientation_override: Option<u16>,
) -> std::result::Result<crate::loader::ImageData, String> {
    unsafe {
        let _com = ComGuard::new().map_err(|e| format!("COM Init failed: {:?}", e))?;

        let factory = get_wic_factory().map_err(|e| format!("Factory access failed: {:?}", e))?;

        let path_os = path.as_os_str();
        use std::os::windows::ffi::OsStrExt;
        let mut path_wide: Vec<u16> = path_os.encode_wide().collect();
        path_wide.push(0);
        let path_ptr = PCWSTR(path_wide.as_ptr());

        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_default();

        let mut decoder_res: Result<IWICBitmapDecoder> = Err(windows::core::Error::from_win32());
        let mut stream_out: Option<IWICStream> = None;
        let mut mmap_out: Option<std::sync::Arc<memmap2::Mmap>> = None;

        // --- Fast Path: Generic direct instantiation via registry CLSID (No sniffing) ---
        let clsid_opt = if let Ok(reg) = get_registry().read() {
            reg.formats
                .iter()
                .find(|f| f.extension == ext)
                .and_then(|f| f.wic_clsid)
        } else {
            None
        };

        if let Some(clsid_bytes) = clsid_opt {
            let mut clsid = GUID::default();
            std::ptr::copy_nonoverlapping(
                clsid_bytes.as_ptr(),
                &mut clsid as *mut GUID as *mut u8,
                16,
            );

            let specific_decoder: Result<IWICBitmapDecoder> =
                CoCreateInstance(&clsid, None, CLSCTX_INPROC_SERVER);
            if let Ok(sd) = specific_decoder {
                if let Ok(stream) = factory.CreateStream() {
                    // --- Mmap Path ---
                    let file = std::fs::File::open(path)
                        .map_err(|e| format!("File open failed: {:?}", e))?;
                    if let Ok(mmap) = memmap2::Mmap::map(&file) {
                        let m_arc = std::sync::Arc::new(mmap);
                        if stream.InitializeFromMemory(&m_arc[..]).is_ok() {
                            if sd
                                .Initialize(&stream, WICDecodeMetadataCacheOnDemand)
                                .is_ok()
                            {
                                decoder_res = Ok(sd);
                                stream_out = Some(stream);
                                mmap_out = Some(m_arc);
                            }
                        }
                    }
                }
            }
        }

        // --- Fallback: Standard Sniffing (If direct fails or not registered) ---
        if decoder_res.is_err() {
            decoder_res = factory.CreateDecoderFromFilename(
                path_ptr,
                None,
                GENERIC_READ,
                WICDecodeMetadataCacheOnDemand,
            );
        }

        // Attempt 2: Explicit decoder creation
        if let Err(ref e) = decoder_res {
            if e.code() == windows::core::HRESULT(0x88982F50u32 as i32) {
                if let Some(ext) = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.to_lowercase())
                {
                    let clsid_bytes_opt = if let Ok(reg) = get_registry().read() {
                        reg.formats
                            .iter()
                            .find(|f| f.extension == ext)
                            .and_then(|f| f.wic_clsid.clone())
                    } else {
                        None
                    };

                    if let Some(clsid_bytes) = clsid_bytes_opt {
                        let mut clsid = GUID::default();
                        std::ptr::copy_nonoverlapping(
                            clsid_bytes.as_ptr(),
                            &mut clsid as *mut GUID as *mut u8,
                            16,
                        );

                        log::info!(
                            "WIC Sniffer failed for {:?} (COMPONENTNOTFOUND), trying explicit decoder instantiation for CLSID {:?}",
                            path,
                            clsid
                        );

                        let specific_decoder: windows::core::Result<IWICBitmapDecoder> =
                            CoCreateInstance(&clsid, None, CLSCTX_INPROC_SERVER);
                        if let Ok(sd) = specific_decoder {
                            if let Ok(stream) = factory.CreateStream() {
                                if stream
                                    .InitializeFromFilename(path_ptr, GENERIC_READ.0)
                                    .is_ok()
                                {
                                    if sd
                                        .Initialize(&stream, WICDecodeMetadataCacheOnDemand)
                                        .is_ok()
                                    {
                                        decoder_res = Ok(sd);
                                        stream_out = Some(stream);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        let decoder = decoder_res.map_err(|e| format!("WIC decoder creation failed: {:?}", e))?;

        let frame_count = decoder.GetFrameCount().unwrap_or(1);
        let mut best_frame_idx = 0;
        let mut max_p = 0;
        let mut width = 0;
        let mut height = 0;

        for i in 0..frame_count {
            if let Ok(f) = decoder.GetFrame(i) {
                let mut w = 0;
                let mut h = 0;
                if f.GetSize(&mut w, &mut h).is_ok() {
                    let p = w as u64 * h as u64;
                    if p > max_p {
                        max_p = p;
                        width = w;
                        height = h;
                        best_frame_idx = i;
                    }
                }
            }
        }

        let frame = decoder
            .GetFrame(best_frame_idx)
            .map_err(|e| format!("failed to get frame: {:?}", e))?;

        let orientation = orientation_override
            .unwrap_or_else(|| crate::metadata_utils::get_exif_orientation(path));
        let transform_options = match orientation {
            2 => WICBitmapTransformOptions(8),     // Flip Horizontal
            3 => WICBitmapTransformOptions(2),     // Rotate180
            4 => WICBitmapTransformOptions(16),    // Flip Vertical
            5 => WICBitmapTransformOptions(3 | 8), // Rotate270 | Flip Horizontal
            6 => WICBitmapTransformOptions(1),     // Rotate90
            7 => WICBitmapTransformOptions(1 | 8), // Rotate90 | Flip Horizontal
            8 => WICBitmapTransformOptions(3),     // Rotate270
            _ => WICBitmapTransformOptions(0),
        };

        let swap_wh = matches!(orientation, 5 | 6 | 7 | 8);
        let logical_width = if swap_wh { height } else { width };
        let logical_height = if swap_wh { width } else { height };

        let base_source: IWICBitmapSource =
            frame.cast().map_err(|e| format!("cast failed: {:?}", e))?;

        // --- PERFORMANCE FIX: Decoder Caching ---
        // JPEG/PNG decoders can be extremely slow when accessed non-linearly (e.g. during rotation).
        // Wrapping the source in a cache ensures the decoder is read linearly and the results
        // are reused, preventing O(N^2) thrashing in the Rotator.
        let cached_source = WicTiledSource::wrap_with_cache(&factory, &base_source);

        let mut final_source = cached_source.clone();

        if transform_options != WICBitmapTransformOptions(0) {
            if let Ok(rotator) = factory.CreateBitmapFlipRotator() {
                if rotator
                    .Initialize(&cached_source, transform_options)
                    .is_ok()
                {
                    if let Ok(src) = rotator.cast::<IWICBitmapSource>() {
                        final_source = src;
                    }
                }
            }
        }

        let pixel_count = logical_width as u64 * logical_height as u64;
        let tiled_limit = crate::tile_cache::TILED_THRESHOLD.load(Ordering::Relaxed);
        // For WIC, use the conservative 8192 limit for the tiling decision
        // rather than the GPU's actual limit (which may be 16384).
        // WIC's tiled source provides a much better UX for wide/tall images:
        // it shows an EXIF preview instantly while loading tiles in the background.
        // The GPU's real limit is used in make_image_data() for non-WIC images
        // that are already fully decoded in memory.
        let limit = crate::constants::ABSOLUTE_MAX_TEXTURE_SIDE;

        // If it's a RAW file, we ALWAYS want a fast preview path for the initial placeholder,
        let is_raw = crate::raw_processor::is_raw_extension(&ext);

        // PERFORMANCE OPTIMIZATION: If we are in performance mode (!high_quality)
        // for a RAW file, we prioritize returning a static preview immediately
        // to avoid all background tiling and refinement overhead.
        if is_raw && !high_quality {
            // Re-use the generate_preview logic but return it as ImageData::Static
            let temp_source = WicTiledSource {
                path: path.to_path_buf(),
                width: logical_width,
                height: logical_height,
                physical_width: width,
                physical_height: height,
                transform_options,
                factory: factory.clone(),
                decoder: decoder.clone(),
                frame: frame.clone(),
                source: final_source.clone(),
                stream: stream_out.clone(),
                _mmap: mmap_out.clone(),
            };
            let (pw, ph, p) = temp_source.generate_preview(
                crate::constants::MAX_QUALITY_PREVIEW_SIZE,
                crate::constants::MAX_QUALITY_PREVIEW_SIZE,
            );
            if pw > 0 && ph > 0 {
                log::debug!(
                    "WIC [Performance Mode]: Using static preview for RAW {:?}",
                    path
                );
                return Ok(crate::loader::ImageData::Static(
                    crate::loader::DecodedImage::new(pw, ph, p),
                ));
            }
        }

        if pixel_count >= tiled_limit || logical_width > limit || logical_height > limit || is_raw {
            // Virtualized path: Create a cached WIC bitmap source to avoid redundant O(N^2) decoding.
            // WICBitmapCacheOnDemand will keep decoded scanlines in memory as we request tiles.
            let cached_bitmap = factory
                .CreateBitmapFromSource(&final_source, WICBitmapCacheOnDemand)
                .map_err(|e| format!("failed to create cached bitmap: {:?}", e))?;
            let cached_source: IWICBitmapSource = cached_bitmap
                .cast()
                .map_err(|e| format!("cast failed: {:?}", e))?;

            return Ok(crate::loader::ImageData::Tiled(std::sync::Arc::new(
                WicTiledSource {
                    path: path.to_path_buf(),
                    width: logical_width,
                    height: logical_height,
                    physical_width: width,
                    physical_height: height,
                    transform_options,
                    factory: factory.clone(),
                    decoder: decoder,
                    frame: frame.clone(),
                    source: cached_source,
                    stream: stream_out,
                    _mmap: mmap_out,
                },
            )));
        }

        // --- Fallback for regular small images (Direct decode remains unchanged) ---
        let sc_converter = factory
            .CreateFormatConverter()
            .map_err(|e| format!("converter creation failed: {:?}", e))?;
        sc_converter
            .Initialize(
                &final_source,
                &GUID_WICPixelFormat32bppRGBA,
                WICBitmapDitherTypeNone,
                None,
                0.0,
                WICBitmapPaletteTypeCustom,
            )
            .map_err(|e| format!("converter init failed: {:?}", e))?;

        let stride = logical_width * 4;
        let mut out = vec![0u8; (stride * logical_height) as usize];
        let rect = WICRect {
            X: 0,
            Y: 0,
            Width: logical_width as i32,
            Height: logical_height as i32,
        };

        sc_converter
            .CopyPixels(&rect, stride, &mut out)
            .map_err(|e| format!("Pixel copy failed: {:?}", e))?;

        Ok(crate::loader::ImageData::Static(
            crate::loader::DecodedImage::new(logical_width, logical_height, out),
        ))
    }
}
