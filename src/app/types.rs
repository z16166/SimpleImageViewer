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

use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use eframe::egui::{self, Pos2, Rect, Vec2};

/// Calls [`winit::window::Window::request_redraw`] on the root viewport window.
pub(crate) type RootRedrawWake = Arc<dyn Fn() + Send + Sync>;

use crate::app::DirectoryTreeRuntime;
use crate::audio::AudioPlayer;
use crate::directory_tree_places::DirectoryTreePlaces;
use crate::ipc::IpcMessage;
use crate::loader::{ImageLoader, TextureCache};
use crate::pixel_inspector::PixelHoverCache;
use crate::settings::{Settings, TransitionStyle};
use crate::theme::{SystemThemeCache, ThemePalette};
use crate::tile_cache::TileCoord;
use crate::tile_cache::TileManager;
use crate::ui::dialogs::modal_state::ActiveModal;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsTab {
    Library,
    Viewing,
    Slideshow,
    Music,
    Appearance,
    Hotkeys,
    ContextMenu,
    System,
    About,
}

impl SettingsTab {
    pub(crate) const ALL: [Self; 9] = [
        Self::Library,
        Self::Viewing,
        Self::Slideshow,
        Self::Music,
        Self::Appearance,
        Self::Hotkeys,
        Self::ContextMenu,
        Self::System,
        Self::About,
    ];

    pub(crate) fn label_key(self) -> &'static str {
        match self {
            Self::Library => "settings_tab.library",
            Self::Viewing => "settings_tab.viewing",
            Self::Slideshow => "settings_tab.slideshow",
            Self::Music => "settings_tab.music",
            Self::Appearance => "settings_tab.appearance",
            Self::Hotkeys => "settings_tab.hotkeys",
            Self::ContextMenu => "settings_tab.context_menu",
            Self::System => "settings_tab.system",
            Self::About => "settings_tab.about",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HdrOutputStateSnapshot {
    output_mode: crate::hdr::types::HdrOutputMode,
    native_presentation_enabled: bool,
    target_format: Option<wgpu::TextureFormat>,
}

impl HdrOutputStateSnapshot {
    pub(crate) fn new(
        output_mode: crate::hdr::types::HdrOutputMode,
        native_presentation_enabled: bool,
        target_format: Option<wgpu::TextureFormat>,
    ) -> Self {
        Self {
            output_mode,
            native_presentation_enabled,
            target_format,
        }
    }

    pub(crate) fn target_format(&self) -> Option<wgpu::TextureFormat> {
        self.target_format
    }
}

pub(crate) fn hdr_output_state_changed(
    previous: HdrOutputStateSnapshot,
    next: HdrOutputStateSnapshot,
) -> bool {
    previous != next
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UltraHdrCapacityRefresh {
    pub(crate) indices_to_invalidate: Vec<usize>,
    pub(crate) reload_current: bool,
}
/// Animation playback state for the currently displayed animated image.
#[derive(Clone)]
pub(crate) struct AnimationPlayback {
    /// Index in the image_files list that this animation belongs to.
    pub(crate) image_index: usize,
    /// Pre-uploaded GPU textures for each frame.
    pub(crate) textures: Vec<egui::TextureHandle>,
    /// Per-frame HDR buffers when the animation uses the HDR / gain-map plane path.
    pub(crate) hdr_frames: Option<Vec<std::sync::Arc<crate::hdr::types::HdrImageBuffer>>>,
    /// Per-frame display duration.
    pub(crate) delays: Vec<Duration>,
    /// Currently displayed frame index.
    pub(crate) current_frame: usize,
    /// When the current frame started displaying.
    pub(crate) frame_start: Instant,
    /// Per-frame raw CPU pixel buffers (zero-copy clone of Arc handles).
    pub(crate) cpu_frames: Option<Vec<std::sync::Arc<Vec<u8>>>>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HardwareTier {
    Low,
    Medium,
    High,
}

impl HardwareTier {
    pub fn max_tile_quota(&self) -> usize {
        match self {
            Self::Low => 16,
            Self::Medium => 64,
            Self::High => 128, // Reduced from 512 to avoid command queue saturation
        }
    }

    pub fn look_ahead_padding(&self) -> f32 {
        match self {
            Self::Low => 512.0,
            Self::Medium => 1024.0,
            Self::High => 2048.0,
        }
    }

    pub fn gpu_cache_tiles(&self) -> usize {
        match self {
            Self::Low => 256,    // Basic coverage
            Self::Medium => 448, // Retina/4K coverage
            Self::High => 1024,  // Performance/Gigapixel coverage
        }
    }

    pub fn cpu_cache_mb(&self) -> usize {
        match self {
            Self::Low => 512,
            Self::Medium => 1024,
            Self::High => 2048,
        }
    }

    pub fn hdr_tile_cache_mb(&self) -> usize {
        match self {
            Self::Low => 256,
            Self::Medium => 512,
            Self::High => 1024,
        }
    }

    pub fn tiled_threshold_pixels(&self) -> u64 {
        64_000_000 // Reverted to 64MP for all tiers as requested
    }

    pub fn max_preview_size(&self) -> u32 {
        match self {
            Self::Low => 1024,
            Self::Medium => 2048,
            Self::High => crate::constants::MAX_QUALITY_PREVIEW_SIZE, // Capped at 4k to prevent VRAM spikes
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileOpError {
    CreateDirFailed(String),
    InvalidSource,
    TargetFileExists,
    CopyFailed(String),
    MoveFailed(String),
    RemoveSourceFailed(String),
}

impl FileOpError {
    pub fn localized_message(&self) -> String {
        match self {
            Self::CreateDirFailed(e) => {
                rust_i18n::t!("file_copy_cut.err_create_dir", err = e).to_string()
            }
            Self::InvalidSource => rust_i18n::t!("file_copy_cut.err_invalid_source").to_string(),
            Self::TargetFileExists => rust_i18n::t!("file_copy_cut.err_target_exists").to_string(),
            Self::CopyFailed(e) => {
                rust_i18n::t!("file_copy_cut.err_copy_failed", err = e).to_string()
            }
            Self::MoveFailed(e) => {
                rust_i18n::t!("file_copy_cut.err_move_failed", err = e).to_string()
            }
            Self::RemoveSourceFailed(e) => {
                rust_i18n::t!("file_copy_cut.err_remove_source", err = e).to_string()
            }
        }
    }
}

// NOTE: This Display implementation is strictly for developer logs and diagnostics.
// Any user-facing interface error messages MUST retrieve the translation via `localized_message()`.
impl std::fmt::Display for FileOpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CreateDirFailed(e) => write!(f, "CreateDirFailed({})", e),
            Self::InvalidSource => write!(f, "InvalidSource"),
            Self::TargetFileExists => write!(f, "TargetFileExists"),
            Self::CopyFailed(e) => write!(f, "CopyFailed({})", e),
            Self::MoveFailed(e) => write!(f, "MoveFailed({})", e),
            Self::RemoveSourceFailed(e) => write!(f, "RemoveSourceFailed({})", e),
        }
    }
}

pub enum FileOpResult {
    Delete {
        path: PathBuf,
        original_index: usize,
        original_size: u64,
        result: Result<(), String>,
    },
    Exif(PathBuf, Option<Vec<(String, String)>>),
    Xmp(PathBuf, Option<(Vec<(String, String)>, String)>),
    Wallpaper {
        current: Option<String>,
        monitors: Vec<crate::ui::dialogs::wallpaper::MonitorOption>,
        supports_per_monitor: bool,
    },
    CopyTo {
        src_path: PathBuf,
        target_dir: PathBuf,
        result: Result<(), FileOpError>,
    },
    CutTo {
        src_path: PathBuf,
        target_dir: PathBuf,
        original_index: usize,
        original_size: u64,
        result: Result<(), FileOpError>,
    },
}

/// Work for the single context-menu background thread (EXIF / XMP / wallpaper introspection).
/// Serialized so rapid menu clicks cannot spawn an unbounded number of threads.
#[derive(Debug)]
pub(crate) enum LightweightFileOpJob {
    Exif(PathBuf),
    Xmp(PathBuf),
    Wallpaper,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CachedWindowPlacement {
    pub outer_position: [i32; 2],
    /// Screen-space center of [`Self::outer_position`] / outer rect (for maximized sessions).
    pub outer_center: [i32; 2],
    pub inner_size: [u32; 2],
    pub maximized: bool,
}
#[derive(Clone)]
pub(crate) struct CurrentHdrImage {
    pub(crate) index: usize,
    pub(crate) image: Arc<crate::hdr::types::HdrImageBuffer>,
}

impl CurrentHdrImage {
    pub(crate) fn new(index: usize, image: Arc<crate::hdr::types::HdrImageBuffer>) -> Self {
        Self { index, image }
    }

    pub(crate) fn image_for_index(
        &self,
        index: usize,
    ) -> Option<&Arc<crate::hdr::types::HdrImageBuffer>> {
        (self.index == index).then_some(&self.image)
    }
}

#[derive(Clone)]
pub(crate) struct CurrentHdrTiledImage {
    pub(crate) index: usize,
    pub(crate) source: Arc<dyn crate::hdr::tiled::HdrTiledSource>,
}

impl CurrentHdrTiledImage {
    pub(crate) fn new(index: usize, source: Arc<dyn crate::hdr::tiled::HdrTiledSource>) -> Self {
        Self { index, source }
    }

    pub(crate) fn source_for_index(
        &self,
        index: usize,
    ) -> Option<&Arc<dyn crate::hdr::tiled::HdrTiledSource>> {
        (self.index == index).then_some(&self.source)
    }
}

/// Cached translated labels for the open image context menu (built once per open).
pub(crate) struct ContextMenuLabelCache {
    /// Parallel to [`RuntimeContextMenuState::config`]` items`; `Some` for builtins only.
    pub labels: Vec<Option<String>>,
    pub fullscreen: bool,
    pub language: String,
}

pub struct ImageViewerApp {
    // Core state
    pub(crate) settings: Settings,
    pub(crate) image_files: Vec<PathBuf>,
    /// Parallel to [`Self::image_files`]: lengths from directory scan (`metadata`).
    pub(crate) file_byte_len_by_index: Vec<u64>,
    /// Parallel to [`Self::image_files`]: modified times from directory scan (`metadata`).
    pub(crate) file_modified_unix_by_index: Vec<Option<i64>>,
    pub(crate) current_index: usize,
    pub(crate) initial_image: Option<PathBuf>,
    pub(crate) scanning: bool,

    // Performance tracking
    pub(crate) hardware_tier: HardwareTier,

    // Image loading
    pub(crate) loader: ImageLoader,
    pub(crate) texture_cache: TextureCache,
    pub(crate) hdr_capabilities: crate::hdr::capabilities::HdrCapabilities,
    pub(crate) hdr_renderer: crate::hdr::renderer::HdrImageRenderer,
    pub(crate) wgpu_pipeline_cache: Option<std::sync::Arc<wgpu::PipelineCache>>,
    pub(crate) wgpu_adapter_info: Option<wgpu::AdapterInfo>,
    /// Epoch tracking live `wgpu::Device`/`Queue` instances (not swap-chain format changes).
    pub(crate) current_device_id: u64,
    /// Last `wgpu::Device` instance pushed to [`ImageLoader`] (detects Device rebuild).
    pub(crate) loader_wgpu_device: Option<wgpu::Device>,
    pub(crate) hdr_callback_resources_prewarm:
        std::sync::Arc<crate::hdr::renderer::HdrCallbackResourcesPrewarm>,
    pub(crate) hdr_target_format: Option<wgpu::TextureFormat>,
    pub(crate) hdr_monitor_state: crate::hdr::monitor::HdrMonitorState,
    /// Last observed viewport placement (`outer_rect`, `inner_size`,
    /// `maximized`). Refreshed each frame from `egui::ViewportInfo` and
    /// flushed into [`Settings::window_outer_position`],
    /// [`Settings::window_inner_size`] and [`Settings::window_maximized`] on
    /// `on_exit` so the next session can place the window onto the same
    /// monitor (and re-pick `Rgba16Float` vs `Bgra8Unorm` accordingly).
    pub(crate) cached_window_placement: Option<CachedWindowPlacement>,
    /// Last non-maximized placement observed this session (valid outer top-left).
    /// Used when closing maximized so the next spawn targets the same monitor.
    pub(crate) cached_restore_placement: Option<CachedWindowPlacement>,
    pub(crate) cached_directory_tree_window_placement: Option<CachedWindowPlacement>,
    pub(crate) cached_directory_tree_restore_placement: Option<CachedWindowPlacement>,
    /// Mailbox used to ask the (patched) egui-wgpu Painter to hot-swap the
    /// swap-chain target format whenever the active monitor's HDR capability
    /// changes. The same mailbox is registered with `WgpuConfiguration`, so
    /// writes here are picked up on the very next paint call.
    pub(crate) requested_target_format: eframe::egui_wgpu::RequestedSurfaceFormat,
    /// Reverse-direction mailbox: the painter publishes the **live** active
    /// swap-chain target format here after every successful runtime hot-swap.
    /// The application reads from this mailbox in `logic()` instead of trusting
    /// `frame.wgpu_render_state().target_format`, because `egui_wgpu::RenderState`
    /// derives `Clone` and eframe stores a clone in `Frame` — the painter's
    /// post-swap mutation of `RenderState.target_format` is therefore never
    /// observable through `wgpu_render_state()`. Without this side channel the
    /// OSD freezes on the very first runtime swap (e.g. moving the window from
    /// HDR to SDR shows the new mode once, but moving back to HDR never
    /// updates).
    pub(crate) active_target_format: eframe::egui_wgpu::ActiveSurfaceFormat,
    /// When the active monitor expects PQ-encoded UI in `Rgb10a2Unorm`, the
    /// app sets this mailbox; KWin KMS HDR offload uses gamma 2.2 instead.
    pub(crate) requested_rgb10a2_pq_encode: eframe::egui_wgpu::RequestedRgb10a2PqEncode,
    pub(crate) gamma22_display_scale: eframe::egui_wgpu::Gamma22DisplayScale,
    /// Vulkan WSI HDR gates published by the painter after the first surface configure.
    pub(crate) vulkan_wsi_hdr_gates: eframe::egui_wgpu::VulkanWsiHdrGatesMailbox,
    /// Per-content ST 2086 metadata for Linux `vkSetHdrMetadataEXT` (app-owned mailbox mirror).
    #[cfg(target_os = "linux")]
    pub(crate) requested_vulkan_hdr_metadata: eframe::egui_wgpu::RequestedVulkanHdrMetadata,
    #[cfg(target_os = "linux")]
    pub(crate) last_vulkan_hdr_metadata: Option<eframe::egui_wgpu::VulkanHdrMetadata>,
    /// Dedupes swap-chain format mismatch diagnostics while a hot-swap is pending.
    pub(crate) last_logged_swap_chain_format_request: Option<wgpu::TextureFormat>,
    /// Dedupes Linux HDR runtime diagnostics (`display` / `compositor_wsi` / `admission` / `app_active`).
    #[cfg(target_os = "linux")]
    pub(crate) last_logged_linux_hdr_runtime_diag:
        Option<crate::hdr::linux_diag::LinuxHdrRuntimeDiagSnapshot>,
    #[cfg(feature = "preload-debug")]
    pub(crate) hdr_preload_gate_log: crate::app::preload_hdr_gate::GateLogState,
    pub(crate) rgb10a2_pq_encode_requested: bool,
    pub(crate) ultra_hdr_decode_capacity: f32,
    pub(crate) ultra_hdr_decode_output_mode: crate::hdr::types::HdrOutputMode,
    /// Startup: defer directory preloads until the first runtime HDR capacity refresh.
    pub(crate) preload_deferred_for_hdr_capacity: bool,
    pub(crate) current_hdr_image: Option<CurrentHdrImage>,
    pub(crate) hdr_image_cache: HashMap<usize, Arc<crate::hdr::types::HdrImageBuffer>>,
    pub(crate) current_hdr_tiled_image: Option<CurrentHdrTiledImage>,
    pub(crate) hdr_tiled_source_cache: HashMap<usize, Arc<dyn crate::hdr::tiled::HdrTiledSource>>,
    pub(crate) current_hdr_tiled_preview: Option<CurrentHdrImage>,
    pub(crate) hdr_tiled_preview_cache: HashMap<usize, Arc<crate::hdr::types::HdrImageBuffer>>,
    pub(crate) hdr_sdr_fallback_indices: HashSet<usize>,
    /// HDR indices whose current SDR fallback texture is a temporary black placeholder.
    pub(crate) hdr_placeholder_fallback_indices: HashSet<usize>,
    /// Static GPU RAW: show embedded preview via SDR until demosaic bake completes.
    pub(crate) hdr_raw_gpu_demosaic_pending_indices: HashSet<usize>,
    /// GPU demosaic finished (binding baked) but bootstrap SDR may still be visible until HDR present.
    pub(crate) hdr_raw_gpu_demosaic_baked_indices: HashSet<usize>,
    /// Maps HDR image key to index while GPU demosaic is pending (survives HDR cache eviction).
    pub(crate) hdr_raw_gpu_demosaic_pending_key_index:
        HashMap<crate::hdr::renderer::HdrImageKey, usize>,
    /// Indices whose `texture_cache` entry is the embedded RAW preview (`img_raw_gpu_bootstrap_*`),
    /// not the tone-mapped HDR fallback texture (`img_hdr_fallback_*`).
    pub(crate) raw_gpu_embedded_bootstrap_indices: HashSet<usize>,
    /// Frames spent waiting for HDR callback prewarm before pre-upload registration is abandoned.
    pub(crate) hdr_register_prewarm_repush_counts: HashMap<usize, u8>,
    pub(crate) gpu_demosaic_failed_indices: HashSet<usize>,
    /// After GPU demosaic completes, defer neighbor preloads until the HDR plane is shown.
    pub(crate) raw_gpu_demosaic_await_hdr_present: bool,
    pub(crate) raw_demosaic_baked_notify:
        Arc<Mutex<Vec<crate::hdr::renderer::RawGpuDemosaicBakedNotice>>>,
    /// HDR indices for which fallback refinement is currently in-flight.
    pub(crate) hdr_in_flight_fallback_refinements: HashSet<usize>,
    /// SDR RGBA decoded during preload but not yet uploaded to egui (avoids VRAM spikes).
    pub(crate) deferred_sdr_uploads: HashMap<usize, crate::loader::DecodedImage>,
    pub(crate) ultra_hdr_capacity_sensitive_indices: HashSet<usize>,
    /// Animated image playback state (None for static images).
    pub(crate) animation: Option<AnimationPlayback>,

    // Pan/drag state (used in non-fullscreen 1:1 mode)
    pub(crate) pan_offset: Vec2,

    // Manual zoom factor (1.0 = 100%); applied on top of any fit-to-screen scale
    pub(crate) zoom_factor: f32,

    // Auto-switch timer
    pub(crate) last_switch_time: Instant,
    pub(crate) slideshow_paused: bool,
    pub(crate) random_slideshow_order_ready: bool,

    // Audio
    pub(crate) audio: AudioPlayer,
    pub(crate) music_seeking_target_ms: Option<u64>,
    pub(crate) music_seek_timeout: Option<std::time::Instant>,
    pub(crate) music_hud_last_activity: std::time::Instant,

    // UI state
    pub(crate) show_settings: bool,
    pub(crate) last_show_settings: bool,
    pub(crate) settings_tab: SettingsTab,
    pub(crate) about_icon_texture: Option<egui::TextureHandle>,
    /// True once the very first directory scan has produced at least one image.
    pub(crate) images_ever_loaded: bool,
    pub(crate) status_message: String,
    pub(crate) error_message: Option<String>,
    pub(crate) is_font_error: bool,
    /// Incremented each time a modal dialog is opened.
    /// Included in each dialog's egui Window Id so that egui has no position
    /// memory from a previous opening — the dialog always starts centered.
    pub(crate) modal_generation: u32,
    // Pending viewport commands (set during input processing for deferred apply)
    pub(crate) pending_fullscreen: Option<bool>,
    /// Set to true by the PickDirectory hotkey; consumed in `logic()` to call
    /// `open_directory_dialog` which requires `&eframe::Frame`.
    pub(crate) pending_open_directory: bool,
    pub(crate) folder_picker: crate::app::folder_picker::FolderPickerRuntime,
    pub(crate) directory_tree: DirectoryTreeRuntime,
    pub(crate) directory_tree_strip_cache:
        crate::app::directory_tree_strip_cache::DirectoryTreeStripCache,
    /// Tiled strip thumbnails requested via [`TiledImageSource::generate_full_image_preview`].
    pub(crate) directory_tree_strip_tiled_attempted: std::collections::HashSet<usize>,
    pub(crate) directory_tree_strip_cold_attempted: std::collections::HashSet<usize>,
    pub(crate) directory_tree_strip_generate_inflight: std::collections::HashSet<usize>,
    pub(crate) directory_tree_strip_preview_tx: crossbeam_channel::Sender<
        crate::app::directory_tree_strip_cache::DirectoryTreeStripPreviewJobResult,
    >,
    pub(crate) directory_tree_strip_preview_rx: crossbeam_channel::Receiver<
        crate::app::directory_tree_strip_cache::DirectoryTreeStripPreviewJobResult,
    >,
    pub(crate) directory_tree_strip_inflight_release_tx: crossbeam_channel::Sender<usize>,
    pub(crate) directory_tree_strip_inflight_release_rx: crossbeam_channel::Receiver<usize>,
    pub(crate) directory_tree_strip_pending_gpu:
        Vec<crate::app::directory_tree_strip_cache::DirectoryTreeStripPendingGpuUpload>,
    /// Background Places loader; polled from `logic()`.
    pub(crate) directory_tree_places_load_rx:
        Option<crossbeam_channel::Receiver<Result<DirectoryTreePlaces, String>>>,
    // Cached system font families
    pub(crate) font_families: Vec<String>,
    /// Filled by a background thread started in `ImageViewerApp::new`; polled in `logic`.
    pub(crate) font_families_rx: Option<Receiver<Vec<String>>>,
    pub(crate) temp_font_size: Option<f32>,

    // Cached state
    pub(crate) cached_music_count: Option<usize>,
    pub(crate) cached_pixels_per_point: f32,

    // Active modal dialog — only one can be open at a time.
    // All per-dialog state lives inside the enum variant; setting this to None
    // automatically drops and cleans up the dialog's temporary data.
    pub(crate) active_modal: Option<ActiveModal>,

    // Async music scanning
    pub(crate) music_scan_rx: Option<Receiver<Vec<PathBuf>>>,
    pub(crate) scanning_music: bool,
    pub(crate) music_scan_cancel: Option<Arc<AtomicBool>>,
    pub(crate) music_scan_path: Option<PathBuf>,
    pub(crate) scan_rx: Option<Receiver<crate::scanner::ScanMessage>>,
    pub(crate) scan_cancel: Option<Arc<AtomicBool>>,
    /// Wakes the root winit window so `App::logic()` runs while a child viewport is focused.
    pub(crate) root_redraw_wake: Option<crate::app::RootRedrawWake>,
    /// Latest palette for the directory-tree deferred viewport (read each paint).
    pub(crate) directory_tree_theme: std::sync::Arc<parking_lot::Mutex<crate::theme::ThemePalette>>,
    /// ROOT paint should synchronously repaint the directory-tree viewport (Windows).
    pub(crate) pending_directory_tree_repaint: bool,
    /// Deferred main-window navigation from directory-tree list clicks (see `process_pending_directory_tree_select`).
    pub(crate) pending_directory_tree_select_index: Option<usize>,
    /// Retry `sync_directory_tree_file_list_state` when the UI thread holds `directory_tree.state`.
    pub(crate) pending_directory_tree_state_sync: bool,
    /// Queued when defer-drop cannot acquire `directory_tree.state` (see `apply_directory_tree_pending_sync_warning`).
    pub(crate) pending_directory_tree_sync_warning: Option<String>,
    pub(crate) directory_tree_sync_defer_frames: u32,
    /// Monotonic id for the active directory scan; stale channel messages are ignored.
    pub(crate) scan_generation: u64,
    /// Set when a directory scan is spawned; used by preload-debug queue-wait logs.
    pub(crate) scan_results_pending_since: Option<std::time::Instant>,
    /// Main-window preloads are deferred until the directory-tree file list viewport paints.
    pub(crate) pending_preload_after_directory_scan: bool,
    pub(crate) directory_tree_strip_bootstrap_after_scan: bool,
    /// Frames elapsed in strip bootstrap mode; used to exit high-throughput limits.
    pub(crate) directory_tree_strip_bootstrap_frames: u32,

    // Current image resolution (used by wallpaper dialog and OSD)
    pub(crate) current_image_res: Option<(u32, u32)>,
    /// Per-index RAW OSD metadata (embedded preview, sensor grid, active pixel source).
    pub(crate) raw_metadata: crate::app::view_status::RawMetadataStore,
    pub(crate) image_status: crate::app::view_status::ImageViewStatus,
    /// File name shown in the image OSD for [`Self::current_index`].
    pub(crate) current_file_name: String,
    pub(crate) cached_keyboard_hint: String,
    /// Cached detached directory-tree viewport title; refreshed on locale change.
    pub(crate) cached_directory_tree_viewport_title: String,
    /// True after the detached directory-tree viewport title was sent via `ViewportBuilder`.
    pub(crate) directory_tree_viewport_title_sent: bool,
    /// Render plan computed during the latest image draw pass; reused by OSD HDR status.
    pub(crate) cached_frame_render_plan: Option<crate::app::rendering::plan::RenderPlan>,
    pub(crate) cached_frame_hdr_render_path: Option<crate::hdr::status::HdrRenderPath>,
    /// HDR monitor selection bound at the start of each root logic pass.
    pub(crate) frame_effective_hdr_monitor_selection:
        Option<crate::hdr::monitor::HdrMonitorSelection>,

    // Transition state
    pub(crate) prev_texture: Option<egui::TextureHandle>,
    pub(crate) prev_hdr_image: Option<Arc<crate::hdr::types::HdrImageBuffer>>,
    pub(crate) prev_transition_rect: Option<Rect>,
    pub(crate) transition_start: Option<Instant>,
    /// Set when a transition animation completes; used to defer HDR SDR refinement uploads
    /// for the current image until the static frame has settled (avoids end-of-flip flash).
    pub(crate) transition_settled_at: Option<Instant>,
    /// One extra geometric-transition frame at t=1.0 before clearing `transition_start`.
    pub(crate) transition_end_hold: bool,
    pub(crate) pending_transition_target: Option<usize>,
    pub(crate) last_background_upload_at: Option<Instant>,
    pub(crate) is_next: bool,
    pub(crate) active_transition: TransitionStyle,

    // OSD renderer
    pub(crate) osd: crate::ui::osd::OsdRenderer,

    // Window lifecycle
    pub(crate) last_minimized: bool,
    pub(crate) last_frame_time: Instant,
    pub(crate) last_logic_shared_at: Option<Instant>,

    // IPC receiver
    pub(crate) ipc_rx: crossbeam_channel::Receiver<IpcMessage>,

    // Predictive animation cache (decoded and uploaded to GPU)
    pub(crate) animation_cache: HashMap<usize, AnimationPlayback>,

    // Tiled rendering for large images
    pub(crate) tile_manager: Option<TileManager>,
    /// Reused each tiled draw frame to avoid per-frame HashSet/Vec allocations.
    pub(crate) tiled_primary_visible_scratch: HashSet<TileCoord>,
    pub(crate) tiled_visible_coords_scratch: Vec<TileCoord>,

    // Tiled rendering instances decoded during prefetch
    pub(crate) prefetched_tiles: HashMap<usize, TileManager>,

    // Theme state
    pub(crate) theme_cache: SystemThemeCache,
    pub(crate) cached_palette: ThemePalette,

    // Printing state
    pub is_printing: Arc<AtomicBool>,
    pub print_status_rx: Option<crossbeam_channel::Receiver<Option<String>>>,

    // Deferred animation frame uploads (throttled to avoid GPU stalls)
    pub(crate) pending_anim_frames: HashMap<usize, PendingAnimUpload>,

    // Async file operations (deletion, etc.)
    pub(crate) file_op_rx: Receiver<FileOpResult>,
    pub(crate) file_op_tx: Sender<FileOpResult>,
    pub(crate) lightweight_file_op_tx: Sender<LightweightFileOpJob>,
    pub(crate) background_threads: crate::app::background_threads::BackgroundThreadJoiner,

    // Debounce for mouse wheel navigation
    pub(crate) last_mouse_wheel_nav: f64,
    /// Canvas area from the latest ROOT `draw_image_canvas_ui` pass (excludes embedded tree panel).
    pub(crate) last_canvas_rect: Option<egui::Rect>,

    /// Last egui time when keyboard Next/Prev was applied (throttles key repeat).
    pub(crate) last_keyboard_nav: Option<f64>,

    // Settings persistence channel
    pub(crate) save_tx: Sender<Settings>,
    pub(crate) save_error_rx: Receiver<String>,
    pub(crate) last_save_error: Option<(String, Instant)>,
    pub(crate) saver_handle: Option<std::thread::JoinHandle<()>>,

    // Preload byte budgets (computed at startup from system RAM)
    pub(crate) preload_budget_forward: u64,
    pub(crate) preload_budget_backward: u64,
    /// Reused for preload memory guard checks so navigation does not rebuild
    /// sysinfo's system snapshot state on every preload scheduling pass.
    pub(crate) preload_memory: crate::app::preload_memory::PreloadMemorySnapshot,

    // Custom right-click context menu (bypasses egui's context_menu which
    // cannot re-open on consecutive right-clicks)
    pub(crate) context_menu_pos: Option<Pos2>,
    pub(crate) context_menu_viewport: Option<egui::ViewportId>,
    pub(crate) context_menu_label_cache: Option<ContextMenuLabelCache>,
    /// Current view rotation in steps of 90 degrees clockwise (0-3).
    pub(crate) current_rotation: i32,

    // Adaptive tile upload quota based on hardware and current frame performance
    pub(crate) tile_upload_quota: usize,

    // Audio device caching
    pub(crate) cached_audio_devices: Vec<String>,

    // Music HUD drag offset (user-adjustable position relative to default bottom-center)
    pub(crate) music_hud_drag_offset: Vec2,
    // Runtime hotkeys loaded from siv_hotkeys.yaml
    pub(crate) hotkeys_runtime: crate::hotkeys::RuntimeHotkeyState,
    pub(crate) hotkeys_draft_config: crate::hotkeys::model::HotkeyConfigFile,
    pub(crate) hotkeys_save_error_rx: Receiver<String>,
    pub(crate) hotkeys_save_tx: Sender<crate::hotkeys::model::HotkeyConfigFile>,
    pub(crate) hotkeys_saver_handle: Option<std::thread::JoinHandle<()>>,
    pub(crate) last_hotkeys_save_error: Option<(String, Instant)>,
    pub(crate) hotkeys_apply_success_at: Option<Instant>,
    pub(crate) hotkeys_load_error: Option<String>,
    pub(crate) startup_hotkeys_alert_shown: bool,
    pub(crate) hotkeys_capture_target:
        Option<(crate::hotkeys::model::HotkeyActionId, usize, usize)>,
    pub(crate) hotkeys_selected_row: Option<(usize, usize)>,
    pub(crate) hotkeys_add_row_dialog_open: bool,
    pub(crate) hotkeys_add_row_action: crate::hotkeys::model::HotkeyActionId,
    /// Add Row dialog: user pressed Record Key and is waiting for the next input.
    pub(crate) hotkeys_add_row_capture_active: bool,
    /// Add Row dialog: key captured via Record Key (not yet committed to the grid).
    pub(crate) hotkeys_add_row_captured_key: Option<String>,
    /// Add Row dialog: OK clicked without a recorded key.
    pub(crate) hotkeys_add_row_need_key_hint: bool,
    pub(crate) context_menu_runtime: crate::context_menu::RuntimeContextMenuState,
    pub(crate) context_menu_draft_config: crate::context_menu::model::ContextMenuConfigFile,
    pub(crate) context_menu_save_error_rx: Receiver<String>,
    pub(crate) context_menu_save_tx: Sender<crate::context_menu::model::ContextMenuConfigFile>,
    pub(crate) context_menu_saver_handle: Option<std::thread::JoinHandle<()>>,
    pub(crate) last_context_menu_save_error: Option<(String, Instant)>,
    pub(crate) context_menu_apply_success_at: Option<Instant>,
    pub(crate) context_menu_apply_error: Option<String>,
    pub(crate) context_menu_selected_row: Option<usize>,
    pub(crate) context_menu_scroll_to_selected: bool,
    pub(crate) context_menu_drag_row: Option<usize>,
    pub(crate) context_menu_help_open: bool,
    pub(crate) context_menu_edit_dialog_open: bool,
    pub(crate) context_menu_edit_target: Option<usize>,
    pub(crate) context_menu_edit_draft: crate::context_menu::model::EditableContextMenuEntry,
    pub(crate) context_menu_exe_browse_requested: bool,
    /// True while a refresh-file-list scan (F5) is in progress.
    /// Prevents re-entry and blocks navigation actions that would
    /// dereference image_files during the incomplete rebuild.
    pub(crate) refresh_scan_in_progress: bool,
    /// Records whether the slideshow was actively playing (not paused)
    /// before the F5 refresh scan started, so it can be restored on completion.
    pub(crate) refresh_scan_slideshow_was_playing: bool,
    /// The absolute path of the image that was current when F5 was pressed.
    /// Survives across multiple scan batches so Done can always relocate the
    /// original file, even when it wasn't present in the first batch.
    pub(crate) refresh_anchor_path: Option<std::path::PathBuf>,
    /// Pre-refresh image paths used to realign strip thumbnails after F5.
    pub(crate) refresh_strip_files_snapshot: Option<Vec<std::path::PathBuf>>,
    pub(crate) pixel_data_source: Option<crate::pixel_inspector::PixelDataSource>,
    pub(crate) pixel_hover_cache: Option<PixelHoverCache>,
    pub(crate) pixel_region_first_point: Option<(u32, u32)>,
    pub(crate) tray_state: Option<TrayState>,
    pub(crate) hidden_to_tray: bool,
    pub(crate) pending_hide_to_tray: bool,
    pub(crate) tray_cmd_rx: crossbeam_channel::Receiver<crate::app::tray_handlers::TrayCommand>,
    /// Session-only preference for the copy/cut dialog overwrite checkbox.
    pub(crate) copy_cut_overwrite_if_exists: bool,
    pub(crate) explicit_quit: bool,
}

pub(crate) struct TrayState {
    pub(crate) _tray_icon: tray_icon::TrayIcon,
    pub(crate) was_maximized: bool,
}
/// Holds animation frame data waiting to be uploaded to GPU across multiple frames.
pub(crate) struct PendingAnimUpload {
    pub(crate) image_index: usize,
    pub(crate) hdr_frames: Option<Vec<std::sync::Arc<crate::hdr::types::HdrImageBuffer>>>,
    pub(crate) frames: Vec<crate::loader::AnimationFrame>,
    pub(crate) textures: Vec<egui::TextureHandle>,
    pub(crate) delays: Vec<std::time::Duration>,
    pub(crate) next_frame: usize,
}

impl ImageViewerApp {
    pub(crate) fn raw_demosaic_mode_for_index(
        &self,
        index: usize,
    ) -> crate::settings::RawDemosaicMode {
        if self.gpu_demosaic_failed_indices.contains(&index) {
            crate::settings::RawDemosaicMode::Cpu
        } else {
            self.settings.raw_demosaic_mode
        }
    }
}
