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

//! Typed failures for directory-tree ISO gain-map JXL baseline decode.

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum JxlStripBaselineError {
    /// Container has no ISO forward gain map suitable for strip baseline decode.
    NoIsoGainMap,
    /// Primary image layout is incompatible with strip baseline (animation, tiling, etc.).
    UnsupportedImageData(String),
    /// Real decode failure; caller should not silently fall back to compose.
    Decode(String),
}

impl JxlStripBaselineError {
    pub(crate) fn allows_compose_fallback(&self) -> bool {
        matches!(self, Self::NoIsoGainMap | Self::UnsupportedImageData(_))
    }
}

impl std::fmt::Display for JxlStripBaselineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoIsoGainMap => {
                write!(f, "JPEG XL strip baseline path requires ISO gain map")
            }
            Self::UnsupportedImageData(message) | Self::Decode(message) => f.write_str(message),
        }
    }
}

#[cfg(feature = "jpegxl")]
pub(crate) fn classify_jxl_strip_baseline_failure(message: &str) -> JxlStripBaselineError {
    if message == "JPEG XL strip baseline path requires ISO gain map"
        || message.contains("jhgm bundle has no ISO gain-map metadata")
        || message.contains("jhgm bundle has no gain-map codestream")
    {
        return JxlStripBaselineError::NoIsoGainMap;
    }
    if message.starts_with("JPEG XL strip baseline expected")
        || message.starts_with("JPEG XL strip baseline does not support")
        || message.contains("animated jhgm strip baseline")
    {
        return JxlStripBaselineError::UnsupportedImageData(message.to_string());
    }
    JxlStripBaselineError::Decode(message.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_strip_baseline_failure_recognizes_missing_gain_map() {
        assert_eq!(
            classify_jxl_strip_baseline_failure(
                "JPEG XL strip baseline path requires ISO gain map"
            ),
            JxlStripBaselineError::NoIsoGainMap
        );
    }

    #[test]
    fn classify_strip_baseline_failure_treats_layout_mismatch_as_unsupported() {
        assert!(matches!(
            classify_jxl_strip_baseline_failure(
                "JPEG XL strip baseline does not support animation or tiling"
            ),
            JxlStripBaselineError::UnsupportedImageData(_)
        ));
    }
}
