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

pub mod avif;
#[cfg(feature = "avif-native")]
pub(crate) mod avif_gain_map_deferred;
pub mod capabilities;
pub(crate) mod cicp;
pub mod decode;
pub mod exr_tiled;
pub(crate) mod gain_map;
pub mod heif;
#[cfg(feature = "heif-native")]
pub(crate) mod heif_apple_gain_map;
#[cfg(feature = "heif-native")]
pub(crate) mod heif_apple_gain_map_compose_simd;
#[cfg(feature = "heif-native")]
pub(crate) mod heif_apple_gain_map_gpu;
#[cfg(feature = "jpegxl")]
pub(crate) mod icc_primaries_lcms;
pub(crate) mod iso_gain_map_compose_simd;
pub(crate) mod iso_gain_map_frame_reuse;
pub(crate) mod jpeg_gain_map_gpu;
pub mod jpegxl;
#[cfg(feature = "jpegxl")]
pub(crate) mod jxl_gain_map_deferred;
#[cfg(any(target_os = "linux", test))]
pub(crate) mod linux_admission;
#[cfg(target_os = "linux")]
pub(crate) mod linux_diag;
pub(crate) mod logluv_decode;
pub mod monitor;
pub(crate) mod mpf;
pub(crate) mod openexr_core;
pub(crate) mod raw_demosaic_gpu;
pub(crate) mod simd_fast_pow;
pub(crate) mod openexr_core_backend {
    pub(crate) use super::openexr_core::*;
}
pub mod platform;
pub mod radiance_tiled;
pub mod renderer;
pub mod status;
pub mod surface;
pub mod tiled;
pub mod types;
pub mod ultra_hdr;
pub(crate) mod ultra_hdr_compose;
pub(crate) mod ultra_hdr_deferred;
pub mod vulkan_metadata;
pub(crate) mod wgsl_color;
pub mod wsi_probe;
