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

//! Shared helpers when embedded-SDR-master fast load fails and the loader continues on the full
//! HDR path. Failures classified as "ineligible" must not repeat an expensive full-image decode.

use std::path::Path;

/// Log a standardized embedded-SDR fallback warning before continuing on the full HDR path.
pub(crate) fn log_embedded_sdr_master_fallback(format_label: &str, path: &Path, err: &str) {
    log::warn!(
        "[Loader] {format_label} embedded SDR master failed for {}: {err}; trying full HDR path",
        path.display()
    );
}

/// Ultra HDR / JPEG_R embedded-SDR failures that mean "use full HDR path" without re-decoding the
/// primary baseline when pixels are already available.
pub(crate) fn ultra_hdr_embedded_sdr_ineligible(err: &str) -> bool {
    err == "JPEG does not advertise Ultra HDR gain map metadata"
        || err == "Ultra HDR JPEG primary is HDR base; embedded SDR master load is invalid"
}

/// AVIF embedded-SDR failures where the decoded [`libavif_sys::avifImage`] can be reused for HDR.
#[cfg(feature = "avif-native")]
pub(crate) fn avif_embedded_sdr_ineligible(err: &str) -> bool {
    err.contains("no gain map for embedded SDR master load")
        || err.contains("no forward ISO gain map for embedded SDR master load")
        || err.contains("primary is HDR base; embedded SDR master load is invalid")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ultra_hdr_ineligible_recognizes_hdr_base_primary() {
        assert!(ultra_hdr_embedded_sdr_ineligible(
            "Ultra HDR JPEG primary is HDR base; embedded SDR master load is invalid"
        ));
        assert!(!ultra_hdr_embedded_sdr_ineligible("libjpeg decode failed"));
    }
}
