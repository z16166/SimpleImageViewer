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

//! Image loading (`ImageLoader`), decode pipeline ([`decode`]), helper modules, and GPU texture cache.

mod decode;
mod hdr_fallback;
mod metadata;
mod orchestrator;
mod orientation;
mod preview_caps;
mod texture_cache;
mod tiled_sources;
mod types;

#[allow(unused_imports)] // Re-export-only surface for `crate::loader::*`; rustc may lint `MONITOR_PREVIEW_CAP`.
pub use preview_caps::{
    hq_preview_max_side, refresh_hq_preview_monitor_cap, MONITOR_PREVIEW_CAP, PREVIEW_LIMIT,
};
pub use orchestrator::ImageLoader;
pub use texture_cache::TextureCache;
pub use types::*;

pub(crate) use hdr_fallback::{
    cheap_hdr_sdr_placeholder_rgba8, hdr_display_requests_sdr_preview,
    hdr_sdr_fallback_rgba8_eager_or_placeholder, hdr_to_sdr_with_user_tone,
};
pub(crate) use metadata::extract_exif_thumbnail;
pub(crate) use orientation::{
    apply_exif_orientation_to_hdr_pair, apply_exif_orientation_to_image_data,
    hdr_gain_map_decode_capacity,
};
