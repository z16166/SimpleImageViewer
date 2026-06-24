//! Win7 legacy GLES: runtime cascade across ANGLE (EGL) and WGL backends.

use core::ops::Deref;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GlBackendTier {
    AngleD3d11,
    AngleOpenGl,
    Wgl,
    AngleWarp,
}

impl GlBackendTier {
    fn log_label(self) -> &'static str {
        match self {
            Self::AngleD3d11 => "angle-d3d11",
            Self::AngleOpenGl => "angle-opengl",
            Self::Wgl => "wgl",
            Self::AngleWarp => "angle-warp",
        }
    }

    fn to_angle_tier(self) -> Option<super::egl::AngleTier> {
        match self {
            Self::AngleD3d11 => Some(super::egl::AngleTier::D3d11),
            Self::AngleOpenGl => Some(super::egl::AngleTier::OpenGl),
            Self::AngleWarp => Some(super::egl::AngleTier::Warp),
            Self::Wgl => None,
        }
    }
}

fn parse_wgpu_gl_backend() -> Option<GlBackendTier> {
    match std::env::var("WGPU_GL_BACKEND").ok().as_deref() {
        None | Some("") | Some("auto") => None,
        Some("angle-d3d11") => Some(GlBackendTier::AngleD3d11),
        Some("angle-opengl") => Some(GlBackendTier::AngleOpenGl),
        Some("wgl") => Some(GlBackendTier::Wgl),
        Some("angle-warp") => Some(GlBackendTier::AngleWarp),
        Some(other) => {
            log::warn!("unknown WGPU_GL_BACKEND={other:?}, using auto cascade");
            None
        }
    }
}

const AUTO_TIERS: [GlBackendTier; 4] = [
    GlBackendTier::AngleD3d11,
    GlBackendTier::AngleOpenGl,
    GlBackendTier::Wgl,
    GlBackendTier::AngleWarp,
];

pub enum AdapterContext {
    Egl(super::egl::AdapterContext),
    Wgl(super::wgl::AdapterContext),
}

unsafe impl Send for AdapterContext {}
unsafe impl Sync for AdapterContext {}

impl AdapterContext {
    pub fn is_owned(&self) -> bool {
        match self {
            Self::Egl(ctx) => ctx.is_owned(),
            Self::Wgl(ctx) => ctx.is_owned(),
        }
    }

    pub fn raw_context(&self) -> *mut core::ffi::c_void {
        match self {
            Self::Egl(ctx) => ctx.raw_context(),
            Self::Wgl(ctx) => ctx.raw_context(),
        }
    }

    pub fn lock(&self) -> AdapterContextLock<'_> {
        match self {
            Self::Egl(ctx) => AdapterContextLock::Egl(ctx.lock()),
            Self::Wgl(ctx) => AdapterContextLock::Wgl(ctx.lock()),
        }
    }

    pub fn lock_with_dc(
        &self,
        device: windows::Win32::Graphics::Gdi::HDC,
    ) -> windows::core::Result<AdapterContextLock<'_>> {
        match self {
            Self::Egl(ctx) => Ok(AdapterContextLock::Egl(ctx.lock())),
            Self::Wgl(ctx) => Ok(AdapterContextLock::Wgl(ctx.lock_with_dc(device)?)),
        }
    }
}

pub enum AdapterContextLock<'a> {
    Egl(super::egl::AdapterContextLock<'a>),
    Wgl(super::wgl::AdapterContextLock<'a>),
}

impl Deref for AdapterContextLock<'_> {
    type Target = glow::Context;

    fn deref(&self) -> &Self::Target {
        match self {
            Self::Egl(lock) => lock.deref(),
            Self::Wgl(lock) => lock.deref(),
        }
    }
}

pub enum Instance {
    Egl(super::egl::Instance),
    Wgl(super::wgl::Instance),
}

unsafe impl Send for Instance {}
unsafe impl Sync for Instance {}

impl crate::Instance for Instance {
    type A = super::Api;

    unsafe fn init(desc: &crate::InstanceDescriptor<'_>) -> Result<Self, crate::InstanceError> {
        profiling::scope!("Init OpenGL (Win7 GLES cascade)");
        let forced = parse_wgpu_gl_backend();
        let mut last_err: Option<crate::InstanceError> = None;

        let try_tiers: [GlBackendTier; 4] = match forced {
            Some(tier) => [tier; 4],
            None => AUTO_TIERS,
        };
        let tier_count = if forced.is_some() { 1 } else { AUTO_TIERS.len() };

        for tier in &try_tiers[..tier_count] {
            match tier {
                GlBackendTier::Wgl => match unsafe { super::wgl::Instance::init_wgl(desc) } {
                    Ok(instance) => return Ok(Self::Wgl(instance)),
                    Err(e) => {
                        log::debug!("Win7 GLES tier wgl failed: {e:?}");
                        last_err = Some(e);
                    }
                },
                _ => {
                    let angle_tier = tier.to_angle_tier().expect("non-WGL tier maps to ANGLE");
                    match super::egl::init_angle_instance(desc, angle_tier) {
                        Ok(instance) => return Ok(Self::Egl(instance)),
                        Err(e) => {
                            log::debug!(
                                "Win7 GLES tier {} failed: {e:?}",
                                tier.log_label()
                            );
                            last_err = Some(e);
                        }
                    }
                }
            }
        }

        Err(last_err.unwrap_or_else(|| {
            crate::InstanceError::new(
                "Win7 GLES: no backend tier could be initialized (tried angle-d3d11, angle-opengl, wgl, angle-warp)"
                    .into(),
            )
        }))
        .inspect_err(|e| {
            log::error!("Win7 GLES init failed: {e:?}");
        })
    }

    unsafe fn create_surface(
        &self,
        display_handle: raw_window_handle::RawDisplayHandle,
        window_handle: raw_window_handle::RawWindowHandle,
    ) -> Result<Surface, crate::InstanceError> {
        match self {
            Self::Egl(instance) => unsafe {
                instance
                    .create_surface_inner(display_handle, window_handle)
                    .map(Surface::Egl)
            },
            Self::Wgl(instance) => unsafe {
                instance
                    .create_surface_inner(display_handle, window_handle)
                    .map(Surface::Wgl)
            },
        }
    }

    unsafe fn enumerate_adapters(
        &self,
        surface_hint: Option<&Surface>,
    ) -> alloc::vec::Vec<crate::ExposedAdapter<super::Api>> {
        match (self, surface_hint) {
            (Self::Egl(instance), None) => {
                unsafe { instance.enumerate_adapters_inner(None) }
            }
            (Self::Egl(instance), Some(Surface::Egl(surface))) => {
                unsafe { instance.enumerate_adapters_inner(Some(surface)) }
            }
            (Self::Wgl(instance), None) => {
                unsafe { instance.enumerate_adapters_inner(None) }
            }
            (Self::Wgl(instance), Some(Surface::Wgl(surface))) => {
                unsafe { instance.enumerate_adapters_inner(Some(surface)) }
            }
            (Self::Egl(_), Some(Surface::Wgl(_))) | (Self::Wgl(_), Some(Surface::Egl(_))) => {
                log::error!("Win7 GLES surface hint backend mismatch");
                alloc::vec::Vec::new()
            }
        }
    }
}

pub enum Surface {
    Egl(super::egl::Surface),
    Wgl(super::wgl::Surface),
}

unsafe impl Send for Surface {}
unsafe impl Sync for Surface {}

impl Surface {
    pub(super) fn is_presentable(&self) -> bool {
        match self {
            Self::Egl(surface) => surface.is_presentable(),
            Self::Wgl(surface) => surface.is_presentable(),
        }
    }

    pub fn supports_srgb(&self) -> bool {
        match self {
            Self::Egl(surface) => surface.supports_srgb(),
            Self::Wgl(surface) => surface.supports_srgb(),
        }
    }

    pub(super) unsafe fn present(
        &self,
        suf_texture: super::Texture,
        context: &AdapterContext,
    ) -> Result<(), crate::SurfaceError> {
        match (self, context) {
            (Self::Egl(surface), AdapterContext::Egl(ctx)) => {
                unsafe { surface.present(suf_texture, ctx) }
            }
            (Self::Wgl(surface), AdapterContext::Wgl(ctx)) => {
                unsafe { surface.present(suf_texture, ctx) }
            }
            _ => Err(crate::SurfaceError::Other(
                "Win7 GLES surface/context backend mismatch",
            )),
        }
    }
}

impl crate::Surface for Surface {
    type A = super::Api;

    unsafe fn configure(
        &self,
        device: &super::Device,
        config: &crate::SurfaceConfiguration,
    ) -> Result<(), crate::SurfaceError> {
        match self {
            Self::Egl(surface) => unsafe { surface.hal_configure(device, config) },
            Self::Wgl(surface) => unsafe { surface.hal_configure(device, config) },
        }
    }

    unsafe fn unconfigure(&self, device: &super::Device) {
        match self {
            Self::Egl(surface) => unsafe { surface.hal_unconfigure(device) },
            Self::Wgl(surface) => unsafe { surface.hal_unconfigure(device) },
        }
    }

    unsafe fn acquire_texture(
        &self,
        timeout: Option<core::time::Duration>,
        fence: &super::Fence,
    ) -> Result<crate::AcquiredSurfaceTexture<super::Api>, crate::SurfaceError> {
        match self {
            Self::Egl(surface) => unsafe { surface.hal_acquire_texture(timeout, fence) },
            Self::Wgl(surface) => unsafe { surface.hal_acquire_texture(timeout, fence) },
        }
    }

    unsafe fn discard_texture(&self, texture: super::Texture) {
        match self {
            Self::Egl(surface) => unsafe { surface.hal_discard_texture(texture) },
            Self::Wgl(surface) => unsafe { surface.hal_discard_texture(texture) },
        }
    }
}
