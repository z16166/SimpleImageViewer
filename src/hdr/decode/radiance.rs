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

use std::path::Path;

#[cfg(test)]
use std::io::{BufRead, Cursor};

use crate::hdr::types::HdrImageBuffer;
pub(crate) fn decode_radiance_hdr_image(path: &Path) -> Result<HdrImageBuffer, String> {
    let mmap = crate::mmap_util::map_file(path)?;
    let img = crate::hdr::radiance_tiled::decode_radiance_rgba32f_from_mmap(&mmap, None)?;
    log::debug!(
        "[HDR] {}: Radiance decode {}x{} (resolution-line orientation unfolded)",
        path.display(),
        img.width,
        img.height
    );
    Ok(img)
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct RadianceHeaderParams {
    exposure: f32,
    colorcorr: [f32; 3],
}

impl RadianceHeaderParams {
    #[cfg(test)]
    pub(crate) fn read_from_bytes(bytes: &[u8]) -> Result<Self, String> {
        let mut reader = Cursor::new(bytes);
        let mut params = Self::default();
        let mut line = String::new();

        loop {
            line.clear();
            let bytes_read = reader.read_line(&mut line).map_err(|err| err.to_string())?;
            if bytes_read == 0 {
                break;
            }

            let trimmed = line.trim();
            if trimmed.is_empty() {
                break;
            }
            params.apply_header_line(trimmed);
        }

        Ok(params)
    }

    pub(crate) fn apply_header_line(&mut self, line: &str) {
        if let Some(value) = line.strip_prefix("EXPOSURE=") {
            if let Ok(exposure) = value.trim().parse::<f32>() {
                if exposure.is_finite() && exposure > 0.0 {
                    self.exposure *= exposure;
                }
            }
        } else if let Some(value) = line.strip_prefix("COLORCORR=") {
            let mut parts = value.split_whitespace();
            let (Some(r), Some(g), Some(b), None) =
                (parts.next(), parts.next(), parts.next(), parts.next())
            else {
                return;
            };
            let Ok(r) = r.parse::<f32>() else { return };
            let Ok(g) = g.parse::<f32>() else { return };
            let Ok(b) = b.parse::<f32>() else { return };
            if r.is_finite() && r > 0.0 && g.is_finite() && g > 0.0 && b.is_finite() && b > 0.0 {
                self.colorcorr[0] *= r;
                self.colorcorr[1] *= g;
                self.colorcorr[2] *= b;
            }
        }
    }

    pub(crate) fn apply_to_pixels(self, pixels: &mut [f32]) {
        let scale = [
            1.0 / (self.exposure * self.colorcorr[0]),
            1.0 / (self.exposure * self.colorcorr[1]),
            1.0 / (self.exposure * self.colorcorr[2]),
        ];
        if scale
            .iter()
            .all(|value| (*value - 1.0).abs() <= f32::EPSILON)
        {
            return;
        }

        for pixel in pixels.chunks_exact_mut(4) {
            pixel[0] *= scale[0];
            pixel[1] *= scale[1];
            pixel[2] *= scale[2];
        }
    }

    pub(crate) fn diagnostic_label(self) -> String {
        format!(
            "Radiance EXPOSURE={:.3} COLORCORR=[{:.3},{:.3},{:.3}]",
            self.exposure, self.colorcorr[0], self.colorcorr[1], self.colorcorr[2]
        )
    }
}

impl Default for RadianceHeaderParams {
    fn default() -> Self {
        Self {
            exposure: 1.0,
            colorcorr: [1.0; 3],
        }
    }
}
