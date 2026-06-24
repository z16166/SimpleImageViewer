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
#[derive(Debug, Clone)]
pub struct RawOsdInfo {
    /// LibRaw logical sensor/output grid before demosaic.
    pub sensor_size: (u32, u32),
    /// Embedded preview from `unpack_thumb`, when present.
    pub embedded_preview: Option<(u32, u32)>,
    pub render_pixels: RawRenderPixels,
    /// Active demosaic backend when full develop/refine runs; absent for embedded-only preview.
    pub demosaic_backend: Option<RawDemosaicBackend>,
    /// LibRaw `develop_scene_linear_hdr` duration for this file (latest CPU run).
    pub cpu_demosaic_ms: Option<u32>,
    /// CFA extract on the load thread (GPU path only).
    pub gpu_extract_ms: Option<u32>,
    /// CFA upload + GPU demosaic compute on the render thread.
    pub gpu_demosaic_ms: Option<u32>,
}

impl PartialEq for RawOsdInfo {
    fn eq(&self, other: &Self) -> bool {
        self.sensor_size == other.sensor_size
            && self.embedded_preview == other.embedded_preview
            && self.render_pixels == other.render_pixels
            && self.demosaic_backend == other.demosaic_backend
            && self.cpu_demosaic_ms == other.cpu_demosaic_ms
            && self.gpu_extract_ms == other.gpu_extract_ms
            && self.gpu_demosaic_ms == other.gpu_demosaic_ms
    }
}

impl Eq for RawOsdInfo {}

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
            cpu_demosaic_ms: None,
            gpu_extract_ms: None,
            gpu_demosaic_ms: None,
        }
    }
}

pub(crate) fn elapsed_ms_u32(start: std::time::Instant) -> u32 {
    start.elapsed().as_millis().min(u128::from(u32::MAX)) as u32
}

fn render_pixels_rank(pixels: &RawRenderPixels) -> u8 {
    match pixels {
        RawRenderPixels::Embedded { .. } => 0,
        RawRenderPixels::HqBootstrap { .. } => 1,
        RawRenderPixels::FullDevelop { .. } => 2,
    }
}

impl RawOsdInfo {
    pub fn with_cpu_demosaic_ms(mut self, ms: u32) -> Self {
        self.cpu_demosaic_ms = Some(ms);
        self
    }

    pub fn with_gpu_extract_ms(mut self, ms: u32) -> Self {
        self.gpu_extract_ms = Some(ms);
        self
    }

    /// Merge loader-channel fields into an existing OSD row without discarding timing data.
    pub(crate) fn merge_loader_fields(&mut self, other: &RawOsdInfo) {
        if other.cpu_demosaic_ms.is_some() {
            self.cpu_demosaic_ms = other.cpu_demosaic_ms;
        }
        if other.gpu_extract_ms.is_some() {
            self.gpu_extract_ms = other.gpu_extract_ms;
        }
        if other.gpu_demosaic_ms.is_some() {
            self.gpu_demosaic_ms = other.gpu_demosaic_ms;
        }
        if other.demosaic_backend.is_some() {
            self.demosaic_backend = other.demosaic_backend;
        }
        if other.embedded_preview.is_some() {
            self.embedded_preview = other.embedded_preview;
        }
        if other.sensor_size != (0, 0) {
            self.sensor_size = other.sensor_size;
        }
        // Tag precedence: FullDevelop > HqBootstrap > Embedded (never downgrade rank).
        let cur_rank = render_pixels_rank(&self.render_pixels);
        let other_rank = render_pixels_rank(&other.render_pixels);
        if other_rank > cur_rank {
            self.render_pixels = other.render_pixels;
        }
    }

    /// GPU refine path: capped preview still maps embedded bootstrap tiles.
    #[cfg(test)]
    pub fn apply_hq_refine_preview(&mut self, width: u32, height: u32) {
        if self.demosaic_backend == Some(RawDemosaicBackend::Video) {
            self.render_pixels = RawRenderPixels::HqBootstrap { width, height };
        }
    }

    /// CPU async refine finished: promote to full develop (tagged, not inferred from preview size).
    #[cfg(test)]
    pub fn apply_refine_complete(&mut self, width: u32, height: u32) {
        if self.demosaic_backend == Some(RawDemosaicBackend::Video) {
            return;
        }
        self.render_pixels = RawRenderPixels::FullDevelop { width, height };
    }

    /// CPU async refine finished: build a partial OSD update tagged `FullDevelop`.
    ///
    /// `width`/`height` must be the actual LibRaw develop output grid from refinement, not
    /// capped preview or sensor dimensions. `sensor_size` and `embedded_preview` are left empty
    /// on purpose: [`Self::merge_loader_fields`] keeps the bootstrap sensor/embedded fields when
    /// those slots are `(0, 0)` / `None`. GPU demosaic completion uses
    /// `promote_gpu_demosaic_complete` instead of this factory.
    pub(crate) fn refine_complete(width: u32, height: u32, cpu_demosaic_ms: u32) -> Self {
        Self {
            sensor_size: (0, 0),
            embedded_preview: None,
            render_pixels: RawRenderPixels::FullDevelop { width, height },
            demosaic_backend: Some(RawDemosaicBackend::Host),
            cpu_demosaic_ms: Some(cpu_demosaic_ms),
            gpu_extract_ms: None,
            gpu_demosaic_ms: None,
        }
    }

    pub(crate) fn promote_gpu_demosaic_complete(&mut self, develop_width: u32, develop_height: u32) {
        if self.demosaic_backend != Some(RawDemosaicBackend::Video) {
            return;
        }
        self.render_pixels = RawRenderPixels::FullDevelop {
            width: develop_width,
            height: develop_height,
        };
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

    /// GPU demosaic failed; OSD should show Host/CPU while a per-index reload runs.
    pub(crate) fn note_gpu_demosaic_failed(&mut self) {
        self.demosaic_backend = Some(RawDemosaicBackend::Host);
        let (width, height) = self.sensor_size;
        self.render_pixels = RawRenderPixels::FullDevelop { width, height };
    }

    pub(crate) fn compose_osd_line(
        sensor_size: (u32, u32),
        embedded_preview: Option<(u32, u32)>,
        render_pixels: RawRenderPixels,
        demosaic_backend: Option<RawDemosaicBackend>,
        cpu_demosaic_ms: Option<u32>,
        gpu_extract_ms: Option<u32>,
        gpu_demosaic_ms: Option<u32>,
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
        let mut line = format!("{embedded} · {sensor} · {demosaic} · {render}");
        if let Some(timing) = Self::format_active_demosaic_timing(
            demosaic_backend,
            cpu_demosaic_ms,
            gpu_extract_ms,
            gpu_demosaic_ms,
        ) {
            line.push_str(" · ");
            line.push_str(&timing);
        }
        Some(line)
    }

    fn format_active_demosaic_timing(
        backend: Option<RawDemosaicBackend>,
        cpu_ms: Option<u32>,
        gpu_extract_ms: Option<u32>,
        gpu_demosaic_ms: Option<u32>,
    ) -> Option<String> {
        match backend {
            Some(RawDemosaicBackend::Host) => {
                cpu_ms.map(|ms| t!("raw.osd.timing.cpu", ms = ms).to_string())
            }
            Some(RawDemosaicBackend::Video) => {
                let total = gpu_extract_ms
                    .into_iter()
                    .chain(gpu_demosaic_ms)
                    .try_fold(0u32, |acc, ms| acc.checked_add(ms))?;
                Some(t!("raw.osd.timing.gpu", ms = total).to_string())
            }
            None => None,
        }
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
            cpu_demosaic_ms: None,
            gpu_extract_ms: None,
            gpu_demosaic_ms: None,
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
            None,
            None,
            None,
        )
        .expect("line");
        assert!(
            line.contains(" · "),
            "expected spaced middle-dot separators, got: {line}"
        );
    }

    #[test]
    fn compose_osd_line_shows_cpu_timing_for_host_backend() {
        let line = RawOsdInfo::compose_osd_line(
            (6000, 4000),
            Some((1920, 1280)),
            RawRenderPixels::FullDevelop {
                width: 6000,
                height: 4000,
            },
            Some(RawDemosaicBackend::Host),
            Some(1234),
            None,
            Some(456),
        )
        .expect("line");
        assert!(line.contains("1234"), "{line}");
        assert!(!line.contains("456"), "{line}");
    }

    #[test]
    fn compose_osd_line_shows_gpu_timing_for_video_backend() {
        let line = RawOsdInfo::compose_osd_line(
            (6000, 4000),
            Some((1920, 1280)),
            RawRenderPixels::FullDevelop {
                width: 6000,
                height: 4000,
            },
            Some(RawDemosaicBackend::Video),
            Some(1234),
            Some(100),
            Some(456),
        )
        .expect("line");
        assert!(line.contains("556"), "{line}");
        assert!(!line.contains("1234"), "{line}");
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
            cpu_demosaic_ms: None,
            gpu_extract_ms: None,
            gpu_demosaic_ms: None,
        };
        info.apply_refine_complete(6000, 4000);
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
            cpu_demosaic_ms: None,
            gpu_extract_ms: None,
            gpu_demosaic_ms: None,
        };
        info.apply_hq_refine_preview(1936, 1288);
        assert_eq!(
            info.render_pixels,
            RawRenderPixels::HqBootstrap {
                width: 1936,
                height: 1288
            }
        );
        info.promote_gpu_demosaic_complete(3908, 2602);
        assert_eq!(
            info.render_pixels,
            RawRenderPixels::FullDevelop {
                width: 3908,
                height: 2602
            }
        );
    }

    #[test]
    fn capped_preview_does_not_change_full_develop_via_merge() {
        let mut info = RawOsdInfo {
            sensor_size: (11662, 8746),
            embedded_preview: Some((4000, 3000)),
            render_pixels: RawRenderPixels::FullDevelop {
                width: 11662,
                height: 8746,
            },
            demosaic_backend: Some(RawDemosaicBackend::Host),
            cpu_demosaic_ms: Some(4000),
            gpu_extract_ms: None,
            gpu_demosaic_ms: None,
        };
        let capped = RawOsdInfo {
            sensor_size: (0, 0),
            embedded_preview: None,
            render_pixels: RawRenderPixels::FullDevelop {
                width: 2048,
                height: 1536,
            },
            demosaic_backend: Some(RawDemosaicBackend::Host),
            cpu_demosaic_ms: None,
            gpu_extract_ms: None,
            gpu_demosaic_ms: None,
        };
        info.merge_loader_fields(&capped);
        assert_eq!(
            info.render_pixels,
            RawRenderPixels::FullDevelop {
                width: 11662,
                height: 8746
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
            cpu_demosaic_ms: None,
            gpu_extract_ms: None,
            gpu_demosaic_ms: None,
        };
        info.apply_refine_complete(3684, 2760);
        assert_eq!(
            info.render_pixels,
            RawRenderPixels::FullDevelop {
                width: 3684,
                height: 2760
            }
        );
    }
}
