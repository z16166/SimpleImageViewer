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

use image::{DynamicImage, RgbaImage};
use parking_lot::RwLock as PLRwLock;
use std::path::PathBuf;
use std::sync::Arc;

/// RGBA8 in a shared [`Arc`] so decode → channel → UI can reuse one allocation (cheap `Clone`).
/// `egui::ColorImage::from_rgba_unmultiplied` still converts RGBA8 → `Color32` once at upload time.
#[derive(Clone)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pixels: Arc<Vec<u8>>,
}

impl std::fmt::Debug for DecodedImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecodedImage")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("rgba_bytes", &self.pixels.len())
            .finish()
    }
}

impl DecodedImage {
    #[inline]
    pub fn rgba(&self) -> &[u8] {
        self.pixels.as_slice()
    }

    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Self {
        Self {
            width,
            height,
            pixels: Arc::new(pixels),
        }
    }

    /// Wrap an existing RGBA8 buffer without copying.
    pub fn from_arc(width: u32, height: u32, pixels: Arc<Vec<u8>>) -> Self {
        Self {
            width,
            height,
            pixels,
        }
    }

    pub fn into_arc_pixels(self) -> Arc<Vec<u8>> {
        self.pixels
    }

    /// Build `RgbaImage`; avoids copying the buffer when this is the only [`Arc`] handle.
    pub fn into_rgba8_image(self) -> RgbaImage {
        let w = self.width;
        let h = self.height;
        let vec = Arc::try_unwrap(self.pixels).unwrap_or_else(|a| (*a).clone());
        RgbaImage::from_raw(w, h, vec).expect("DecodedImage dimensions must match RGBA buffer")
    }

    pub fn set_rgba_buffer(&mut self, width: u32, height: u32, pixels: Vec<u8>) {
        self.width = width;
        self.height = height;
        self.pixels = Arc::new(pixels);
    }

    /// Take ownership of the RGBA buffer for in-place transforms.
    /// If shared, clones the bytes; leaves `self` with an empty buffer until reassigned.
    pub fn take_rgba_owned(&mut self) -> Vec<u8> {
        let arc = std::mem::replace(&mut self.pixels, Arc::new(Vec::new()));
        Arc::try_unwrap(arc).unwrap_or_else(|a| (*a).clone())
    }
}

impl From<image::RgbaImage> for DecodedImage {
    fn from(img: image::RgbaImage) -> Self {
        Self::new(img.width(), img.height(), img.into_raw())
    }
}

/// Interface for images that can provide pixel data in tiles/chunks on demand.
pub trait TiledImageSource: Send + Sync {
    fn width(&self) -> u32;
    fn height(&self) -> u32;
    fn is_hdr_sdr_fallback(&self) -> bool {
        false
    }
    /// Extract a rectangular region of the image as RGBA8.
    fn extract_tile(&self, x: u32, y: u32, w: u32, h: u32) -> Arc<Vec<u8>>;
    /// Generate a downscaled preview of the full image.
    fn generate_preview(&self, max_w: u32, max_h: u32) -> (u32, u32, Vec<u8>);
    /// Optionally provide the full pixel buffer if already in memory.
    fn full_pixels(&self) -> Option<Arc<Vec<u8>>>;
    /// Trigger background refinement to replace preview data with full-quality pixels.
    /// Default no-op; only RAW sources need background demosaicing.
    fn request_refinement(&self, _index: usize, _generation: u64) {}
}

/// A single frame of an animated image. RGBA8 lives in a shared [`Arc`] so frame lists and
/// deferred GPU uploads clone handles instead of duplicating megabytes per frame.
#[derive(Clone)]
pub struct AnimationFrame {
    pub width: u32,
    pub height: u32,
    pixels: Arc<Vec<u8>>,
    pub delay: std::time::Duration,
}

impl std::fmt::Debug for AnimationFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnimationFrame")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("rgba_bytes", &self.pixels.len())
            .field("delay", &self.delay)
            .finish()
    }
}

impl AnimationFrame {
    #[inline]
    pub fn rgba(&self) -> &[u8] {
        self.pixels.as_slice()
    }

    pub fn new(width: u32, height: u32, pixels: Vec<u8>, delay: std::time::Duration) -> Self {
        Self {
            width,
            height,
            pixels: Arc::new(pixels),
            delay,
        }
    }

    #[inline]
    pub fn arc_pixels(&self) -> Arc<Vec<u8>> {
        Arc::clone(&self.pixels)
    }
}

/// Decoded image data — either a static image, a large image (for tiled rendering), or an animated sequence.
#[derive(Clone)]
pub enum ImageData {
    Static(DecodedImage),
    /// HDR image with its original float buffer plus an SDR fallback texture for compatibility.
    Hdr {
        hdr: crate::hdr::types::HdrImageBuffer,
        fallback: DecodedImage,
    },
    /// Large HDR image that keeps its float source for future native HDR tiled rendering,
    /// with an SDR tiled fallback for the existing tile renderer.
    HdrTiled {
        hdr: Arc<dyn crate::hdr::tiled::HdrTiledSource>,
        fallback: Arc<dyn TiledImageSource>,
    },
    /// Virtualized image source — tiles are decoded on-demand from disk or other sources.
    Tiled(Arc<dyn TiledImageSource>),
    Animated(Vec<AnimationFrame>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RenderShape {
    Static,
    Tiled,
    Animated,
}

impl ImageData {
    pub fn static_sdr(&self) -> Option<&DecodedImage> {
        match self {
            Self::Static(image) => Some(image),
            Self::Hdr { fallback, .. } => Some(fallback),
            _ => None,
        }
    }

    pub fn static_hdr(&self) -> Option<&crate::hdr::types::HdrImageBuffer> {
        match self {
            Self::Hdr { hdr, .. } => Some(hdr),
            _ => None,
        }
    }

    pub fn tiled_sdr_source(&self) -> Option<&Arc<dyn TiledImageSource>> {
        match self {
            Self::Tiled(source) => Some(source),
            Self::HdrTiled { fallback, .. } => Some(fallback),
            _ => None,
        }
    }

    pub fn tiled_hdr_source(&self) -> Option<&Arc<dyn crate::hdr::tiled::HdrTiledSource>> {
        match self {
            Self::HdrTiled { hdr, .. } => Some(hdr),
            _ => None,
        }
    }

    pub fn preferred_render_shape(&self) -> RenderShape {
        match self {
            Self::Static(_) | Self::Hdr { .. } => RenderShape::Static,
            Self::Tiled(_) | Self::HdrTiled { .. } => RenderShape::Tiled,
            Self::Animated(_) => RenderShape::Animated,
        }
    }

    pub fn has_plane(&self, plane_kind: PixelPlaneKind) -> bool {
        match plane_kind {
            PixelPlaneKind::Sdr => self.static_sdr().is_some() || self.tiled_sdr_source().is_some(),
            PixelPlaneKind::Hdr => self.static_hdr().is_some() || self.tiled_hdr_source().is_some(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PixelPlaneKind {
    Sdr,
    Hdr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PreviewStage {
    Initial,
    Refined,
}

#[derive(Clone)]
pub enum PreviewPlane {
    Sdr(DecodedImage),
    Hdr(Arc<crate::hdr::types::HdrImageBuffer>),
}

impl PreviewPlane {
    pub fn kind(&self) -> PixelPlaneKind {
        match self {
            Self::Sdr(_) => PixelPlaneKind::Sdr,
            Self::Hdr(_) => PixelPlaneKind::Hdr,
        }
    }

    pub fn dimensions(&self) -> (u32, u32) {
        match self {
            Self::Sdr(preview) => (preview.width, preview.height),
            Self::Hdr(preview) => (preview.width, preview.height),
        }
    }
}

#[derive(Clone)]
pub struct PreviewBundle {
    stage: PreviewStage,
    sdr: Option<DecodedImage>,
    hdr: Option<Arc<crate::hdr::types::HdrImageBuffer>>,
}

impl PreviewBundle {
    pub fn empty(stage: PreviewStage) -> Self {
        Self {
            stage,
            sdr: None,
            hdr: None,
        }
    }

    pub fn initial() -> Self {
        Self::empty(PreviewStage::Initial)
    }

    pub fn refined() -> Self {
        Self::empty(PreviewStage::Refined)
    }

    pub fn from_planes(
        stage: PreviewStage,
        sdr: Option<DecodedImage>,
        hdr: Option<Arc<crate::hdr::types::HdrImageBuffer>>,
    ) -> Self {
        Self { stage, sdr, hdr }
    }

    pub fn with_sdr(mut self, preview: DecodedImage) -> Self {
        self.sdr = Some(preview);
        self
    }

    pub fn with_hdr(mut self, preview: Arc<crate::hdr::types::HdrImageBuffer>) -> Self {
        self.hdr = Some(preview);
        self
    }

    pub fn stage(&self) -> PreviewStage {
        self.stage
    }

    pub fn sdr(&self) -> Option<&DecodedImage> {
        self.sdr.as_ref()
    }

    pub fn hdr(&self) -> Option<&Arc<crate::hdr::types::HdrImageBuffer>> {
        self.hdr.as_ref()
    }

    pub fn plane(&self, kind: PixelPlaneKind) -> Option<PreviewPlane> {
        match kind {
            PixelPlaneKind::Sdr => self.sdr.clone().map(PreviewPlane::Sdr),
            PixelPlaneKind::Hdr => self.hdr.clone().map(PreviewPlane::Hdr),
        }
    }
}

#[derive(Clone)]
pub struct LoadResult {
    pub index: usize,
    pub generation: u64,
    pub result: Result<ImageData, String>,
    pub preview_bundle: PreviewBundle,
    pub ultra_hdr_capacity_sensitive: bool,
    /// True when [`ImageData::Hdr`] used a cheap SDR placeholder because the display HDR target
    /// capacity indicated native HDR output; a follow-up [`LoaderOutput::HdrSdrFallback`] may
    /// replace the texture with a tone-mapped fallback for SDR-only draw paths (e.g. Ripple).
    pub sdr_fallback_is_placeholder: bool,
}

/// Refined full-resolution SDR RGBA8 for a static HDR image that initially loaded with a
/// placeholder fallback (see [`LoadResult::sdr_fallback_is_placeholder`]).
pub struct HdrSdrFallbackResult {
    pub index: usize,
    pub generation: u64,
    pub fallback: DecodedImage,
}

pub struct TileResult {
    pub index: usize,
    pub generation: u64,
    pub col: u32,
    pub row: u32,
    pub pixel_kind: TilePixelKind,
}

impl TileResult {
    pub fn pending_key(&self) -> crate::tile_cache::PendingTileKey {
        crate::tile_cache::PendingTileKey::new(
            crate::tile_cache::TileCoord {
                col: self.col,
                row: self.row,
            },
            self.pixel_kind,
        )
    }

    pub fn should_request_repaint(&self) -> bool {
        match self.pixel_kind {
            TilePixelKind::Sdr | TilePixelKind::Hdr => true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TilePixelKind {
    Sdr,
    Hdr,
}

#[derive(Clone)]
pub enum TileDecodeSource {
    Sdr(Arc<dyn TiledImageSource>),
    Hdr(Arc<dyn crate::hdr::tiled::HdrTiledSource>),
}

impl TileDecodeSource {
    pub(crate) fn pixel_kind(&self) -> TilePixelKind {
        match self {
            Self::Sdr(_) => TilePixelKind::Sdr,
            Self::Hdr(_) => TilePixelKind::Hdr,
        }
    }
}

pub struct PreviewResult {
    pub index: usize,
    pub generation: u64,
    pub preview_bundle: PreviewBundle,
    pub error: Option<String>,
}

impl PreviewResult {
    pub fn from_sdr_preview(
        index: usize,
        generation: u64,
        result: Result<DecodedImage, String>,
    ) -> Self {
        let (preview_bundle, error) = match result {
            Ok(preview) => (PreviewBundle::refined().with_sdr(preview), None),
            Err(error) => (PreviewBundle::refined(), Some(error)),
        };
        Self {
            index,
            generation,
            preview_bundle,
            error,
        }
    }
}

pub enum LoaderOutput {
    Image(LoadResult),
    Tile(TileResult),
    Preview(PreviewResult),
    /// Tone-mapped SDR fallback for static HDR (after native-HDR placeholder load).
    HdrSdrFallback(HdrSdrFallbackResult),
    /// Background refinement finished (e.g. LibRaw demosaic)
    Refined(usize, u64),
}

pub struct RefinementRequest {
    pub path: PathBuf,
    pub index: usize,
    pub generation: u64,
    pub orientation_override: Option<i32>,
    pub developed_image: Arc<PLRwLock<Option<DynamicImage>>>,
}
