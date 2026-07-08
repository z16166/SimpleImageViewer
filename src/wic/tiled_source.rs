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

use super::com::ComGuard;
/// A tiled source for Windows Imaging Component (WIC) decoders.
/// Allows on-demand decoding of any WIC-supported format without full memory allocation.
pub struct WicTiledSource {
    pub(crate) path: std::path::PathBuf,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) factory: IWICImagingFactory,
    pub(crate) decoder: IWICBitmapDecoder,
    pub(crate) frame: IWICBitmapFrameDecode,
    pub(crate) source: IWICBitmapSource,
    pub(crate) physical_width: u32,
    pub(crate) physical_height: u32,
    pub(crate) transform_options: WICBitmapTransformOptions,
    // Removed shared Mutex converter to enable true thread-parallel decoding.
    // Each thread will now create its own local converter in extract_tile.
    // Explicitly keep the stream alive to prevent data source invalidation
    #[allow(dead_code)]
    pub(crate) stream: Option<IWICStream>,
    // Removed shared converter to enable thread-parallel decoding
    // _mmap MUST be at the end to ensure it is dropped AFTER the WIC objects
    pub(crate) _mmap: Option<std::sync::Arc<memmap2::Mmap>>,
}

impl WicTiledSource {
    // extract_tile handles its own converter initialization to avoid deadlocks.

    /// Wraps a source in a WICBitmapCacheOnDemand to prevent decoder thrashing during transforms.
    pub(crate) fn wrap_with_cache(
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

    /// Flip/rotate per `transform_options` when non-zero; on error returns `cached` unchanged.
    fn apply_wic_transform(
        factory: &IWICImagingFactory,
        cached: IWICBitmapSource,
        transform_options: WICBitmapTransformOptions,
    ) -> IWICBitmapSource {
        if transform_options == WICBitmapTransformOptions(0) {
            return cached;
        }
        unsafe {
            let Ok(rotator) = factory.CreateBitmapFlipRotator() else {
                return cached;
            };
            if rotator.Initialize(&cached, transform_options).is_err() {
                return cached;
            }
            rotator.cast::<IWICBitmapSource>().unwrap_or(cached)
        }
    }

    /// Downscaled multi-frame preview (e.g. pyramid TIFF): pick a suitable secondary frame and render.
    fn try_get_preview_from_secondary_frame(
        &self,
        factory: &IWICImagingFactory,
    ) -> Option<(u32, u32, Vec<u8>)> {
        let Ok(frame_count) = (unsafe { self.decoder.GetFrameCount() }) else {
            return None;
        };
        if frame_count <= 1 {
            return None;
        }
        for frame_index in 1..frame_count {
            let Ok(f) = (unsafe { self.decoder.GetFrame(frame_index) }) else {
                continue;
            };
            let mut fw = 0;
            let mut fh = 0;
            if unsafe { f.GetSize(&mut fw, &mut fh) }.is_err() {
                continue;
            }
            if fw <= 512 || fw >= self.physical_width / 2 {
                continue;
            }
            let Ok(f_src) = f.cast::<IWICBitmapSource>() else {
                continue;
            };
            let f_cached = Self::wrap_with_cache(factory, &f_src);
            let f_final = Self::apply_wic_transform(factory, f_cached, self.transform_options);
            let Some(res) = render_source_to_pixels(&f_final, factory) else {
                continue;
            };
            log::debug!(
                "WIC: Using secondary frame {} as preview ({}x{})",
                frame_index,
                fw,
                fh
            );
            return Some(res);
        }
        None
    }

    fn generate_preview_internal(
        &self,
        max_w: u32,
        max_h: u32,
        allow_embedded: bool,
    ) -> (u32, u32, Vec<u8>) {
        let scale = (max_w as f64 / self.width as f64)
            .min(max_h as f64 / self.height as f64)
            .min(1.0);
        let out_w = (self.width as f64 * scale).round().max(1.0) as u32;
        let out_h = (self.height as f64 * scale).round().max(1.0) as u32;

        unsafe {
            let factory = &self.factory;

            if allow_embedded {
                // Path 1: Extract embedded thumbnail if large enough and aspect matches logical size.
                if let Ok(thumbnail) = self.frame.GetThumbnail() {
                    let mut fw = 0;
                    let mut fh = 0;
                    if thumbnail.GetSize(&mut fw, &mut fh).is_ok() && fw >= out_w && fh >= out_h {
                        if let Ok(thumb_src) = thumbnail.cast::<IWICBitmapSource>() {
                            let thumb_cached = Self::wrap_with_cache(factory, &thumb_src);
                            let thumb_final = Self::apply_wic_transform(
                                factory,
                                thumb_cached,
                                self.transform_options,
                            );
                            if let Some((pw, ph, pixels)) =
                                render_source_to_pixels(&thumb_final, factory)
                            {
                                if crate::loader::preview_aspect_matches_logical(
                                    pw,
                                    ph,
                                    self.width,
                                    self.height,
                                ) {
                                    log::debug!(
                                        "WIC [Idx={}]: Using embedded thumbnail as preview ({}x{})",
                                        self.path.display(),
                                        pw,
                                        ph
                                    );
                                    return (pw, ph, pixels);
                                }
                                log::debug!(
                                    "WIC [Idx={}]: Skipping embedded thumbnail due to aspect mismatch ({}x{} vs {}x{})",
                                    self.path.display(),
                                    pw,
                                    ph,
                                    self.width,
                                    self.height
                                );
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

                if let Some((pw, ph, pixels)) = self.try_get_preview_from_secondary_frame(factory) {
                    if crate::loader::preview_aspect_matches_logical(
                        pw,
                        ph,
                        self.width,
                        self.height,
                    ) {
                        return (pw, ph, pixels);
                    }
                    log::debug!(
                        "WIC [Idx={}]: Skipping secondary frame preview due to aspect mismatch ({}x{} vs {}x{})",
                        self.path.display(),
                        pw,
                        ph,
                        self.width,
                        self.height
                    );
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
                        log::debug!(
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
                            if crate::loader::preview_aspect_matches_logical(
                                log_final_w,
                                log_final_h,
                                self.width,
                                self.height,
                            ) {
                                return (log_final_w, log_final_h, out);
                            }
                            log::debug!(
                                "WIC [Idx={}]: Skipping Native Source Transform due to aspect mismatch ({}x{} vs {}x{})",
                                self.path.display(),
                                log_final_w,
                                log_final_h,
                                self.width,
                                self.height
                            );
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
            log::debug!(
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
}

// `Send`/`Sync`: COM pointers are opaque to rustc. We expose `Arc<WicTiledSource>` to tile workers
// that decode in parallel (`extract_tile`).
//
// Threading contract (see Microsoft Learn, "Multi-threaded apartment support in WIC"):
// - Workers use [`ComGuard`] (`CoInitializeEx` + `COINIT_MULTITHREADED`, i.e. MTA), matching WIC's
//   documented model for concurrent calls from multiple threads inside the MTA—not STA.
// - In-box WIC codecs from Windows 7 onward are documented for MTA; third-party codecs may vary.
//
// This is still `unsafe`: Rust cannot prove COM/WIC or arbitrary decoder DLLs are free of data
// races; correctness relies on the above coinit discipline and in-box codec behavior. Third-party
// WIC codecs may not be MTA-safe -- prefer static decode or a serial tile path when using them.
unsafe impl Send for WicTiledSource {}
unsafe impl Sync for WicTiledSource {}

impl crate::loader::TiledImageSource for WicTiledSource {
    fn width(&self) -> u32 {
        self.width
    }
    fn height(&self) -> u32 {
        self.height
    }

    fn extract_tile(&self, x: u32, y: u32, w: u32, h: u32) -> std::sync::Arc<Vec<u8>> {
        thread_local! {
            static WIC_TILE_CONVERTER: std::cell::RefCell<Option<IWICFormatConverter>> =
                const { std::cell::RefCell::new(None) };
        }

        let mut pixels = vec![0u8; (w * h * 4) as usize];
        let stride = w * 4;

        // Ensure COM is initialized on the current worker thread
        let _com = match ComGuard::new() {
            Ok(guard) => guard,
            Err(err) => {
                log::error!("[WIC] COM init failed in extract_tile: {err:?}");
                return std::sync::Arc::new(pixels);
            }
        };

        unsafe {
            let init_ok = WIC_TILE_CONVERTER.with(|slot| {
                let mut slot = slot.borrow_mut();
                if slot.is_none() {
                    *slot = self.factory.CreateFormatConverter().ok();
                }
                if let Some(converter) = slot.as_ref() {
                    converter
                        .Initialize(
                            &self.source,
                            &GUID_WICPixelFormat32bppRGBA,
                            WICBitmapDitherTypeNone,
                            None,
                            0.0,
                            WICBitmapPaletteTypeCustom,
                        )
                        .is_ok()
                } else {
                    false
                }
            });
            if init_ok {
                WIC_TILE_CONVERTER.with(|slot| {
                    if let Some(converter) = slot.borrow().as_ref() {
                        let rect = WICRect {
                            X: x as i32,
                            Y: y as i32,
                            Width: w as i32,
                            Height: h as i32,
                        };
                        if let Err(err) = converter.CopyPixels(&rect, stride, &mut pixels) {
                            log::warn!(
                                "[WIC] CopyPixels failed for tile ({x},{y}) {w}x{h}: {err:?}"
                            );
                        }
                    }
                });
            }
        }

        std::sync::Arc::new(pixels)
    }

    fn generate_preview(&self, max_w: u32, max_h: u32) -> (u32, u32, Vec<u8>) {
        self.generate_preview_internal(max_w, max_h, true)
    }

    fn generate_full_image_preview(&self, max_w: u32, max_h: u32) -> (u32, u32, Vec<u8>) {
        self.generate_preview_internal(max_w, max_h, false)
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
