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
mod decode_profile;
mod hdr_fallback;
mod metadata;
mod orchestrator;
mod orientation;
mod preview_aspect;
mod preview_caps;
mod raw_osd;
mod texture_cache;
mod tiled_sources;
mod types;

pub use decode_profile::{
    DecodeProfile, DisplayRequirements, InFlightLoad, LoadIntent, ProfileSpawnRelation,
    decode_profile_stub, decode_profile_with_epoch, output_mode_is_hdr, profile_satisfies_display,
    profile_spawn_relation,
};
pub use orchestrator::ImageLoader;
pub(crate) use orchestrator::should_prefetch_raw_gpu_open;
#[allow(unused_imports)]
// Re-export-only surface for `crate::loader::*`; rustc may lint unused items here.
pub use preview_aspect::preview_aspect_matches_logical;
pub(crate) use preview_caps::DIRECTORY_TREE_STRIP_POOL;
#[allow(unused_imports)]
// `MONITOR_PREVIEW_CAP` is part of the public loader re-export surface.
pub use preview_caps::{
    GPU_DEMOSAIC_SUPPORTED, MONITOR_PREVIEW_CAP, PREVIEW_LIMIT, hq_preview_max_side,
    refresh_hq_preview_monitor_cap,
};
pub(crate) use raw_osd::elapsed_ms_u32;
pub use raw_osd::{RawDemosaicBackend, RawLoadOutput, RawOsdInfo, RawRenderPixels};
pub use texture_cache::TextureCache;
pub use types::*;

pub(crate) use decode::downsample_decoded_for_strip;
pub(crate) use decode::generate_directory_tree_thumb_from_path;
pub(crate) use hdr_fallback::{
    cheap_hdr_sdr_placeholder_rgba8, hdr_display_requests_sdr_preview,
    hdr_has_iso_deferred_gain_map, hdr_raw_gpu_demosaic_pending,
    hdr_raw_gpu_refinement_is_pointless, hdr_sdr_fallback_is_placeholder_for_load,
    hdr_sdr_fallback_rgba8_eager_or_placeholder,     directory_tree_strip_composed_from_iso_deferred,
    directory_tree_strip_from_hdr_or_fallback,
    hdr_directory_tree_strip_sdr_at_max_side,
    hdr_tone_map_settings_for_directory_tree_strip,
    hdr_to_sdr_with_user_tone, libraw_scene_linear_needs_eager_sdr_fallback,
    raw_gpu_source_has_bootstrap_preview, static_hdr_background_plane_upload_eligible,
};
pub(crate) use metadata::{extract_exif_thumbnail, extract_exif_thumbnail_from_mmap};
pub(crate) use orientation::{
    apply_exif_orientation_to_hdr_pair, apply_exif_orientation_to_image_data,
    hdr_gain_map_decode_capacity,
};

pub(crate) fn tiff_may_be_camera_raw(path: &std::path::Path) -> bool {
    decode::tiff_may_be_camera_raw(path)
}

pub(crate) use decode::is_maybe_animated;
