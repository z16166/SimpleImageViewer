# Changelog

All notable changes to this project will be documented in this file.


## [2.0.1] - 2026-05-06

### Fixed
- **HEIF / HEIC (HDR)**: Phone and camera shots that looked sideways or upside down now open in the correct orientation, including many **HDR / wide-color** `.heic` files where the viewer previously ignored rotation metadata.

### Improved
- **OpenEXR (.exr)**: Noticeably **faster** to open and preview very large files—scrolling and zooming huge EXRs should feel **snappier**, with work spread across your CPU instead of stalling one core.
- **HDR color labels**: Images that carry an **embedded ICC profile** are more reliably described in HDR status (for example **Display P3** vs **Rec.709**), instead of falling back to a vague “unknown” gamut when the file really did include profile data.
- **HDR preview noise**: Fewer **duplicate preview jobs** when you flip through folders quickly, and **less log spam** from harmless HDR preview updates so troubleshooting stays readable (`tile-debug` still exposes extra detail when you need it).

### Changed
- **Heavy HDR / tiled previews**: Background preview work is **capped more safely** so the app stays responsive when you stress it with huge images or rapid navigation.


## [2.0.0] - 2026-05-05

### Added
- **HDR viewing & tone mapping**: Scene-linear HDR pipeline with adjustable exposure (EV), PQ/HLG and scRGB-style paths where supported, tiled HDR for large images, and on-screen HDR status where applicable.
- **GPU backends**: HDR and modern formats use **WGPU** with **DirectX 12** on Windows and **Metal** on macOS for composition and presentation.
- **Format support** (native decode paths where noted):
    - **OpenEXR** (.exr) via OpenEXRCore, including large/tiled EXR workflows.
    - **AVIF / AVIFS** and **HEIF / HEIC** via libavif / libheif (HDR-capable where the bitstream allows).
    - **JPEG gain-map HDR** (Ultra HDR / `JPEG_R`): decode and display with capacity-aware handling.
    - **JPEG XL** (.jxl) as an optional native path when enabled in the build.
    - **TIFF**: extended coverage for float / LogLuv / high bit-depth and HDR-oriented TIFFs via libtiff integration (not every TIFF variant).

### Changed
- **TurboJPEG**: Treat non-fatal **`tjGetErrorCode` warning** as success after `tjDecompressHeader3` / `tjDecompress2` so JPEGs with reserved/unknown markers (e.g. `0x9d`) still decode instead of aborting.
- **MINISWHITE float grayscale TIFF**: File-level white reference using `SMaxSampleValue` or image-wide maximum (not per-scanline pivot); corrected **`TIFFGetField`** scalar read for `SMinSampleValue` / `SMaxSampleValue`.

### Notes
- Requires up-to-date **libjpeg-turbo** (TurboJPEG **`tjGetErrorCode`**, ≥ 1.6) when using the bundled static link.


## [1.5.8] - 2026-04-29

### Changed
- **Zero-Copy Pixel Pipeline**: Major optimization of the image decoding and rendering path to minimize memory allocations and redundant data copies.
    - **LibRaw RAII & Single-Pass Packing**: Implemented `LibRawMemory` RAII wrapper for automatic FFI memory management. RAW development now uses a SIMD-accelerated "single-pass" conversion from LibRaw's internal RGB buffers directly to Rust RGBA buffers, eliminating a redundant intermediate 400MB copy.
    - **Zero-Copy Tile Management**: Updated `TiledImageSource` trait and `TilePixelCache` to use `Arc<Vec<u8>>`. Decoded tiles are now passed by reference (Arc) from decoders to the cache, avoiding megabytes of buffer moves per frame during gigapixel image exploration.
    - **Buffer Reuse**: Replaced `to_rgba8()` with `into_rgba8()` in hot paths (refinement worker, preview generation) to move existing buffers instead of cloning them.
- **SIMD Interleaving Utility**: Centralized high-performance pixel swizzling logic in a new `simd_swizzle` module with AVX2, SSE4.1, and Neon support, ensuring consistent performance across RAW, PSB, and TIFF loaders. Added SSE4.1 paths for `interleave_rgb_with_alpha` on x86_64; completed Neon coverage for planar RGB/RGBA interleave helpers on aarch64; moved duplicated PSB SIMD out of `psb_reader` into the shared module.
- **Tiled Preview Cache Policy**: When re-opening a large (tiled) image, the synchronous stage-1 preview (EXIF thumbnail or small `generate_preview`) no longer overwrites `TextureCache` if it already holds a **larger** uploaded preview texture from stage-2 HQ generation (`TextureCache::cached_preview_max_side` compares the long side of the GPU texture). Prevents a brief “downgrade” from HQ back to LQ on navigation.
- **macOS Giant Stripped TIFFs**: `TiffStripCachingSource` is used for oversized strip-based TIFFs with **any** EXIF orientation (not only orientation `1`). Logical display coordinates are mapped to physical strip pixels via an inverse of the same EXIF transform used elsewhere; oriented tiles sample horizontal strips with a per-strip `Arc` buffer cache to avoid repeated `strip_cache` mutex traffic.
- **RGBA buffer sharing**: `DecodedImage` and `AnimationFrame` keep decoded RGBA8 in `Arc` buffers through decode, channels, and tiled memory sources where applicable, avoiding redundant full-buffer clones; tiled HQ preview work reuses `Arc::clone` on the source instead of cloning an entire `LoadResult` for the channel send.
- **Loader queue hygiene**: On navigation, stale entries are discarded from the unbounded loader receive path; a single delayed-fallback worker replaces per-request OS threads for the slow decode path. Arrow-key navigation is throttled to reduce load storms.
- **Async housekeeping**: Metadata extraction and wallpaper queries are deferred off the UI thread; a shared `FileOp` channel ensures delete/rename results are not dropped under load. Added i18n strings for async loading states.
- **HQ preview / refine resolution cap**: RAW refine, tiled HQ preview generation, and WIC/ImageIO “performance mode” RAW previews cap the longest side with `min(hardware tier, monitor cap, 4096)`. Tier limits (`HardwareTier::max_preview_size`: 1024 / 2048 / 4096) apply via `PREVIEW_LIMIT`. The monitor cap uses each visible frame’s egui viewport `monitor_size` (UI points) × `native_pixels_per_point` for physical pixels, then `ceil(max(width,height) × HQ_PREVIEW_MONITOR_HEADROOM)` (1.1), clamped to `[256, 4096]`; eframe supplies the monitor for the current window. `refresh_hq_preview_monitor_cap` runs on the UI thread while the window is not minimized.

### Fixed
- **LibRaw Memory Leak**: Fixed a critical bug where `libraw_dcraw_make_mem_image` was called twice per image, causing massive heap memory leaks and redundant buffer allocations.
- **SIMD Unsafe Warnings**: Resolved compiler warnings related to unsafe intrinsic calls in the new SIMD module.
- **SIMD RGB→RGBA Packed Bounds**: `interleave_rgb_packed_to_rgba_packed` now caps work to valid RGBA output and input length so LibRaw buffers with trailing padding cannot read past the intended RGB extent.
- **TilePixelCache Re-Insert**: Inserting a tile key that already exists now evicts the old entry first (LRU + byte accounting), avoiding overstated CPU cache usage.
- **macOS TIFF Strip Tile**: Restored a missing assignment in the CoreGraphics strip path so oriented tile assembly does not drop decoded strip data.
- **RAW Refinement Race**: Fixed a race condition where stale background refinement results (from previous navigations) could overwrite the current image or cause flickering by prematurely evicting texture caches. Re-enabled strict generation (gen_id) validation for all asynchronous RAW updates.
- **Deletion Race Safety**: Fixed a bug where deleting an image could cause the next image at the same index to briefly display data from the deleted file due to stale loader results being accepted. File removal now runs off the UI thread with optimistic delete and rollback on failure; rollback restores viewer state and re-queues the image load when appropriate.
- **Scan Consistency**: Fixed a consistency issue where preloading during a directory scan could result in displaying wrong images if the file indices shifted during the final global sort. Cancelling in-flight scans prevents background work from piling up; index-dependent live state is cleared after the final sort.
- **Loader stale decode**: Corrected an inverted guard in the image decode pool (and the coalescing delayed fallback worker): tasks whose navigation `generation` no longer matches the current global counter exit before decoding, instead of continuing when the load slot still held the old generation. Added a matching early check in `do_load`. Rapid paging no longer stacks full decodes for obsolete generations.
- **Post-scan “infinite loading”**: `ImageLoader::is_loading` is now generation-aware so a superseded load for an index does not block later `request_load` calls (e.g. after a directory scan completes).
- **Stale preview delivery**: `PreviewResult` handling and prefetched tiled-image preview upgrades validate generation so background previews cannot repoint the wrong entry in the texture cache.
- **Async metadata races**: EXIF/XMP and wallpaper queries validate the file path against the current scan generation before applying results, avoiding cross-talk when the directory list changes mid-flight.

## [1.5.6] - 2026-04-28

### Changed
- **Tiled Rendering Optimization**: Removed tile fade-in animations to eliminate redundant UI repaints, significantly reducing CPU/GPU usage during idle periods. Tiles now pop-in instantly at full opacity.
- **GPU Upload Quota**: Refined the per-frame GPU upload quota system. Background preloading is now strictly limited to prevent GPU command queue saturation, while the active image and high-quality previews bypass the quota for maximum responsiveness.
- **UI Refinement**: Streamlined the settings panel by removing the redundant "Exit Application" button and OS-specific quit hints.
- **High Quality RAW Control**: Fully implemented the "High Quality" toggle logic for the RAW pipeline. When disabled, the viewer prioritizes fast embedded thumbnails to save power; when enabled, it performs high-fidelity demosaicing for maximum visual accuracy.
- **Unified RAW Pipeline**: Standardized the RAW image loading sequence across all paths (preview, full development, and background refinement). Orientation is now determined by a centralized "source of truth" (LibRaw metadata with EXIF fallback), ensuring perfect visual parity between Windows and macOS.
- **Metadata Consistency**: Migrated EXIF orientation detection to a unified utility (`metadata_utils`), eliminating platform-specific metadata disparities between WIC, ImageIO, and native decoders.
- **RAW Compatibility Boost**: Enhanced support for high-end digital backs (e.g., Leaf MOS) by enabling hardware color matrices and optimizing auto-brightness normalization.

### Fixed
- **WGPU Stability**: Fixed a critical "Dimension X is zero" panic in the rendering pipeline by adding dimension sanitization for corrupted or malformed images.
- **Process Lifecycle**: Ensured the application terminates cleanly after a fatal crash by adding an explicit exit call to the emergency error dialog.
- **IPC Robustness**: Fixed a critical bug where oversized IPC messages were silently truncated and accepted. The system now explicitly detects and rejects payloads exceeding the 8KB safety limit, preventing malformed command execution. Improved handling with non-blocking operation on Windows to prevent application freezes.
- **IPC Consistency**: Unified Unix socket paths in `cleanup_stale_socket` to use the `IPC_SOCKET_NAME` constant.
- **Input System**: Enabled `F1` as a global toggle to both show and hide the settings panel.

## [1.5.5] - 2026-04-27

### Added
- **Input System Refactoring**: Replaced the hardcoded input logic with a prioritized, bitmask-based lookup table. This ensures consistent modifier matching (Ctrl/Cmd, Shift, Alt) across platforms and provides a foundation for future user-configurable hotkeys.
- **Unified Dialogs**: Replaced native system dialogs for Windows file association management with custom, theme-aware modal dialogs, achieving a more consistent and professional UI experience.
- **Modal Sequencing**: Improved the modal dispatching system to support sequential dialog flows, enabling "Success" or "Confirm" prompts to appear immediately after a primary operation is completed.

### Fixed
- **Hotkey Conflicts**: Resolved an issue where modified shortcuts (e.g., Ctrl+Arrow keys for rotation) were sometimes intercepted by simple navigation keys.
- **UI Focus**: Fixed a bug where the Tab key (used for OSD toggle) could cause egui to trap focus, leading to non-responsive keyboard input.
- **Accessibility**: Added the `=` key as a secondary shortcut for zooming in to improve accessibility for laptop keyboards without numeric pads.

## [1.5.4] - 2026-04-26

### Added
- **Audio Engine Refactoring**: Major structural overhaul of the audio thread. Extracted state into `AudioLoopState` and shared objects into `AudioSlots`, reducing the monolithic `run_audio_loop` from 700+ lines to a lean event loop for better maintainability.

### Fixed
- **APE+CUE Playback**: Resolved high-precision synchronization issues where the UI slider would lag behind track changes.
- **Playlist Looping**: Implemented seamless automatic looping of the music playlist (APE+CUE and standard files).
- **Audio Reliability**: Fixed potential deadlocks and UI flickering during file transitions by implementing synchronous state updates.
- **UI Settings**: Compacted music settings by grouping checkboxes horizontally to conserve vertical space.


## [1.5.3] - 2026-04-25

### Added
- **UI Architecture**: Introduced unified `MovableModal` system for all pop-up dialogs (EXIF, XMP, File Association, Go-to, etc.), featuring improved centering and modal backdrop logic.
- **Music Persistence**: Added support for resuming music playback across application restarts, including track selection and CUE sheet position.
- **File Association**: Refined the Windows file association dialog with localized format group names and a more professional, platform-agnostic terminology.

### Fixed
- **UI**: Fixed inconsistent button colors in light theme and resolved checkbox interaction issues in modal dialogs.
- **Egui 0.34.1**: Resolved all remaining deprecation warnings from the egui 0.34.1 update.

## [1.5.2] - 2026-04-24

### Added
- **UI**: Added `TAB` hotkey to quickly toggle the visibility of the on-screen display (OSD) HUD.

### Fixed
- **TIFF**: Replaced buggy manual scanline decoding with native libtiff RGBA output, fixing visual artifacts in 32-bit HDR, float TIFFs, and color inversion in CMYK/non-standard bit depths.
- **CI / Build**: Resolved MSVC `/MT` vs `/MD` CRT linkage conflicts on legacy Win7 CI pipelines.
- **CI / Build**: Updated Linux CI environment to GCC-10/Clang to fix AVX2 intrinsic bugs.
- **Cross-Compilation**: Fixed string pointer casting mismatch (`i8` vs `u8`) for `c_char` on AArch64 Linux.

## [1.5.1] - 2026-04-23

### Added
- **Monkey Audio (APE)**: Migrated to official CMake-based build system for the SDK.
- **SIMD Acceleration**: Enabled AVX2, AVX512, and Neon hardware acceleration for Monkey Audio decoding.
- **Unified JPEG Decoding**: Migrated all platforms to high-performance `libjpeg-turbo` for JPEG decoding, replacing system-native decoders (WIC/ImageIO) to ensure consistent and faster loading.
- **Zero-Copy Loading**: Implemented `memmap2` based memory-mapping for JPEG decoding to minimize memory allocations and improve performance for large images.
- **LibRaw Resilience**: Enabled JPEG support within LibRaw to improve loading for certain hybrid RAW/JPEG formats.

### Changed
- Decoupled Monkey SDK build from manual source lists, improving cross-platform maintainability.

### Fixed
- Cleaned up compiler warnings in `libraw-sys` and UI transitions logic.

## [1.5.0] - 2026-04-23

### Fixed
- **RAW Stability**: Resolved `ACCESS_VIOLATION` (0xc0000005) when loading Nikon NEF files by implementing strict FFI memory boundary checks using `data_size`.
- **RAW Color Accuracy**: Fixed the lavender/purple tint issue in RAW images by correctly enabling camera white balance and auto-brightness in the LibRaw engine.
- **Concurrency**: Fixed "data corrupted" errors when preloading multiple RAW files concurrently by enabling LibRaw's internal thread-safety mechanisms (removing `LIBRAW_NOTHREADS`).

### Added
- **High-Performance Parallelism**: Switched to a fully lock-free RAW processing pipeline. Demosaicing (the most intensive part) now runs in true parallel across all CPU cores.
- **Robustness**: Added automatic fallback to system WIC/preview rendering if native RAW development reports warnings or corruption.
- **I18n**: Added missing translations for buffer alignment and memory errors.

### Changed
- Updated LibRaw internal C API to expose necessary white balance and error-tracking controls.
