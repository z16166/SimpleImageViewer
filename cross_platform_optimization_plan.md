# Cross-Platform Gigapixel Rendering Optimization Plan

This document outlines the strategy for aligning macOS and Linux rendering performance with the Windows WIC "Extremity Balance" pipeline. The goal is to achieve near-instantaneous 512px previews followed by seamless, asynchronous 4096px high-fidelity refinement for gigapixel (tiled) images.

## 1. Audit Findings

### Windows (Reference State: WIC)
- **Status**: **Fully Optimized**.
- **Mechanics**: Dual-source strategy using a raw (non-cached) `IWICBitmapSource` for preview generation and a cached source for tiling. Asynchronous 4096px Refinement is decoupled from the main UI and tiling threads.

### macOS (State: Sub-optimal)
- **Current Path**: `src/macos_image_io.rs` -> `ImageIOTiledSource`.
- **Latency Issue**: Currently shares a single `CGImage` handle. Generating a large preview (4096px) can trigger internal ImageIO locks or "thrash" the cache, causing tile-rendering stutters during the refinement phase.
- **Resource Usage**: Without explicit `kCGImageSourceShouldCache: false` on the preview path, ImageIO might cache the 4096px result in a way that competes with tile memory.

### Linux (State: Basic)
- **Current Path**: `src/linux_tiff.rs` or `GenericTiledSource`.
- **Latency Issue**: Relies on `image-rs` or naive LibTIFF scaling. Preview generation is often "all-or-nothing" (load whole image then scale), which is catastrophic for 100MP+ images.

---

## 2. Implementation Plan for macOS (ImageIO)

### Objective: Decouple the Refinement Path
Modify `ImageIOTiledSource` to support independent preview generation.

#### [MODIFY] macos_image_io.rs

1.  **Dual Handle Strategy**:
    - Ensure `ImageIOTiledSource` keeps the `CGImageSourceRef` alive.
    - Inside `generate_preview`, create a **separate** transient `options` dictionary with `kCGImageSourceShouldCache` set to `false`.
    
2.  **Optimized Native Scaling**:
    - Utilize `CGImageSourceCreateThumbnailAtIndex` with `kCGImageSourceThumbnailMaxPixelSize` set to 4096.
    - Ensure `kCGImageSourceCreateThumbnailWithTransform` is `true` to respect EXIF orientation.
    - **Crucial**: Perform this inside `generate_preview` so the `loader.rs` refinement thread can handle it asynchronously.

3.  **Memory Guard**:
    - Use `CFRelease` promptly for transient objects to ensure the background refinement doesn't spike memory usage on low-RAM Macs.

---

## 3. Implementation Plan for Linux (Generic/TIFF)

### Objective: Progressive Down-sampling
Optimize `GenericTiledSource` to avoid full-resolution decodes for previews.

#### [MODIFY] loader.rs / linux_tiff.rs

1.  **Thumbnail Extraction First**:
    - Strengthen `extract_exif_thumbnail` to handle more Linux-common formats (e.g., specific TIFF sub-IFDs).
    
2.  **Streamed Scaling (For TIFF)**:
    - If using `linux_tiff.rs`, implement a "stride-based" reader for `generate_preview`. Instead of reading every pixel, skip rows/columns at the LibTIFF level to generate the 512px and 4096px previews.

3.  **Thread Priority**:
    - In `loader.rs`, use `thread_priority` (if available) or simply ensure the 4096px task on Linux is partitioned into smaller chunks to keep the event loop responsive.

---

## 4. Shared Refinement Logic (UI/App Level)

### Continuous OSD
- Ensure OSD (Status text) is drawn at the END of the rendering loop (highest Z-index).
- Use the "Pre-detection" logic in `app.rs`:
  ```rust
  if width * height > TILED_THRESHOLD { 
      mode_tag = "TILED"; // Display immediately even if manager is pending
  }
  ```

### Progressive Transition
- The `texture_cache` should seamlessly transition from the 512px fallback to the 4096px refinement without flickering. Ensure `ctx.request_repaint()` is called appropriately when the refinement result arrives in `handle_preview_update`.

---

## 5. Summary of Key Instructions for AI Agents

> [!IMPORTANT]
> **Priority 1**: Do not let `generate_preview` block the main thread.
> **Priority 2**: Bypassing internal cache for the preview path is mandatory (prevent memory spikes).
> **Priority 3**: Visual continuity — the UI must show `[TILED]` and the 512px image immediately while the 4096px version is "filling in".

---
**Document Status**: *Ready for implementation on target platforms.*
