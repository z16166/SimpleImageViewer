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

//! Preload / RAW load diagnostics behind the `preload-debug` feature.
//!
//! Build: `cargo build --features preload-debug`
//! Run:   `SIV_LOG_LEVEL=info` (and optional `SIV_LOG_FILE=1` for a log file).
//! Logs use `info` so they appear without raising the global log level to `debug`.

/// Preload / RAW pipeline diagnostics. No-op unless `--features preload-debug`.
#[macro_export]
macro_rules! preload_debug {
    ($($arg:tt)*) => {
        #[cfg(feature = "preload-debug")]
        {
            log::info!($($arg)*);
        }
    };
}

/// Crash-safe diagnostics: stderr (flushed) + Windows `OutputDebugStringW` for WinDbg/VS.
#[macro_export]
macro_rules! preload_debugger {
    ($($arg:tt)*) => {
        #[cfg(feature = "preload-debug")]
        {
            $crate::preload_debug::debugger_line(format!($($arg)*));
        }
    };
}

#[cfg(feature = "preload-debug")]
pub(crate) fn debugger_line(message: String) {
    use std::io::Write;
    let _ = writeln!(std::io::stderr(), "{message}");
    let _ = std::io::stderr().flush();
    #[cfg(windows)]
    output_debug_string(&message);
}

#[cfg(all(feature = "preload-debug", windows))]
fn output_debug_string(message: &str) {
    use winapi::um::debugapi::OutputDebugStringW;
    let mut line = String::from(message);
    if !line.ends_with('\n') {
        line.push('\n');
    }
    let mut wide: Vec<u16> = line.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        OutputDebugStringW(wide.as_mut_ptr());
    }
}

#[inline]
pub(crate) fn path_is_raw(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(crate::raw_processor::is_raw_extension)
}

#[cfg(feature = "preload-debug")]
#[inline]
pub(crate) fn elapsed_ms(start: std::time::Instant) -> u128 {
    start.elapsed().as_millis()
}

/// Tracks main-canvas `loading...` -> first drawable frame for [`preload-debug`] logs.
#[derive(Debug, Default)]
pub(crate) struct CanvasDisplayTiming {
    #[cfg(feature = "preload-debug")]
    hint_index: Option<usize>,
    #[cfg(feature = "preload-debug")]
    hint_since: Option<std::time::Instant>,
}

impl CanvasDisplayTiming {
    pub(crate) fn reset(&mut self) {
        #[cfg(feature = "preload-debug")]
        {
            self.hint_index = None;
            self.hint_since = None;
        }
    }

    /// Call when navigation or directory reset abandons an in-flight measurement.
    pub(crate) fn on_navigate(&mut self) {
        self.reset();
    }

    /// Call once per paint after loading-hint visibility and drawable state are known.
    pub(crate) fn tick_paint(
        &mut self,
        current_index: usize,
        show_loading_hint: bool,
        has_current_drawable: bool,
        drawable_kind: &str,
    ) {
        #[cfg(feature = "preload-debug")]
        {
            if show_loading_hint {
                if self.hint_index != Some(current_index) {
                    self.hint_since = Some(std::time::Instant::now());
                    self.hint_index = Some(current_index);
                    crate::preload_debug!(
                        "[PreloadDebug][Canvas] loading_hint shown idx={current_index}"
                    );
                }
                return;
            }

            if self.hint_index == Some(current_index)
                && self.hint_since.is_some()
                && has_current_drawable
            {
                let since = self.hint_since.take().unwrap();
                self.hint_index = None;
                let ms = elapsed_ms(since);
                crate::preload_debug!(
                    "[PreloadDebug][Canvas] display_ready idx={current_index} loading_to_draw_ms={ms} drawable={drawable_kind}"
                );
            }
        }
        let _ = (
            current_index,
            show_loading_hint,
            has_current_drawable,
            drawable_kind,
        );
    }
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

#[cfg(all(test, feature = "preload-debug"))]
mod canvas_display_timing_tests {
    use super::CanvasDisplayTiming;

    #[test]
    fn display_ready_logged_once_when_drawable_appears() {
        let mut timing = CanvasDisplayTiming::default();
        timing.tick_paint(3, true, false, "unknown");
        assert_eq!(timing.hint_index, Some(3));
        assert!(timing.hint_since.is_some());

        timing.tick_paint(3, true, false, "unknown");
        assert_eq!(timing.hint_index, Some(3));

        timing.tick_paint(3, false, true, "sdr_texture");
        assert!(timing.hint_index.is_none());
        assert!(timing.hint_since.is_none());
    }

    #[test]
    fn navigate_reset_abandons_in_flight_measurement() {
        let mut timing = CanvasDisplayTiming::default();
        timing.tick_paint(1, true, false, "unknown");
        timing.on_navigate();
        assert!(timing.hint_index.is_none());
        assert!(timing.hint_since.is_none());
    }

    #[test]
    fn hold_frame_without_drawable_does_not_complete_measurement() {
        let mut timing = CanvasDisplayTiming::default();
        timing.tick_paint(2, true, false, "unknown");
        timing.tick_paint(2, false, false, "unknown");
        assert_eq!(timing.hint_index, Some(2));
        assert!(timing.hint_since.is_some());
    }
}
