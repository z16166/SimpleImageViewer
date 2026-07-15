//! Types for the PSD/PSB layer compositor — data structures, dimension
//! helpers, and budget checks extracted from the compositor for clarity.

use crate::psb_reader_util::SharedSlice;

/// Cap on `width * height` for a single layer/mask rect.
///
/// `PSD_MAX_DIMENSION` alone still allows `300_000 x 300_000` (~90GB for one
/// 8-bit channel). This pixel budget keeps malicious/malformed layer bounds
/// from OOM-killing the process while still allowing large legitimate layers
/// (e.g. 32k x 32k, or a long strip up to `PSD_MAX_DIMENSION` on one side).
pub(crate) const MAX_LAYER_PIXELS: u64 = crate::psb_reader::MAX_DOCUMENT_PIXELS;

/// Cap on decoded layer RGBA8 bytes (CPU batch) and estimated GPU peak VRAM
/// (layer textures + canvas + readback + clip scratch) for one composite pass.
///
/// Per-layer pixel caps alone still allow many large layers to be decoded in
/// parallel and retained until blending finishes. 8 GiB bounds that without
/// rejecting typical multi-layer comps on a desktop viewer.
pub(crate) const MAX_COMPOSITE_DECODED_BYTES: u64 = 8 * 1024 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct LayerChannel {
    pub id: i16,
    pub data_len: u32,
}

/// Raw path records from a `vmsk`/`vsms` vector mask tagged block.
///
/// Each entry is one 26-byte path record (selector i16 + three 8-byte
/// fixed-point sub-points). The decode step rasterises these into a mask
/// bitmap (see `psb_layer_decode::rasterize_vector_mask`).
pub(crate) const VMSK_RECORD_LEN: usize = 26;

/// Vector mask origin clipping/relative flags, parsed from the vmsk Flags u32.
///
/// Bit 0: invert     — the opaque interior becomes transparent and vice versa.
/// Bit 1: not-linked  — mask moves/scales independently of layer pixels.
/// Bit 2: disable     — mask is present on disk but should not be applied.
#[derive(Debug, Clone, Copy, Default)]
pub struct VectorMaskFlags {
    pub invert: bool,
    /// Mask moves/scales independently of layer pixels.  Parsed from the
    /// vmsk Flags u32 bit 1.  Retained for spec completeness.
    #[allow(dead_code)]
    pub not_linked: bool,
    pub disabled: bool,
}

#[derive(Debug, Clone)]
pub struct VectorMaskData {
    pub records: Vec<[u8; VMSK_RECORD_LEN]>,
    pub flags: VectorMaskFlags,
}

/// Parsed rectangle + flags from a layer's mask data block (channel id -2).
/// The mask's own rect can differ from the layer's rect (smaller, larger, or
/// offset), so it is decoded and blitted separately -- see `build_layer_sized_mask`.
#[derive(Debug, Clone, Copy)]
pub struct LayerMaskInfo {
    pub top: i32,
    pub left: i32,
    pub bottom: i32,
    pub right: i32,
    /// Value (0 or 255) used for layer pixels outside the mask rect.
    pub default_color: u8,
    /// Mask disabled (bit 1 of the mask flags byte): treat as if no mask.
    pub disabled: bool,
    /// Flags bit 4: user/vector masks have density/feather parameters. When
    /// set, extra parameter bytes sit between the user-mask header and any
    /// real-user-mask rect in the mask data block.
    pub has_parameters_applied: bool,
    /// Mask density (0-255): 255 = full opacity (no change), 0 = fully
    /// transparent (mask becomes all zeros).  Parsed from the density/feather
    /// parameter prefix when `has_parameters_applied` is true.
    pub density: u8,
    /// Mask feather radius in pixels (0.0 = no feather).  Parsed from the
    /// density/feather parameter prefix when `has_parameters_applied` is true.
    pub feather: f64,
}

impl LayerMaskInfo {
    pub(crate) fn is_empty_bounds(&self) -> bool {
        self.left >= self.right || self.top >= self.bottom
    }

    pub fn width(&self) -> u32 {
        if self.is_empty_bounds() {
            0
        } else {
            self.right.saturating_sub(self.left) as u32
        }
    }

    pub fn height(&self) -> u32 {
        if self.is_empty_bounds() {
            0
        } else {
            self.bottom.saturating_sub(self.top) as u32
        }
    }

    /// Returns `true` when the mask needs density scaling or feather blur.
    pub fn needs_post_process(&self) -> bool {
        self.density < 255 || self.feather > 0.0
    }
}

#[derive(Debug, Clone)]
pub struct LayerRecord {
    pub top: i32,
    pub left: i32,
    pub bottom: i32,
    pub right: i32,
    /// Layer name, preferring the Unicode `luni` tagged block over the Pascal
    /// layer-name fallback in the extra data.
    pub name: String,
    /// Layer ID from the `lyid` tagged block, used externally and in tests.
    #[allow(dead_code)]
    pub layer_id: Option<u32>,
    /// Raw `cmls` descriptor payload found inside `shmd`, retained for the
    /// later Layer Comp descriptor pass.
    pub cmls_payload: Option<Vec<u8>>,
    pub channels: Vec<LayerChannel>,
    pub blend: [u8; 4],
    pub opacity: u8,
    /// Fill opacity from the `iOpa` tagged block (0-255). `None` means the
    /// block was absent (treat as 255). Distinct from layer `opacity`: fill
    /// affects pixel fill only; without layer effects the two combine into
    /// source alpha for this compositor.
    pub fill_opacity: Option<u8>,
    pub clipping: u8,
    pub flags: u8,
    /// Raw length of the layer mask data block, retained for mask parsing.
    #[allow(dead_code)]
    pub mask_size: u32,
    /// User mask rect/flags parsed from the mask data block, when present and
    /// long enough to contain the standard rect + default color + flags fields.
    pub mask: Option<LayerMaskInfo>,
    /// Real user mask rect (channel id -3), when the mask data block includes
    /// a second rect after the user-mask header (typically `mask_size >= 36`).
    pub real_mask: Option<LayerMaskInfo>,
    /// Vector mask path data from a `vmsk`/`vsms` tagged block.
    pub vector_mask: Option<VectorMaskData>,
    /// Vector mask density (0-255) from the Layer Mask Data parameters block.
    pub vector_mask_density: u8,
    /// Vector mask feather radius (pixels) from the Layer Mask Data parameters.
    pub vector_mask_feather: f64,
    pub is_section_divider: bool,
    pub section_type: Option<u32>,
}

/// `lsct` section type constants for [`LayerRecord::section_type`].
///
/// See Adobe Photoshop PSD specification § `Layer section divider`.
pub(crate) const SECTION_TYPE_OPEN_FOLDER: u32 = 1;
pub(crate) const SECTION_TYPE_CLOSED_FOLDER: u32 = 2;
pub(crate) const SECTION_TYPE_BOUNDING_DIVIDER: u32 = 3;
pub(crate) const SECTION_TYPE_LAYER_GROUP: u32 = 4;

impl LayerRecord {
    pub fn is_hidden(&self) -> bool {
        self.flags & 2 != 0
    }

    /// Fill opacity byte used when assembling pixels (`255` when `iOpa` absent).
    #[inline]
    pub fn fill_opacity_or_full(&self) -> u8 {
        self.fill_opacity.unwrap_or(255)
    }

    /// Layer opacity combined with fill opacity (no layer-effects path yet).
    #[inline]
    pub fn effective_fill_opacity(&self) -> u8 {
        let fill = u16::from(self.fill_opacity_or_full());
        ((u16::from(self.opacity) * fill) / 255) as u8
    }

    pub fn is_empty_bounds(&self) -> bool {
        self.left >= self.right || self.top >= self.bottom
    }

    pub fn width(&self) -> u32 {
        if self.is_empty_bounds() {
            0
        } else {
            self.right.saturating_sub(self.left) as u32
        }
    }

    pub fn height(&self) -> u32 {
        if self.is_empty_bounds() {
            0
        } else {
            self.bottom.saturating_sub(self.top) as u32
        }
    }
}

/// Whether `width`/`height` are within the per-side limit used for the
/// document canvas (`psb_reader::PSD_MAX_DIMENSION`) and the total pixel cap
/// (`MAX_LAYER_PIXELS`).
///
/// A malformed or malicious layer record can claim absurd bounds (e.g.
/// `right - left` near `u32::MAX`, or both sides at `PSD_MAX_DIMENSION`) that
/// would make `decode_channel_image`'s allocation try to reserve tens of GB
/// and abort the process. Layer and mask rects are checked before any such
/// allocation.
pub(crate) fn dimensions_within_limit(width: u32, height: u32) -> bool {
    if width > crate::psb_reader::PSD_MAX_DIMENSION || height > crate::psb_reader::PSD_MAX_DIMENSION
    {
        return false;
    }
    match (width as u64).checked_mul(height as u64) {
        Some(pixels) => pixels <= MAX_LAYER_PIXELS,
        None => false,
    }
}

/// `width * height` for a layer/mask buffer, rejecting overflow.
pub(crate) fn checked_layer_pixel_count(width: u32, height: u32) -> Option<usize> {
    (width as u64)
        .checked_mul(height as u64)
        .filter(|&n| n <= MAX_LAYER_PIXELS)
        .and_then(|n| usize::try_from(n).ok())
}

/// Byte footprint of a layer's decoded straight-alpha RGBA8 buffer.
fn layer_rgba_byte_len(width: u32, height: u32) -> Option<u64> {
    checked_layer_pixel_count(width, height).map(|pixels| pixels as u64 * 4)
}

/// Add one layer's RGBA8 footprint to `acc`, enforcing
/// [`MAX_COMPOSITE_DECODED_BYTES`].
pub(crate) fn accumulate_decoded_layer_bytes(
    acc: u64,
    width: u32,
    height: u32,
) -> Result<u64, String> {
    let rgba = layer_rgba_byte_len(width, height)
        .ok_or_else(|| format!("PSD/PSB layer channel size {width}x{height} exceeds limit"))?;
    let next = acc
        .checked_add(rgba)
        .ok_or_else(|| "PSD/PSB decoded layer byte total overflow".to_string())?;
    if next > MAX_COMPOSITE_DECODED_BYTES {
        return Err(format!(
            "PSD/PSB decoded layer byte budget exceeded ({next} > {MAX_COMPOSITE_DECODED_BYTES})"
        ));
    }
    Ok(next)
}

/// CPU streaming budget check: the layer currently held plus the next one
/// about to be prefetched (the only [`LAYER_PREFETCH_WINDOW`] `DecodedLayer`s
/// ever resident at once) must fit [`MAX_COMPOSITE_DECODED_BYTES`]. Unlike the
/// GPU batch path, this never sums the whole layer stack up front.
pub(crate) fn check_streaming_pair_budget(
    current: &crate::psb_layer_decode::DecodedLayer,
    next_width: u32,
    next_height: u32,
) -> Result<(), crate::loader::DecodeError> {
    let current_bytes = layer_rgba_byte_len(current.width, current.height).unwrap_or(0);
    let Some(next_bytes) = layer_rgba_byte_len(next_width, next_height) else {
        return Ok(());
    };
    let total = current_bytes
        .checked_add(next_bytes)
        .ok_or_else(|| "PSD/PSB decoded layer byte total overflow".to_string())?;
    if total > MAX_COMPOSITE_DECODED_BYTES {
        return Err(format!(
            "PSD/PSB decoded layer byte budget exceeded ({total} > {MAX_COMPOSITE_DECODED_BYTES})"
        )
        .into());
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) struct LayerInfo<'a> {
    pub records: Vec<LayerRecord>,
    pub channel_data: &'a [u8],
    /// Shared ownership of the channel data section, enabling zero-copy
    /// sub-slicing for RAW-compressed channels.
    pub channel_data_shared: Option<SharedSlice>,
    pub width: u32,
    pub height: u32,
    pub depth: u16,
    pub color_mode: u16,
    pub is_psb: bool,
    /// Resolved CMYK ICC (embedded or default). Empty when not CMYK.
    pub cmyk_icc: Vec<u8>,
}

pub(crate) struct CompositeTiming {
    #[cfg_attr(not(feature = "preload-debug"), allow(dead_code))]
    pub(crate) parse_ms: f64,
    #[cfg_attr(not(feature = "preload-debug"), allow(dead_code))]
    pub(crate) unpack_ms: f64,
    #[cfg_attr(not(feature = "preload-debug"), allow(dead_code))]
    pub(crate) cmyk_ms: f64,
    #[cfg_attr(not(feature = "preload-debug"), allow(dead_code))]
    pub(crate) blend_ms: f64,
    #[cfg_attr(not(feature = "preload-debug"), allow(dead_code))]
    pub(crate) readback_ms: f64,
    #[cfg_attr(not(feature = "preload-debug"), allow(dead_code))]
    pub(crate) mode: &'static str,
    #[cfg_attr(not(feature = "preload-debug"), allow(dead_code))]
    pub(crate) layers: usize,
}
