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

use std::path::PathBuf;

use eframe::egui::{self};
use rust_i18n::t;

use crate::ui::dialogs::modal_state::ActiveModal;

use super::hotkeys_ui::build_hotkeys_issue_message;
use super::types::ImageViewerApp;
use crate::tile_cache::TileManager;

impl ImageViewerApp {
    /// Live swap-chain format: prefer the painter mailbox over the startup clone in [`Self::hdr_target_format`].
    pub(crate) fn effective_hdr_target_format(&self) -> Option<wgpu::TextureFormat> {
        self.hdr_target_format
            .or_else(|| self.active_target_format.get())
    }

    /// True when GPU RAW demosaic has baked and the frame still needs a forced HDR plane draw
    /// (render plan routes SDR, or the first post-bake present is pending).
    ///
    /// Does not include in-flight GPU bake (`hdr_raw_gpu_demosaic_pending_indices`); during
    /// pending the canvas should draw the SDR bootstrap via the normal render path.
    pub(crate) fn raw_gpu_demosaic_needs_sync_present(&self) -> bool {
        self.raw_gpu_demosaic_await_hdr_present
            || self
                .hdr_raw_gpu_demosaic_baked_indices
                .contains(&self.current_index)
    }

    /// True while GPU RAW demosaic is in flight or awaiting the sync-present HDR draw.
    pub(crate) fn raw_gpu_demosaic_needs_repaint_wake(&self) -> bool {
        self.raw_gpu_demosaic_needs_sync_present()
            || self
                .hdr_raw_gpu_demosaic_pending_indices
                .contains(&self.current_index)
    }

    /// True while the current image's CPU LibRaw HQ refine worker is in flight.
    pub(crate) fn cpu_raw_refinement_needs_repaint_wake(&self) -> bool {
        self.cpu_raw_refinement_pending_indices
            .contains(&self.current_index)
    }

    /// True while any async RAW develop work for the current image still needs frame wake.
    pub(crate) fn raw_async_work_needs_repaint_wake(&self) -> bool {
        self.raw_gpu_demosaic_needs_repaint_wake()
            || self.cpu_raw_refinement_needs_repaint_wake()
            || self
                .hq_tiled_preview_pending_indices
                .contains(&self.current_index)
    }

    /// True while the main thread still needs to poll loader output and/or upload deferred work.
    pub(crate) fn needs_process_loaded_images(&self) -> bool {
        self.loader.has_pending_outputs()
            || self.loader.is_loading(self.current_index)
            || !self.pending_anim_frames.is_empty()
    }

    pub(crate) fn layout_uses_fullscreen_metrics(&self) -> bool {
        self.settings.fullscreen
    }

    pub(crate) fn is_hotkey_capture_active(&self) -> bool {
        self.hotkeys_capture_target.is_some() || self.hotkeys_add_row_capture_active
    }

    pub(crate) fn reset_hotkeys_add_row_dialog_state(&mut self) {
        self.hotkeys_add_row_capture_active = false;
        self.hotkeys_add_row_captured_key = None;
        self.hotkeys_add_row_need_key_hint = false;
    }

    pub(crate) fn hotkeys_status_message(&self) -> Option<String> {
        build_hotkeys_issue_message(
            self.hotkeys_load_error.as_deref(),
            &self.hotkeys_runtime.conflicts,
            &self.hotkeys_runtime.warnings,
        )
    }

    pub(crate) fn refresh_current_file_name(&mut self) {
        let next = self
            .image_files
            .get(self.current_index)
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if self.current_file_name != next {
            self.current_file_name = next;
            self.image_status
                .set_file_name(self.current_file_name.as_str());
            self.invalidate_view_text_layout();
        }
    }

    pub(crate) fn set_current_index(&mut self, current_index: usize) {
        if self.current_index == current_index {
            return;
        }
        self.current_index = current_index;
        self.image_status.set_current_index(current_index);
        self.raw_metadata.set_current_index(current_index);
    }

    pub(crate) fn set_current_image_resolution(&mut self, resolution: Option<(u32, u32)>) {
        if self.current_image_res == resolution {
            return;
        }
        self.current_image_res = resolution;
        self.image_status.set_image_resolution(resolution);
    }

    pub(crate) fn set_zoom_factor(&mut self, zoom_factor: f32) {
        if (self.zoom_factor - zoom_factor).abs() <= f32::EPSILON {
            return;
        }
        self.zoom_factor = zoom_factor;
    }

    /// Bottom inset for the on-canvas hotkeys issue overlay so it sits above the OSD stack.
    pub(crate) fn hotkeys_issue_bottom_inset(&self) -> f32 {
        let mut inset = crate::constants::OSD_MARGIN;
        if self.settings.show_osd {
            inset += crate::constants::OSD_TEXT_SIZE;
            if self.osd.has_hdr_line() {
                inset += crate::constants::OSD_TEXT_SIZE + crate::constants::OSD_HDR_LINE_GAP;
            }
            if self.osd.has_raw_line() {
                inset += crate::constants::OSD_TEXT_SIZE + crate::constants::OSD_HDR_LINE_GAP;
            }
            if self.last_save_error.is_some() {
                inset = inset.max(
                    crate::constants::OSD_ERROR_OFFSET + crate::constants::OSD_ERROR_TEXT_SIZE,
                );
            }
        }
        inset + crate::constants::HOTKEYS_ISSUE_GAP_ABOVE_OSD
    }

    pub(crate) fn open_startup_hotkeys_alert_if_needed(&mut self) {
        if self.startup_hotkeys_alert_shown || self.active_modal.is_some() {
            return;
        }
        let Some(message) = self.hotkeys_status_message() else {
            return;
        };
        self.active_modal = Some(ActiveModal::Confirm(
            crate::ui::dialogs::confirm::State::info(t!("hotkeys.startup_issue_title"), message),
        ));
        self.startup_hotkeys_alert_shown = true;
    }

    pub(crate) fn native_hdr_swapchain_requests_enabled(&self) -> bool {
        crate::hdr::surface::native_hdr_swapchain_requests_enabled(
            self.settings.hdr_native_surface_enabled_effective(),
            self.hdr_capabilities.backend,
        )
    }

    pub(crate) fn effective_hdr_monitor_selection(
        &self,
    ) -> Option<crate::hdr::monitor::HdrMonitorSelection> {
        let wsi = self.vulkan_wsi_hdr_gates.get();
        crate::hdr::monitor::effective_monitor_selection(
            self.hdr_monitor_state.selection(),
            crate::hdr::wsi_probe::WsiHdrSurfaceGates {
                hdr10_st2084_rgb10a2: wsi.hdr10_st2084_rgb10a2,
                extended_srgb_linear_rgba16f: wsi.extended_srgb_linear_rgba16f,
                srgb_nonlinear_rgb10a2: wsi.srgb_nonlinear_rgb10a2,
                probed: wsi.probed,
            },
        )
    }

    pub(crate) fn effective_hdr_tone_map_settings(&self) -> crate::hdr::types::HdrToneMapSettings {
        let render_output_mode = crate::hdr::monitor::effective_render_output_mode(
            self.hdr_target_format,
            self.effective_hdr_monitor_selection().as_ref(),
        );
        self.settings.hdr_tone_map_settings_for_monitor(
            self.effective_hdr_monitor_selection().as_ref(),
            render_output_mode,
        )
    }

    /// Shortcut for `self.tile_manager.as_ref().unwrap()` — the tiled draw path
    /// is only entered when a [`TileManager`] is active.
    #[inline]
    pub(crate) fn tile_manager(&self) -> &TileManager {
        self.tile_manager
            .as_ref()
            .expect("tile_manager accessed without active tiled source")
    }

    pub(crate) fn focus_and_unminimize_window(ctx: &egui::Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        crate::ipc::force_foreground();
    }

    /// Shared handler for IPC open-file requests (`OpenImage` vs `OpenImageNoRecursive`).
    pub(crate) fn handle_ipc_open_image(
        &mut self,
        path: PathBuf,
        ctx: &egui::Context,
        no_recursive: bool,
    ) {
        let Some(parent) = path.parent() else {
            return;
        };

        self.settings.browse_mode = crate::settings::BrowseMode::Linear;
        self.settings.show_directory_tree_nav = false;
        self.settings.tree_nav_selected_dir = None;
        self.settings.tree_nav_selected_namespace_path = None;

        let same_dir = self
            .settings
            .last_image_dir
            .as_ref()
            .map(|d| d == &parent.to_path_buf())
            .unwrap_or(false);

        if same_dir && !self.image_files.is_empty() {
            if let Some(pos) = self.image_files.iter().position(|p| p == &path) {
                if self.settings.auto_switch {
                    self.settings.auto_switch = false;
                }
                self.navigate_to(pos, ctx);
            } else {
                self.initial_image = Some(path.clone());
                if no_recursive {
                    self.settings.recursive = false;
                }
                if self.settings.auto_switch {
                    self.settings.auto_switch = false;
                }
                self.load_directory(parent.to_path_buf());
            }
        } else {
            self.settings.last_image_dir = Some(parent.to_path_buf());
            if no_recursive {
                self.settings.recursive = false;
            }
            self.queue_save();
            self.initial_image = Some(path.clone());
            if self.settings.auto_switch {
                self.settings.auto_switch = false;
            }
            self.load_directory(parent.to_path_buf());
        }

        Self::focus_and_unminimize_window(ctx);
    }
}
