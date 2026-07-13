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

//! Baseline JPEG and Ultra HDR (JPEG_R).

use crate::hdr::types::{HdrImageBuffer, HdrToneMapSettings};
use crate::loader::{DecodedImage, ImageData};
use crate::loader::{hdr_gain_map_decode_capacity, hdr_sdr_fallback_rgba8_or_placeholder};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use super::assemble::{make_hdr_image_data, make_image_data};
use crate::loader::tiled_sources::MemoryImageSource;

type JpegStripWithLogicalSize = (DecodedImage, (u32, u32));
type OptionalJpegStripResult = Option<Result<JpegStripWithLogicalSize, String>>;

fn finish_ultra_hdr_loaded(
    _path: &Path,
    hdr: HdrImageBuffer,
    orientation: u16,
    cancel: Option<&AtomicBool>,
) -> Result<ImageData, String> {
    crate::loader::check_decode_cancel_str(cancel)?;
    let hdr = crate::hdr::ultra_hdr::apply_orientation_to_hdr_buffer(hdr, orientation);
    let fallback = DecodedImage::from_hdr_sdr_fallback(
        hdr.width,
        hdr.height,
        hdr_sdr_fallback_rgba8_or_placeholder(&hdr)?,
    );
    crate::loader::check_decode_cancel_str(cancel)?;
    Ok(make_hdr_image_data(hdr, fallback))
}

#[cfg(test)]
pub(crate) fn load_jpeg(path: &Path) -> Result<ImageData, String> {
    load_jpeg_with_target_capacity(
        path,
        HdrToneMapSettings::default().target_hdr_capacity(),
        HdrToneMapSettings::default(),
        false,
        None,
    )
}

#[cfg(test)]
pub(crate) fn load_jpeg_with_target_capacity(
    path: &Path,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
    prefer_embedded_sdr_master: bool,
    cancel: Option<&AtomicBool>,
) -> Result<ImageData, String> {
    let (mmap, _) = crate::mmap_util::map_file(path)?;
    load_jpeg_from_mapped(
        path,
        &mmap,
        hdr_target_capacity,
        hdr_tone_map,
        prefer_embedded_sdr_master,
        cancel,
    )
}

pub(crate) fn load_jpeg_primary_attempt(
    path: &Path,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
    prefer_embedded_sdr_master: bool,
    cancel: Option<&AtomicBool>,
) -> super::detect::PrimaryDecodeAttempt {
    use super::detect::PrimaryDecodeAttempt;
    match crate::mmap_util::map_file(path) {
        Ok((mmap, _)) => {
            let arc = Arc::new(mmap);
            let result = load_jpeg_from_mapped(
                path,
                arc.as_ref(),
                hdr_target_capacity,
                hdr_tone_map,
                prefer_embedded_sdr_master,
                cancel,
            );
            PrimaryDecodeAttempt::with_mmap(result, Some(arc))
        }
        Err(e) => PrimaryDecodeAttempt::from_result(Err(e)),
    }
}

pub(crate) fn load_jpeg_from_mapped(
    path: &Path,
    mmap: &memmap2::Mmap,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
    prefer_embedded_sdr_master: bool,
    cancel: Option<&AtomicBool>,
) -> Result<ImageData, String> {
    crate::loader::check_decode_cancel_str(cancel)?;
    let decode_capacity = hdr_gain_map_decode_capacity(hdr_target_capacity, &hdr_tone_map);
    if mmap.len() < 3 || !mmap.starts_with(&[0xFF, 0xD8, 0xFF]) {
        if let Some(brand) = super::detect::bmff_ftyp_brand(mmap)
            && super::detect::is_motion_video_bmff_brand(&brand)
        {
            return Err(super::detect::motion_video_bmff_error(&brand));
        }
        return Err(format!(
            "not a JPEG bitstream (header {:02x?}); file extension may not match container",
            &mmap[..mmap.len().min(4)]
        ));
    }
    // Sole orientation pass for all JPEG decodes (baseline SDR, **JPEG_R / Ultra HDR**). Do not
    // combine with [`apply_exif_orientation_to_image_data`] -- that would double-rotate.
    let orientation = crate::metadata_utils::get_exif_orientation_from_bytes(&mmap[..], Some(path));
    // Apply EXIF Orientation per TIFF/EXIF rules (same transform family as Pillow `exif_transpose`).
    // Some reference JPEGs (e.g. libavif `paris_exif_orientation_5.jpg`) store a raster that already
    // looks like a normal landscape before correction; the tag still requests transpose, so the
    // result can differ from viewers that ignore the tag or use heuristics.
    let is_ultra_hdr = crate::hdr::ultra_hdr::inspect_ultra_hdr_jpeg_bytes(mmap)
        .ok()
        .is_some_and(|info| info.is_ultra_hdr);
    if is_ultra_hdr {
        crate::loader::check_decode_cancel_str(cancel)?;
        let try_embedded_sdr_master = crate::loader::should_use_embedded_sdr_master_load(
            prefer_embedded_sdr_master,
            hdr_target_capacity,
        );
        match crate::hdr::ultra_hdr::decode_ultra_hdr_jpeg_with_optional_embedded_sdr_master(
            mmap,
            decode_capacity,
            orientation,
            try_embedded_sdr_master,
            Some(path),
            cancel,
        ) {
            Ok(hdr) => {
                crate::loader::check_decode_cancel_str(cancel)?;
                let pixel_count = hdr.width as u64 * hdr.height as u64;
                let tiled_limit = crate::tile_cache::get_tiled_threshold();
                let max_side = hdr.width.max(hdr.height);
                let use_tiled_deferred = hdr.rgba_f32.is_empty()
                    && crate::hdr::jpeg_gain_map_gpu::iso_deferred_from_metadata(&hdr.metadata)
                        .is_some()
                    && (pixel_count >= tiled_limit
                        || max_side >= crate::constants::ABSOLUTE_MAX_TEXTURE_SIDE);
                if use_tiled_deferred {
                    let Some(deferred) =
                        crate::hdr::jpeg_gain_map_gpu::iso_deferred_from_metadata(&hdr.metadata)
                    else {
                        log::warn!(
                            "[Loader] Ultra HDR tiled deferred missing iso_deferred metadata for {}",
                            path.display()
                        );
                        // fall through to non-tiled path below
                        return finish_ultra_hdr_loaded(path, hdr, orientation, cancel);
                    };
                    if let Ok(hdr_source) =
                        crate::hdr::ultra_hdr::UltraHdrTiledImageSource::open_from_iso_deferred(
                            path.to_path_buf(),
                            &hdr,
                            deferred,
                            decode_capacity,
                        )
                    {
                        let fallback = Arc::new(MemoryImageSource::new_with_hdr_sdr_fallback(
                            hdr.width,
                            hdr.height,
                            Arc::clone(&deferred.sdr_rgba),
                            true,
                        ));
                        return Ok(ImageData::HdrTiled {
                            hdr: Arc::new(hdr_source),
                            fallback,
                        });
                    }
                }

                return finish_ultra_hdr_loaded(path, hdr, orientation, cancel);
            }
            Err(err) => {
                if err == crate::loader::DECODE_CANCELLED {
                    return Err(err);
                }
                log::warn!(
                    "[Loader] Ultra HDR JPEG decode failed for {}: {err}; falling back to baseline SDR (no HDR OSD)",
                    path.display()
                );
            }
        }
    }

    // Stage boundary before baseline turbo JPEG decode.
    crate::loader::check_decode_cancel_str(cancel)?;
    let (mut w, mut h, mut pixels) = libjpeg_turbo::decode_to_rgba(mmap)?;
    crate::loader::check_decode_cancel_str(cancel)?;

    if orientation > 1 {
        let (out_w, out_h, out_pixels) =
            crate::libtiff_loader::apply_orientation_buffer(pixels, w, h, orientation);
        w = out_w;
        h = out_h;
        pixels = out_pixels;
    }

    Ok(make_image_data(DecodedImage::new(w, h, pixels)))
}

/// Strip preview fast path: decode the baseline SDR JPEG with DCT-domain scaling.
///
/// Ultra HDR / JPEG_R strip thumbnails intentionally ignore the gain map here; the
/// directory strip needs a fast SDR preview, not a full HDR composition.
pub(crate) fn try_decode_jpeg_strip_dct(
    jpeg_data: &[u8],
    max_side: u32,
) -> OptionalJpegStripResult {
    // Use the bytes variant to avoid re-opening the already mmap'd file
    // (checklist #29 -- "avoid opening the same file multiple times").
    let orientation = crate::metadata_utils::get_exif_orientation_from_bytes(jpeg_data, None);
    let (orig_w, orig_h, scaled_w, scaled_h, pixels) =
        match libjpeg_turbo::decode_to_rgba_with_max_side(jpeg_data, max_side) {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
    // Logical = oriented original dimensions (rotation swaps width/height).
    let logical = if orientation > 4 {
        (orig_h, orig_w)
    } else {
        (orig_w, orig_h)
    };

    let decoded = if orientation > 1 {
        let (out_w, out_h, out_pixels) = crate::libtiff_loader::apply_orientation_buffer(
            pixels,
            scaled_w,
            scaled_h,
            orientation,
        );
        DecodedImage::new(out_w, out_h, out_pixels)
    } else {
        DecodedImage::new(scaled_w, scaled_h, pixels)
    };
    let decoded = match crate::loader::downsample_decoded_for_strip(&decoded, max_side) {
        Ok(decoded) => decoded,
        Err(err) => return Some(Err(err)),
    };
    Some(Ok((decoded, logical)))
}

#[cfg(test)]
mod tests {
    use super::{load_jpeg_with_target_capacity, try_decode_jpeg_strip_dct};
    use crate::hdr::types::HdrToneMapSettings;
    use std::path::PathBuf;

    fn jpeg_with_ultra_hdr_xmp(width: u32, height: u32) -> Vec<u8> {
        let mut rgb = image::RgbImage::new(width, height);
        for (x, y, pixel) in rgb.enumerate_pixels_mut() {
            *pixel = image::Rgb([
                ((x * 255) / width.max(1)) as u8,
                ((y * 255) / height.max(1)) as u8,
                96,
            ]);
        }

        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 90)
            .encode_image(&rgb)
            .expect("encode baseline test JPEG");

        let xmp = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
<rdf:RDF>
<rdf:Description xmlns:hdrgm="http://ns.adobe.com/hdr-gain-map/1.0/" hdrgm:Version="1.0"/>
<Container:Directory>
<Container:Item Item:Mime="image/jpeg" Item:Semantic="GainMap" Item:Length="1"/>
</Container:Directory>
</rdf:RDF>
</x:xmpmeta>"#;
        let payload = format!("http://ns.adobe.com/xap/1.0/\0{xmp}");
        let len = u16::try_from(payload.len() + 2).expect("test XMP fits in JPEG segment");
        let mut out = Vec::with_capacity(jpeg.len() + payload.len() + 4);
        out.extend_from_slice(&jpeg[..2]);
        out.extend_from_slice(&[0xFF, 0xE1]);
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(payload.as_bytes());
        out.extend_from_slice(&jpeg[2..]);
        out
    }

    #[test]
    fn mislabeled_quicktime_jpg_errors_on_first_mmap_pass() {
        let Some(path) = std::env::var_os("SIV_QT_JPG_SAMPLE").map(PathBuf::from) else {
            eprintln!("skip; set SIV_QT_JPG_SAMPLE");
            return;
        };
        let settings = HdrToneMapSettings::default();
        let err = match load_jpeg_with_target_capacity(
            &path,
            settings.target_hdr_capacity(),
            settings,
            false,
            None,
        ) {
            Err(err) => err,
            Ok(_) => panic!("expected QuickTime mislabeled JPG to fail"),
        };
        assert!(
            err.contains(crate::loader::decode::detect::MOTION_VIDEO_BMFF_ERROR_TAG),
            "unexpected error: {err}"
        );
        assert!(crate::loader::decode::detect::primary_decode_failure_is_final(&err));
    }

    #[test]
    fn ultra_hdr_strip_dct_decodes_baseline_sdr_preview() {
        let jpeg = jpeg_with_ultra_hdr_xmp(64, 48);
        let info = crate::hdr::ultra_hdr::inspect_ultra_hdr_jpeg_bytes(&jpeg)
            .expect("inspect generated Ultra HDR-like JPEG");
        assert!(info.is_ultra_hdr);

        let (decoded, logical) = try_decode_jpeg_strip_dct(&jpeg, 16)
            .expect("Ultra HDR strip preview should use baseline SDR DCT scaling")
            .expect("decode baseline SDR preview");

        assert_eq!(logical, (64, 48));
        assert!(decoded.width > 0);
        assert!(decoded.height > 0);
        assert!(decoded.width.max(decoded.height) <= 16);
        assert_eq!(
            decoded.rgba().len(),
            decoded.width as usize * decoded.height as usize * 4
        );
    }
}
