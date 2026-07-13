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
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::decode_profile::DecodeProfile;
use super::raw_osd::RawOsdInfo;

pub type SourceKey = u64;

/// Best-effort key used to drop stale async loader results after navigation.
///
/// This intentionally derives from a Unicode-lowercased path because the key is only a guardrail for
/// the current file list, not a persisted identifier or proof of file identity. A hash collision would
/// at worst fail to reject one stale in-flight result; normal index/generation checks still apply.
pub fn source_key_for_path(path: &Path) -> SourceKey {
    let normalized = path.to_string_lossy().to_lowercase();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    normalized.hash(&mut hasher);
    hasher.finish()
}

/// RGBA8 in a shared [`Arc`] so decode → channel → UI can reuse one allocation (cheap `Clone`).
/// `egui::ColorImage::from_rgba_unmultiplied` still converts RGBA8 → `Color32` once at upload time.
#[derive(Clone, PartialEq)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pixels: Arc<Vec<u8>>,
    /// Set at decode/load when this RGBA8 buffer is a cheap deferred SDR placeholder
    /// (see [`crate::loader::cheap_hdr_sdr_placeholder_rgba8`]), not display-ready pixels.
    sdr_deferred_placeholder: bool,
}

impl std::fmt::Debug for DecodedImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecodedImage")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("rgba_bytes", &self.pixels.len())
            .field("sdr_deferred_placeholder", &self.sdr_deferred_placeholder)
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
            sdr_deferred_placeholder: false,
        }
    }

    #[cfg(test)]
    pub fn new_sdr_deferred_placeholder(width: u32, height: u32, pixels: Vec<u8>) -> Self {
        Self {
            width,
            height,
            pixels: Arc::new(pixels),
            sdr_deferred_placeholder: true,
        }
    }

    /// Wrap an existing RGBA8 buffer without copying.
    pub fn from_arc(width: u32, height: u32, pixels: Arc<Vec<u8>>) -> Self {
        Self {
            width,
            height,
            pixels,
            sdr_deferred_placeholder: false,
        }
    }

    pub fn from_arc_sdr_deferred_placeholder(
        width: u32,
        height: u32,
        pixels: Arc<Vec<u8>>,
    ) -> Self {
        Self {
            width,
            height,
            pixels,
            sdr_deferred_placeholder: true,
        }
    }

    pub fn from_hdr_sdr_fallback(
        width: u32,
        height: u32,
        fallback: super::hdr_fallback::HdrSdrFallbackRgba8,
    ) -> Self {
        Self {
            width,
            height,
            pixels: fallback.pixels,
            sdr_deferred_placeholder: fallback.is_deferred_placeholder,
        }
    }

    #[inline]
    pub fn is_sdr_deferred_placeholder(&self) -> bool {
        self.sdr_deferred_placeholder
    }

    pub fn mark_sdr_deferred_placeholder(&mut self) {
        self.sdr_deferred_placeholder = true;
    }

    pub fn into_arc_pixels(self) -> Arc<Vec<u8>> {
        self.pixels
    }

    pub fn arc_pixels(&self) -> Arc<Vec<u8>> {
        Arc::clone(&self.pixels)
    }

    /// Build `RgbaImage`; avoids copying the buffer when this is the only [`Arc`] handle.
    pub fn into_rgba8_image(self) -> Result<RgbaImage, String> {
        let w = self.width;
        let h = self.height;
        let vec = Arc::try_unwrap(self.pixels).unwrap_or_else(|a| (*a).clone());
        match RgbaImage::from_raw(w, h, vec) {
            Some(img) => Ok(img),
            None => Err(format!(
                "DecodedImage dimensions {}x{} do not match RGBA buffer size",
                w, h
            )),
        }
    }

    pub fn set_rgba_buffer(&mut self, width: u32, height: u32, pixels: Vec<u8>) {
        self.set_rgba_buffer_preserving_placeholder(width, height, pixels, false);
    }

    pub(crate) fn set_rgba_buffer_preserving_placeholder(
        &mut self,
        width: u32,
        height: u32,
        pixels: Vec<u8>,
        preserve_placeholder: bool,
    ) {
        let placeholder = preserve_placeholder && self.sdr_deferred_placeholder;
        self.width = width;
        self.height = height;
        self.pixels = Arc::new(pixels);
        self.sdr_deferred_placeholder = placeholder;
    }

    /// Take ownership of the RGBA buffer for in-place transforms.
    /// If shared, clones the bytes; leaves `self` with an empty buffer until reassigned.
    pub fn take_rgba_owned(&mut self) -> Vec<u8> {
        let arc = std::mem::replace(&mut self.pixels, Arc::new(Vec::new()));
        Arc::try_unwrap(arc).unwrap_or_else(|a| (*a).clone())
    }

    /// Take the pixel [`Arc`] for transforms that can read from a shared slice when not unique.
    pub(crate) fn take_pixels_arc(&mut self) -> Arc<Vec<u8>> {
        std::mem::replace(&mut self.pixels, Arc::new(Vec::new()))
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
    /// Downscaled preview representing full-image geometry (embedded EXIF thumbs may be skipped).
    fn generate_full_image_preview(&self, max_w: u32, max_h: u32) -> (u32, u32, Vec<u8>) {
        self.generate_preview(max_w, max_h)
    }
    /// Optionally provide the full pixel buffer if already in memory.
    fn full_pixels(&self) -> Option<Arc<Vec<u8>>>;

    /// When true, [`crate::loader::orientation::apply_exif_orientation_to_image_data`] may rotate pixels
    /// from [`Self::full_pixels`] using [`crate::metadata_utils::get_exif_orientation`].
    ///
    /// Only non-HDR-fallback [`crate::loader::tiled_sources::MemoryImageSource`] enables this — JPEG /
    /// TIFF paths and HDR/SDR pairs apply orientation elsewhere; LibRAW tiled sources use flip metadata.
    fn exif_orientation_rotate_in_memory_rgba(&self) -> bool {
        false
    }
    /// Trigger background refinement to replace preview data with full-quality pixels.
    /// Default no-op; only RAW sources need background demosaicing.
    fn request_refinement(&self, _index: usize, _decode_profile: DecodeProfile) {}

    /// When true, the loader must not spawn a second HQ preview from [`Self::generate_preview`]
    /// because an async RAW demosaic worker owns HQ refinement (embedded bootstrap path).
    fn defers_loader_hq_preview(&self) -> bool {
        false
    }

    /// Block until async pixel data is ready (PSD v1 bootstrap). Default: already ready.
    fn wait_for_async_pixels(&self, _timeout: std::time::Duration) -> Result<(), String> {
        Ok(())
    }

    /// Cooperative cancel for long async decode work owned by this source (e.g. PSD composite).
    /// Default no-op; directory switch / eviction should call this before dropping the source.
    fn request_cancel(&self) {}
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

/// One HDR animation frame: float/HDR-deferred plane plus an SDR fallback texture.
#[derive(Clone)]
pub struct HdrAnimationFrame {
    pub hdr: crate::hdr::types::HdrImageBuffer,
    pub fallback: DecodedImage,
    pub delay: std::time::Duration,
}

impl HdrAnimationFrame {
    pub fn new(
        hdr: crate::hdr::types::HdrImageBuffer,
        fallback: DecodedImage,
        delay: std::time::Duration,
    ) -> Self {
        Self {
            hdr,
            fallback,
            delay,
        }
    }

    pub fn width(&self) -> u32 {
        self.hdr.width
    }

    pub fn height(&self) -> u32 {
        self.hdr.height
    }
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

    pub fn from_arc(
        width: u32,
        height: u32,
        pixels: Arc<Vec<u8>>,
        delay: std::time::Duration,
    ) -> Self {
        Self {
            width,
            height,
            pixels,
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
        hdr: Box<crate::hdr::types::HdrImageBuffer>,
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
    /// Animated JPEG XL (or similar) with per-frame HDR / ISO gain-map GPU compose.
    HdrAnimated(Vec<HdrAnimationFrame>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RenderShape {
    Static,
    Tiled,
    Animated,
    /// Decode shape not known at request time; install must still hard-check actual `ImageData`.
    Unknown,
}

impl ImageData {
    pub fn static_sdr(&self) -> Option<&DecodedImage> {
        match self {
            Self::Static(image) => Some(image),
            Self::Hdr { fallback, .. } => Some(fallback),
            Self::HdrAnimated(frames) => frames.first().map(|frame| &frame.fallback),
            _ => None,
        }
    }

    pub fn static_hdr(&self) -> Option<&crate::hdr::types::HdrImageBuffer> {
        match self {
            Self::Hdr { hdr, .. } => Some(hdr),
            _ => None,
        }
    }

    pub fn hdr_animated_frames(&self) -> Option<&[HdrAnimationFrame]> {
        match self {
            Self::HdrAnimated(frames) => Some(frames.as_slice()),
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
            Self::Animated(_) | Self::HdrAnimated(_) => RenderShape::Animated,
        }
    }

    pub fn has_plane(&self, plane_kind: PixelPlaneKind) -> bool {
        match plane_kind {
            PixelPlaneKind::Sdr => {
                self.static_sdr().is_some()
                    || self.tiled_sdr_source().is_some()
                    || self
                        .hdr_animated_frames()
                        .is_some_and(|frames| !frames.is_empty())
            }
            PixelPlaneKind::Hdr => {
                self.static_hdr().is_some()
                    || self.tiled_hdr_source().is_some()
                    || self.hdr_animated_frames().is_some()
            }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RawDevelopedImageRank {
    EmbeddedPreview,
    FullResolutionDeveloped,
}

/// Provenance of pixels stored in the main-window [`crate::loader::TextureCache`].
///
/// HQ/sync decisions use tag + stage, not decoded or GPU texture dimensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TexturePreviewBufferTag {
    MainWindowSdr,
    /// Bootstrap tiled SDR preview from initial load. Pair with [`PreviewStage::Initial`] only;
    /// refined tiled previews use [`Self::TiledRefinedLoader`].
    TiledBootstrap,
    TiledRefinedLoader,
    /// `ImageLoader::trigger_hq_tiled_sdr_preview`; not a substitute for loader HDR refine.
    TiledOnDemandSdr,
    HdrSdrFallback,
    RawGpuBootstrap,
}

impl TexturePreviewBufferTag {
    const PREVIEW_STAGE_COUNT: u16 = 2;

    pub fn quality_rank(self, stage: PreviewStage) -> u16 {
        // `TiledBootstrap` is only stored with `PreviewStage::Initial`; refined loader previews
        // always use `TiledRefinedLoader`.
        let base = match self {
            Self::RawGpuBootstrap => 0,
            Self::HdrSdrFallback => 1,
            Self::MainWindowSdr => 2,
            Self::TiledBootstrap => 3,
            Self::TiledOnDemandSdr => 4,
            Self::TiledRefinedLoader => 5,
        };
        let stage_bonus = match stage {
            PreviewStage::Initial => 0,
            PreviewStage::Refined => 1,
        };
        base * Self::PREVIEW_STAGE_COUNT + stage_bonus
    }

    pub fn satisfies_tiled_sdr_hq(self, stage: PreviewStage) -> bool {
        matches!(
            (self, stage),
            (Self::TiledRefinedLoader, PreviewStage::Refined)
        )
    }
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

pub struct LoadResult {
    pub index: usize,
    pub decode_profile: DecodeProfile,
    pub source_key: SourceKey,
    pub result: Result<ImageData, crate::loader::DecodeError>,
    pub preview_bundle: PreviewBundle,
    pub ultra_hdr_capacity_sensitive: bool,
    /// True when [`ImageData::Hdr`] used a cheap SDR placeholder because the display HDR target
    /// capacity indicated native HDR output; the HDR plane shader tone-maps for SDR draw paths.
    pub sdr_fallback_is_placeholder: bool,
    /// The HDR capacity of the display when this load was processed, used to detect capacity mismatch.
    pub target_hdr_capacity: f32,
    /// RAW-only OSD metadata (embedded preview, sensor grid, active pixel source).
    pub raw_osd: Option<RawOsdInfo>,
    /// PSD/PSB decode-stage OSD (P1/P2/P2.5/P3 and compat-reveal disclosure).
    pub psd_osd: Option<crate::loader::PsdOsdInfo>,
    /// GPU textures uploaded on a background loader thread (static HDR plane only).
    ///
    /// `ImagePlaneUpload` contains `Send` wgpu handles; the loader worker fills this field and
    /// the main thread consumes it in `try_register_preuploaded_hdr_plane` before paint callbacks run.
    pub uploaded_planes: Option<crate::hdr::renderer::ImagePlaneUpload>,
    /// [`ImageViewerApp::current_device_id`] under which `uploaded_planes` was created.
    pub device_id: Option<u64>,
    /// True when plane bytes were enqueued into [`HdrPendingGpuWriteQueues`] instead of
    /// flushed on the loader worker; main thread must flush before GPU bind.
    pub staged_gpu_plane_upload: bool,
}

impl Clone for LoadResult {
    fn clone(&self) -> Self {
        if self.uploaded_planes.is_some() {
            log::debug!(
                "[Loader] LoadResult::clone dropping pre-uploaded HDR planes for index {}",
                self.index
            );
        }
        Self {
            index: self.index,
            decode_profile: self.decode_profile.clone(),
            source_key: self.source_key,
            result: self.result.clone(),
            preview_bundle: self.preview_bundle.clone(),
            ultra_hdr_capacity_sensitive: self.ultra_hdr_capacity_sensitive,
            sdr_fallback_is_placeholder: self.sdr_fallback_is_placeholder,
            target_hdr_capacity: self.target_hdr_capacity,
            raw_osd: self.raw_osd.clone(),
            psd_osd: self.psd_osd.clone(),
            uploaded_planes: None,
            device_id: self.device_id,
            staged_gpu_plane_upload: false,
        }
    }
}

pub struct TileResult {
    pub index: usize,
    pub decode_profile: DecodeProfile,
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

    pub(crate) fn dimensions(&self) -> (u32, u32) {
        match self {
            Self::Sdr(source) => (source.width(), source.height()),
            Self::Hdr(source) => (source.width(), source.height()),
        }
    }

    pub(crate) fn tile_cols(&self) -> u32 {
        crate::tile_cache::tile_count_for_extent(self.dimensions().0)
    }

    pub(crate) fn tile_rows(&self) -> u32 {
        crate::tile_cache::tile_count_for_extent(self.dimensions().1)
    }

    pub(crate) fn tile_rect(&self, col: u32, row: u32) -> Option<crate::tile_cache::TileRect> {
        let (width, height) = self.dimensions();
        crate::tile_cache::tile_rect_for_dimensions(
            width,
            height,
            crate::tile_cache::TileCoord { col, row },
        )
    }
}

pub struct PreviewResult {
    pub index: usize,
    pub decode_profile: DecodeProfile,
    pub source_key: SourceKey,
    pub preview_bundle: PreviewBundle,
    pub error: Option<String>,
    /// LibRaw CPU demosaic duration when this preview came from HQ refine.
    pub cpu_demosaic_ms: Option<u32>,
    /// Partial RAW OSD for HQ bootstrap previews before the full `LoadResult` arrives.
    pub raw_bootstrap_osd: Option<RawOsdInfo>,
    /// Tag for SDR pixels when written into `TextureCache`; None uses loader-refined default.
    pub sdr_texture_tag: Option<TexturePreviewBufferTag>,
}

impl PreviewResult {
    pub fn from_sdr_preview(
        index: usize,
        decode_profile: DecodeProfile,
        source_key: SourceKey,
        result: Result<DecodedImage, String>,
        sdr_texture_tag: TexturePreviewBufferTag,
    ) -> Self {
        let (preview_bundle, error) = match result {
            Ok(preview) => (PreviewBundle::refined().with_sdr(preview), None),
            Err(error) => (PreviewBundle::refined(), Some(error)),
        };
        Self {
            index,
            decode_profile,
            source_key,
            preview_bundle,
            error,
            cpu_demosaic_ms: None,
            raw_bootstrap_osd: None,
            sdr_texture_tag: Some(sdr_texture_tag),
        }
    }

    pub fn effective_sdr_texture_tag(&self) -> TexturePreviewBufferTag {
        self.sdr_texture_tag
            .unwrap_or(TexturePreviewBufferTag::TiledRefinedLoader)
    }
}

pub enum LoaderOutput {
    Image(Box<LoadResult>),
    Tile(TileResult),
    Preview(PreviewResult),
    /// Background refinement finished (e.g. LibRaw demosaic)
    Refined {
        index: usize,
        source_key: SourceKey,
    },
}

pub struct RefinementRequest {
    pub path: PathBuf,
    pub index: usize,
    pub decode_profile: DecodeProfile,
    pub source_key: SourceKey,
    pub orientation_override: Option<i32>,
    pub logical_width: u32,
    pub logical_height: u32,
    pub developed_image: Arc<PLRwLock<Option<DynamicImage>>>,
    pub developed_image_rank: Arc<PLRwLock<RawDevelopedImageRank>>,
    /// Shared with [`crate::loader::tiled_sources::RawHdrRefiningSource`] on HDR displays.
    pub hdr_developed_image: Option<Arc<PLRwLock<Option<crate::hdr::types::HdrImageBuffer>>>>,
    #[cfg_attr(not(feature = "preload-debug"), allow(dead_code))]
    pub hdr_target_capacity: f32,
    #[allow(dead_code)]
    // Snapshot at refine queue time; display tone comes from settings at render.
    pub hdr_tone_map: crate::hdr::types::HdrToneMapSettings,
}
