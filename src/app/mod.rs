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

// ── Submodules ──────────────────────────────────────────────────────────────
mod background_threads;
mod background_yaml_saver;
mod directory_tree;
mod directory_tree_strip_cache;
pub(crate) mod folder_picker;
mod hdr_prewarm;
pub(crate) mod hdr_status;
pub(crate) mod hdr_vulkan_metadata;
pub(crate) mod image_management;
mod index_cache_permute;
pub(crate) mod input;
pub(crate) mod lifecycle;
pub(crate) mod media;
pub(crate) mod rendering;
pub(crate) mod rfd_parent;
pub(crate) mod view_status;

mod app_methods;
mod eframe_app;
mod hotkeys_ui;
mod logic_update;
mod metadata_extract;
mod pixel_inspector_ui;
mod preload;
#[cfg(feature = "preload-debug")]
mod preload_hdr_gate;
mod preload_memory;
mod tray_handlers;
mod types;

pub use lifecycle::ImageViewerInit;
pub use types::{FileOpResult, HardwareTier, ImageViewerApp};

pub(crate) use types::{
    AnimationPlayback, CurrentHdrImage, CurrentHdrTiledImage, LightweightFileOpJob,
    PendingAnimUpload, RootRedrawWake, SettingsTab,
};

pub(crate) use directory_tree::DirectoryTreeRuntime;

pub(crate) use preload::{
    CACHE_SIZE, MAX_CONCURRENT_DECODER_LOADS, MAX_DEFERRED_SDR_UPLOADS,
    PRELOAD_MEMORY_REFRESH_MIN_INTERVAL, capacity_refresh_should_reschedule_preloads,
    compute_preload_budgets, memory_aware_tile_cache_budgets_mb, plan_ultra_hdr_capacity_refresh,
    ultra_hdr_decode_capacity_for_output_mode,
};

pub(crate) use hotkeys_ui::localized_hotkey_warning;
pub(crate) use metadata_extract::{extract_exif, extract_xmp};

#[allow(unused_imports)]
pub(crate) use crate::settings::Settings;
pub(crate) use crate::settings::{ScaleMode, TransitionStyle};
pub(crate) use crate::theme::AppTheme;

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) use hotkeys_ui::build_hotkeys_issue_message;
#[cfg(test)]
pub(crate) use preload::collect_ultra_hdr_capacity_sensitive_indices;
#[cfg(test)]
pub(crate) use types::{HdrOutputStateSnapshot, UltraHdrCapacityRefresh, hdr_output_state_changed};
