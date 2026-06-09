// along with this program.  If not, see <https://www.gnu.org/licenses/>.
use super::types::HdrMonitorSelection;
use super::windows::windows_active_monitor_hdr_status;
#[cfg(target_os = "macos")]
use super::macos::macos_active_monitor_hdr_status;
#[cfg(target_os = "linux")]
use super::wayland;
use crate::hdr::renderer::HdrRenderOutputMode;
use crate::hdr::types::HdrOutputMode;

pub fn effective_render_output_mode(
    target_format: Option<wgpu::TextureFormat>,
    selection: Option<&HdrMonitorSelection>,
) -> HdrRenderOutputMode {
    let Some(target_format) = target_format else {
        return HdrRenderOutputMode::SdrToneMapped;
    };
    // Conservative fail-closed gate: only enable native scRGB / EDR presentation when we
    // have an explicit, positive confirmation that the active monitor supports HDR. When
    // the probe has not yet completed, failed silently, or reports `hdr_supported = false`
    // (e.g. Windows Settings says "不支持" / "not supported"), composit through the SDR
    // tone-mapped path so γ encoding for the actual SDR panel is correct.
    let Some(selection) = selection else {
        return HdrRenderOutputMode::SdrToneMapped;
    };
    if !selection.hdr_supported {
        return HdrRenderOutputMode::SdrToneMapped;
    }
    HdrRenderOutputMode::for_target_format(target_format, selection.native_surface_encoding)
}

pub fn effective_capability_output_mode(
    target_format: Option<wgpu::TextureFormat>,
    selection: Option<&HdrMonitorSelection>,
) -> HdrOutputMode {
    match effective_render_output_mode(target_format, selection) {
        HdrRenderOutputMode::SdrToneMapped => HdrOutputMode::SdrToneMapped,
        _ => {
            if cfg!(target_os = "windows") {
                HdrOutputMode::WindowsScRgb
            } else if cfg!(target_os = "macos") {
                HdrOutputMode::MacOsEdr
            } else if cfg!(target_os = "linux") {
                HdrOutputMode::WaylandHdr
            } else {
                HdrOutputMode::SdrToneMapped
            }
        }
    }
}

/// Merge Wayland monitor metadata with Vulkan WSI gates on Linux.
pub fn effective_monitor_selection(
    wp: Option<&HdrMonitorSelection>,
    wsi: crate::hdr::wsi_probe::WsiHdrSurfaceGates,
) -> Option<HdrMonitorSelection> {
    crate::hdr::wsi_probe::linux_effective_monitor_selection(wp, wsi)
}

/// `viewport_outer_rect_screen_px` is [`HdrMonitorSignature::outer_rect`] (used for
/// scheduling *and* as a signal for which monitor the user perceives the window on).
/// On Windows we normally resolve DXGI from the process **largest** visible top-level
/// `HWND` via `GetWindowRect` center + `MonitorFromPoint` (not `MonitorFromWindow`), so
/// wide cross-monitor drags track the center monitor. During the first frames after
/// launch, however, the OS / winit frame can still report a **tiny** `GetWindowRect`
/// (e.g. near `(0,0)`) before the saved YAML placement is applied — the center then
/// lands on the wrong display even though `ViewportInfo::outer_rect` already matches the
/// restored position. When the viewport outer rect is **plausible** and strictly larger
/// than the HWND rect area, we prefer the viewport center for this probe.
#[cfg(target_os = "windows")]
pub fn active_monitor_hdr_status(
    viewport_outer_rect_screen_px: Option<[i32; 4]>,
) -> Result<HdrMonitorSelection, String> {
    windows_active_monitor_hdr_status(viewport_outer_rect_screen_px)
}

#[cfg(target_os = "macos")]
pub fn active_monitor_hdr_status(
    _viewport_outer_rect_screen_px: Option<[i32; 4]>,
) -> Result<HdrMonitorSelection, String> {
    macos_active_monitor_hdr_status()
}

#[cfg(target_os = "linux")]
pub fn active_monitor_hdr_status(
    viewport_outer_rect_screen_px: Option<[i32; 4]>,
) -> Result<HdrMonitorSelection, String> {
    if crate::hdr::platform::linux_native_hdr_platform_eligible() {
        wayland::active_monitor_hdr_status(viewport_outer_rect_screen_px)
    } else {
        Err("HDR probing requires a Wayland session".to_string())
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
pub fn active_monitor_hdr_status(
    _viewport_outer_rect_screen_px: Option<[i32; 4]>,
) -> Result<HdrMonitorSelection, String> {
    Err("active monitor HDR probing is not implemented on this platform".to_string())
}
