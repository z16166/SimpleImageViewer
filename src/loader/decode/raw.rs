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

//! LibRAW and raw tiled refinement.
//!
//! `raw_high_quality` controls whether LibRaw's expensive demosaic runs:
//! - **Off:** use embedded previews whenever present (SDR pipeline on all displays).
//!   Full develop only when the file has no embedded preview; on HDR displays that
//!   develop result uses the HDR pipeline.
//! - **On:** use embedded previews when they meet HQ size requirements; otherwise
//!   demosaic and downscale to [`hq_preview_max_side`] (2048/4096 depending on monitor).
//!   Developed pixels use the HDR pipeline on HDR displays.

use crate::hdr::types::HdrToneMapSettings;
use crate::loader::preview_caps::{
    finalize_raw_hq_developed_image, finalize_raw_hq_hdr_buffer, hq_preview_max_side,
};
use crate::loader::tiled_sources::{RawHdrRefiningSource, RawImageSource};
use crate::loader::{
    DecodedImage, ImageData, RawLoadOutput, RefinementRequest, hdr_display_requests_sdr_preview,
    hdr_sdr_fallback_rgba8_eager_or_placeholder,
};
use crate::loader::raw_osd::{RawOsdContext, RawOsdInfo};
use crate::raw_processor::RawProcessor;
use crossbeam_channel::Sender;
use parking_lot::RwLock as PLRwLock;
use std::path::PathBuf;
use std::sync::Arc;

use super::assemble::{make_hdr_image_data, make_image_data};

/// True when an embedded preview is large enough to substitute for a full demosaic.
fn raw_embedded_preview_covers_sensor(preview: &DecodedImage, raw_w: u32, raw_h: u32) -> bool {
    let pw = preview.width as u64;
    let ph = preview.height as u64;
    let rw = raw_w as u64;
    let rh = raw_h as u64;
    if rw == 0 || rh == 0 {
        return false;
    }
    let sensor_px = rw * rh;
    let preview_px = pw * ph;
    // Orientation may swap axes; accept either mapping.
    let axis_cover = (pw >= rw && ph >= rh) || (pw >= rh && ph >= rw);
    preview_px * 10 >= sensor_px * 8 || axis_cover
}

/// Embedded preview is sharp enough for high-quality browsing without demosaicing.
///
/// Requires either monitor HQ cap (2048/4096) or a near-full sensor JPEG — tiny thumbs like
/// Epson ERF 640×424 must not pass just because LibRaw reported matching `iwidth`/`iheight`.
fn raw_embedded_preview_meets_hq_requirement(preview: &DecodedImage, raw_w: u32, raw_h: u32) -> bool {
    let hq_side = hq_preview_max_side();
    let preview_long = preview.width.max(preview.height);
    if preview_long >= hq_side {
        return true;
    }
    // Accept camera full-size embedded JPEGs that are slightly below the monitor HQ cap.
    let hq_floor = (hq_side / 2).max(1024);
    preview_long >= hq_floor && raw_embedded_preview_covers_sensor(preview, raw_w, raw_h)
}

fn apply_orientation_to_embedded_preview(
    mut preview: DecodedImage,
    final_orientation: u16,
) -> DecodedImage {
    if final_orientation <= 1 {
        return preview;
    }
    let pixels = preview.take_rgba_owned();
    if let Some(rgba) = image::RgbaImage::from_raw(preview.width, preview.height, pixels) {
        let mut img = image::DynamicImage::ImageRgba8(rgba);
        match final_orientation {
            2 => img = img.fliph(),
            3 => img = img.rotate180(),
            4 => img = img.flipv(),
            5 => img = img.fliph().rotate270(),
            6 => img = img.rotate90(),
            7 => img = img.fliph().rotate90(),
            8 => img = img.rotate270(),
            _ => {}
        }
        let rgba_rotated = img.to_rgba8();
        preview.set_rgba_buffer(
            rgba_rotated.width(),
            rgba_rotated.height(),
            rgba_rotated.into_raw(),
        );
    }
    preview
}

fn extract_embedded_preview(
    processor: &mut RawProcessor,
    path: &PathBuf,
    final_orientation: u16,
) -> Option<DecodedImage> {
    let mut preview = processor.unpack_thumb().ok()?;
    preview = apply_orientation_to_embedded_preview(preview, final_orientation);
    if preview.width == 0 || preview.height == 0 {
        log::warn!(
            "[Loader] Preview path returned a zero-dimension image for {:?}. Invalidate and fallback.",
            path.file_name().unwrap_or_default()
        );
        return None;
    }
    Some(preview)
}

/// Demosaic at full sensor resolution (only when no embedded preview exists).
fn develop_full_resolution(
    processor: &mut RawProcessor,
    path: &PathBuf,
    width: u32,
    height: u32,
    area: u64,
    threshold: u64,
    refine_tx: Sender<RefinementRequest>,
    final_lr_flip: i32,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
    osd_ctx: &RawOsdContext,
) -> Result<RawLoadOutput, String> {
    if area < threshold
        && width <= crate::constants::ABSOLUTE_MAX_TEXTURE_SIDE
        && height <= crate::constants::ABSOLUTE_MAX_TEXTURE_SIDE
    {
        log::info!(
            "[Loader] RAW {}x{} ({:.1} MP) — full develop (no embedded preview).",
            width,
            height,
            area as f64 / 1_000_000.0
        );

        if !hdr_display_requests_sdr_preview(hdr_target_capacity) {
            if let Ok(hdr) = processor.develop_scene_linear_hdr() {
                let warnings = processor.process_warnings();
                if warnings != 0 {
                    log::info!(
                        "[Loader] LibRaw reported informational warnings (0x{:x}) for {:?}, proceeding with native pixels.",
                        warnings,
                        path
                    );
                }

                if hdr.width == 0 || hdr.height == 0 {
                    log::error!(
                        "[Loader] LibRaw developed a zero-dimension HDR image for {:?}. Falling through.",
                        path
                    );
                } else {
                    let hw = hdr.width;
                    let hh = hdr.height;
                    let fallback_pixels = hdr_sdr_fallback_rgba8_eager_or_placeholder(
                        &hdr,
                        hdr_target_capacity,
                        &hdr_tone_map,
                    )?;
                    let fallback =
                        DecodedImage::from_arc(hw, hh, fallback_pixels);
                    return Ok(RawLoadOutput {
                        image: make_hdr_image_data(hdr, fallback),
                        osd: osd_ctx.full_develop(hw, hh),
                    });
                }
            } else {
                log::error!(
                    "[Loader] RAW scene-linear HDR develop failed for {:?}. Falling back to SDR develop.",
                    path
                );
            }
        }

        match processor.develop() {
            Ok(img) => {
                let rgba = img.to_rgba8();
                return Ok(RawLoadOutput {
                    image: make_image_data(DecodedImage::from(rgba.clone())),
                    osd: osd_ctx.full_develop(rgba.width(), rgba.height()),
                });
            }
            Err(e) => {
                log::error!(
                    "[Loader] LibRaw develop failed for {:?}: {}. Falling through to tiled fallback.",
                    path,
                    e
                );
            }
        }
    }

    log::warn!(
        "[Loader] All fast RAW thumbnail paths failed for {:?}. Falling back to slow development...",
        path.file_name().unwrap_or_default()
    );
    let preview = processor.develop()?.to_rgba8().into();
    // Performance mode only (`load_raw` with `!high_quality`). Never queue HQ refinement.
    let source = Arc::new(RawImageSource::new(
        path.clone(),
        preview,
        width,
        height,
        refine_tx,
        final_lr_flip,
        false,
        hdr_target_capacity,
        hdr_tone_map,
        None,
    )?);

    log::info!(
        "[Loader] RAW {}x{} ({:.1} MP) — tiled fallback after failed full develop.",
        width,
        height,
        area as f64 / 1_000_000.0
    );
    Ok(RawLoadOutput {
        image: ImageData::Tiled(source),
        osd: osd_ctx.full_develop(width, height),
    })
}

/// Demosaic once, then downscale to monitor HQ cap. Used when HQ mode needs better pixels
/// than the embedded preview provides, or when HQ mode has no embedded preview at all.
///
/// Intentionally **does not** check [`crate::tile_cache::TILED_THRESHOLD`]: HQ without an
/// embedded bootstrap is a rare sync path where quality beats loader latency. Demosaic still
/// runs at full sensor resolution; only the stored preview is capped. Very large sensors may
/// block the loader thread for several seconds — prefer [`load_raw_with_embedded_bootstrap`]
/// when an embedded thumb exists.
fn develop_hq_preview(
    processor: &mut RawProcessor,
    _path: &PathBuf,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
    osd_ctx: &RawOsdContext,
) -> Result<RawLoadOutput, String> {
    crate::preload_debug!(
        "[PreloadDebug][RAW] sync HqDevelop path={:?} limit={} hdr={}",
        _path.file_name().unwrap_or_default(),
        hq_preview_max_side(),
        !hdr_display_requests_sdr_preview(hdr_target_capacity)
    );

    if !hdr_display_requests_sdr_preview(hdr_target_capacity) {
        let hdr = processor.develop_scene_linear_hdr()?;
        let (logical_w, logical_h) = processor.developed_output_dimensions(None);
        let hdr = finalize_raw_hq_hdr_buffer(hdr, logical_w, logical_h)?;
        let fallback_pixels = hdr_sdr_fallback_rgba8_eager_or_placeholder(
            &hdr,
            hdr_target_capacity,
            &hdr_tone_map,
        )?;
        let fallback = DecodedImage::from_arc(hdr.width, hdr.height, fallback_pixels);
        let osd = if hdr.width.abs_diff(osd_ctx.sensor_size().0) <= 2
            && hdr.height.abs_diff(osd_ctx.sensor_size().1) <= 2
        {
            osd_ctx.full_develop(hdr.width, hdr.height)
        } else {
            osd_ctx.hq_develop(hdr.width, hdr.height)
        };
        return Ok(RawLoadOutput {
            image: make_hdr_image_data(hdr, fallback),
            osd,
        });
    }

    let img = processor.develop()?;
    let (logical_w, logical_h) = processor.developed_output_dimensions(None);
    let finalized = finalize_raw_hq_developed_image(img, logical_w, logical_h);
    let rgba = finalized.to_rgba8();
    let (pw, ph) = (rgba.width(), rgba.height());
    let osd = if pw.abs_diff(osd_ctx.sensor_size().0) <= 2 && ph.abs_diff(osd_ctx.sensor_size().1) <= 2
    {
        osd_ctx.full_develop(pw, ph)
    } else {
        osd_ctx.hq_develop(pw, ph)
    };
    Ok(RawLoadOutput {
        image: make_image_data(DecodedImage::from(rgba)),
        osd,
    })
}

fn load_raw_with_embedded_bootstrap(
    path: PathBuf,
    preview: DecodedImage,
    width: u32,
    height: u32,
    refine_tx: Sender<RefinementRequest>,
    final_lr_flip: i32,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
    osd_ctx: &RawOsdContext,
) -> Result<RawLoadOutput, String> {
    let use_hdr = !hdr_display_requests_sdr_preview(hdr_target_capacity);
    let hdr_buffer_slot = if use_hdr {
        Some(Arc::new(PLRwLock::new(None)))
    } else {
        None
    };

    let bootstrap_w = preview.width;
    let bootstrap_h = preview.height;

    let source = Arc::new(RawImageSource::new(
        path.clone(),
        preview,
        width,
        height,
        refine_tx,
        final_lr_flip,
        true,
        hdr_target_capacity,
        hdr_tone_map,
        hdr_buffer_slot.clone(),
    )?);

    crate::preload_debug!(
        "[PreloadDebug][RAW] TiledBootstrap logical={}x{} refine=true hdr={} hdr_cap={:.3}",
        width,
        height,
        use_hdr,
        hdr_target_capacity
    );

    if use_hdr {
        let hdr_slot = hdr_buffer_slot.expect("hdr slot when use_hdr");
        let hdr_source = Arc::new(RawHdrRefiningSource::new(
            hdr_slot,
            width,
            height,
        )) as Arc<dyn crate::hdr::tiled::HdrTiledSource>;
        return Ok(RawLoadOutput {
            image: ImageData::HdrTiled {
                hdr: hdr_source,
                fallback: source,
            },
            osd: osd_ctx.hq_bootstrap_dims(bootstrap_w, bootstrap_h),
        });
    }

    Ok(RawLoadOutput {
        image: ImageData::Tiled(source),
        osd: osd_ctx.hq_bootstrap_dims(bootstrap_w, bootstrap_h),
    })
}

pub(crate) fn load_raw(
    _index: usize,
    _generation: u64,
    path: &PathBuf,
    refine_tx: Sender<RefinementRequest>,
    high_quality: bool,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
) -> Result<RawLoadOutput, String> {
    let mut processor =
        RawProcessor::new().ok_or_else(|| rust_i18n::t!("error.libraw_init").to_string())?;
    if let Err(e) = processor.open(path) {
        log::warn!(
            "[Loader] LibRaw could not open {:?}: {}. Falling back to Rule 2 (WIC/ImageIO).",
            path,
            e
        );
        #[cfg(target_os = "windows")]
        return crate::wic::load_via_wic(path, high_quality, None).map(|image| RawLoadOutput {
            image,
            osd: RawOsdInfo::empty(),
        });
        #[cfg(target_os = "macos")]
        return crate::macos_image_io::load_via_image_io(path, high_quality, None).map(|image| {
            RawLoadOutput {
                image,
                osd: RawOsdInfo::empty(),
            }
        });
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        return Err(format!(
            "LibRaw failed and no platform fallback available: {}",
            e
        ));
    }

    let lr_flip = processor.flip();
    let final_orientation = match lr_flip {
        0 => 1,
        1 => 2,
        2 => 4,
        3 => 3,
        4 => 5,
        5 => 8,
        6 => 6,
        7 => 7,
        _ => crate::metadata_utils::get_exif_orientation(path),
    };

    let final_lr_flip = match final_orientation {
        1 => 0,
        2 => 1,
        3 => 3,
        4 => 2,
        5 => 4,
        6 => 6,
        7 => 7,
        8 => 5,
        _ => 0,
    };
    processor.set_user_flip(final_lr_flip);

    let preview_opt = extract_embedded_preview(&mut processor, path, final_orientation);
    let (width, height) = processor.developed_output_dimensions(preview_opt.as_ref());
    let area = width as u64 * height as u64;
    let threshold = crate::tile_cache::TILED_THRESHOLD.load(std::sync::atomic::Ordering::Relaxed);
    let osd_ctx = RawOsdContext::new((width, height), preview_opt.as_ref());

    if !high_quality {
        if let Some(p) = preview_opt {
            crate::preload_debug!(
                "[PreloadDebug][RAW] path={:?} mode=performance embedded={}x{} output={}x{} → StaticSdr",
                path.file_name().unwrap_or_default(),
                p.width,
                p.height,
                width,
                height
            );
            log::debug!(
                "[Loader] Performance mode: embedded preview for {:?} ({}x{}, sensor {}x{})",
                path.file_name().unwrap_or_default(),
                p.width,
                p.height,
                width,
                height
            );
            return Ok(RawLoadOutput {
                image: make_image_data(p.clone()),
                osd: osd_ctx.embedded_render(&p),
            });
        }
        crate::preload_debug!(
            "[PreloadDebug][RAW] path={:?} mode=performance no_embedded output={}x{} → full develop",
            path.file_name().unwrap_or_default(),
            width,
            height
        );
        return develop_full_resolution(
            &mut processor,
            path,
            width,
            height,
            area,
            threshold,
            refine_tx,
            final_lr_flip,
            hdr_target_capacity,
            hdr_tone_map,
            &osd_ctx,
        );
    }

    // High-quality mode: use embedded preview when it already meets HQ requirements.
    if let Some(ref p) = preview_opt {
        if raw_embedded_preview_meets_hq_requirement(p, width, height) {
            crate::preload_debug!(
                "[PreloadDebug][RAW] path={:?} mode=hq embedded={}x{} output={}x{} hq_side={} meets_hq=true → StaticSdr",
                path.file_name().unwrap_or_default(),
                p.width,
                p.height,
                width,
                height,
                hq_preview_max_side()
            );
            log::debug!(
                "[Loader] HQ mode: embedded preview meets size requirement for {:?} ({}x{} vs output {}x{})",
                path.file_name().unwrap_or_default(),
                p.width,
                p.height,
                width,
                height
            );
            return Ok(RawLoadOutput {
                image: make_image_data(p.clone()),
                osd: osd_ctx.embedded_render(p),
            });
        }
        crate::preload_debug!(
            "[PreloadDebug][RAW] path={:?} mode=hq embedded={}x{} output={}x{} hq_side={} meets_hq=false → TiledBootstrap+Refine",
            path.file_name().unwrap_or_default(),
            p.width,
            p.height,
            width,
            height,
            hq_preview_max_side()
        );
        log::debug!(
            "[Loader] HQ mode: embedded preview {}x{} insufficient for output {}x{} — HQ demosaic queued",
            p.width,
            p.height,
            width,
            height
        );
    }

    // HQ mode needs demosaic. Bootstrap with embedded preview when available.
    if let Some(p) = preview_opt {
        return load_raw_with_embedded_bootstrap(
            path.clone(),
            p,
            width,
            height,
            refine_tx,
            final_lr_flip,
            hdr_target_capacity,
            hdr_tone_map,
            &osd_ctx,
        );
    }

    crate::preload_debug!(
        "[PreloadDebug][RAW] path={:?} mode=hq no_embedded output={}x{} hq_side={} → sync HqDevelop",
        path.file_name().unwrap_or_default(),
        width,
        height,
        hq_preview_max_side()
    );
    develop_hq_preview(
        &mut processor,
        path,
        hdr_target_capacity,
        hdr_tone_map,
        &osd_ctx,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hdr::types::HdrToneMapSettings;
    use crate::loader::DecodedImage;
    use crate::loader::ImageData;
    use std::path::PathBuf;

    #[test]
    fn nikon1_embedded_thumb_covers_sensor() {
        let thumb = DecodedImage::new(3872, 2592, vec![0; 3872 * 2592 * 4]);
        assert!(raw_embedded_preview_covers_sensor(&thumb, 3904, 2604));
        assert!(raw_embedded_preview_meets_hq_requirement(&thumb, 3904, 2604));
    }

    #[test]
    fn performance_mode_uses_small_embedded_on_hdr_display_capacity() {
        let thumb = DecodedImage::new(1616, 1080, vec![0; 1616 * 1080 * 4]);
        assert!(!raw_embedded_preview_covers_sensor(&thumb, 6000, 4000));
        assert!(!raw_embedded_preview_meets_hq_requirement(&thumb, 6000, 4000));
        // Performance path returns embedded regardless of HDR capacity — verified by load_raw
        // integration; size helpers document the HQ vs performance distinction here.
    }

    #[test]
    fn epson_small_embedded_thumb_does_not_satisfy_hq_requirement() {
        // Epson R-D1 style: 640×424 embedded JPEG vs ~2240×1680 developed output.
        let thumb = DecodedImage::new(640, 424, vec![0; 640 * 424 * 4]);
        assert!(!raw_embedded_preview_covers_sensor(&thumb, 2240, 1680));
        assert!(!raw_embedded_preview_meets_hq_requirement(&thumb, 2240, 1680));
    }

    #[test]
    fn epson_rd1_erf_embedded_does_not_skip_hq_demosaic_when_file_present() {
        let path = PathBuf::from(r"F:\win7\raws\epson\rd1\RAW_EPSON_RD1.ERF");
        if !path.is_file() {
            eprintln!("skip: {}", path.display());
            return;
        }

        let mut processor = RawProcessor::new().expect("libraw init");
        processor.open(&path).expect("libraw open");
        let thumb = processor
            .unpack_thumb()
            .expect("epson rd1 should ship an embedded thumb");
        let (iw, ih) = (processor.width(), processor.height());
        let (rw, rh) = (processor.raw_width(), processor.raw_height());
        let (out_w, out_h) = processor.developed_output_dimensions(Some(&thumb));

        eprintln!(
            "ERF thumb={}x{} iwidth/iheight={}x{} raw={}x{} developed_output={}x{} hq_side={}",
            thumb.width,
            thumb.height,
            iw,
            ih,
            rw,
            rh,
            out_w,
            out_h,
            hq_preview_max_side()
        );

        assert_eq!(
            (thumb.width, thumb.height),
            (640, 424),
            "unexpected embedded preview size for RD1 sample"
        );
        assert!(
            out_w > thumb.width && out_h > thumb.height,
            "developed output should exceed embedded thumb (got {out_w}x{out_h})"
        );
        assert!(
            !raw_embedded_preview_meets_hq_requirement(&thumb, out_w, out_h),
            "640×424 thumb must not satisfy HQ requirement for RD1"
        );
    }

    #[test]
    fn epson_rd1_erf_hq_load_uses_tiled_bootstrap_when_file_present() {
        use crossbeam_channel::unbounded;

        let path = PathBuf::from(r"F:\win7\raws\epson\rd1\RAW_EPSON_RD1.ERF");
        if !path.is_file() {
            eprintln!("skip: {}", path.display());
            return;
        }

        let (refine_tx, refine_rx) = unbounded();
        let result = load_raw(
            0,
            0,
            &path,
            refine_tx,
            true,
            4.0,
            HdrToneMapSettings::default(),
        )
        .expect("load_raw hq");

        match result.image {
            ImageData::HdrTiled { fallback, .. } | ImageData::Tiled(fallback) => {
                assert_eq!(fallback.width(), 3040);
                assert_eq!(fallback.height(), 2024);
                eprintln!("HQ load: tiled bootstrap {}x{}", fallback.width(), fallback.height());
            }
            other => panic!(
                "expected tiled HQ bootstrap, got {:?}",
                std::mem::discriminant(&other)
            ),
        }
        assert!(
            refine_rx.try_recv().is_err(),
            "refinement is deferred until the image becomes active"
        );
    }

    #[test]
    fn epson_rd1_erf_performance_load_uses_embedded_static_when_file_present() {
        use crossbeam_channel::unbounded;

        let path = PathBuf::from(r"F:\win7\raws\epson\rd1\RAW_EPSON_RD1.ERF");
        if !path.is_file() {
            eprintln!("skip: {}", path.display());
            return;
        }

        let (refine_tx, _refine_rx) = unbounded();
        let result = load_raw(
            0,
            0,
            &path,
            refine_tx,
            false,
            4.0,
            HdrToneMapSettings::default(),
        )
        .expect("load_raw perf");

        match result.image {
            ImageData::Static(img) => {
                assert_eq!(img.width, 640);
                assert_eq!(img.height, 424);
                eprintln!("Perf load: static embedded {}x{}", img.width, img.height);
            }
            other => panic!(
                "expected static embedded in performance mode, got {:?}",
                std::mem::discriminant(&other)
            ),
        }
    }

    #[test]
    fn probe_epson_and_fuji_on_local_samples() {
        // Simulate 5120×1440 HDR monitor HQ cap (physical long edge × 1.1, capped at 4096).
        crate::loader::MONITOR_PREVIEW_CAP.store(4096, std::sync::atomic::Ordering::Relaxed);

        for (path, name) in [
            (
                PathBuf::from(r"F:\win7\raws\epson\rd1\RAW_EPSON_RD1.ERF"),
                "epson_rd1",
            ),
            (
                PathBuf::from(r"F:\win7\raws\fuji\RAW_FUIJI_X-E2.RAF"),
                "fuji_xe2",
            ),
            (
                PathBuf::from(r"F:\win7\raws\canon\RAW_CANON_S90.CR2"),
                "canon_s90",
            ),
        ] {
            if !path.is_file() {
                eprintln!("skip {name}: {}", path.display());
                continue;
            }

            let mut processor = RawProcessor::new().expect("libraw init");
            processor.open(&path).expect("open");
            let thumb = processor.unpack_thumb().ok();
            let (out_w, out_h) = processor.developed_output_dimensions(thumb.as_ref());

            eprintln!("\n=== {name} ===");
            eprintln!(
                "i={}x{} raw={}x{} out={}x{} hq_side={}",
                processor.width(),
                processor.height(),
                processor.raw_width(),
                processor.raw_height(),
                out_w,
                out_h,
                hq_preview_max_side()
            );
            if let Some(ref t) = thumb {
                eprintln!(
                    "thumb={}x{} covers={} meets_hq={}",
                    t.width,
                    t.height,
                    raw_embedded_preview_covers_sensor(t, out_w, out_h),
                    raw_embedded_preview_meets_hq_requirement(t, out_w, out_h)
                );
            }

            for (label, hq) in [("performance", false), ("high_quality", true)] {
                let (refine_tx, _rx) = crossbeam_channel::unbounded();
                let result = load_raw(
                    0,
                    0,
                    &path,
                    refine_tx,
                    hq,
                    4.0,
                    HdrToneMapSettings::default(),
                )
                .expect("load_raw");
                match result.image {
                    ImageData::Static(img) => {
                        eprintln!("{name} {label}: Static {}x{}", img.width, img.height);
                    }
                    ImageData::Tiled(src) => {
                        eprintln!(
                            "{name} {label}: Tiled logical {}x{}",
                            src.width(),
                            src.height()
                        );
                    }
                    ImageData::HdrTiled { fallback, .. } => {
                        eprintln!(
                            "{name} {label}: HdrTiled logical {}x{}",
                            fallback.width(),
                            fallback.height()
                        );
                    }
                    ImageData::Hdr { fallback, hdr } => {
                        eprintln!(
                            "{name} {label}: Hdr {}x{} fallback {}x{}",
                            hdr.width,
                            hdr.height,
                            fallback.width,
                            fallback.height
                        );
                    }
                    _ => eprintln!("{name} {label}: other"),
                }
            }
        }
    }

    #[test]
    fn canon_s90_hq_load_routes_hdr_tiled_on_hdr_display_when_file_present() {
        use crossbeam_channel::unbounded;

        let path = PathBuf::from(r"F:\win7\raws\canon\RAW_CANON_S90.CR2");
        if !path.is_file() {
            eprintln!("skip: {}", path.display());
            return;
        }

        let (refine_tx, _refine_rx) = unbounded();
        let result = load_raw(
            0,
            0,
            &path,
            refine_tx,
            true,
            4.0,
            HdrToneMapSettings::default(),
        )
        .expect("load_raw hq hdr");

        match result.image {
            ImageData::HdrTiled { hdr, fallback } => {
                assert_eq!(fallback.width(), 3684);
                assert_eq!(fallback.height(), 2760);
                assert_eq!(hdr.width(), 3684);
                assert_eq!(hdr.height(), 2760);
                eprintln!(
                    "Canon S90 HQ HDR bootstrap: logical {}x{}",
                    fallback.width(),
                    fallback.height()
                );
            }
            other => panic!(
                "expected HdrTiled on HDR display for Canon S90 HQ, got {:?}",
                std::mem::discriminant(&other)
            ),
        }
    }

    #[test]
    fn finalize_hq_develop_keeps_canon_s90_full_resolution() {
        use crate::loader::preview_caps::finalize_raw_hq_developed_image;
        use image::{DynamicImage, GenericImageView, RgbaImage};

        crate::loader::MONITOR_PREVIEW_CAP.store(2048, std::sync::atomic::Ordering::Relaxed);
        let rgba = RgbaImage::from_pixel(3684, 2760, image::Rgba([128, 128, 128, 255]));
        let img = DynamicImage::ImageRgba8(rgba);
        let result = finalize_raw_hq_developed_image(img, 3684, 2760);
        assert_eq!(result.dimensions(), (3684, 2760));
    }

    #[test]
    fn canon_s90_cr2_develop_dimensions_when_file_present() {
        let path = PathBuf::from(r"F:\win7\raws\canon\RAW_CANON_S90.CR2");
        if !path.is_file() {
            eprintln!("skip: {}", path.display());
            return;
        }

        let mut processor = RawProcessor::new().expect("libraw init");
        processor.open(&path).expect("open");
        let thumb = processor.unpack_thumb().expect("thumb");
        let (out_w, out_h) = processor.developed_output_dimensions(Some(&thumb));
        let sdr = processor.develop().expect("develop");
        let limit = hq_preview_max_side();
        let old_scaled = sdr.thumbnail(limit, limit).to_rgba8();
        let finalized =
            crate::loader::preview_caps::finalize_raw_hq_developed_image(sdr, out_w, out_h);
        let finalized_rgba = finalized.to_rgba8();

        eprintln!(
            "canon_s90 thumb={}x{} logical={}x{} hq_side={} old_scaled={}x{} finalized={}x{}",
            thumb.width,
            thumb.height,
            out_w,
            out_h,
            limit,
            old_scaled.width(),
            old_scaled.height(),
            finalized_rgba.width(),
            finalized_rgba.height()
        );
        assert_eq!((out_w, out_h), (3684, 2760), "unexpected logical output");
        assert_eq!(
            (finalized_rgba.width(), finalized_rgba.height()),
            (3684, 2760),
            "HQ refine must keep full develop for 10MP Canon S90"
        );
    }

    #[test]
    fn raw_embedded_preview_covers_sensor_requires_near_full_resolution() {
        let misleading_wic = DecodedImage::new(4096, 3067, vec![0; 4096 * 3067 * 4]);
        assert!(!raw_embedded_preview_covers_sensor(
            &misleading_wic,
            4992,
            6666
        ));
        let full = DecodedImage::new(4992, 6666, vec![0; 4992 * 6666 * 4]);
        assert!(raw_embedded_preview_covers_sensor(&full, 4992, 6666));
    }
}
