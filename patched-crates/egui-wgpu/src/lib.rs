//! This crates provides bindings between [`egui`](https://github.com/emilk/egui) and [wgpu](https://crates.io/crates/wgpu).
//!
//! If you're targeting WebGL you also need to turn on the
//! `webgl` feature of the `wgpu` crate:
//!
//! ```toml
//! # Enable both WebGL and WebGPU backends on web.
//! wgpu = { version = "*", features = ["webgpu", "webgl"] }
//! ```
//!
//! You can control whether WebGL or WebGPU will be picked at runtime by configuring
//! [`WgpuConfiguration::wgpu_setup`].
//! The default is to prefer WebGPU and fall back on WebGL.
//!
//! ## Feature flags
#![doc = document_features::document_features!()]
//!

pub use wgpu;

/// Low-level painting of [`egui`](https://github.com/emilk/egui) on [`wgpu`].
mod renderer;

mod setup;

pub use renderer::*;
pub use setup::{
    EguiDisplayHandle, NativeAdapterSelectorMethod, WgpuSetup, WgpuSetupCreateNew,
    WgpuSetupExisting,
};

/// Helpers for capturing screenshots of the UI.
#[cfg(feature = "capture")]
pub mod capture;

/// Module for painting [`egui`](https://github.com/emilk/egui) with [`wgpu`] on [`winit`].
#[cfg(feature = "winit")]
pub mod winit;

use std::sync::Arc;

use epaint::mutex::RwLock;

/// An error produced by egui-wgpu.
#[derive(thiserror::Error, Debug)]
pub enum WgpuError {
    #[error(transparent)]
    RequestAdapterError(#[from] wgpu::RequestAdapterError),

    #[error("Adapter selection failed: {0}")]
    CustomNativeAdapterSelectionError(String),

    #[error("There was no valid format for the surface at all.")]
    NoSurfaceFormatsAvailable,

    #[error(transparent)]
    RequestDeviceError(#[from] wgpu::RequestDeviceError),

    #[error(transparent)]
    CreateSurfaceError(#[from] wgpu::CreateSurfaceError),

    #[cfg(feature = "winit")]
    #[error(transparent)]
    HandleError(#[from] ::winit::raw_window_handle::HandleError),
}

/// Access to the render state for egui.
#[derive(Clone)]
pub struct RenderState {
    /// Wgpu adapter used for rendering.
    pub adapter: wgpu::Adapter,

    /// All the available adapters.
    ///
    /// This is not available on web.
    /// On web, we always select WebGPU is available, then fall back to WebGL if not.
    #[cfg(not(target_arch = "wasm32"))]
    pub available_adapters: Vec<wgpu::Adapter>,

    /// Wgpu device used for rendering, created from the adapter.
    pub device: wgpu::Device,

    /// Wgpu queue used for rendering, created from the adapter.
    pub queue: wgpu::Queue,

    /// The target texture format used for presenting to the window.
    pub target_format: wgpu::TextureFormat,

    /// Egui renderer responsible for drawing the UI.
    pub renderer: Arc<RwLock<Renderer>>,
}

async fn request_adapter(
    instance: &wgpu::Instance,
    power_preference: wgpu::PowerPreference,
    compatible_surface: Option<&wgpu::Surface<'_>>,
    available_adapters: &[wgpu::Adapter],
) -> Result<wgpu::Adapter, WgpuError> {
    profiling::function_scope!();

    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference,
            compatible_surface,
            // We don't expose this as an option right now since it's fairly rarely useful:
            // * only has an effect on native
            // * fails if there's no software rasterizer available
            // * can achieve the same with `native_adapter_selector`
            force_fallback_adapter: false,
        })
        .await
        .inspect_err(|_err| {
            if cfg!(target_arch = "wasm32") {
                // Nothing to add here
            } else if available_adapters.is_empty() {
                if std::env::var("DYLD_LIBRARY_PATH").is_ok() {
                    // DYLD_LIBRARY_PATH can sometimes lead to loading dylibs that cause
                    // us to find zero adapters. Very strange.
                    // I don't want to debug this again.
                    // See https://github.com/rerun-io/rerun/issues/11351 for more
                    log::warn!(
                        "No wgpu adapter found. This could be because DYLD_LIBRARY_PATH causes dylibs to be loaded that interfere with Metal device creation. Try restarting with DYLD_LIBRARY_PATH=''"
                    );
                } else {
                    log::info!("No wgpu adapter found");
                }
            } else if available_adapters.len() == 1 {
                log::info!(
                    "The only available wgpu adapter was not suitable: {}",
                    adapter_info_summary(&available_adapters[0].get_info())
                );
            } else {
                log::info!(
                    "No suitable wgpu adapter found out of the {} available ones: {}",
                    available_adapters.len(),
                    describe_adapters(available_adapters)
                );
            }
        })?;

    if cfg!(target_arch = "wasm32") {
        log::debug!(
            "Picked wgpu adapter: {}",
            adapter_info_summary(&adapter.get_info())
        );
    } else {
        // native:
        if available_adapters.len() == 1 {
            log::debug!(
                "Picked the only available wgpu adapter: {}",
                adapter_info_summary(&adapter.get_info())
            );
        } else {
            log::info!(
                "There were {} available wgpu adapters: {}",
                available_adapters.len(),
                describe_adapters(available_adapters)
            );
            log::debug!(
                "Picked wgpu adapter: {}",
                adapter_info_summary(&adapter.get_info())
            );
        }
    }

    Ok(adapter)
}

impl RenderState {
    /// Creates a new [`RenderState`], containing everything needed for drawing egui with wgpu.
    ///
    /// # Errors
    /// Wgpu initialization may fail due to incompatible hardware or driver for a given config.
    pub async fn create(
        config: &WgpuConfiguration,
        instance: &wgpu::Instance,
        compatible_surface: Option<&wgpu::Surface<'static>>,
        options: RendererOptions,
    ) -> Result<Self, WgpuError> {
        profiling::scope!("RenderState::create"); // async yield give bad names using `profile_function`

        // This is always an empty list on web.
        #[cfg(not(target_arch = "wasm32"))]
        let available_adapters = {
            let backends = if let WgpuSetup::CreateNew(create_new) = &config.wgpu_setup {
                create_new.instance_descriptor.backends
            } else {
                wgpu::Backends::all()
            };

            instance.enumerate_adapters(backends).await
        };

        let (adapter, device, queue) = match config.wgpu_setup.clone() {
            WgpuSetup::CreateNew(WgpuSetupCreateNew {
                instance_descriptor: _,
                display_handle: _,
                power_preference,
                native_adapter_selector: _native_adapter_selector,
                device_descriptor,
            }) => {
                let adapter = {
                    #[cfg(target_arch = "wasm32")]
                    {
                        request_adapter(instance, power_preference, compatible_surface, &[]).await
                    }
                    #[cfg(not(target_arch = "wasm32"))]
                    if let Some(native_adapter_selector) = _native_adapter_selector {
                        native_adapter_selector(&available_adapters, compatible_surface)
                            .map_err(WgpuError::CustomNativeAdapterSelectionError)
                    } else {
                        request_adapter(
                            instance,
                            power_preference,
                            compatible_surface,
                            &available_adapters,
                        )
                        .await
                    }
                }?;

                let (device, queue) = {
                    profiling::scope!("request_device");
                    adapter
                        .request_device(&(*device_descriptor)(&adapter))
                        .await?
                };

                (adapter, device, queue)
            }
            WgpuSetup::Existing(WgpuSetupExisting {
                instance: _,
                adapter,
                device,
                queue,
            }) => (adapter, device, queue),
        };

        let surface_formats = {
            profiling::scope!("get_capabilities");
            compatible_surface.map_or_else(
                || vec![wgpu::TextureFormat::Rgba8Unorm],
                |s| s.get_capabilities(&adapter).formats,
            )
        };
        let target_format = crate::preferred_framebuffer_format_with_preference(
            &surface_formats,
            config.preferred_target_format,
        )?;

        let renderer = Renderer::new(&device, target_format, options);

        // On wasm, depending on feature flags, wgpu objects may or may not implement sync.
        // It doesn't make sense to switch to Rc for that special usecase, so simply disable the lint.
        #[allow(clippy::allow_attributes, clippy::arc_with_non_send_sync)] // For wasm
        Ok(Self {
            adapter,
            #[cfg(not(target_arch = "wasm32"))]
            available_adapters,
            device,
            queue,
            target_format,
            renderer: Arc::new(RwLock::new(renderer)),
        })
    }
}

fn describe_adapters(adapters: &[wgpu::Adapter]) -> String {
    if adapters.is_empty() {
        "(none)".to_owned()
    } else if adapters.len() == 1 {
        adapter_info_summary(&adapters[0].get_info())
    } else {
        adapters
            .iter()
            .map(|a| format!("{{{}}}", adapter_info_summary(&a.get_info())))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Specifies which action should be taken as consequence of a surface error.
pub enum SurfaceErrorAction {
    /// Do nothing and skip the current frame.
    SkipFrame,

    /// Instructs egui to recreate the surface, then skip the current frame.
    RecreateSurface,
}

/// Shared mailbox used by the application to request that the egui-wgpu
/// `Painter` change its surface (swap-chain) target format **at runtime**.
///
/// This is a downstream patch on top of upstream egui 0.34: vanilla egui-wgpu
/// only honours [`WgpuConfiguration::preferred_target_format`] at startup and
/// locks the swap-chain into that format for the lifetime of the
/// `RenderState`. We need the format to follow the active monitor's HDR
/// capability (`Rgba16Float` on HDR, `Bgra8Unorm` on SDR) when the user
/// drags the window across monitors, so the [`crate::winit::Painter`] checks
/// this mailbox at the start of every `paint_and_update_textures` call and,
/// if a different but surface-supported format has been requested, hot-swaps
/// the egui pipeline format and triggers a swap-chain reconfigure.
///
/// `None` means "do not request a format change". `Some(format)` is consumed
/// (taken out) by the painter once the change has been applied.
#[derive(Clone, Default)]
pub struct RequestedSurfaceFormat {
    inner: Arc<std::sync::Mutex<Option<wgpu::TextureFormat>>>,
}

impl std::fmt::Debug for RequestedSurfaceFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestedSurfaceFormat")
            .field("pending", &self.peek())
            .finish()
    }
}

impl RequestedSurfaceFormat {
    /// Create a new mailbox with no pending request.
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask the painter to switch to `format` on its next paint. Replaces any
    /// previous pending request (we only ever care about the latest one).
    pub fn request(&self, format: wgpu::TextureFormat) {
        if let Ok(mut slot) = self.inner.lock() {
            *slot = Some(format);
        }
    }

    /// Clear any pending request.
    pub fn clear(&self) {
        if let Ok(mut slot) = self.inner.lock() {
            *slot = None;
        }
    }

    /// Peek the current request without consuming it. Used by tests and
    /// diagnostics.
    pub fn peek(&self) -> Option<wgpu::TextureFormat> {
        self.inner.lock().ok().and_then(|s| *s)
    }

    /// Consume any pending request. Returns `Some(format)` exactly once per
    /// `request()` call.
    pub fn take(&self) -> Option<wgpu::TextureFormat> {
        self.inner.lock().ok().and_then(|mut s| s.take())
    }
}

/// Reverse-direction mailbox that the painter uses to publish the **current
/// active** swap-chain target format back to the application.
///
/// This exists because vanilla [`RenderState`] derives `Clone`, and eframe
/// stores a `RenderState` clone in `Frame` (see `wgpu_integration.rs`:
/// `wgpu_render_state.clone()`). Mutating `painter.render_state.target_format`
/// in [`crate::winit::Painter::try_apply_runtime_target_format_switch`]
/// updates only the painter's copy — `frame.wgpu_render_state().target_format`
/// in the application keeps returning the original startup format forever,
/// which made the OSD / shader-mode logic believe the swap chain was still
/// `Rgba16Float` even after a runtime hot-swap to `Bgra8Unorm` (and vice
/// versa).
///
/// The contract is the dual of [`RequestedSurfaceFormat`]:
/// * The painter `set(...)`s the new active format every time it successfully
///   applies a runtime swap.
/// * The application `get()`s the latest published format every frame.
/// * `None` means the painter hasn't published anything yet; callers should
///   fall back to `RenderState::target_format` for the initial format.
#[derive(Clone, Default)]
pub struct ActiveSurfaceFormat {
    inner: Arc<std::sync::Mutex<Option<wgpu::TextureFormat>>>,
}

impl std::fmt::Debug for ActiveSurfaceFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActiveSurfaceFormat")
            .field("active", &self.get())
            .finish()
    }
}

impl ActiveSurfaceFormat {
    /// Create a new mailbox with no published format.
    pub fn new() -> Self {
        Self::default()
    }

    /// Painter-side: publish the format that the swap chain is now using.
    pub fn set(&self, format: wgpu::TextureFormat) {
        if let Ok(mut slot) = self.inner.lock() {
            *slot = Some(format);
        }
    }

    /// Application-side: read the latest published format. Returns `None`
    /// until the painter has published at least once.
    pub fn get(&self) -> Option<wgpu::TextureFormat> {
        self.inner.lock().ok().and_then(|s| *s)
    }
}

/// Configuration for using wgpu with eframe or the egui-wgpu winit feature.
#[derive(Clone)]
pub struct WgpuConfiguration {
    /// Present mode used for the primary surface.
    pub present_mode: wgpu::PresentMode,

    /// Desired maximum number of frames that the presentation engine should queue in advance.
    ///
    /// Use `1` for low-latency, and `2` for high-throughput.
    ///
    /// See [`wgpu::SurfaceConfiguration::desired_maximum_frame_latency`] for details.
    ///
    /// `None` = `wgpu` default.
    pub desired_maximum_frame_latency: Option<u32>,

    /// Preferred surface format for the primary surface, if supported.
    ///
    /// This is useful for applications that need a non-default presentation
    /// format, such as an HDR float swapchain. If the requested format is not
    /// exposed by the surface, egui-wgpu falls back to its standard SDR-safe
    /// format preference.
    pub preferred_target_format: Option<wgpu::TextureFormat>,

    /// Mailbox the application can use to ask the painter to swap to a
    /// different surface target format **at runtime** (e.g. when the window
    /// is dragged from an SDR monitor onto an HDR monitor or vice versa).
    ///
    /// The painter polls this on every frame. See [`RequestedSurfaceFormat`]
    /// for the full contract. When unused (`Default::default()` / not
    /// written to), the painter behaves identically to upstream egui-wgpu.
    pub requested_target_format: RequestedSurfaceFormat,

    /// Reverse-direction observer the painter writes the **current active**
    /// swap-chain target format into after every successful runtime hot-swap.
    ///
    /// Required because `RenderState` derives `Clone` and eframe stores a
    /// clone in `Frame`, so mutating the painter's `RenderState.target_format`
    /// is not visible through `frame.wgpu_render_state().target_format`. The
    /// application must read the live format from this mailbox to keep its
    /// OSD / shader-mode state in sync after cross-monitor drags.
    /// See [`ActiveSurfaceFormat`] for the contract.
    pub active_target_format: ActiveSurfaceFormat,

    /// How to create the wgpu adapter & device
    pub wgpu_setup: WgpuSetup,

    /// Callback for surface status changes.
    ///
    /// Called with the [`wgpu::CurrentSurfaceTexture`] result whenever acquiring a frame
    /// does not return [`wgpu::CurrentSurfaceTexture::Success`]. For
    /// [`wgpu::CurrentSurfaceTexture::Suboptimal`], egui uses the frame as-is and
    /// defers surface reconfiguration to the next frame — the callback is not invoked
    /// in that case either.
    pub on_surface_status:
        Arc<dyn Fn(&wgpu::CurrentSurfaceTexture) -> SurfaceErrorAction + Send + Sync>,
}

#[test]
fn wgpu_config_impl_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<WgpuConfiguration>();
}

#[cfg(test)]
mod requested_surface_format_tests {
    use super::RequestedSurfaceFormat;

    #[test]
    fn defaults_to_no_pending_request() {
        let mailbox = RequestedSurfaceFormat::new();
        assert_eq!(mailbox.peek(), None);
        assert_eq!(mailbox.take(), None);
    }

    #[test]
    fn request_writes_a_pending_format_that_take_returns_exactly_once() {
        let mailbox = RequestedSurfaceFormat::new();
        mailbox.request(wgpu::TextureFormat::Rgba16Float);
        assert_eq!(mailbox.peek(), Some(wgpu::TextureFormat::Rgba16Float));
        assert_eq!(mailbox.take(), Some(wgpu::TextureFormat::Rgba16Float));
        assert_eq!(
            mailbox.take(),
            None,
            "subsequent takes must yield None until the next request()"
        );
    }

    #[test]
    fn last_request_overwrites_a_pending_one() {
        let mailbox = RequestedSurfaceFormat::new();
        mailbox.request(wgpu::TextureFormat::Rgba16Float);
        mailbox.request(wgpu::TextureFormat::Bgra8Unorm);
        assert_eq!(
            mailbox.take(),
            Some(wgpu::TextureFormat::Bgra8Unorm),
            "only the most-recent request matters; we never queue stale formats"
        );
    }

    #[test]
    fn clear_drops_a_pending_request() {
        let mailbox = RequestedSurfaceFormat::new();
        mailbox.request(wgpu::TextureFormat::Rgba16Float);
        mailbox.clear();
        assert_eq!(mailbox.take(), None);
    }

    #[test]
    fn clones_share_the_same_mailbox() {
        // The painter and the application hold separate clones; writes from
        // one side must be visible to the other so the painter actually picks
        // up monitor-change requests.
        let writer = RequestedSurfaceFormat::new();
        let reader = writer.clone();
        writer.request(wgpu::TextureFormat::Rgba16Float);
        assert_eq!(reader.take(), Some(wgpu::TextureFormat::Rgba16Float));
        assert_eq!(writer.peek(), None);
    }

    #[test]
    fn requested_surface_format_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<RequestedSurfaceFormat>();
    }
}

#[cfg(test)]
mod active_surface_format_tests {
    use super::ActiveSurfaceFormat;

    #[test]
    fn defaults_to_no_published_format() {
        let mailbox = ActiveSurfaceFormat::new();
        assert_eq!(
            mailbox.get(),
            None,
            "before the painter has hot-swapped, the mailbox is empty and \
             the application should fall back to RenderState::target_format"
        );
    }

    #[test]
    fn set_publishes_a_format_that_get_returns_repeatedly() {
        // Regression: this is the dual of `RequestedSurfaceFormat::take` —
        // unlike a request mailbox, the active-format mailbox must keep
        // returning the latest format on every read so the application's
        // per-frame `update()` can poll it without losing the value.
        let mailbox = ActiveSurfaceFormat::new();
        mailbox.set(wgpu::TextureFormat::Rgba16Float);
        for _ in 0..3 {
            assert_eq!(mailbox.get(), Some(wgpu::TextureFormat::Rgba16Float));
        }
    }

    #[test]
    fn last_set_overwrites_a_previously_published_one() {
        let mailbox = ActiveSurfaceFormat::new();
        mailbox.set(wgpu::TextureFormat::Rgba16Float);
        mailbox.set(wgpu::TextureFormat::Bgra8Unorm);
        assert_eq!(
            mailbox.get(),
            Some(wgpu::TextureFormat::Bgra8Unorm),
            "the mailbox always reflects the most-recent painter swap"
        );
    }

    #[test]
    fn clones_share_the_same_mailbox() {
        // Critical: the painter and the application hold separate clones —
        // `WgpuConfiguration` is itself `Clone` and `main.rs` clones the
        // mailbox into both the configuration handed to eframe AND the
        // application struct. Painter writes must be visible to the
        // application reader, otherwise the OSD never updates.
        let painter_side = ActiveSurfaceFormat::new();
        let app_side = painter_side.clone();
        painter_side.set(wgpu::TextureFormat::Bgra8Unorm);
        assert_eq!(app_side.get(), Some(wgpu::TextureFormat::Bgra8Unorm));
    }

    #[test]
    fn active_surface_format_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ActiveSurfaceFormat>();
    }
}

impl std::fmt::Debug for WgpuConfiguration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self {
            present_mode,
            desired_maximum_frame_latency,
            preferred_target_format,
            requested_target_format,
            active_target_format,
            wgpu_setup,
            on_surface_status: _,
        } = self;
        f.debug_struct("WgpuConfiguration")
            .field("present_mode", &present_mode)
            .field(
                "desired_maximum_frame_latency",
                &desired_maximum_frame_latency,
            )
            .field("wgpu_setup", &wgpu_setup)
            .field("preferred_target_format", &preferred_target_format)
            .field("requested_target_format", &requested_target_format)
            .field("active_target_format", &active_target_format)
            .finish_non_exhaustive()
    }
}

impl Default for WgpuConfiguration {
    fn default() -> Self {
        Self {
            present_mode: wgpu::PresentMode::AutoVsync,
            desired_maximum_frame_latency: None,
            preferred_target_format: None,
            requested_target_format: RequestedSurfaceFormat::new(),
            active_target_format: ActiveSurfaceFormat::new(),
            // No display handle available at this point — callers should replace this with
            // `WgpuSetup::from_display_handle(...)` before creating the instance if one is available.
            wgpu_setup: WgpuSetup::without_display_handle(),
            on_surface_status: Arc::new(|status| {
                match status {
                    wgpu::CurrentSurfaceTexture::Outdated => {
                        // This error occurs when the app is minimized on Windows.
                        // Silently return here to prevent spamming the console with:
                        // "The underlying surface has changed, and therefore the swap chain must be updated"
                    }
                    wgpu::CurrentSurfaceTexture::Occluded => {
                        // This error occurs when the application is occluded (e.g. minimized or behind another window).
                        log::debug!("Dropped frame with error: {status:?}");
                    }
                    _ => {
                        log::warn!("Dropped frame with error: {status:?}");
                    }
                }

                SurfaceErrorAction::SkipFrame
            }),
        }
    }
}

/// Find the framebuffer format that egui prefers
///
/// # Errors
/// Returns [`WgpuError::NoSurfaceFormatsAvailable`] if the given list of formats is empty.
pub fn preferred_framebuffer_format(
    formats: &[wgpu::TextureFormat],
) -> Result<wgpu::TextureFormat, WgpuError> {
    preferred_framebuffer_format_with_preference(formats, None)
}

/// Find the framebuffer format that egui prefers, with an optional caller preference.
///
/// The caller preference only wins if the surface reports it as supported.
pub fn preferred_framebuffer_format_with_preference(
    formats: &[wgpu::TextureFormat],
    preferred_format: Option<wgpu::TextureFormat>,
) -> Result<wgpu::TextureFormat, WgpuError> {
    if let Some(preferred_format) = preferred_format {
        if formats.contains(&preferred_format) {
            return Ok(preferred_format);
        }
    }

    for &format in formats {
        if matches!(
            format,
            wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Bgra8Unorm
        ) {
            return Ok(format);
        }
    }

    formats
        .first()
        .copied()
        .ok_or(WgpuError::NoSurfaceFormatsAvailable)
}

#[cfg(test)]
mod preferred_framebuffer_format_tests {
    use super::*;

    #[test]
    fn preferred_target_format_wins_when_supported() {
        let format = preferred_framebuffer_format_with_preference(
            &[
                wgpu::TextureFormat::Bgra8Unorm,
                wgpu::TextureFormat::Rgba16Float,
            ],
            Some(wgpu::TextureFormat::Rgba16Float),
        )
        .unwrap();

        assert_eq!(format, wgpu::TextureFormat::Rgba16Float);
    }

    #[test]
    fn unsupported_preferred_target_format_falls_back_to_egui_default() {
        let format = preferred_framebuffer_format_with_preference(
            &[
                wgpu::TextureFormat::Bgra8Unorm,
                wgpu::TextureFormat::Rgba8Unorm,
            ],
            Some(wgpu::TextureFormat::Rgba16Float),
        )
        .unwrap();

        assert_eq!(format, wgpu::TextureFormat::Bgra8Unorm);
    }
}

/// Take's epi's depth/stencil bits and returns the corresponding wgpu format.
pub fn depth_format_from_bits(depth_buffer: u8, stencil_buffer: u8) -> Option<wgpu::TextureFormat> {
    match (depth_buffer, stencil_buffer) {
        (0, 8) => Some(wgpu::TextureFormat::Stencil8),
        (16, 0) => Some(wgpu::TextureFormat::Depth16Unorm),
        (24, 0) => Some(wgpu::TextureFormat::Depth24Plus),
        (24, 8) => Some(wgpu::TextureFormat::Depth24PlusStencil8),
        (32, 0) => Some(wgpu::TextureFormat::Depth32Float),
        (32, 8) => Some(wgpu::TextureFormat::Depth32FloatStencil8),
        _ => None,
    }
}

// ---------------------------------------------------------------------------

/// A human-readable summary about an adapter
pub fn adapter_info_summary(info: &wgpu::AdapterInfo) -> String {
    let wgpu::AdapterInfo {
        name,
        vendor,
        device,
        device_type,
        driver,
        driver_info,
        backend,
        device_pci_bus_id,
        subgroup_min_size,
        subgroup_max_size,
        transient_saves_memory,
    } = &info;

    // Example values:
    // > name: "llvmpipe (LLVM 16.0.6, 256 bits)", device_type: Cpu, backend: Vulkan, driver: "llvmpipe", driver_info: "Mesa 23.1.6-arch1.4 (LLVM 16.0.6)"
    // > name: "Apple M1 Pro", device_type: IntegratedGpu, backend: Metal, driver: "", driver_info: ""
    // > name: "ANGLE (Apple, Apple M1 Pro, OpenGL 4.1)", device_type: IntegratedGpu, backend: Gl, driver: "", driver_info: ""

    let mut summary = format!("backend: {backend:?}, device_type: {device_type:?}");

    if !name.is_empty() {
        summary += &format!(", name: {name:?}");
    }
    if !driver.is_empty() {
        summary += &format!(", driver: {driver:?}");
    }
    if !driver_info.is_empty() {
        summary += &format!(", driver_info: {driver_info:?}");
    }
    if *vendor != 0 {
        #[cfg(not(target_arch = "wasm32"))]
        {
            summary += &format!(", vendor: {} (0x{vendor:04X})", parse_vendor_id(*vendor));
        }
        #[cfg(target_arch = "wasm32")]
        {
            summary += &format!(", vendor: 0x{vendor:04X}");
        }
    }
    if *device != 0 {
        summary += &format!(", device: 0x{device:02X}");
    }
    if !device_pci_bus_id.is_empty() {
        summary += &format!(", pci_bus_id: {device_pci_bus_id:?}");
    }
    if *subgroup_min_size != 0 || *subgroup_max_size != 0 {
        summary += &format!(", subgroup_size: {subgroup_min_size}..={subgroup_max_size}");
    }
    summary += &format!(", transient_saves_memory: {transient_saves_memory}");

    summary
}

/// Tries to parse the adapter's vendor ID to a human-readable string.
#[cfg(not(target_arch = "wasm32"))]
pub fn parse_vendor_id(vendor_id: u32) -> &'static str {
    match vendor_id {
        wgpu::hal::auxil::db::amd::VENDOR => "AMD",
        wgpu::hal::auxil::db::apple::VENDOR => "Apple",
        wgpu::hal::auxil::db::arm::VENDOR => "ARM",
        wgpu::hal::auxil::db::broadcom::VENDOR => "Broadcom",
        wgpu::hal::auxil::db::imgtec::VENDOR => "Imagination Technologies",
        wgpu::hal::auxil::db::intel::VENDOR => "Intel",
        wgpu::hal::auxil::db::mesa::VENDOR => "Mesa",
        wgpu::hal::auxil::db::nvidia::VENDOR => "NVIDIA",
        wgpu::hal::auxil::db::qualcomm::VENDOR => "Qualcomm",
        _ => "Unknown",
    }
}
