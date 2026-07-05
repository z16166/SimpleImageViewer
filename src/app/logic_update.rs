// Simple Image Viewer - logic() split for ISSUE-19/17 (viewport pass + coalesce).

use std::time::{Duration, Instant};

use eframe::egui::{self, Context};

use crate::constants::DEFAULT_ANIMATION_DELAY_MS;
use crate::ipc::IpcMessage;
use crate::settings::Settings;

use super::types::{
    CachedWindowPlacement, HdrOutputStateSnapshot, ImageViewerApp, hdr_output_state_changed,
};

impl ImageViewerApp {
    pub(super) const LOGIC_SHARED_COALESCE: Duration = Duration::from_millis(4);

    pub(super) fn should_run_logic_shared(&mut self) -> bool {
        let now = Instant::now();
        let run = self
            .last_logic_shared_at
            .is_none_or(|t| now.saturating_duration_since(t) >= Self::LOGIC_SHARED_COALESCE);
        if run {
            self.last_logic_shared_at = Some(now);
        }
        run
    }

    /// Poll tray menu/icon commands. Must run on every ROOT logic pass (not coalesced) so
    /// Quit is handled while hidden to tray.
    pub(super) fn poll_tray_commands(&mut self, ctx: &Context) {
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
    }

    pub(super) fn logic_shared(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        // `frame` is always ROOT integration (eframe fork); safe for loader wgpu on aux paint.

        // Process IPC messages (needs to happen before minimized check to wake up immediately)
        let mut ipc_handled = false;
        while let Ok(msg) = self.ipc_rx.try_recv() {
            ipc_handled = true;
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
        if ipc_handled {
            ctx.request_repaint();
            self.wake_root_for_logic();
        }

        // Minimize-to-tray on close is handled in `raw_input_hook` (see eframe_app.rs):
        // the eframe fork runs `logic` before the frame's RawInput is applied to ctx.

        self.cache_directory_tree_viewport_placement(ctx);

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

        if let Some(dropped_file) = ctx.input(|i| i.raw.dropped_files.first().cloned())
            && let Some(path) = dropped_file.path
        {
            // Guard: don't re-trigger if we're already scanning from a previous drop
            if !self.scanning {
                if path.is_dir() {
                    // Dropped a directory — scan it (non-recursive to avoid surprises)
                    log::info!("Drop: opening directory {:?}", path);
                    self.settings.browse_mode = crate::settings::BrowseMode::Linear;
                    self.settings.show_directory_tree_nav = false;
                    self.settings.tree_nav_selected_dir = None;
                    self.settings.tree_nav_selected_namespace_path = None;
                    self.settings.recursive = false;
                    self.load_directory(path);
                    self.queue_save();
                } else if path.is_file() {
                    // Dropped a single file — check if it's a supported format
                    let is_supported = path
                        .extension()
                        .map(crate::scanner::is_supported_extension)
                        .unwrap_or(false);

                    if is_supported {
                        log::info!("Drop: opening file {:?}", path);
                        if let Some(parent) = path.parent() {
                            self.initial_image = Some(path.clone());
                            self.settings.browse_mode = crate::settings::BrowseMode::Linear;
                            self.settings.show_directory_tree_nav = false;
                            self.settings.tree_nav_selected_dir = None;
                            self.settings.tree_nav_selected_namespace_path = None;
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
            self.run_directory_tree_logic_updates(ctx);

            // Limit background processing while hidden
            self.process_music_scan_results(); // Allow music to start if scanning finishes

            // Keyboard runs from ROOT ui() only; logic() also runs on deferred child repaints
            // with the wrong pass input and would double-fire hotkeys if handled here too.
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
        if let Some((_, start)) = self.last_save_error
            && start.elapsed().as_secs() >= 5
        {
            self.last_save_error = None;
        }
        if let Some((_, start)) = self.last_hotkeys_save_error
            && start.elapsed().as_secs() >= 5
        {
            self.last_hotkeys_save_error = None;
        }
        if let Some((_, start)) = self.last_context_menu_save_error
            && start.elapsed().as_secs() >= 5
        {
            self.last_context_menu_save_error = None;
        }
        if let Some(start) = self.hotkeys_apply_success_at
            && start.elapsed().as_secs() >= 3
        {
            self.hotkeys_apply_success_at = None;
        }
        if let Some(start) = self.context_menu_apply_success_at
            && start.elapsed().as_secs() >= 3
        {
            self.context_menu_apply_success_at = None;
        }

        if scanning_at_frame_start != self.scanning {
            ctx.request_repaint();
        }
        if !self.scanning {
            // Upload deferred CPU pixels for the outgoing frame before navigation captures
            // `prev_texture` (preloaded neighbors often skip GPU upload until display).
            self.flush_deferred_sdr_upload_for_index(self.current_index, ctx);
            self.wake_root_for_logic();
        }
        self.process_music_scan_results();
        self.check_auto_switch(ctx);
        self.process_file_op_results();
        self.run_directory_tree_logic_updates(ctx);
        let had_tree_select = self.pending_directory_tree_select_index.is_some();
        self.process_pending_directory_tree_select(ctx);
        self.sync_directory_tree_file_list_state(ctx);
        self.process_pending_directory_tree_state_sync(ctx);
        if had_tree_select {
            self.run_directory_tree_logic_updates(ctx);
            self.sync_directory_tree_file_list_state(ctx);
        }

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

        self.poll_folder_picker_results(ctx);

        // Keep repainting while loading, auto-switching, playing music, or folder picker open
        let is_music_playing = self.settings.play_music && self.cached_music_count.unwrap_or(0) > 0;
        let awaiting_raw_hdr_present = self.raw_async_work_needs_repaint_wake();
        let animation_active = self.animation_needs_repaint_wake();
        let animation_upload_pending = self.animation_upload_pending_for_current();
        let loader_has_pending = self.loader.has_pending_outputs();
        let current_still_loading = self.loader.is_loading(self.current_index);
        if self.settings.auto_switch
            || self.scanning
            || loader_has_pending
            || current_still_loading
            || self.folder_picker.in_flight()
            || awaiting_raw_hdr_present
            || animation_upload_pending
        {
            ctx.request_repaint();
        } else if animation_active {
            ctx.request_repaint_after(
                self.next_animation_repaint_after()
                    .unwrap_or(Duration::from_millis(
                        DEFAULT_ANIMATION_DELAY_MS as u64,
                    )),
            );
        } else if is_music_playing {
            // Music only needs low-frequency polling for track-name updates (~2 fps)
            ctx.request_repaint_after(Duration::from_millis(500));
        } else {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    pub(super) fn logic_root_only(
        &mut self,
        ctx: &Context,
        frame: &mut eframe::Frame,
        pass: &eframe::LogicPass,
    ) {
        if !pass.is_root() {
            return;
        }
        debug_assert!(
            frame.is_root_painting(),
            "frame.painting_viewport_id should match ROOT paint"
        );
        self.ensure_root_redraw_wake(frame, ctx);
        if self.raw_async_work_needs_repaint_wake()
            || self.animation_upload_pending_for_current()
            || self.needs_process_loaded_images()
        {
            ctx.request_repaint();
            self.wake_root_for_logic();
        } else if self.animation_needs_repaint_wake() {
            ctx.request_repaint_after(
                self.next_animation_repaint_after()
                    .unwrap_or(Duration::from_millis(
                        DEFAULT_ANIMATION_DELAY_MS as u64,
                    )),
            );
            self.wake_root_for_logic();
        }
        // Cache window placement (outer position, inner size, maximized) so
        // `on_exit` can persist it without needing a `ctx`. egui exposes the
        // OS-level outer rect via `ViewportInfo::outer_rect`; on multi-monitor
        // systems this is what determines which monitor the next session
        // spawns onto, and therefore whether `Rgba16Float` or `Bgra8Unorm` is
        // selected for the swap chain.
        let minimized_or_hidden = self.hidden_to_tray
            || ctx.viewport_for(egui::ViewportId::ROOT, |viewport| {
                viewport.input.viewport().minimized.unwrap_or(false)
            });
        if !minimized_or_hidden
            && let Some((placement, is_fullscreen)) =
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

        // ── HDR / swap-chain target format (ROOT pass only) ───────────────
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

        let now = Instant::now();
        self.clear_frame_render_plan_cache();
        self.frame_effective_hdr_monitor_selection = self.effective_hdr_monitor_selection();
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
        let (output_mode, render_output_mode) = {
            let es = self.frame_effective_hdr_monitor_selection.as_ref();
            let om =
                crate::hdr::monitor::effective_capability_output_mode(self.hdr_target_format, es);
            let rom = crate::hdr::monitor::effective_render_output_mode(self.hdr_target_format, es);
            (om, rom)
        };
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
        // Re-borrow after potential &mut self calls above (e.g. sync_hdr_tone_map_settings).
        // Safe because `frame_effective_hdr_monitor_selection` is assigned once at the top of
        // this method (line 412) and never mutated afterward — the re-borrow sees the same
        // value as the first borrow at line 436.  HdrMonitorSelection carries a heap-allocated
        // String; re-borrowing avoids the per-frame clone.
        let effective_selection = self.frame_effective_hdr_monitor_selection.as_ref();

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
        let desired_target_format = crate::hdr::surface::desired_target_format_for_active_monitor(
            native_surface_requests_enabled,
            effective_selection,
        );
        if let Some(desired_format) = desired_target_format
            && Some(desired_format) != self.hdr_target_format
        {
            if self.last_logged_swap_chain_format_request != Some(desired_format) {
                log::info!(
                    "[HDR] runtime swap-chain format mismatch: current={:?} desired={:?} \
                     monitor={:?} hdr_supported={:?} native_surface_enabled={}",
                    self.hdr_target_format,
                    desired_format,
                    effective_selection.map(|s| s.label.as_str()),
                    effective_selection.map(|s| s.hdr_supported),
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
        // Cross-platform: keep loader/preload gate state aligned with the per-frame probe.
        // Startup `detect_from_wgpu_state` may leave `output_mode = SdrToneMapped` while the
        // live swap chain is still `Bgra8Unorm`; macOS has no spawn-time format probe
        // and relies on a runtime hot-swap to `Rgba16Float`.
        self.hdr_capabilities.output_mode = output_mode;
        self.hdr_capabilities.current_surface_format = self.hdr_target_format;
        #[cfg(target_os = "linux")]
        {
            let wsi = self.vulkan_wsi_hdr_gates.get();
            crate::hdr::linux_diag::log_runtime_if_changed(
                &mut self.last_logged_linux_hdr_runtime_diag,
                crate::hdr::linux_diag::LinuxHdrRuntimeDiagInput {
                    wp: self.hdr_monitor_state.selection(),
                    effective: effective_selection,
                    wsi: crate::hdr::wsi_probe::WsiHdrSurfaceGates {
                        hdr10_st2084_rgb10a2: wsi.hdr10_st2084_rgb10a2,
                        extended_srgb_linear_rgba16f: wsi.extended_srgb_linear_rgba16f,
                        srgb_nonlinear_rgb10a2: wsi.srgb_nonlinear_rgb10a2,
                        probed: wsi.probed,
                    },
                    settings_native_surface_enabled: self.settings.hdr_native_surface_enabled,
                    settings_native_surface_effective: self
                        .settings
                        .hdr_native_surface_enabled_effective(),
                    native_swapchain_requests_enabled: native_surface_requests_enabled,
                    target_format: self.hdr_target_format,
                    desired_target_format,
                    output_mode,
                    native_presentation_enabled: self.hdr_capabilities.native_presentation_enabled,
                },
            );
        }
        #[cfg(feature = "preload-debug")]
        {
            use crate::app::preload_hdr_gate::HdrSwapGateSnapshot;
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
            self.hdr_preload_gate_log
                .log_swap_chain_gate(HdrSwapGateSnapshot {
                    native_surface_requests_enabled,
                    settings_native_surface_effective: self
                        .settings
                        .hdr_native_surface_enabled_effective(),
                    settings_hdr_native_surface_enabled: self.settings.hdr_native_surface_enabled,
                    backend: self.hdr_capabilities.backend,
                    current_target_format: self.hdr_target_format,
                    desired_target_format,
                    swap_request_outcome,
                    wsi_probed: wsi.probed,
                    wsi_hdr10_st2084_rgb10a2: wsi.hdr10_st2084_rgb10a2,
                    wsi_extended_srgb_linear_rgba16f: wsi.extended_srgb_linear_rgba16f,
                    wp_selection_present: wp_selection.is_some(),
                    wp_hdr_supported: wp_selection.map(|s| s.hdr_supported),
                    effective_selection_present: effective_selection.is_some(),
                    effective_hdr_supported: effective_selection.map(|s| s.hdr_supported),
                    output_mode,
                    native_presentation_enabled: self.hdr_capabilities.native_presentation_enabled,
                });
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
            self.refresh_hdr_view_status();
        }
        self.refresh_ultra_hdr_decode_capacity(ctx);
        crate::loader::refresh_hq_preview_monitor_cap(ctx);
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
    }
}
