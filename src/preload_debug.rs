// Simple Image Viewer - A high-performance, cross-platform image viewer
// Copyright (C) 2024-2026 Simple Image Viewer Contributors
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

//! Preload / RAW load diagnostics behind the `preload-debug` feature.
//!
//! Build: `cargo build --features preload-debug`
//! Run:   set `SIV_LOG_LEVEL=info` (optional `SIV_LOG_FILE=1` for a log file)

/// Info-level preload / RAW pipeline diagnostics. No-op unless `--features preload-debug`.
#[macro_export]
macro_rules! preload_debug {
    ($($arg:tt)*) => {
        #[cfg(feature = "preload-debug")]
        {
            log::info!($($arg)*);
        }
    };
}

#[inline]
pub(crate) fn path_is_raw(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| crate::raw_processor::is_raw_extension(ext))
}

#[cfg(feature = "preload-debug")]
pub(crate) fn summarize_image_data(data: &crate::loader::ImageData) -> String {
    use crate::loader::ImageData;
    match data {
        ImageData::Static(img) => format!("StaticSdr {}x{}", img.width, img.height),
        ImageData::Hdr { hdr, fallback, .. } => format!(
            "StaticHdr {}x{} fallback {}x{}",
            hdr.width, hdr.height, fallback.width, fallback.height
        ),
        ImageData::HdrTiled { hdr, fallback, .. } => format!(
            "HdrTiled {}x{} fallback {}x{}",
            hdr.width(),
            hdr.height(),
            fallback.width(),
            fallback.height()
        ),
        ImageData::Tiled(source) => format!("Tiled {}x{}", source.width(), source.height()),
        ImageData::Animated(frames) => {
            if let Some(first) = frames.first() {
                format!(
                    "Animated {}x{} frames={}",
                    first.width,
                    first.height,
                    frames.len()
                )
            } else {
                "Animated empty".to_string()
            }
        }
        ImageData::HdrAnimated(frames) => {
            if let Some(first) = frames.first() {
                format!(
                    "HdrAnimated {}x{} frames={}",
                    first.hdr.width,
                    first.hdr.height,
                    frames.len()
                )
            } else {
                "HdrAnimated empty".to_string()
            }
        }
    }
}
