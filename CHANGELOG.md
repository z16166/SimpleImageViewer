# Changelog

All notable changes to this project will be documented in this file.

## [1.5.1] - 2026-04-23

### Added
- **Monkey Audio (APE)**: Migrated to official CMake-based build system for the SDK.
- **SIMD Acceleration**: Enabled AVX2, AVX512, and Neon hardware acceleration for Monkey Audio decoding.
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
