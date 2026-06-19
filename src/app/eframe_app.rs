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

use std::time::{Duration, Instant};

use eframe::egui::{self, Context};
use rust_i18n::t;

use crate::ipc::IpcMessage;
use crate::settings::Settings;
use crate::ui::utils::setup_visuals_with_font_size;

use super::types::{
    CachedWindowPlacement, HdrOutputStateSnapshot, ImageViewerApp, hdr_output_state_changed,
};

impl eframe::App for ImageViewerApp {
    fn on_exit(&mut self) {
        if self.settings.resume_last_image && !self.image_files.is_empty() {
            self.settings.last_viewed_image = Some(self.image_files[self.current_index].clone());
        }
        // Persist the last-known window placement. The HDR backend selection
        // (Rgba16Float vs Bgra8Unorm) is locked at swap-chain creation, so
        // remembering which monitor we closed on is what lets the user keep
        // testing HDR by simply reopening the app.
        if let Some(placement) = self.cached_window_placement {
            self.settings.window_maximized = placement.maximized;
            self.settings.window_outer_position = Some(placement.outer_position);
            self.settings.window_inner_size = Some(placement.inner_size);
            self.settings.window_maximized_screen_center = Some(placement.outer_center);
            if placement.maximized {
                self.settings.window_maximized_inner_size = Some(placement.inner_size);
                let restore_inner = self
                    .cached_restore_placement
                    .map(|p| p.inner_size)
                    .or(self.settings.window_restore_inner_size)
                    .unwrap_or(placement.inner_size);
                if let Some(restore) = self.cached_restore_placement {
                    self.settings.window_restore_outer_position = Some(restore.outer_position);
                    self.settings.window_restore_inner_size = Some(restore.inner_size);
                } else if let Some(pos) = Settings::valid_outer_position(placement.outer_position) {
                    self.settings.window_restore_outer_position = Some(pos);
                    self.settings.window_restore_inner_size = Some(restore_inner);
                } else if let Some(top_left) = Settings::restore_outer_top_left_for_screen_center(
                    placement.outer_center,
                    restore_inner,
                ) {
                    self.settings.window_restore_outer_position = Some(top_left);
                    self.settings.window_restore_inner_size = Some(restore_inner);
                }
            } else {
                self.settings.window_restore_outer_position = Some(placement.outer_position);
                self.settings.window_restore_inner_size = Some(placement.inner_size);
                self.settings.window_maximized_inner_size = None;
            }
        }
        // Shut down the async saver thread first: dropping the sender closes the
        // channel, causing the saver's `recv()` loop to exit after finishing any
        // in-progress write. This eliminates the race between the saver and our
        // synchronous save below.
        let (dummy_tx, _) = crossbeam_channel::unbounded::<Settings>();
        let old_tx = std::mem::replace(&mut self.save_tx, dummy_tx);
        drop(old_tx);
        let (dummy_hotkey_tx, _) =
            crossbeam_channel::unbounded::<crate::hotkeys::model::HotkeyConfigFile>();
        let old_hotkey_tx = std::mem::replace(&mut self.hotkeys_save_tx, dummy_hotkey_tx);
        drop(old_hotkey_tx);
        let (dummy_context_menu_tx, _) =
            crossbeam_channel::unbounded::<crate::context_menu::model::ContextMenuConfigFile>();
        let old_context_menu_tx =
            std::mem::replace(&mut self.context_menu_save_tx, dummy_context_menu_tx);
        drop(old_context_menu_tx);

        // Wait for the saver thread to finish any in-progress I/O
        if let Some(handle) = self.saver_handle.take() {
            if let Err(e) = handle.join() {
                log::error!("[on_exit] Saver thread panicked: {:?}", e);
            }
        }
        if let Some(handle) = self.hotkeys_saver_handle.take() {
            if let Err(e) = handle.join() {
                log::error!("[on_exit] Hotkeys saver thread panicked: {:?}", e);
            }
        }
        if let Some(handle) = self.context_menu_saver_handle.take() {
            if let Err(e) = handle.join() {
                log::error!("[on_exit] Context menu saver thread panicked: {:?}", e);
            }
        }

        if let Err(e) = self.settings.save() {
            log::error!("[on_exit] Failed to save settings: {}", e);
        }
        if let Err(e) = crate::hotkeys::io::save_hotkeys_file(&self.hotkeys_runtime.config) {
            log::error!("[on_exit] Failed to save hotkeys: {}", e);
        }
        if let Err(e) =
            crate::context_menu::io::save_context_menu_file(&self.context_menu_runtime.config)
        {
            log::error!("[on_exit] Failed to save context menu: {}", e);
        }

        if let (Some(info), Some(cache)) = (
            self.wgpu_adapter_info.as_ref(),
            self.wgpu_pipeline_cache.as_deref(),
        ) {
            crate::wgpu_pipeline_cache::persist(info, cache);
        }

        // Force-terminate BEFORE eframe tries to tear down GPU resources.
        // This avoids a DLL loader lock deadlock on Windows where:
        //   - rayon worker threads hold the loader lock during TLS cleanup
        //   - WIC's CCodecFactory destructor calls MFShutdown which waits for internal timer threads
        //   - main thread's D3D12 adapter drop calls FreeLibrary which needs the loader lock
        // Settings are already persisted above, so this is safe.
        #[cfg(all(target_os = "windows", not(feature = "legacy_win7")))]
        crate::startup::take_and_join_dx12_cache_validate_thread();

        // Explicitly drop tray icon state so it gets cleaned up from the taskbar before process termination.
        self.tray_state = None;
        crate::app::tray_handlers::clear_menu_ids();
        self.hidden_to_tray = false;
        self.pending_hide_to_tray = false;

        self.loader.prepare_for_process_exit();

        crate::startup::shutdown_logger();
        #[cfg(target_os = "windows")]
        std::process::exit(0);
        #[cfg(unix)]
        crate::startup::force_process_exit(0);
    }

    /// Background logic: scanning, loading, auto-switch, keyboard, timers.
    /// Called before each ui() call (and also when hidden but repaint requested).
    fn logic(&mut self, ctx: &Context, frame: &mut eframe::Frame) {
        self.ensure_root_redraw_wake(frame);
        // Poll tray commands (handlers wake the event loop via request_repaint).
        while let Ok(cmd) = self.tray_cmd_rx.try_recv() {
            match cmd {
                crate::app::tray_handlers::TrayCommand::ShowMainWindow => {
                    self.show_main_window_from_tray(ctx);
                }
                crate::app::tray_handlers::TrayCommand::OpenSettings => {
                    self.show_main_window_from_tray(ctx);
                    self.show_settings = true;
                }
                crate::app::tray_handlers::TrayCommand::Quit => {
                    self.explicit_quit = true;
                    self.quit_process_now();
                }
            }
        }

        // Process IPC messages (needs to happen before minimized check to wake up immediately)
        while let Ok(msg) = self.ipc_rx.try_recv() {
            if self.hidden_to_tray {
                self.show_main_window_from_tray(ctx);
            }
            match msg {
                IpcMessage::OpenImage(path) => {
                    log::info!("IPC: open image {:?}", path);
                    self.handle_ipc_open_image(path, ctx, false);
                }
                IpcMessage::OpenImageNoRecursive(path) => {
                    log::info!("IPC: open image (no-recursive) {:?}", path);
                    self.handle_ipc_open_image(path, ctx, true);
                }
                IpcMessage::Focus => {
                    log::info!("IPC received empty ping, requesting window focus");
                    Self::focus_and_unminimize_window(ctx);
                }
            }
        }

        // Intercept close request if minimize to tray is enabled and not an explicit quit.
        // eframe may report the close request for more than one frame, so keep
        // canceling it while we are in tray mode instead of only on the first frame.
        // Do not return early — fall through to `finish_hide_to_tray` below so hide
        // completes even when `close_requested` stays true, and scan/load logic still runs.
        if ctx.input(|i| i.viewport().close_requested()) && !self.explicit_quit {
            let is_shutting_down = {
                #[cfg(target_os = "windows")]
                {
                    use windows::Win32::UI::WindowsAndMessaging::{
                        GetSystemMetrics, SM_SHUTTINGDOWN,
                    };
                    unsafe { GetSystemMetrics(SM_SHUTTINGDOWN) != 0 }
                }
                #[cfg(not(target_os = "windows"))]
                false
            };

            if self.settings.minimize_to_tray_on_close && !is_shutting_down {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                if !self.hidden_to_tray && !self.pending_hide_to_tray {
                    self.prepare_hide_to_tray(ctx);
                }
            }
        }

        if self.pending_hide_to_tray {
            self.finish_hide_to_tray(ctx);
        }

        // Cache window placement (outer position, inner size, maximized) so
        // `on_exit` can persist it without needing a `ctx`. egui exposes the
        // OS-level outer rect via `ViewportInfo::outer_rect`; on multi-monitor
        // systems this is what determines which monitor the next session
        // spawns onto, and therefore whether `Rgba16Float` or `Bgra8Unorm` is
        // selected for the swap chain.
        if let Some((placement, is_fullscreen)) =
            ctx.viewport_for(egui::ViewportId::ROOT, |viewport| {
                let viewport = viewport.input.viewport();
                let outer_rect = viewport.outer_rect?;
                let inner_size = viewport.inner_rect.unwrap_or(outer_rect).size();
                let center = outer_rect.center();
                let is_fullscreen = viewport.fullscreen.unwrap_or(false);
                Some((
                    CachedWindowPlacement {
                        outer_position: [
                            outer_rect.min.x.round() as i32,
                            outer_rect.min.y.round() as i32,
                        ],
                        outer_center: [center.x.round() as i32, center.y.round() as i32],
                        inner_size: [
                            inner_size.x.round().max(1.0) as u32,
                            inner_size.y.round().max(1.0) as u32,
                        ],
                        maximized: viewport.maximized.unwrap_or(false),
                    },
                    is_fullscreen,
                ))
            })
        {
            if !placement.maximized
                && !is_fullscreen
                && !self.layout_uses_fullscreen_metrics()
                && Settings::valid_outer_position(placement.outer_position).is_some()
            {
                self.cached_restore_placement = Some(placement);
            }
            // Diagnostic: log the FIRST time we observe a placement, then
            // only on subsequent changes at debug level. If the first-time
            // log never appears, `viewport.outer_rect` is `None` on this
            // build and we have no position to persist — that would explain
            // why the saved-position recall does nothing on a fresh install.
            let was_unset = self.cached_window_placement.is_none();
            let changed = self.cached_window_placement != Some(placement);
            self.cached_window_placement = Some(placement);
            if was_unset {
                log::info!(
                    "[Window] first placement observed: outer_position={:?} inner_size={:?} maximized={}",
                    placement.outer_position,
                    placement.inner_size,
                    placement.maximized,
                );
            } else if changed {
                log::debug!(
                    "[Window] placement updated: outer_position={:?} inner_size={:?} maximized={}",
                    placement.outer_position,
                    placement.inner_size,
                    placement.maximized,
                );
            }
        }

        // Global mouse activity detection to wake up Music HUD
        if ctx.input(|i| i.pointer.delta().length_sq() > 0.0) {
            self.music_hud_last_activity = Instant::now();
        }

        if let Some(rx) = self.font_families_rx.as_ref() {
            match rx.try_recv() {
                Ok(families) => {
                    self.font_families = families;
                    self.font_families_rx = None;
                    ctx.request_repaint();
                }
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    log::warn!("[Core] Font enumeration finished without sending a result");
                    self.font_families_rx = None;
                }
                Err(crossbeam_channel::TryRecvError::Empty) => {}
            }
        }

        // ── Drag-and-Drop handling (cross-platform via egui/winit) ───────
        let dropped: Vec<_> = ctx.input(|i| i.raw.dropped_files.clone());
        if let Some(dropped_file) = dropped.into_iter().next() {
            if let Some(path) = dropped_file.path {
                // Guard: don't re-trigger if we're already scanning from a previous drop
                if !self.scanning {
                    if path.is_dir() {
                        // Dropped a directory — scan it (non-recursive to avoid surprises)
                        log::info!("Drop: opening directory {:?}", path);
                        self.settings.browse_mode = crate::settings::BrowseMode::Linear;
                        self.settings.show_directory_tree_nav = false;
                        self.settings.tree_nav_root_dir = None;
                        self.settings.tree_nav_selected_dir = None;
                        self.settings.recursive = false;
                        self.load_directory(path);
                        self.queue_save();
                    } else if path.is_file() {
                        // Dropped a single file — check if it's a supported format
                        let is_supported = path
                            .extension()
                            .map(|ext| crate::scanner::is_supported_extension(ext))
                            .unwrap_or(false);

                        if is_supported {
                            log::info!("Drop: opening file {:?}", path);
                            if let Some(parent) = path.parent() {
                                self.initial_image = Some(path.clone());
                                self.settings.browse_mode = crate::settings::BrowseMode::Linear;
                                self.settings.show_directory_tree_nav = false;
                                self.settings.tree_nav_root_dir = None;
                                self.settings.tree_nav_selected_dir = None;
                                self.settings.auto_switch = false;
                                self.load_directory(parent.to_path_buf());
                                self.queue_save();
                            }
                        } else {
                            log::warn!("Drop: ignored unsupported file format {:?}", path);
                        }
                    }
                    ctx.request_repaint();
                }
            }
        }

        let now = Instant::now();
        let dt = now.duration_since(self.last_frame_time);
        self.last_frame_time = now;

        let minimized = ctx.viewport_for(egui::ViewportId::ROOT, |viewport| {
            viewport.input.viewport().minimized.unwrap_or(false)
        }) || self.hidden_to_tray;

        if minimized {
            // Pause the auto-switch timer while minimized by offsetting its start
            if self.settings.auto_switch {
                self.last_switch_time += dt;
            }

            self.process_directory_scan_pipeline(ctx);

            // Limit background processing while hidden
            self.process_music_scan_results(); // Allow music to start if scanning finishes

            // Process keyboard input (like Ctrl+Shift+T hotkey toggle) and file operation results even when minimized/in tray
            self.handle_keyboard(ctx);
            self.process_file_op_results();
            // Pick-directory is main-window only; drop any flag queued before hide/minimize.
            self.pending_open_directory = false;

            self.last_minimized = true;
            ctx.request_repaint_after(Duration::from_millis(500));
            return;
        }

        // Just restored from minimized state: force a clean UI refresh
        if self.last_minimized {
            self.last_minimized = false;
            self.invalidate_view_text_layout();
            ctx.request_repaint();
        }

        let scanning_at_frame_start = self.scanning;
        self.process_directory_scan_pipeline(ctx);

        // Pull the live swap-chain target format every frame so all downstream
        // consumers (`HdrImageRenderer`, `effective_render_output_mode`, OSD,
        // etc.) base their decisions on the actually-active format.
        //
        // Crucially: we must NOT read from `frame.wgpu_render_state()` after
        // the first runtime hot-swap. `egui_wgpu::RenderState` derives `Clone`
        // and eframe stores a CLONE in `Frame`; the painter's post-swap
        // mutation of `render_state.target_format` only updates the painter's
        // copy, so `wgpu_render_state().target_format` permanently returns the
        // startup format. The painter therefore publishes the live format on
        // a dedicated reverse-direction mailbox (`active_target_format`),
        // which we read first; we only fall back to `wgpu_render_state()` for
        // the *initial* format before any runtime swap has happened.
        let live_target_format = self
            .active_target_format
            .get()
            .or_else(|| frame.wgpu_render_state().map(|s| s.target_format));
        if let Some(format) = live_target_format {
            self.hdr_target_format = Some(format);
        }
        self.sync_hdr_callback_resources_prewarm(frame);
        self.sync_loader_wgpu_context_from_frame(frame);
        self.sync_loader_hdr_callback_upload_snapshot();

        let hdr_content_visible = self.current_hdr_render_path().is_some();
        let main_window_outer_top_left = self
            .cached_window_placement
            .map(|placement| placement.outer_position)
            .or_else(|| {
                ctx.viewport_for(egui::ViewportId::ROOT, |viewport| {
                    let rect = viewport.input.viewport().outer_rect?;
                    Some([rect.min.x.round() as i32, rect.min.y.round() as i32])
                })
            });
        self.hdr_monitor_state.refresh_from_viewport(
            ctx,
            now,
            hdr_content_visible,
            main_window_outer_top_left,
            self.settings.window_spawn_top_left_for_hdr(),
        );
        let previous_hdr_output_state = HdrOutputStateSnapshot::new(
            self.hdr_capabilities.output_mode,
            self.hdr_capabilities.native_presentation_enabled,
            self.hdr_target_format,
        );
        let output_mode = crate::hdr::monitor::effective_capability_output_mode(
            self.hdr_target_format,
            self.effective_hdr_monitor_selection().as_ref(),
        );
        self.hdr_capabilities.output_mode = output_mode;

        let render_output_mode = crate::hdr::monitor::effective_render_output_mode(
            self.hdr_target_format,
            self.effective_hdr_monitor_selection().as_ref(),
        );
        if matches!(
            self.hdr_target_format,
            Some(wgpu::TextureFormat::Rgb10a2Unorm)
        ) {
            let wants_pq = render_output_mode.rgb10a2_uses_pq_shader();
            if self.rgb10a2_pq_encode_requested != wants_pq {
                self.rgb10a2_pq_encode_requested = wants_pq;
                self.requested_rgb10a2_pq_encode.request(wants_pq);
            }
            if matches!(
                render_output_mode,
                crate::hdr::renderer::HdrRenderOutputMode::NativeHdrGamma22
            ) {
                let tone = self.effective_hdr_tone_map_settings();
                let scale = tone.sdr_white_nits / tone.max_display_nits.max(tone.sdr_white_nits);
                self.gamma22_display_scale.set(scale);
            }
        }
        let tone = self.effective_hdr_tone_map_settings();
        if tone != self.hdr_renderer.tone_map {
            self.sync_hdr_tone_map_settings();
        }

        // If the active monitor's HDR capability disagrees with the current
        // swap-chain target format, ask the Painter to hot-swap. This is what
        // makes `Rgba16Float` → `Bgra8Unorm` follow the user as they drag the
        // window between an HDR monitor and an SDR monitor at runtime.
        //
        // `desired_target_format_for_active_monitor` returns `None` when the
        // per-frame monitor probe has not produced positive evidence yet
        // (transient DXGI hand-off, brief `EnumWindows` hiccups, the very
        // first frames before the first probe has completed). We MUST treat
        // that as "no opinion / keep current format" rather than blindly
        // demoting to `Bgra8Unorm` — otherwise we would request a swap-chain
        // demotion every frame the probe was pending, defeating the
        // spawn-time HDR detection that already chose the correct initial
        // format.
        let native_surface_requests_enabled =
            crate::hdr::surface::native_hdr_swapchain_requests_enabled(
                self.settings.hdr_native_surface_enabled_effective(),
                self.hdr_capabilities.backend,
            );
        let effective_selection = self.effective_hdr_monitor_selection();
        let desired_target_format = crate::hdr::surface::desired_target_format_for_active_monitor(
            native_surface_requests_enabled,
            effective_selection.as_ref(),
        );
        if let Some(desired_format) = desired_target_format
            && Some(desired_format) != self.hdr_target_format
        {
            if self.last_logged_swap_chain_format_request != Some(desired_format) {
                log::debug!(
                    "[HDR] runtime swap-chain format mismatch: current={:?} desired={:?} \
                     monitor={:?} hdr_supported={:?} native_surface_enabled={}",
                    self.hdr_target_format,
                    desired_format,
                    effective_selection.as_ref().map(|s| s.label.as_str()),
                    effective_selection.as_ref().map(|s| s.hdr_supported),
                    native_surface_requests_enabled,
                );
                self.last_logged_swap_chain_format_request = Some(desired_format);
            }
            self.requested_target_format.request(desired_format);
            if let Some(state) = frame.wgpu_render_state() {
                self.hdr_callback_resources_prewarm.ensure_started(
                    &state.device,
                    desired_format,
                    self.wgpu_pipeline_cache.as_deref(),
                );
            }
            ctx.request_repaint();
        } else {
            self.last_logged_swap_chain_format_request = None;
        }
        self.hdr_capabilities.native_presentation_enabled =
            crate::hdr::surface::native_hdr_swapchain_active(
                self.settings.hdr_native_surface_enabled_effective(),
                self.hdr_capabilities.backend,
                self.hdr_target_format,
            );
        #[cfg(feature = "preload-debug")]
        {
            use crate::app::preload_hdr_gate::SwapRequestOutcome;
            let wsi = self.vulkan_wsi_hdr_gates.get();
            let wp_selection = self.hdr_monitor_state.selection();
            let swap_request_outcome = if !native_surface_requests_enabled {
                SwapRequestOutcome::Disabled
            } else if desired_target_format.is_none() {
                SwapRequestOutcome::NoMonitorOpinion
            } else if desired_target_format == self.hdr_target_format {
                SwapRequestOutcome::AlreadyMatched
            } else {
                SwapRequestOutcome::Requested
            };
            self.hdr_preload_gate_log.log_swap_chain_gate(
                native_surface_requests_enabled,
                self.settings.hdr_native_surface_enabled_effective(),
                self.settings.hdr_native_surface_enabled,
                self.hdr_capabilities.backend,
                self.hdr_target_format,
                desired_target_format,
                swap_request_outcome,
                wsi,
                wp_selection,
                effective_selection.as_ref(),
                output_mode,
                self.hdr_capabilities.native_presentation_enabled,
            );
        }
        self.hdr_capabilities.available = self.hdr_capabilities.native_presentation_enabled
            || output_mode != crate::hdr::types::HdrOutputMode::SdrToneMapped;
        let next_hdr_output_state = HdrOutputStateSnapshot::new(
            self.hdr_capabilities.output_mode,
            self.hdr_capabilities.native_presentation_enabled,
            self.hdr_target_format,
        );
        if hdr_output_state_changed(previous_hdr_output_state, next_hdr_output_state) {
            if previous_hdr_output_state.target_format() != next_hdr_output_state.target_format() {
                self.invalidate_directory_tree_strip_gpu_textures();
            }
            if let Some(selection) = self.effective_hdr_monitor_selection() {
                log::info!(
                    "[HDR] presentation active: output_mode={:?} native_presentation={} \
                     target_format={:?} monitor={} hdr_supported_effective={} \
                     hdr_capacity_source={:?} max_luminance_nits={:?}",
                    self.hdr_capabilities.output_mode,
                    self.hdr_capabilities.native_presentation_enabled,
                    self.hdr_target_format,
                    selection.label,
                    selection.hdr_supported,
                    selection.hdr_capacity_source,
                    selection.max_luminance_nits,
                );
            }
            self.refresh_hdr_view_status();
        }
        self.refresh_ultra_hdr_decode_capacity(ctx);
        crate::loader::refresh_hq_preview_monitor_cap(ctx);
        self.sync_linux_vulkan_hdr_metadata();

        // Poll persistence errors from the saver thread
        while let Ok(err) = self.save_error_rx.try_recv() {
            log::error!("Settings persistence error: {}", err);
            self.last_save_error = Some((err, Instant::now()));
        }
        while let Ok(err) = self.hotkeys_save_error_rx.try_recv() {
            log::error!("Hotkeys persistence error: {}", err);
            self.last_hotkeys_save_error = Some((err, Instant::now()));
        }
        while let Ok(err) = self.context_menu_save_error_rx.try_recv() {
            log::error!("Context menu persistence error: {}", err);
            self.last_context_menu_save_error = Some((err, Instant::now()));
        }

        // Clear persistence error after 5 seconds
        if let Some((_, start)) = self.last_save_error {
            if start.elapsed().as_secs() >= 5 {
                self.last_save_error = None;
            }
        }
        if let Some((_, start)) = self.last_hotkeys_save_error {
            if start.elapsed().as_secs() >= 5 {
                self.last_hotkeys_save_error = None;
            }
        }
        if let Some((_, start)) = self.last_context_menu_save_error {
            if start.elapsed().as_secs() >= 5 {
                self.last_context_menu_save_error = None;
            }
        }
        if let Some(start) = self.hotkeys_apply_success_at {
            if start.elapsed().as_secs() >= 3 {
                self.hotkeys_apply_success_at = None;
            }
        }
        if let Some(start) = self.context_menu_apply_success_at {
            if start.elapsed().as_secs() >= 3 {
                self.context_menu_apply_success_at = None;
            }
        }

        if scanning_at_frame_start != self.scanning {
            ctx.request_repaint();
        }
        if !self.scanning {
            self.process_loaded_images(ctx, &mut Some(frame));
            self.refresh_raw_gpu_demosaic_pending_from_gpu_bindings(ctx, Some(frame));
            // Upload deferred CPU pixels for the outgoing frame before navigation captures
            // `prev_texture` (preloaded neighbors often skip GPU upload until display).
            self.flush_deferred_sdr_upload_for_index(self.current_index, ctx);
            self.wake_root_for_logic();
        }
        self.process_music_scan_results();
        self.check_auto_switch(ctx);
        self.handle_keyboard(ctx);
        self.process_file_op_results();

        // Check if the audio thread detected a hardware stall (e.g. WASAPI exclusive
        // mode preemption) and needs a full restart — same path as toggling the checkbox.
        if self.settings.play_music && self.audio.take_needs_restart() {
            log::warn!("[UI] Audio stall detected by watchdog, triggering full restart");
            self.force_restart_audio();
        }

        // Sync currently playing track path and CUE track for persistence
        if self.settings.play_music {
            let mut changed = false;
            if let Some(current_path) = self.audio.get_current_track_path() {
                if self.settings.last_music_file.as_ref() != Some(&current_path) {
                    self.settings.last_music_file = Some(current_path);
                    changed = true;
                }

                let cue_idx = self.audio.get_current_cue_track();
                if self.settings.last_music_cue_track != cue_idx {
                    self.settings.last_music_cue_track = cue_idx;
                    changed = true;
                }
            }

            if changed {
                self.queue_save();
            }
        }

        // Deferred logic actions (require `logic()` / `Frame`; set from input dispatch):
        // - `pending_fullscreen`: Option<bool> — viewport fullscreen toggle
        // - `pending_open_directory`: bool — native folder picker (PickDirectory hotkey)
        if let Some(fs) = self.pending_fullscreen.take() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(fs));
        }
        if std::mem::take(&mut self.pending_open_directory)
            && self.pick_directory_hotkey_allowed(ctx)
        {
            self.open_directory_dialog(frame);
        }

        // Keep repainting while loading, auto-switching, or playing music
        let is_music_playing = self.settings.play_music && self.cached_music_count.unwrap_or(0) > 0;
        if self.settings.auto_switch || self.scanning || !self.loader.rx.is_empty() {
            ctx.request_repaint();
        } else if is_music_playing {
            // Music only needs low-frequency polling for track-name updates (~2 fps)
            ctx.request_repaint_after(Duration::from_millis(500));
        } else {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    /// Draw the UI. In eframe 0.34 this is the required method; `ui` is called
    /// with the root `Ui` for the window's central area.
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        // Theme/visual sync runs here (ROOT ui pass), not in logic(). With the eframe fork,
        // logic() also runs when a deferred child viewport repaints; pixels_per_point and
        // style there are wrong and would corrupt dark-theme widget colors on the main window.
        self.sync_theme_and_visuals(&ctx);

        // Directory-tree auxiliary viewport (state synced in `logic()`; draw-only here).
        self.prepare_directory_tree_file_list_viewport(&ctx);

        // Draw image canvas (fills the central area)
        self.draw_image_canvas_ui(ui);

        if self.is_printing.load(std::sync::atomic::Ordering::Relaxed) {
            egui::Window::new(if cfg!(not(target_os = "windows")) {
                t!("print.title_pdf").to_string()
            } else {
                t!("print.title").to_string()
            })
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(&ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(t!("print.processing").to_string());
                });
            });

            if let Some(rx) = &self.print_status_rx {
                while let Ok(msg) = rx.try_recv() {
                    if let Some(m) = msg {
                        self.status_message = t!("print.failed", err = m).to_string();
                    }
                }
            }
        } else if let Some(rx) = self.print_status_rx.take() {
            while let Ok(msg) = rx.try_recv() {
                if let Some(m) = msg {
                    self.status_message = t!("print.failed", err = m).to_string();
                }
            }
        }

        // Settings panel overlay.
        // Suppressed while a modal dialog is open: the modal dialog's backdrop
        // only dims visually (Order::Background); to achieve true modality we
        // must prevent the settings panel from being rendered (and thus from
        // receiving input) while a dialog is on screen.
        self.open_startup_hotkeys_alert_if_needed();

        let modal_open = self.active_modal.is_some();
        if self.show_settings && !modal_open {
            self.draw_settings_panel(&ctx, frame);
        } else if !self.show_settings {
            self.last_show_settings = false;
        }

        // Detect modal transitions: None → Some means a new dialog just opened.
        // Incrementing modal_generation makes the egui::Window Id unique for this
        // opening — egui has no position memory from previous openings, so the
        // dialog always appears at the calculated center position.
        {
            let id = egui::Id::new(crate::ui::dialogs::modal_state::ID_PREV_HAD_MODAL);
            let had_modal = ctx.data(|d| d.get_temp::<bool>(id).unwrap_or(false));
            let has_modal = self.active_modal.is_some();
            if has_modal && !had_modal {
                self.modal_generation = self.modal_generation.wrapping_add(1);
            }
            ctx.data_mut(|d| d.insert_temp(id, has_modal));
        }

        // Dispatch the single active modal dialog (MovableModal handles the overlay)
        self.dispatch_active_modal(&ctx);

        // ── Music HUD (Foreground Layer) ─────────────────────────────────
        self.draw_music_hud_foreground(&ctx);
    }
}

impl ImageViewerApp {
    /// System-theme trailing detection and DPI-driven style refresh. Must run from the ROOT
    /// `ui()` pass only (see comment in `ui()`).
    fn sync_theme_and_visuals(&mut self, ctx: &Context) {
        let font_size = self.temp_font_size.unwrap_or(self.settings.font_size);
        let mut style_changed = false;

        if let Some(new_palette) = self
            .settings
            .theme
            .resolve_if_changed(&mut self.theme_cache)
        {
            self.cached_palette = new_palette;
            style_changed = true;
        }

        let ppp = ctx.pixels_per_point();
        if (ppp - self.cached_pixels_per_point).abs() > 0.001 {
            self.cached_pixels_per_point = ppp;
            style_changed = true;
        }

        if style_changed {
            setup_visuals_with_font_size(ctx, &self.settings, &self.cached_palette, font_size);
            self.request_directory_tree_viewport_repaint(ctx);
        }
    }

    /// Re-apply theme palette, egui visuals/fonts, and repaint auxiliary viewports.
    pub(crate) fn refresh_global_ui_style(&mut self, ctx: &Context) {
        let font_size = self.temp_font_size.unwrap_or(self.settings.font_size);
        if let Some(new_palette) = self
            .settings
            .theme
            .resolve_if_changed(&mut self.theme_cache)
        {
            self.cached_palette = new_palette;
        }
        setup_visuals_with_font_size(ctx, &self.settings, &self.cached_palette, font_size);
        ctx.request_repaint();
        self.request_directory_tree_viewport_repaint(ctx);
        self.wake_root_for_logic();
    }

    fn build_tray_state(was_maximized: bool) -> Option<super::types::TrayState> {
        let icon_data = crate::startup::icon::load_icon();
        let icon =
            match tray_icon::Icon::from_rgba(icon_data.rgba, icon_data.width, icon_data.height) {
                Ok(icon) => icon,
                Err(e) => {
                    log::error!("Failed to convert tray icon: {:?}", e);
                    return None;
                }
            };

        let show_item =
            tray_icon::menu::MenuItem::new(t!("tray.show_window").to_string(), true, None);
        let settings_item =
            tray_icon::menu::MenuItem::new(t!("tray.settings").to_string(), true, None);
        let quit_item = tray_icon::menu::MenuItem::new(t!("tray.quit").to_string(), true, None);
        let show_item_id = show_item.id().clone();
        let settings_item_id = settings_item.id().clone();
        let quit_item_id = quit_item.id().clone();

        let tray_menu = tray_icon::menu::Menu::new();
        let _ = tray_menu.append_items(&[
            &show_item,
            &settings_item,
            &tray_icon::menu::PredefinedMenuItem::separator(),
            &quit_item,
        ]);

        match tray_icon::TrayIconBuilder::new()
            .with_menu(Box::new(tray_menu))
            .with_menu_on_left_click(false)
            .with_tooltip(t!("app.name").to_string())
            .with_icon(icon)
            .build()
        {
            Ok(t) => {
                crate::app::tray_handlers::set_menu_ids(
                    show_item_id,
                    settings_item_id,
                    quit_item_id,
                );
                Some(super::types::TrayState {
                    _tray_icon: t,
                    was_maximized,
                })
            }
            Err(e) => {
                log::error!("Failed to build tray icon: {:?}", e);
                None
            }
        }
    }

    fn show_main_window_from_tray_viewport(ctx: &Context, was_maximized: bool) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        if was_maximized {
            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        crate::ipc::force_foreground();
    }

    fn focus_main_window(ctx: &Context) {
        // Win32 foreground first while the tray click is still fresh, then sync egui state.
        crate::ipc::force_foreground();
        Self::focus_and_unminimize_window(ctx);
    }

    fn quit_process_now(&mut self) -> ! {
        <Self as eframe::App>::on_exit(self);
        crate::startup::force_process_exit(0);
    }

    fn ensure_tray_icon(&mut self, was_maximized: bool) -> bool {
        if self.tray_state.is_none()
            && let Some(state) = Self::build_tray_state(was_maximized)
        {
            self.tray_state = Some(state);
        }

        if let Some(state) = &mut self.tray_state {
            state.was_maximized = was_maximized;
            true
        } else {
            false
        }
    }

    pub(crate) fn prepare_hide_to_tray(&mut self, ctx: &Context) {
        self.explicit_quit = false; // Reset explicit quit flag
        let was_maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        if self.ensure_tray_icon(was_maximized) {
            self.pending_hide_to_tray = true;
            ctx.request_repaint();
        }
    }

    fn finish_hide_to_tray(&mut self, ctx: &Context) {
        self.pending_hide_to_tray = false;
        self.hidden_to_tray = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        crate::ipc::hide_main_window();
    }

    pub(crate) fn minimize_to_tray(&mut self, ctx: &Context) {
        self.prepare_hide_to_tray(ctx);
        if self.pending_hide_to_tray {
            self.finish_hide_to_tray(ctx);
        }
    }

    /// Rebuild the tray icon/menu after locale change. When already minimized to tray,
    /// replaces the tray in place so the user is not left with a hidden window and no icon.
    pub(crate) fn refresh_tray_after_language_change(&mut self, ctx: &Context) {
        let Some(old) = self.tray_state.take() else {
            return;
        };
        let was_maximized = old.was_maximized;
        match Self::build_tray_state(was_maximized) {
            Some(state) => self.tray_state = Some(state),
            None => {
                log::warn!("Failed to rebuild tray after language change; restoring main window");
                crate::app::tray_handlers::clear_menu_ids();
                self.hidden_to_tray = false;
                self.pending_hide_to_tray = false;
                Self::show_main_window_from_tray_viewport(ctx, was_maximized);
            }
        }
    }

    pub(crate) fn show_main_window_from_tray(&mut self, ctx: &Context) {
        self.explicit_quit = false; // Reset explicit quit flag when restoring
        if let Some(state) = &self.tray_state {
            if self.hidden_to_tray || self.pending_hide_to_tray {
                self.hidden_to_tray = false;
                self.pending_hide_to_tray = false;
                Self::show_main_window_from_tray_viewport(ctx, state.was_maximized);
            } else {
                Self::focus_main_window(ctx);
            }
        }
    }

    pub(crate) fn minimize_to_tray_from_hotkey(&mut self, ctx: &Context) {
        if !self.hidden_to_tray {
            self.minimize_to_tray(ctx);
        }
    }
}
