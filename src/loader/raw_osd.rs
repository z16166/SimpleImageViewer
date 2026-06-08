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

//! RAW-specific OSD metadata (embedded preview, sensor size, active pixel source).

use crate::loader::DecodedImage;
use crate::loader::ImageData;
use rust_i18n::t;

/// LibRaw load product including OSD metadata for the viewer.
#[derive(Clone)]
pub struct RawLoadOutput {
    pub image: ImageData,
    pub osd: RawOsdInfo,
}

/// Which pixel buffer is currently driving the display for a RAW file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawRenderPixels {
    /// Camera/file embedded JPEG (performance mode or HQ when thumb is large enough).
    Embedded { width: u32, height: u32 },
    /// Full demosaic output (HQ refine or no usable embedded preview).
    FullDevelop { width: u32, height: u32 },
    /// HQ demosaic queued; tiles still map the embedded bootstrap preview.
    HqBootstrap { width: u32, height: u32 },
}

/// Persistent RAW diagnostics shown on the OSD while browsing a RAW file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawOsdInfo {
    /// LibRaw logical sensor/output grid before demosaic.
    pub sensor_size: (u32, u32),
    /// Embedded preview from `unpack_thumb`, when present.
    pub embedded_preview: Option<(u32, u32)>,
    pub render_pixels: RawRenderPixels,
    /// Pre-formatted OSD line; built when this struct is created or updated.
    pub(crate) osd_line: Option<String>,
}

pub(crate) struct RawOsdContext {
    sensor_size: (u32, u32),
    embedded_preview: Option<(u32, u32)>,
}

impl RawOsdContext {
    pub(crate) fn new(sensor_size: (u32, u32), embedded: Option<&DecodedImage>) -> Self {
        Self {
            sensor_size,
            embedded_preview: embedded.map(|p| (p.width, p.height)),
        }
    }

    pub(crate) fn embedded_render(&self, preview: &DecodedImage) -> RawOsdInfo {
        RawOsdInfo {
            sensor_size: self.sensor_size,
            embedded_preview: self.embedded_preview,
            render_pixels: RawRenderPixels::Embedded {
                width: preview.width,
                height: preview.height,
            },
            osd_line: None,
        }
        .with_osd_line()
    }

    pub(crate) fn hq_bootstrap_dims(&self, width: u32, height: u32) -> RawOsdInfo {
        RawOsdInfo {
            sensor_size: self.sensor_size,
            embedded_preview: self.embedded_preview,
            render_pixels: RawRenderPixels::HqBootstrap { width, height },
            osd_line: None,
        }
        .with_osd_line()
    }

    pub(crate) fn full_develop(&self, width: u32, height: u32) -> RawOsdInfo {
        RawOsdInfo {
            sensor_size: self.sensor_size,
            embedded_preview: self.embedded_preview,
            render_pixels: RawRenderPixels::FullDevelop { width, height },
            osd_line: None,
        }
        .with_osd_line()
    }
}

impl RawOsdInfo {
    pub(crate) fn with_osd_line(mut self) -> Self {
        self.osd_line = Self::compose_osd_line(
            self.sensor_size,
            self.embedded_preview,
            self.render_pixels,
        );
        self
    }

    /// Update after async/sync HQ refinement replaces the bootstrap buffer.
    pub fn apply_hq_refine_preview(&mut self, width: u32, height: u32) {
        self.render_pixels = RawRenderPixels::FullDevelop { width, height };
        self.osd_line = Self::compose_osd_line(
            self.sensor_size,
            self.embedded_preview,
            self.render_pixels,
        );
    }

    fn compose_osd_line(
        sensor_size: (u32, u32),
        embedded_preview: Option<(u32, u32)>,
        render_pixels: RawRenderPixels,
    ) -> Option<String> {
        if sensor_size.0 == 0 || sensor_size.1 == 0 {
            return None;
        }
        let embedded = match embedded_preview {
            Some((w, h)) => t!("raw.osd.embedded", size = format_dims(w, h)).to_string(),
            None => t!("raw.osd.no_embedded").to_string(),
        };
        let (sw, sh) = sensor_size;
        let sensor = t!("raw.osd.sensor", size = format_dims(sw, sh)).to_string();
        let render = match render_pixels {
            RawRenderPixels::Embedded { width, height } => {
                t!("raw.osd.render.embedded", size = format_dims(width, height)).to_string()
            }
            RawRenderPixels::FullDevelop { width, height } => {
                t!("raw.osd.render.full", size = format_dims(width, height)).to_string()
            }
            RawRenderPixels::HqBootstrap { width, height } => t!(
                "raw.osd.render.hq_bootstrap",
                size = format_dims(width, height)
            )
            .to_string(),
        };
        Some(format!("{embedded}·{sensor}·{render}"))
    }

    pub(crate) fn empty() -> Self {
        Self {
            sensor_size: (0, 0),
            embedded_preview: None,
            render_pixels: RawRenderPixels::Embedded {
                width: 0,
                height: 0,
            },
            osd_line: None,
        }
    }
}

fn format_dims(w: u32, h: u32) -> String {
    format!("{w}×{h}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hq_refine_promotes_bootstrap_to_full_develop() {
        let mut info = RawOsdInfo {
            sensor_size: (6000, 4000),
            embedded_preview: Some((1920, 1280)),
            render_pixels: RawRenderPixels::HqBootstrap {
                width: 1920,
                height: 1280,
            },
            osd_line: None,
        };
        info.apply_hq_refine_preview(6000, 4000);
        assert_eq!(
            info.render_pixels,
            RawRenderPixels::FullDevelop {
                width: 6000,
                height: 4000
            }
        );
    }

    #[test]
    fn hq_refine_at_sensor_size_is_full_develop() {
        let mut info = RawOsdInfo {
            sensor_size: (3684, 2760),
            embedded_preview: Some((1600, 1200)),
            render_pixels: RawRenderPixels::HqBootstrap {
                width: 1600,
                height: 1200,
            },
            osd_line: None,
        };
        info.apply_hq_refine_preview(3684, 2760);
        assert_eq!(
            info.render_pixels,
            RawRenderPixels::FullDevelop {
                width: 3684,
                height: 2760
            }
        );
    }
}
