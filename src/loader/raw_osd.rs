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

/// Demosaic execution backend shown on the RAW OSD (distinct labels avoid C/G ambiguity).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawDemosaicBackend {
    /// LibRaw demosaic on the host processor.
    Host,
    /// Compute-shader demosaic on the video adapter.
    Video,
}

impl RawDemosaicBackend {
    pub(crate) fn osd_label(self) -> String {
        match self {
            Self::Host => t!("raw.osd.demosaic.host").to_string(),
            Self::Video => t!("raw.osd.demosaic.video").to_string(),
        }
    }
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
    /// Active demosaic backend when full develop/refine runs; absent for embedded-only preview.
    pub demosaic_backend: Option<RawDemosaicBackend>,
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
        self.make_info(
            RawRenderPixels::Embedded {
                width: preview.width,
                height: preview.height,
            },
            None,
        )
    }

    pub(crate) fn hq_bootstrap_dims(&self, width: u32, height: u32) -> RawOsdInfo {
        self.make_info(
            RawRenderPixels::HqBootstrap { width, height },
            Some(RawDemosaicBackend::Host),
        )
    }

    pub(crate) fn gpu_bootstrap_dims(&self, width: u32, height: u32) -> RawOsdInfo {
        self.make_info(
            RawRenderPixels::HqBootstrap { width, height },
            Some(RawDemosaicBackend::Video),
        )
    }

    pub(crate) fn full_develop(
        &self,
        width: u32,
        height: u32,
        backend: RawDemosaicBackend,
    ) -> RawOsdInfo {
        self.make_info(
            RawRenderPixels::FullDevelop { width, height },
            Some(backend),
        )
    }

    fn make_info(
        &self,
        render_pixels: RawRenderPixels,
        demosaic_backend: Option<RawDemosaicBackend>,
    ) -> RawOsdInfo {
        RawOsdInfo {
            sensor_size: self.sensor_size,
            embedded_preview: self.embedded_preview,
            render_pixels,
            demosaic_backend,
        }
    }
}

impl RawOsdInfo {
    /// Update after async/sync HQ refinement replaces the bootstrap buffer (CPU path).
    pub fn apply_hq_refine_preview(&mut self, width: u32, height: u32) {
        if self.demosaic_backend == Some(RawDemosaicBackend::Video) {
            self.render_pixels = RawRenderPixels::HqBootstrap { width, height };
            return;
        }
        self.render_pixels = RawRenderPixels::FullDevelop { width, height };
    }

    pub(crate) fn promote_gpu_demosaic_complete(&mut self) {
        if self.demosaic_backend != Some(RawDemosaicBackend::Video) {
            return;
        }
        let (width, height) = self.sensor_size;
        self.render_pixels = RawRenderPixels::FullDevelop { width, height };
    }

    pub(crate) fn note_gpu_demosaic_pending(&mut self, bootstrap: Option<(u32, u32)>) {
        self.demosaic_backend = Some(RawDemosaicBackend::Video);
        if let Some((width, height)) = bootstrap {
            self.render_pixels = RawRenderPixels::HqBootstrap { width, height };
        } else {
            let (width, height) = self.sensor_size;
            self.render_pixels = RawRenderPixels::FullDevelop { width, height };
        }
    }

    pub(crate) fn compose_osd_line(
        sensor_size: (u32, u32),
        embedded_preview: Option<(u32, u32)>,
        render_pixels: RawRenderPixels,
        demosaic_backend: Option<RawDemosaicBackend>,
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
        let demosaic = match demosaic_backend {
            Some(backend) => backend.osd_label(),
            None => t!("raw.osd.demosaic.preview").to_string(),
        };
        Some(format!("{embedded} · {sensor} · {demosaic} · {render}"))
    }

    #[cfg(any(test, target_os = "windows", target_os = "macos"))]
    pub(crate) fn empty() -> Self {
        Self {
            sensor_size: (0, 0),
            embedded_preview: None,
            render_pixels: RawRenderPixels::Embedded {
                width: 0,
                height: 0,
            },
            demosaic_backend: None,
        }
    }
}

fn format_dims(w: u32, h: u32) -> String {
    format!("{w}x{h}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compose_osd_line_separates_fields_with_spaces() {
        let line = RawOsdInfo::compose_osd_line(
            (6000, 4000),
            Some((1920, 1280)),
            RawRenderPixels::HqBootstrap {
                width: 1920,
                height: 1280,
            },
            Some(RawDemosaicBackend::Host),
        )
        .expect("line");
        assert!(
            line.contains(" · "),
            "expected spaced middle-dot separators, got: {line}"
        );
    }

    #[test]
    fn hq_refine_promotes_bootstrap_to_full_develop() {
        let mut info = RawOsdInfo {
            sensor_size: (6000, 4000),
            embedded_preview: Some((1920, 1280)),
            render_pixels: RawRenderPixels::HqBootstrap {
                width: 1920,
                height: 1280,
            },
            demosaic_backend: Some(RawDemosaicBackend::Host),
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
    fn gpu_refine_preview_keeps_bootstrap_until_demosaic_complete() {
        let mut info = RawOsdInfo {
            sensor_size: (3908, 2602),
            embedded_preview: Some((1936, 1288)),
            render_pixels: RawRenderPixels::HqBootstrap {
                width: 1936,
                height: 1288,
            },
            demosaic_backend: Some(RawDemosaicBackend::Video),
        };
        info.apply_hq_refine_preview(1936, 1288);
        assert_eq!(
            info.render_pixels,
            RawRenderPixels::HqBootstrap {
                width: 1936,
                height: 1288
            }
        );
        info.promote_gpu_demosaic_complete();
        assert_eq!(
            info.render_pixels,
            RawRenderPixels::FullDevelop {
                width: 3908,
                height: 2602
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
            demosaic_backend: Some(RawDemosaicBackend::Host),
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
