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

pub const DEFAULT_SDR_WHITE_NITS: f32 = 203.0;
pub const DEFAULT_MAX_DISPLAY_NITS: f32 = 1000.0;

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HdrPixelFormat {
    Rgba16Float,
    Rgba32Float,
}

#[allow(dead_code)]
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HdrColorSpace {
    LinearSrgb = 0,
    LinearScRgb = 1,
    Rec2020Linear = 2,
    Aces2065_1 = 3,
    Xyz = 4,
    Unknown = 5,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HdrOutputMode {
    SdrToneMapped,
    WindowsScRgb,
    MacOsEdr,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub struct HdrToneMapSettings {
    pub exposure_ev: f32,
    pub sdr_white_nits: f32,
    pub max_display_nits: f32,
}

impl Default for HdrToneMapSettings {
    fn default() -> Self {
        Self {
            exposure_ev: 0.0,
            sdr_white_nits: DEFAULT_SDR_WHITE_NITS,
            max_display_nits: DEFAULT_MAX_DISPLAY_NITS,
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct HdrImageBuffer {
    pub width: u32,
    pub height: u32,
    pub format: HdrPixelFormat,
    pub color_space: HdrColorSpace,
    pub rgba_f32: std::sync::Arc<Vec<f32>>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TilePixelFormat {
    SdrRgba8,
    HdrRgba32F,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum TilePixelBuffer {
    SdrRgba8(std::sync::Arc<Vec<u8>>),
    HdrRgba32F(std::sync::Arc<Vec<f32>>),
}

#[allow(dead_code)]
impl TilePixelBuffer {
    pub fn pixel_format(&self) -> TilePixelFormat {
        match self {
            Self::SdrRgba8(_) => TilePixelFormat::SdrRgba8,
            Self::HdrRgba32F(_) => TilePixelFormat::HdrRgba32F,
        }
    }

    pub fn bytes_per_pixel(&self) -> usize {
        match self {
            Self::SdrRgba8(_) => 4,
            Self::HdrRgba32F(_) => 4 * std::mem::size_of::<f32>(),
        }
    }

    pub fn len_bytes(&self) -> usize {
        match self {
            Self::SdrRgba8(pixels) => pixels.len(),
            Self::HdrRgba32F(pixels) => pixels.len() * std::mem::size_of::<f32>(),
        }
    }

    pub fn is_hdr(&self) -> bool {
        matches!(self, Self::HdrRgba32F(_))
    }
}

#[cfg(test)]
mod tests {
    use super::{TilePixelBuffer, TilePixelFormat};
    use std::sync::Arc;

    #[test]
    fn sdr_tile_buffer_reports_rgba8_accounting() {
        let pixels = Arc::new(vec![0_u8; 2 * 3 * 4]);
        let buffer = TilePixelBuffer::SdrRgba8(Arc::clone(&pixels));

        assert_eq!(buffer.pixel_format(), TilePixelFormat::SdrRgba8);
        assert_eq!(buffer.bytes_per_pixel(), 4);
        assert_eq!(buffer.len_bytes(), pixels.len());
        assert!(!buffer.is_hdr());
    }

    #[test]
    fn hdr_tile_buffer_reports_rgba32f_accounting() {
        let pixels = Arc::new(vec![0.0_f32; 2 * 3 * 4]);
        let buffer = TilePixelBuffer::HdrRgba32F(Arc::clone(&pixels));

        assert_eq!(buffer.pixel_format(), TilePixelFormat::HdrRgba32F);
        assert_eq!(buffer.bytes_per_pixel(), 16);
        assert_eq!(
            buffer.len_bytes(),
            pixels.len() * std::mem::size_of::<f32>()
        );
        assert!(buffer.is_hdr());
    }
}
