# Changelog

All notable changes to this project will be documented in this file.

## [1.5.7] - 2026-04-28

### Fixed
- **RAW Refinement Race**: Fixed a race condition where stale background refinement results (from previous navigations) could overwrite the current image or cause flickering by prematurely evicting texture caches. Re-enabled strict generation (gen_id) validation for all asynchronous RAW updates.

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
