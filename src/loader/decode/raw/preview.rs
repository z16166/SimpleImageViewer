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
//! - **On:** embedded previews bootstrap the UI while demosaic runs; developed pixels always use
//!   the HDR pipeline (even on SDR displays to support exposure adjustments).

use crate::loader::DecodedImage;
use crate::loader::preview_caps::hq_preview_max_side;
use crate::raw_processor::RawProcessor;
use std::path::PathBuf;

/// True when an embedded preview is large enough to substitute for a full demosaic.
pub(crate) fn raw_embedded_preview_covers_sensor(
    preview: &DecodedImage,
    raw_w: u32,
    raw_h: u32,
) -> bool {
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
pub(crate) fn raw_embedded_preview_meets_hq_requirement(
    preview: &DecodedImage,
    raw_w: u32,
    raw_h: u32,
) -> bool {
    let hq_side = hq_preview_max_side();
    let preview_long = preview.width.max(preview.height);
    if preview_long >= hq_side {
        return true;
    }
    // Accept camera full-size embedded JPEGs that are slightly below the monitor HQ cap.
    const EMBEDDED_PREVIEW_HQ_MIN_FLOOR: u32 = 1024;
    let hq_floor = (hq_side / 2).max(EMBEDDED_PREVIEW_HQ_MIN_FLOOR);
    preview_long >= hq_floor && raw_embedded_preview_covers_sensor(preview, raw_w, raw_h)
}

fn apply_orientation_to_embedded_preview(
    mut preview: DecodedImage,
    final_orientation: u16,
) -> DecodedImage {
    if final_orientation <= 1 {
        return preview;
    }
    let expected_bytes = preview
        .width
        .saturating_mul(preview.height)
        .saturating_mul(4) as usize;
    if preview.rgba().len() != expected_bytes {
        log::warn!(
            "[Loader] Embedded preview dimensions mismatch: {}x{} vs {} bytes; skipping orientation",
            preview.width,
            preview.height,
            preview.rgba().len()
        );
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

pub(crate) fn extract_embedded_preview(
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

/// Fast SDR preview for directory-tree strip when embedded thumbnail is missing.
///
/// Half-size LibRaw develop is used when platform WIC/ImageIO decode fails (e.g. CHDK CRW)
/// and on Linux where no platform still decoder exists. The processor is mutated in place
/// (`set_half_size`); callers must not reuse it afterward.
pub(crate) fn develop_half_size_sdr_strip_preview(
    processor: &mut RawProcessor,
    path: &std::path::Path,
) -> Option<DecodedImage> {
    processor.set_half_size(true);
    let dynamic = processor.develop().ok()?;
    let rgba = dynamic.into_rgba8();
    let decoded = DecodedImage::new(rgba.width(), rgba.height(), rgba.into_raw());
    if decoded.width == 0 || decoded.height == 0 {
        log::warn!(
            "[DirectoryTree] LibRaw half-size develop returned zero dimensions for {:?}",
            path.file_name().unwrap_or_default()
        );
        return None;
    }
    log::debug!(
        "[DirectoryTree] LibRaw half-size develop for {:?} ({}x{})",
        path.file_name().unwrap_or_default(),
        decoded.width,
        decoded.height
    );
    Some(decoded)
}
