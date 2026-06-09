#!/usr/bin/env python3
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
src = subprocess.check_output(
    ["git", "show", "HEAD:src/app/mod.rs"], text=True, encoding="utf-8"
)
lines = src.splitlines(keepends=True)


def slice_lines(start: int, end: int) -> str:
    return "".join(lines[start - 1:end])


copyright = "".join(lines[0:15])

types_imports = """use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use eframe::egui::{self, Pos2, Rect, Vec2};

use crate::audio::AudioPlayer;
use crate::ipc::IpcMessage;
use crate::loader::{ImageLoader, TextureCache};
use crate::settings::{Settings, TransitionStyle};
use crate::theme::{SystemThemeCache, ThemePalette};
use crate::tile_cache::TileManager;
use crate::ui::dialogs::modal_state::ActiveModal;

"""

types_body = types_imports + (
    slice_lines(56, 108)
    + slice_lines(144, 170)
    + slice_lines(172, 176)
    + slice_lines(226, 240)
    + slice_lines(242, 301)
    + slice_lines(333, 351)
    + slice_lines(357, 364)
    + slice_lines(366, 402)
    + slice_lines(404, 685)
    + slice_lines(687, 695)
)
for old, new in [
    ("    image_index: usize,", "    pub(crate) image_index: usize,"),
    ("    textures: Vec", "    pub(crate) textures: Vec"),
    ("    hdr_frames: Option", "    pub(crate) hdr_frames: Option"),
    ("    delays: Vec", "    pub(crate) delays: Vec"),
    ("    current_frame: usize,", "    pub(crate) current_frame: usize,"),
    ("    frame_start: Instant,", "    pub(crate) frame_start: Instant,"),
    ("    frames: Vec", "    pub(crate) frames: Vec"),
    ("    next_frame: usize,", "    pub(crate) next_frame: usize,"),
]:
    types_body = types_body.replace(old, new)
(ROOT / "src/app/types.rs").write_text(copyright + types_body, encoding="utf-8")

preload_imports = """use std::collections::HashSet;

use super::types::{HardwareTier, UltraHdrCapacityRefresh};

"""
(ROOT / "src/app/preload.rs").write_text(
    copyright
    + preload_imports
    + slice_lines(49, 54)
    + slice_lines(110, 142)
    + slice_lines(178, 222)
    + slice_lines(303, 331),
    encoding="utf-8",
)
(ROOT / "src/app/hotkeys_ui.rs").write_text(
    copyright + slice_lines(697, 756), encoding="utf-8"
)
(ROOT / "src/app/metadata_extract.rs").write_text(
    copyright + slice_lines(1498, 1642), encoding="utf-8"
)

app_methods = (
    """use std::path::PathBuf;

use eframe::egui::{self, Context};
use rust_i18n::t;

use crate::ui::dialogs::modal_state::ActiveModal;

use super::hotkeys_ui::build_hotkeys_issue_message;
use super::types::ImageViewerApp;

"""
    + slice_lines(758, 928)
)
app_methods = app_methods.replace(
    "    fn focus_and_unminimize_window", "    pub(crate) fn focus_and_unminimize_window"
).replace(
    "    fn handle_ipc_open_image(", "    pub(crate) fn handle_ipc_open_image("
)
(ROOT / "src/app/app_methods.rs").write_text(copyright + app_methods, encoding="utf-8")

eframe = (
    """use std::time::{Duration, Instant};

use eframe::egui::{self, Context};
use rust_i18n::t;

use crate::ipc::IpcMessage;
use crate::settings::Settings;
use crate::ui::utils::setup_visuals;

use super::types::{hdr_output_state_changed, CachedWindowPlacement, HdrOutputStateSnapshot, ImageViewerApp};

"""
    + slice_lines(930, 1496)
)
(ROOT / "src/app/eframe_app.rs").write_text(copyright + eframe, encoding="utf-8")

mod_rs = copyright + """// ── Submodules ──────────────────────────────────────────────────────────────
pub(crate) mod hdr_status;
pub(crate) mod hdr_vulkan_metadata;
pub(crate) mod image_management;
pub(crate) mod input;
pub(crate) mod lifecycle;
pub(crate) mod media;
pub(crate) mod rendering;
pub(crate) mod rfd_parent;
pub(crate) mod view_status;

mod app_methods;
mod eframe_app;
mod hotkeys_ui;
mod metadata_extract;
mod preload;
mod types;

pub use types::{FileOpResult, HardwareTier, ImageViewerApp};

pub(crate) use types::{
    AnimationPlayback, CachedWindowPlacement, CurrentHdrImage, CurrentHdrTiledImage,
    HdrOutputStateSnapshot, LightweightFileOpJob, PendingAnimUpload, SettingsTab,
    UltraHdrCapacityRefresh,
};
pub(crate) use types::hdr_output_state_changed;

pub(crate) use preload::{
    CACHE_SIZE, MAX_PRELOAD_BACKWARD, MAX_PRELOAD_FORWARD,
    capacity_refresh_should_reschedule_preloads, collect_ultra_hdr_capacity_sensitive_indices,
    compute_preload_budgets, memory_aware_tile_cache_budgets_mb, plan_ultra_hdr_capacity_refresh,
    ultra_hdr_decode_capacity_for_output_mode,
};

pub(crate) use hotkeys_ui::{build_hotkeys_issue_message, localized_hotkey_warning};
pub(crate) use metadata_extract::{extract_exif, extract_xmp};

pub(crate) use crate::settings::{ScaleMode, Settings, TransitionStyle};
pub(crate) use crate::theme::AppTheme;

#[cfg(test)]
mod tests;
"""
(ROOT / "src/app/mod.rs").write_text(mod_rs, encoding="utf-8")
print("recreated app split files")
