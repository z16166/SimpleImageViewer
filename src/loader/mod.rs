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
pub(crate) mod embedded_sdr_fallback;
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

#[allow(unused_imports)]
// Re-export-only surface for `crate::loader::*`; rustc may lint unused items here.
pub use decode_profile::{
    DEFAULT_PREFETCH_WINDOW_DISTANCE, DecodeProfile, DisplayRequirements,
    HDR_CAPACITY_MATCH_EPSILON, InFlightLoad, LoadIntent, MAX_CURRENT_IMAGE_OS_THREADS,
    MAX_IMG_LOADER_THREADS, ProfileSpawnRelation, decode_profile_stub, decode_profile_with_epoch,
    in_flight_profile_supersedes_hq_refinement, in_flight_profile_supersedes_load_result,
    output_mode_is_hdr, profile_satisfies_display, profile_spawn_relation,
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
pub use texture_cache::{TextureCache, TextureCacheInsert};
pub use types::*;

pub(crate) use decode::downsample_decoded_for_strip;
pub(crate) use decode::{
    DirectoryTreeThumbDecodeOptions, STRIP_DEFER_SLOW_EMBEDDED_SDR,
    generate_directory_tree_thumb_decode_from_path,
};
pub(crate) use hdr_fallback::{
    cheap_hdr_sdr_placeholder_rgba8, directory_tree_strip_from_hdr_or_fallback,
    directory_tree_strip_logical_for_preview, hdr_directory_tree_strip_sdr_at_max_side,
    hdr_display_requests_sdr_preview, hdr_has_embedded_sdr_master_display,
    hdr_has_iso_deferred_gain_map, hdr_gain_map_sdr_display_mode_affects_image,
    hdr_raw_gpu_bootstrap_fallback_decoded, hdr_raw_gpu_demosaic_pending,
    hdr_sdr_fallback_is_placeholder_for_load, hdr_sdr_fallback_rgba8_or_placeholder,
    hdr_tone_map_settings_for_directory_tree_strip, prefer_embedded_iso_gain_map_sdr_on_sdr_output,
    raw_gpu_source_has_bootstrap_preview, should_use_embedded_sdr_master_load,
    static_hdr_background_plane_upload_eligible,
};
pub(crate) use metadata::{
    extract_exif_thumbnail, extract_exif_thumbnail_from_bytes,
    extract_exif_thumbnail_from_mmap_probed, extract_exif_thumbnail_probed,
};
pub(crate) use orientation::{
    apply_exif_orientation_to_hdr_pair, apply_exif_orientation_to_image_data,
    hdr_gain_map_decode_capacity,
};

pub(crate) fn tiff_may_be_camera_raw(path: &std::path::Path) -> bool {
    decode::tiff_may_be_camera_raw(path)
}

pub(crate) use decode::is_maybe_animated;
