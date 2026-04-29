# HDR Image Plane Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an HDR-capable image rendering path for Windows 11 and macOS where the image plane can render HDR content while egui remains the SDR overlay/UI layer.

**Architecture:** Keep the existing RGBA8/egui texture pipeline as the default path. Add a parallel HDR path that preserves floating-point image data, uploads it to wgpu textures, and draws it with a custom image-plane renderer before egui overlays. Platform HDR output is isolated behind capability/config modules so unsupported systems fall back to SDR tone mapping.

**Tech Stack:** Rust 2024, eframe/egui 0.34, wgpu 29, image 0.25 (`hdr`, `exr`), Windows DX12/scRGB detection, macOS Metal/EDR detection.

---

## File Structure

- Create `src/hdr/mod.rs`: module exports and feature boundaries.
- Create `src/hdr/types.rs`: HDR pixel formats, color-space tags, display mode, tone-map settings.
- Create `src/hdr/capabilities.rs`: cross-platform HDR capability model and public detection API.
- Create `src/hdr/decode.rs`: EXR/HDR decode helpers that preserve float data and produce SDR fallback previews.
- Create `src/hdr/renderer.rs`: custom wgpu image-plane renderer interface and SDR fallback implementation.
- Modify `src/main.rs`: register `hdr` module.
- Modify `src/loader.rs`: add HDR-aware image data type without disturbing current SDR path.
- Modify `src/app/lifecycle.rs`: initialize HDR capabilities from the eframe/wgpu creation context.
- Modify `src/app/rendering/mod.rs` and `src/app/rendering/tiled.rs`: route HDR images through the image plane when available, egui overlays remain unchanged.
- Modify `Cargo.toml`: add `half` only if `image`/EXR output needs explicit `f16` buffers after initial experiments.

---

## Task 1: HDR Domain Types and Capability Model

**Files:**
- Create: `src/hdr/mod.rs`
- Create: `src/hdr/types.rs`
- Create: `src/hdr/capabilities.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Add module shell**

Create `src/hdr/mod.rs`:

```rust
pub mod capabilities;
pub mod types;
```

Modify `src/main.rs` near existing module declarations:

```rust
mod hdr;
```

- [ ] **Step 2: Define HDR data/control types**

Create `src/hdr/types.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HdrPixelFormat {
    Rgba16Float,
    Rgba32Float,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HdrColorSpace {
    LinearSrgb,
    LinearScRgb,
    Rec2020Linear,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HdrOutputMode {
    SdrToneMapped,
    WindowsScRgb,
    MacOsEdr,
}

#[derive(Debug, Clone, Copy)]
pub struct HdrToneMapSettings {
    pub exposure_ev: f32,
    pub sdr_white_nits: f32,
    pub max_display_nits: f32,
}

impl Default for HdrToneMapSettings {
    fn default() -> Self {
        Self {
            exposure_ev: 0.0,
            sdr_white_nits: 203.0,
            max_display_nits: 1000.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct HdrImageBuffer {
    pub width: u32,
    pub height: u32,
    pub format: HdrPixelFormat,
    pub color_space: HdrColorSpace,
    pub rgba_f32: std::sync::Arc<Vec<f32>>,
}
```

- [ ] **Step 3: Define capability API**

Create `src/hdr/capabilities.rs`:

```rust
use super::types::HdrOutputMode;

#[derive(Debug, Clone)]
pub struct HdrCapabilities {
    pub available: bool,
    pub output_mode: HdrOutputMode,
    pub reason: String,
    pub preferred_texture_format: Option<wgpu::TextureFormat>,
}

impl HdrCapabilities {
    pub fn sdr(reason: impl Into<String>) -> Self {
        Self {
            available: false,
            output_mode: HdrOutputMode::SdrToneMapped,
            reason: reason.into(),
            preferred_texture_format: None,
        }
    }
}

pub fn detect_from_wgpu_state(state: Option<&eframe::egui_wgpu::RenderState>) -> HdrCapabilities {
    let Some(state) = state else {
        return HdrCapabilities::sdr("wgpu render state unavailable");
    };
    let backend = state.adapter.get_info().backend;
    match backend {
        #[cfg(target_os = "windows")]
        wgpu::Backend::Dx12 => HdrCapabilities {
            available: false,
            output_mode: HdrOutputMode::SdrToneMapped,
            reason: "DX12 available; HDR surface configuration pending".to_string(),
            preferred_texture_format: Some(wgpu::TextureFormat::Rgba16Float),
        },
        #[cfg(target_os = "macos")]
        wgpu::Backend::Metal => HdrCapabilities {
            available: false,
            output_mode: HdrOutputMode::SdrToneMapped,
            reason: "Metal available; EDR layer configuration pending".to_string(),
            preferred_texture_format: Some(wgpu::TextureFormat::Rgba16Float),
        },
        _ => HdrCapabilities::sdr(format!("unsupported HDR backend: {backend:?}")),
    }
}
```

- [ ] **Step 4: Verify**

Run:

```powershell
cargo check
```

Expected: build succeeds; no behavior change.

---

## Task 2: Decode HDR Sources Without Losing Float Data

**Files:**
- Create: `src/hdr/decode.rs`
- Modify: `src/hdr/mod.rs`
- Modify: `src/loader.rs`

- [ ] **Step 1: Add decode module**

Modify `src/hdr/mod.rs`:

```rust
pub mod capabilities;
pub mod decode;
pub mod types;
```

- [ ] **Step 2: Add decode result helper**

Create `src/hdr/decode.rs`:

```rust
use super::types::{HdrColorSpace, HdrImageBuffer, HdrPixelFormat};
use std::path::Path;
use std::sync::Arc;

pub fn is_hdr_candidate_ext(ext: &str) -> bool {
    matches!(ext.to_ascii_lowercase().as_str(), "exr" | "hdr")
}

pub fn decode_hdr_image(path: &Path) -> Result<HdrImageBuffer, String> {
    let dyn_img = image::ImageReader::open(path)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?
        .decode()
        .map_err(|e| e.to_string())?;

    let rgba = dyn_img.into_rgba32f();
    let (width, height) = rgba.dimensions();
    Ok(HdrImageBuffer {
        width,
        height,
        format: HdrPixelFormat::Rgba32Float,
        color_space: HdrColorSpace::LinearSrgb,
        rgba_f32: Arc::new(rgba.into_raw()),
    })
}

pub fn hdr_to_sdr_rgba8(buffer: &HdrImageBuffer, exposure_ev: f32) -> Vec<u8> {
    let exposure = 2.0_f32.powf(exposure_ev);
    let mut out = Vec::with_capacity(buffer.width as usize * buffer.height as usize * 4);
    for px in buffer.rgba_f32.chunks_exact(4) {
        let r = tone_map_channel(px[0] * exposure);
        let g = tone_map_channel(px[1] * exposure);
        let b = tone_map_channel(px[2] * exposure);
        let a = px[3].clamp(0.0, 1.0);
        out.extend_from_slice(&[
            (r * 255.0 + 0.5) as u8,
            (g * 255.0 + 0.5) as u8,
            (b * 255.0 + 0.5) as u8,
            (a * 255.0 + 0.5) as u8,
        ]);
    }
    out
}

fn tone_map_channel(v: f32) -> f32 {
    let v = v.max(0.0);
    let mapped = v / (1.0 + v);
    mapped.powf(1.0 / 2.2).clamp(0.0, 1.0)
}
```

- [ ] **Step 3: Add `ImageData::Hdr` only after SDR fallback is proven**

Modify `src/loader.rs` enum `ImageData` by adding:

```rust
Hdr(crate::hdr::types::HdrImageBuffer),
```

Then route `exr`/`hdr` through `decode_hdr_image` and build an SDR fallback with `hdr_to_sdr_rgba8` for existing UI until Task 3 exists.

- [ ] **Step 4: Verify**

Run:

```powershell
cargo check
```

Expected: HDR files still display through the existing SDR path; logs can confirm HDR decode path.

---

## Task 3: Custom wgpu Image Plane Interface

**Files:**
- Create: `src/hdr/renderer.rs`
- Modify: `src/hdr/mod.rs`
- Modify: `src/app/lifecycle.rs`
- Modify: `src/app/rendering/mod.rs`

- [ ] **Step 1: Add renderer module**

Modify `src/hdr/mod.rs`:

```rust
pub mod capabilities;
pub mod decode;
pub mod renderer;
pub mod types;
```

- [ ] **Step 2: Add renderer struct skeleton**

Create `src/hdr/renderer.rs`:

```rust
use super::types::{HdrImageBuffer, HdrToneMapSettings};

pub struct HdrImageRenderer {
    pub tone_map: HdrToneMapSettings,
}

impl HdrImageRenderer {
    pub fn new() -> Self {
        Self {
            tone_map: HdrToneMapSettings::default(),
        }
    }

    pub fn upload_image(&mut self, _device: &wgpu::Device, _queue: &wgpu::Queue, _image: &HdrImageBuffer) {
        // First implementation keeps existing SDR path; Task 4 owns GPU texture upload.
    }
}
```

- [ ] **Step 3: Store renderer/capabilities in app state**

Add fields to `ImageViewerApp`:

```rust
pub(crate) hdr_capabilities: crate::hdr::capabilities::HdrCapabilities,
pub(crate) hdr_renderer: crate::hdr::renderer::HdrImageRenderer,
```

Initialize in `app/lifecycle.rs` from `cc.wgpu_render_state.as_ref()`.

- [ ] **Step 4: Verify**

Run:

```powershell
cargo check
```

Expected: build succeeds; Settings/OSD can later expose capability reason.

---

## Task 4: SDR Fallback via Shader-Compatible Image Plane

**Files:**
- Modify: `src/hdr/renderer.rs`
- Modify: `src/app/rendering/mod.rs`

- [ ] **Step 1: Upload HDR float buffer to `Rgba32Float` or `Rgba16Float` texture**

Implement `upload_image` to create a `wgpu::Texture` with `TEXTURE_BINDING | COPY_DST`, write rows from `HdrImageBuffer::rgba_f32`, and store `TextureView`.

- [ ] **Step 2: Add WGSL shader**

Embed WGSL in `src/hdr/renderer.rs`:

```wgsl
@group(0) @binding(0) var hdr_tex: texture_2d<f32>;
@group(0) @binding(1) var hdr_sampler: sampler;

fn tone_map(v: vec3<f32>) -> vec3<f32> {
    let x = max(v, vec3<f32>(0.0));
    return pow(x / (vec3<f32>(1.0) + x), vec3<f32>(1.0 / 2.2));
}
```

Final first-pass output should be SDR-compatible so the path is testable before real HDR surfaces.

- [ ] **Step 3: Draw before egui overlay**

Use egui/wgpu custom paint callback or a dedicated renderer hook if available in eframe 0.34. The pass must draw the image rect and let egui draw UI after it.

- [ ] **Step 4: Verify**

Run:

```powershell
cargo check
cargo test
```

Manual expected: EXR/HDR looks similar to existing SDR conversion, but comes from custom renderer.

---

## Task 5: Platform HDR Output Detection and Enablement

**Files:**
- Modify: `src/hdr/capabilities.rs`
- Modify: `src/main.rs`
- Modify: `Cargo.toml` if platform crates are required.

- [ ] **Step 1: Windows 11 DX12/scRGB research spike**

Check whether eframe/wgpu 29 exposes surface color-space selection required for `Rgba16Float`/scRGB presentation. If not, keep HDR output disabled and document the missing API boundary in code comments.

- [ ] **Step 2: macOS Metal EDR research spike**

Check whether eframe/wgpu 29 exposes enough access to `CAMetalLayer` to set EDR/extended dynamic range. If not, keep HDR output disabled and document the missing API boundary.

- [ ] **Step 3: Capability reporting**

Add log lines on startup:

```rust
log::info!("HDR output: {:?} ({})", caps.output_mode, caps.reason);
```

- [ ] **Step 4: Verify**

Run on Windows:

```powershell
cargo check
```

Run on macOS:

```bash
cargo check
```

Expected: no crash; capability logs are clear.

---

## Task 6: HDR Tiled Rendering Extension

**Files:**
- Modify: `src/tile_cache.rs`
- Modify: `src/loader.rs`
- Modify: `src/hdr/types.rs`

- [ ] **Step 1: Generalize tile pixel format**

Add:

```rust
pub enum TilePixelBuffer {
    SdrRgba8(std::sync::Arc<Vec<u8>>),
    HdrRgba32F(std::sync::Arc<Vec<f32>>),
}
```

- [ ] **Step 2: Keep existing tile path unchanged**

Do not migrate SDR tile code until HDR full-image path is working. Add separate HDR tile cache only for EXR/HDR images larger than the tiled threshold.

- [ ] **Step 3: Verify**

Run:

```powershell
cargo check
cargo test
```

Manual expected: existing large SDR images behave exactly as before; HDR large images can use SDR fallback until HDR tile path is complete.

---

## Self-Review

- Spec coverage: The plan covers decode preservation, renderer separation, SDR egui overlay, Windows/macOS HDR detection, and tiled extension.
- Placeholder scan: No implementation step depends on an unnamed future API without first making it a research spike and SDR fallback.
- Type consistency: `HdrImageBuffer`, `HdrCapabilities`, `HdrImageRenderer`, and `HdrToneMapSettings` are introduced before use.
- Scope: Linux HDR is explicitly out of first implementation scope; unsupported systems use SDR tone mapping.

