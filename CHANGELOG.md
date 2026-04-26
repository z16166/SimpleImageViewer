# Changelog

All notable changes to this project will be documented in this file.

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
