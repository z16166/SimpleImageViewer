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
use super::tiled_source::WicTiledSource;

use super::com::ComGuard;
use super::imports::*;
use std::sync::atomic::AtomicBool;

pub fn load_via_wic(
    path: &std::path::Path,
    high_quality: bool,
    orientation_override: Option<u16>,
    cancel: Option<&AtomicBool>,
) -> std::result::Result<crate::loader::ImageData, String> {
    load_via_wic_inner(
        path,
        high_quality,
        orientation_override,
        false,
        None,
        cancel,
    )
}

/// Decode by sniffing the bitstream (ISO BMFF / QuickTime / mislabeled extensions), not the path suffix.
pub fn load_via_wic_stream_sniff(
    path: &std::path::Path,
    high_quality: bool,
    orientation_override: Option<u16>,
    cancel: Option<&AtomicBool>,
) -> std::result::Result<crate::loader::ImageData, String> {
    load_via_wic_inner(path, high_quality, orientation_override, true, None, cancel)
}

/// Decode from an already-mapped file buffer (avoids reopening the file on recovery paths).
pub fn load_via_wic_from_mmap(
    path: &std::path::Path,
    mmap: std::sync::Arc<memmap2::Mmap>,
    high_quality: bool,
    orientation_override: Option<u16>,
    cancel: Option<&AtomicBool>,
) -> std::result::Result<crate::loader::ImageData, String> {
    load_via_wic_inner(
        path,
        high_quality,
        orientation_override,
        true,
        Some(mmap),
        cancel,
    )
}

fn load_via_wic_inner(
    path: &std::path::Path,
    high_quality: bool,
    orientation_override: Option<u16>,
    prefer_stream_sniff: bool,
    existing_mmap: Option<std::sync::Arc<memmap2::Mmap>>,
    cancel: Option<&AtomicBool>,
) -> std::result::Result<crate::loader::ImageData, String> {
    crate::loader::check_decode_cancel_str(cancel)?;
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

        if prefer_stream_sniff {
            let mmap_source = existing_mmap.or_else(|| {
                crate::mmap_util::map_file(path)
                    .ok()
                    .map(|(m, _)| std::sync::Arc::new(m))
            });
            if let Some(m_arc) = mmap_source {
                match factory.CreateStream() {
                    Ok(stream) => {
                        if stream.InitializeFromMemory(&m_arc[..]).is_ok() {
                            decoder_res = factory.CreateDecoderFromStream(
                                &stream,
                                std::ptr::null(),
                                WICDecodeMetadataCacheOnDemand,
                            );
                            if decoder_res.is_ok() {
                                stream_out = Some(stream);
                                mmap_out = Some(m_arc);
                            } else {
                                log::debug!(
                                    "[WIC] stream_sniff CreateDecoderFromStream failed for {:?}",
                                    path
                                );
                            }
                        } else {
                            log::debug!(
                                "[WIC] stream_sniff InitializeFromMemory failed for {:?}",
                                path
                            );
                        }
                    }
                    Err(e) => {
                        log::debug!(
                            "[WIC] stream_sniff CreateStream failed for {:?}: {:?}",
                            path,
                            e
                        );
                    }
                }
            } else {
                log::debug!("[WIC] stream_sniff map_file failed for {:?}", path);
            }
        }

        // --- Fast Path: Generic direct instantiation via registry CLSID (No sniffing) ---
        if !prefer_stream_sniff {
            let clsid_opt = get_registry()
                .read()
                .formats
                .iter()
                .find(|f| f.extension == ext)
                .and_then(|f| f.wic_clsid);

            if let Some(clsid_bytes) = clsid_opt {
                let mut clsid = GUID::default();
                std::ptr::copy_nonoverlapping(
                    clsid_bytes.as_ptr(),
                    &mut clsid as *mut GUID as *mut u8,
                    16,
                );

                let specific_decoder: Result<IWICBitmapDecoder> =
                    CoCreateInstance(&clsid, None, CLSCTX_INPROC_SERVER);
                if let Ok(sd) = specific_decoder
                    && let Ok(stream) = factory.CreateStream()
                {
                    // --- Mmap Path ---
                    if let Ok((mmap, _)) = crate::mmap_util::map_file(path) {
                        let m_arc = std::sync::Arc::new(mmap);
                        if stream.InitializeFromMemory(&m_arc[..]).is_ok()
                            && sd
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
        if let Err(ref e) = decoder_res
            && e.code() == windows::core::HRESULT(0x88982F50u32 as i32)
            && let Some(ext) = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_lowercase())
        {
            let clsid_bytes_opt = get_registry()
                .read()
                .formats
                .iter()
                .find(|f| f.extension == ext)
                .and_then(|f| f.wic_clsid);

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
                if let Ok(sd) = specific_decoder
                    && let Ok(stream) = factory.CreateStream()
                    && stream
                        .InitializeFromFilename(path_ptr, GENERIC_READ.0)
                        .is_ok()
                    && sd
                        .Initialize(&stream, WICDecodeMetadataCacheOnDemand)
                        .is_ok()
                {
                    decoder_res = Ok(sd);
                    stream_out = Some(stream);
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
                    let p = u64::from(w).checked_mul(u64::from(h)).unwrap_or(0);
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

        if width == 0 || height == 0 {
            return Err(format!("WIC image has zero dimensions ({width}x{height})"));
        }
        // Hard ceiling for WIC frame dimensions. Larger claims are treated as corrupt.
        let max_side = crate::constants::WIC_ABSOLUTE_MAX_SIDE;
        if width > max_side || height > max_side {
            return Err(format!(
                "WIC image dimensions {width}x{height} exceed maximum side {max_side}"
            ));
        }

        let orientation = orientation_override.unwrap_or_else(|| {
            mmap_out
                .as_ref()
                .map(|mmap| {
                    crate::metadata_utils::get_exif_orientation_from_bytes(&mmap[..], Some(path))
                })
                .unwrap_or(1)
        });
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

        let swap_wh = matches!(orientation, 5..=8);
        let logical_width = if swap_wh { height } else { width };
        let logical_height = if swap_wh { width } else { height };
        // Validate total pixel count before any allocation or tiled source creation.
        crate::constants::validate_static_decode_dimensions(logical_width, logical_height)?;

        let base_source: IWICBitmapSource =
            frame.cast().map_err(|e| format!("cast failed: {:?}", e))?;

        // --- PERFORMANCE FIX: Decoder Caching ---
        // JPEG/PNG decoders can be extremely slow when accessed non-linearly (e.g. during rotation).
        // Wrapping the source in a cache ensures the decoder is read linearly and the results
        // are reused, preventing O(N^2) thrashing in the Rotator.
        let cached_source = WicTiledSource::wrap_with_cache(&factory, &base_source);

        let mut final_source = cached_source.clone();

        if transform_options != WICBitmapTransformOptions(0)
            && let Ok(rotator) = factory.CreateBitmapFlipRotator()
            && rotator
                .Initialize(&cached_source, transform_options)
                .is_ok()
            && let Ok(src) = rotator.cast::<IWICBitmapSource>()
        {
            final_source = src;
        }

        // Configurable tiled-routing side limit (default 8192), not the GPU upload cap.
        // WIC's tiled source provides a much better UX for wide/tall images:
        // it shows an EXIF preview instantly while loading tiles in the background.

        // If it's a RAW file, we ALWAYS want a fast preview path for the initial placeholder,
        let is_raw = crate::raw_processor::is_raw_extension(&ext);

        // PERFORMANCE OPTIMIZATION: If we are in performance mode (!high_quality)
        // for a RAW file, we prioritize returning a static preview immediately
        // to avoid all background tiling and refinement overhead.
        if is_raw && !high_quality {
            // Re-use the generate_preview logic but return it as ImageData::Static
            crate::loader::check_decode_cancel_str(cancel)?;
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
            let lim = crate::loader::hq_preview_max_side();
            let (pw, ph, p) = temp_source.generate_preview(lim, lim);
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

        if crate::tile_cache::image_requires_tiled_plane(logical_width, logical_height) || is_raw {
            // Virtualized path: Create a cached WIC bitmap source to avoid redundant O(N^2) decoding.
            // WICBitmapCacheOnDemand will keep decoded scanlines in memory as we request tiles.
            crate::loader::check_decode_cancel_str(cancel)?;
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
                    decoder,
                    frame: frame.clone(),
                    source: cached_source,
                    stream: stream_out,
                    _mmap: mmap_out,
                },
            )));
        }

        // --- Fallback for regular small images (Direct decode remains unchanged) ---
        crate::loader::check_decode_cancel_str(cancel)?;
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

        let stride = logical_width
            .checked_mul(4)
            .ok_or_else(|| format!("WIC stride overflow: width={logical_width}"))?;
        let mut out = {
            let buf_size = stride
                .checked_mul(logical_height)
                .ok_or_else(|| format!("WIC buffer size overflow: stride={stride}"))?;
            vec![0u8; buf_size as usize]
        };
        let rect = WICRect {
            X: 0,
            Y: 0,
            Width: logical_width as i32,
            Height: logical_height as i32,
        };

        sc_converter
            .CopyPixels(&rect, stride, &mut out)
            .map_err(|e| format!("Pixel copy failed: {:?}", e))?;
        crate::loader::check_decode_cancel_str(cancel)?;

        Ok(crate::loader::ImageData::Static(
            crate::loader::DecodedImage::new(logical_width, logical_height, out),
        ))
    }
}
