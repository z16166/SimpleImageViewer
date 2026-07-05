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

//! Fast directory-tree strip decode for classic float HDR (OpenEXR, Radiance `.hdr`).

use std::path::Path;
use std::sync::Arc;

use crate::hdr::exr_tiled::ExrTiledImageSource;
use crate::hdr::radiance_tiled::RadianceHdrTiledImageSource;
use crate::hdr::tiled::HdrTiledSource;
use crate::loader::DecodedImage;

type StripWithLogicalSize = (DecodedImage, (u32, u32));
type OptionalStripResult = Option<Result<StripWithLogicalSize, String>>;

fn finish_hdr_float_strip(
    path: &Path,
    logical: (u32, u32),
    preview: (u32, u32, Vec<u8>),
) -> Result<StripWithLogicalSize, String> {
    let (width, height, pixels) = preview;
    if width == 0 || height == 0 {
        return Err(format!(
            "HDR strip preview returned zero size for {}",
            path.display()
        ));
    }
    Ok((DecodedImage::new(width, height, pixels), logical))
}

fn open_exr_source(
    path: &Path,
    mmap: Option<&Arc<memmap2::Mmap>>,
) -> Result<ExrTiledImageSource, String> {
    match mmap {
        Some(m) => ExrTiledImageSource::open_from_mmap(path, Arc::clone(m)),
        None => ExrTiledImageSource::open(path),
    }
}

fn open_radiance_source(
    path: &Path,
    mmap: Option<&Arc<memmap2::Mmap>>,
) -> Result<RadianceHdrTiledImageSource, String> {
    match mmap {
        Some(m) => RadianceHdrTiledImageSource::open_from_mmap(path, Arc::clone(m)),
        None => RadianceHdrTiledImageSource::open(path),
    }
}

fn try_hdr_float_strip_with_source<S, F>(
    path: &Path,
    mmap: Option<&Arc<memmap2::Mmap>>,
    max_side: u32,
    format_label: &str,
    open: F,
) -> OptionalStripResult
where
    S: HdrTiledSource,
    F: FnOnce(&Path, Option<&Arc<memmap2::Mmap>>) -> Result<S, String>,
{
    let source = match open(path, mmap) {
        Ok(source) => source,
        Err(err) => {
            log::debug!(
                "[DirectoryTree] {format_label} strip fast path open failed for {}: {err}",
                path.display()
            );
            return None;
        }
    };
    let logical = (source.width(), source.height());
    // Same max bound on both axes (directory-tree convention); aspect is preserved inside
    // `generate_sdr_preview` via preview_dimensions / nearest sampling.
    match source.generate_sdr_preview(max_side, max_side) {
        Ok(preview) => Some(finish_hdr_float_strip(path, logical, preview)),
        Err(err) => {
            log::debug!(
                "[DirectoryTree] {format_label} strip fast path preview failed for {}: {err}",
                path.display()
            );
            None
        }
    }
}

/// Sparse nearest / mip-aware SDR preview for OpenEXR and Radiance `.hdr` strips.
///
/// Returns `None` when the path is not a supported float HDR extension or the fast preview
/// cannot be produced (caller falls back to full decode).
pub(crate) fn try_fast_hdr_float_strip_from_path(
    path: &Path,
    mmap: Option<&Arc<memmap2::Mmap>>,
    max_side: u32,
) -> OptionalStripResult {
    let ext = path
        .extension()
        .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "exr" => try_hdr_float_strip_with_source(path, mmap, max_side, "EXR", open_exr_source),
        "hdr" | "pic" => try_hdr_float_strip_with_source(
            path,
            mmap,
            max_side,
            "Radiance HDR",
            open_radiance_source,
        ),
        _ => None,
    }
}
