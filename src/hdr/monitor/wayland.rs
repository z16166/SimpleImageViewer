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

use super::probe::SpawnMonitorHdrProbe;
use super::types::{HdrMonitorSelection, LinuxWaylandColorPrimaries, LinuxWaylandTransferFunction};

#[cfg(target_os = "linux")]
use std::collections::HashMap;

#[cfg(target_os = "linux")]
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum,
    protocol::{wl_output, wl_pointer, wl_registry, wl_seat},
};
#[cfg(target_os = "linux")]
use wayland_protocols::wp::color_management::v1::client::wp_color_manager_v1::{
    Primaries, TransferFunction,
};
#[cfg(target_os = "linux")]
use wayland_protocols::wp::color_management::v1::client::{
    wp_color_management_output_v1, wp_color_manager_v1, wp_image_description_info_v1,
    wp_image_description_v1,
};

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum WaylandTransferFunction {
    Srgb,
    /// Pure gamma curve from `tf_power` (exponent × 10000).
    GammaPower(f32),
    Gamma22,
    Bt1886,
    /// IEC 61966-2-1 compound curve (wp_color_management v2).
    CompoundPower24,
    St2084,
    /// BT.2100 HLG from `wp_color_management`. Native **output** is not implemented (see
    /// `hdr_supported_from_wayland_probe`); HLG **input** decode exists for AVIF/JXL/etc.
    Hlg,
    Unknown,
}

/// Wayland `wp_color_management` classifies explicit PQ output. The effective Linux
/// native HDR gate merges this metadata with Vulkan WSI in [`crate::hdr::linux_admission`].
pub(crate) fn hdr_supported_from_wayland_probe(tf: WaylandTransferFunction) -> bool {
    matches!(tf, WaylandTransferFunction::St2084)
}

pub(crate) fn map_wayland_transfer_function(
    tf: WaylandTransferFunction,
) -> LinuxWaylandTransferFunction {
    match tf {
        WaylandTransferFunction::Srgb => LinuxWaylandTransferFunction::Srgb,
        WaylandTransferFunction::Gamma22 => LinuxWaylandTransferFunction::Gamma22,
        WaylandTransferFunction::Bt1886 => LinuxWaylandTransferFunction::Bt1886,
        WaylandTransferFunction::CompoundPower24 => LinuxWaylandTransferFunction::CompoundPower24,
        WaylandTransferFunction::St2084 => LinuxWaylandTransferFunction::St2084,
        WaylandTransferFunction::Hlg => LinuxWaylandTransferFunction::Hlg,
        WaylandTransferFunction::GammaPower(_) | WaylandTransferFunction::Unknown => {
            LinuxWaylandTransferFunction::Unknown
        }
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn map_wayland_primaries(primaries: Option<Primaries>) -> LinuxWaylandColorPrimaries {
    match primaries {
        // `Primaries::Srgb` is BT.709 / IEC sRGB (H.273 cp 1); no separate Bt709 variant in wp v1.
        Some(Primaries::Srgb)
        | Some(Primaries::PalM)
        | Some(Primaries::Pal)
        | Some(Primaries::Ntsc) => LinuxWaylandColorPrimaries::Narrow,
        Some(Primaries::Bt2020)
        | Some(Primaries::DciP3)
        | Some(Primaries::DisplayP3)
        | Some(Primaries::AdobeRgb)
        | Some(Primaries::Cie1931Xyz) => LinuxWaylandColorPrimaries::Wide,
        Some(Primaries::GenericFilm) | Some(_) => LinuxWaylandColorPrimaries::Unknown,
        None => LinuxWaylandColorPrimaries::Unknown,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WaylandOutputRect {
    pub label: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

fn native_surface_encoding_from_transfer(
    tf: WaylandTransferFunction,
) -> Option<super::HdrNativeSurfaceEncoding> {
    use super::HdrNativeSurfaceEncoding;
    match tf {
        WaylandTransferFunction::St2084 => Some(HdrNativeSurfaceEncoding::PqHdr10),
        // Gamma 2.2 / HLG / SDR transfer functions are not native HDR encodings on Wayland.
        WaylandTransferFunction::Gamma22
        | WaylandTransferFunction::CompoundPower24
        | WaylandTransferFunction::GammaPower(_)
        | WaylandTransferFunction::Hlg
        | WaylandTransferFunction::Srgb
        | WaylandTransferFunction::Bt1886
        | WaylandTransferFunction::Unknown => None,
    }
}

pub(crate) fn wayland_output_selection(
    label: String,
    tf: WaylandTransferFunction,
    max_luminance_nits: Option<f32>,
    reference_luminance_nits: Option<f32>,
    primaries: Option<Primaries>,
) -> HdrMonitorSelection {
    let hdr_supported = hdr_supported_from_wayland_probe(tf);
    HdrMonitorSelection {
        hdr_supported,
        label,
        max_luminance_nits,
        max_full_frame_luminance_nits: None,
        max_hdr_capacity: None,
        hdr_capacity_source: hdr_supported.then_some("Wayland wp_color_management"),
        native_surface_encoding: hdr_supported
            .then(|| native_surface_encoding_from_transfer(tf))
            .flatten(),
        reference_luminance_nits,
        linux_wp_transfer: Some(map_wayland_transfer_function(tf)),
        linux_wp_primaries: Some(map_wayland_primaries(primaries)),
        linux_explicit_hdr_state: None,
        linux_explicit_hdr_state_source: None,
    }
}

#[cfg(target_os = "linux")]
fn spawn_hdr_supported(selection: &HdrMonitorSelection) -> bool {
    match selection.linux_explicit_hdr_state {
        Some(super::LinuxExplicitHdrState::Enabled) => true,
        Some(super::LinuxExplicitHdrState::Disabled | super::LinuxExplicitHdrState::Incapable) => {
            false
        }
        None => selection.hdr_supported,
    }
}

pub(crate) fn resolve_spawn_probe_point(
    saved_window_top_left: Option<[i32; 2]>,
    cursor_position: Option<[i32; 2]>,
) -> ([i32; 2], &'static str) {
    if let Some([x, y]) = saved_window_top_left {
        ([x + 20, y + 20], "saved_window_position")
    } else if let Some(point) = cursor_position {
        (point, "cursor")
    } else {
        ([0, 0], "primary")
    }
}

pub(crate) fn resolve_active_probe_point(
    viewport_outer_rect_screen_px: Option<[i32; 4]>,
) -> ([i32; 2], &'static str) {
    const MIN_PLAUSIBLE_OUTER_AREA: i64 = 64 * 64;

    if let Some([left, top, right, bottom]) = viewport_outer_rect_screen_px {
        let area = i64::from(right.saturating_sub(left)).max(0)
            * i64::from(bottom.saturating_sub(top)).max(0);
        if area >= MIN_PLAUSIBLE_OUTER_AREA {
            return (
                [(left + right) / 2, (top + bottom) / 2],
                "viewport_outer_rect",
            );
        }
    }
    ([0, 0], "primary")
}

pub(crate) fn select_output_index_at_point(
    outputs: &[WaylandOutputRect],
    point: [i32; 2],
) -> Option<usize> {
    let [px, py] = point;
    let mut best: Option<(usize, i64)> = None;
    for (index, output) in outputs.iter().enumerate() {
        if px < output.x
            || py < output.y
            || px >= output.x.saturating_add(output.width)
            || py >= output.y.saturating_add(output.height)
        {
            continue;
        }
        let area = i64::from(output.width.max(0)) * i64::from(output.height.max(0));
        if best.map_or(true, |(_, best_area)| area < best_area) {
            best = Some((index, area));
        }
    }
    best.map(|(index, _)| index)
}

pub(crate) fn primary_output_index(outputs: &[WaylandOutputRect]) -> Option<usize> {
    select_output_index_at_point(outputs, [0, 0]).or(if outputs.is_empty() {
        None
    } else {
        Some(0)
    })
}

pub(crate) fn pick_output_index(
    outputs: &[WaylandOutputRect],
    point: [i32; 2],
    origin: &str,
) -> Option<usize> {
    select_output_index_at_point(outputs, point).or_else(|| {
        if origin == "primary" {
            primary_output_index(outputs)
        } else {
            None
        }
    })
}

#[cfg(target_os = "linux")]
pub(crate) fn transfer_function_from_protocol(tf: TransferFunction) -> WaylandTransferFunction {
    match tf {
        TransferFunction::Srgb | TransferFunction::ExtSrgb => WaylandTransferFunction::Srgb,
        TransferFunction::Gamma22 => WaylandTransferFunction::Gamma22,
        TransferFunction::Bt1886 => WaylandTransferFunction::Bt1886,
        TransferFunction::CompoundPower24 => WaylandTransferFunction::CompoundPower24,
        TransferFunction::St2084Pq => WaylandTransferFunction::St2084,
        TransferFunction::Hlg => WaylandTransferFunction::Hlg,
        _ => WaylandTransferFunction::Unknown,
    }
}

pub(crate) fn transfer_function_from_tf_power(eexp: u32) -> WaylandTransferFunction {
    WaylandTransferFunction::GammaPower(eexp as f32 / 10_000.0)
}

#[cfg(target_os = "linux")]
fn finite_positive_luminance(value: f32) -> Option<f32> {
    (value.is_finite() && value > 0.0).then_some(value)
}

#[cfg(target_os = "linux")]
fn ensure_wayland_session() -> Result<(), String> {
    if !crate::hdr::platform::is_wayland_session() {
        return Err("not a Wayland session".to_string());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
struct OutputState {
    global_name: u32,
    wl_output: wl_output::WlOutput,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    name: String,
    done: bool,
}

#[cfg(target_os = "linux")]
struct ImageDescriptionState {
    ready: bool,
    failed: bool,
    transfer_function: Option<WaylandTransferFunction>,
    max_luminance_nits: Option<f32>,
    reference_luminance_nits: Option<f32>,
    primaries: Option<Primaries>,
    info_requested: bool,
    info_done: bool,
}

#[cfg(target_os = "linux")]
impl Default for ImageDescriptionState {
    fn default() -> Self {
        Self {
            ready: false,
            failed: false,
            transfer_function: None,
            max_luminance_nits: None,
            reference_luminance_nits: None,
            primaries: None,
            info_requested: false,
            info_done: false,
        }
    }
}

#[cfg(target_os = "linux")]
enum ProbePhase {
    CollectGlobals,
    WaitImageDescription,
    WaitImageInfo,
    Done,
}

#[cfg(target_os = "linux")]
struct ProbeState {
    phase: ProbePhase,
    color_manager: Option<wp_color_manager_v1::WpColorManagerV1>,
    outputs: HashMap<u32, OutputState>,
    selected_output_name: Option<u32>,
    selected_output_label: String,
    color_output: Option<wp_color_management_output_v1::WpColorManagementOutputV1>,
    image_description: Option<wp_image_description_v1::WpImageDescriptionV1>,
    image_state: ImageDescriptionState,
    probe_point: [i32; 2],
    probe_origin: &'static str,
    spawn_probe: bool,
    error: Option<String>,
    result: Option<Result<HdrMonitorSelection, String>>,
    spawn_result: Option<Result<SpawnMonitorHdrProbe, String>>,
}

#[cfg(target_os = "linux")]
impl ProbeState {
    fn new(probe_point: [i32; 2], probe_origin: &'static str, spawn_probe: bool) -> Self {
        Self {
            phase: ProbePhase::CollectGlobals,
            color_manager: None,
            outputs: HashMap::new(),
            selected_output_name: None,
            selected_output_label: String::new(),
            color_output: None,
            image_description: None,
            image_state: ImageDescriptionState::default(),
            probe_point,
            probe_origin,
            spawn_probe,
            error: None,
            result: None,
            spawn_result: None,
        }
    }

    fn output_rects(&self) -> Vec<WaylandOutputRect> {
        let mut outputs: Vec<_> = self
            .outputs
            .values()
            .map(|output| WaylandOutputRect {
                label: output_label(output),
                x: output.x,
                y: output.y,
                width: output.width,
                height: output.height,
            })
            .collect();
        outputs.sort_by(|left, right| left.label.cmp(&right.label));
        outputs
    }

    fn all_outputs_done(&self) -> bool {
        !self.outputs.is_empty() && self.outputs.values().all(|output| output.done)
    }

    fn fail(&mut self, message: String) {
        self.error = Some(message);
        self.phase = ProbePhase::Done;
    }

    fn begin_output_query(&mut self, qh: &QueueHandle<ProbeState>) {
        let Some(color_manager) = self.color_manager.as_ref() else {
            self.fail("wp_color_management unavailable".to_string());
            return;
        };

        let rects = self.output_rects();
        let Some(index) = pick_output_index(&rects, self.probe_point, self.probe_origin) else {
            self.fail("no Wayland output matched the probe point".to_string());
            return;
        };
        let label = rects[index].label.clone();
        let Some((global_name, wl_output)) = self.outputs.values().find_map(|output| {
            (output_label(output) == label)
                .then_some((output.global_name, output.wl_output.clone()))
        }) else {
            self.fail("selected Wayland output was not found".to_string());
            return;
        };

        self.selected_output_name = Some(global_name);
        self.selected_output_label = label.clone();

        log::debug!(
            "[HDR] Wayland probe: origin={} point={:?} output={label} global_name={global_name}",
            self.probe_origin,
            self.probe_point,
        );

        let color_output = color_manager.get_output(&wl_output, qh, global_name);
        let image_description = color_output.get_image_description(qh, global_name);
        self.color_output = Some(color_output);
        self.image_description = Some(image_description);
        self.phase = ProbePhase::WaitImageDescription;
    }

    fn request_image_information(&mut self, qh: &QueueHandle<ProbeState>) {
        if self.image_state.info_requested {
            return;
        }
        if let Some(image_description) = self.image_description.as_ref() {
            image_description.get_information(qh, ());
            self.image_state.info_requested = true;
            self.phase = ProbePhase::WaitImageInfo;
        }
    }

    fn finish_probe(&mut self) {
        if let Some(err) = self.error.clone() {
            if self.spawn_probe {
                self.spawn_result = Some(Err(err));
            } else {
                self.result = Some(Err(err));
            }
            self.phase = ProbePhase::Done;
            return;
        }

        let tf = self
            .image_state
            .transfer_function
            .unwrap_or(WaylandTransferFunction::Unknown);
        let mut selection = wayland_output_selection(
            self.selected_output_label.clone(),
            tf,
            self.image_state.max_luminance_nits,
            self.image_state.reference_luminance_nits,
            self.image_state.primaries,
        );
        let explicit_state = if self.spawn_probe {
            super::kde::explicit_hdr_state_for_output_blocking(&selection.label)
        } else {
            super::kde::explicit_hdr_state_for_output(&selection.label)
        };
        if let Some(state) = explicit_state {
            selection.linux_explicit_hdr_state = Some(state);
            selection.linux_explicit_hdr_state_source =
                Some(super::kde::KDE_KSCREEN_HDR_STATE_SOURCE);
        }

        log::debug!(
            "[HDR] Wayland image description: tf={tf:?} primaries={:?} \
             max_luminance_nits={:?} reference_luminance_nits={:?}",
            self.image_state.primaries,
            selection.max_luminance_nits,
            self.image_state.reference_luminance_nits,
        );

        log::debug!(
            "[HDR] Wayland wp_color_management probe (metadata; merged with Vulkan WSI on Linux): \
             output={} wp_hdr_supported={} transfer_function={tf:?} \
             max_luminance_nits={:?} reference_luminance_nits={:?} primaries={:?} origin={}",
            selection.label,
            selection.hdr_supported,
            selection.max_luminance_nits,
            selection.reference_luminance_nits,
            selection.linux_wp_primaries,
            self.probe_origin,
        );

        if self.spawn_probe {
            self.spawn_result = Some(Ok(SpawnMonitorHdrProbe {
                hdr_supported: spawn_hdr_supported(&selection),
                label: selection.label,
                origin: self.probe_origin,
                max_luminance_nits: selection.max_luminance_nits,
                max_full_frame_luminance_nits: selection.max_full_frame_luminance_nits,
            }));
        } else {
            self.result = Some(Ok(selection));
        }
        self.phase = ProbePhase::Done;
    }
}

#[cfg(target_os = "linux")]
fn output_label(output: &OutputState) -> String {
    if output.name.is_empty() {
        format!("Wayland output {}", output.global_name)
    } else {
        output.name.clone()
    }
}

#[cfg(target_os = "linux")]
impl Dispatch<wl_registry::WlRegistry, ()> for ProbeState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        else {
            return;
        };

        match interface.as_str() {
            "wl_output" => {
                let wl_output = registry.bind(name, version.min(4), qh, name);
                state.outputs.insert(
                    name,
                    OutputState {
                        global_name: name,
                        wl_output,
                        x: 0,
                        y: 0,
                        width: 0,
                        height: 0,
                        name: String::new(),
                        done: false,
                    },
                );
            }
            "wp_color_manager_v1" if state.color_manager.is_none() => {
                state.color_manager = Some(registry.bind(name, version.min(2), qh, ()));
            }
            "wl_seat" => {
                let seat = registry.bind::<wl_seat::WlSeat, _, _>(name, version.min(7), qh, ());
                if seat.version() >= 5 {
                    let _pointer = seat.get_pointer(qh, ());
                }
            }
            _ => {}
        }
    }
}

#[cfg(target_os = "linux")]
impl Dispatch<wl_output::WlOutput, u32> for ProbeState {
    fn event(
        state: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        global_name: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(output) = state.outputs.get_mut(global_name) else {
            return;
        };
        match event {
            wl_output::Event::Geometry { x, y, .. } => {
                output.x = x;
                output.y = y;
            }
            wl_output::Event::Mode { width, height, .. } => {
                if width > 0 && height > 0 {
                    output.width = width;
                    output.height = height;
                }
            }
            wl_output::Event::Name { name } => output.name = name,
            wl_output::Event::Done => output.done = true,
            _ => {}
        }
    }
}

#[cfg(target_os = "linux")]
impl Dispatch<wl_seat::WlSeat, ()> for ProbeState {
    fn event(
        _: &mut Self,
        _: &wl_seat::WlSeat,
        _: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

#[cfg(target_os = "linux")]
impl Dispatch<wl_pointer::WlPointer, ()> for ProbeState {
    fn event(
        _: &mut Self,
        _: &wl_pointer::WlPointer,
        _: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

#[cfg(target_os = "linux")]
impl Dispatch<wp_color_manager_v1::WpColorManagerV1, ()> for ProbeState {
    fn event(
        _: &mut Self,
        _: &wp_color_manager_v1::WpColorManagerV1,
        _: wp_color_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

#[cfg(target_os = "linux")]
impl Dispatch<wp_color_management_output_v1::WpColorManagementOutputV1, u32> for ProbeState {
    fn event(
        _: &mut Self,
        _: &wp_color_management_output_v1::WpColorManagementOutputV1,
        _: wp_color_management_output_v1::Event,
        _: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

#[cfg(target_os = "linux")]
impl Dispatch<wp_image_description_v1::WpImageDescriptionV1, u32> for ProbeState {
    fn event(
        state: &mut Self,
        image_description: &wp_image_description_v1::WpImageDescriptionV1,
        event: wp_image_description_v1::Event,
        _: &u32,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wp_image_description_v1::Event::Ready { .. }
            | wp_image_description_v1::Event::Ready2 { .. } => {
                state.image_state.ready = true;
                state.request_image_information(qh);
            }
            wp_image_description_v1::Event::Failed { cause, msg } => {
                state.image_state.failed = true;
                state.fail(format!(
                    "Wayland image description failed ({cause:?}): {msg}"
                ));
                let _ = image_description;
            }
            _ => {}
        }
    }
}

#[cfg(target_os = "linux")]
impl Dispatch<wp_image_description_info_v1::WpImageDescriptionInfoV1, ()> for ProbeState {
    fn event(
        state: &mut Self,
        _: &wp_image_description_info_v1::WpImageDescriptionInfoV1,
        event: wp_image_description_info_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wp_image_description_info_v1::Event::TfNamed { tf } => match tf {
                WEnum::Value(tf) => {
                    let mapped = transfer_function_from_protocol(tf);
                    log::debug!(
                        "[HDR] Wayland image info tf_named: protocol={tf:?} mapped={mapped:?}"
                    );
                    state.image_state.transfer_function = Some(mapped);
                }
                other => {
                    log::debug!("[HDR] Wayland image info tf_named: unmapped wire value {other:?}");
                }
            },
            wp_image_description_info_v1::Event::TfPower { eexp } => {
                let mapped = transfer_function_from_tf_power(eexp);
                log::debug!(
                    "[HDR] Wayland image info tf_power: eexp={eexp} exponent={} mapped={mapped:?}",
                    eexp as f32 / 10_000.0,
                );
                state.image_state.transfer_function = Some(mapped);
            }
            wp_image_description_info_v1::Event::PrimariesNamed { primaries } => {
                if let WEnum::Value(primaries) = primaries {
                    log::debug!("[HDR] Wayland image info primaries_named: {primaries:?}");
                    state.image_state.primaries = Some(primaries);
                }
            }
            wp_image_description_info_v1::Event::Luminances {
                min_lum,
                max_lum,
                reference_lum,
            } => {
                log::debug!(
                    "[HDR] Wayland image info luminances: min={min_lum} max={max_lum} reference={reference_lum}"
                );
                state.image_state.reference_luminance_nits =
                    finite_positive_luminance(reference_lum as f32);
                state.image_state.max_luminance_nits = finite_positive_luminance(max_lum as f32)
                    .or_else(|| state.image_state.reference_luminance_nits);
                let _ = min_lum;
            }
            wp_image_description_info_v1::Event::TargetLuminance { max_lum, .. } => {
                log::debug!("[HDR] Wayland image info target_luminance: max={max_lum}");
                if state.image_state.max_luminance_nits.is_none() {
                    state.image_state.max_luminance_nits =
                        finite_positive_luminance(max_lum as f32);
                }
            }
            wp_image_description_info_v1::Event::Done => {
                state.image_state.info_done = true;
            }
            _ => {}
        }
    }
}

#[cfg(target_os = "linux")]
fn run_probe(
    probe_point: [i32; 2],
    probe_origin: &'static str,
    spawn_probe: bool,
) -> Result<ProbeState, String> {
    ensure_wayland_session()?;

    let connection =
        Connection::connect_to_env().map_err(|err| format!("Wayland connection failed: {err}"))?;
    let display = connection.display();
    let mut event_queue: EventQueue<ProbeState> = connection.new_event_queue();
    let qh = event_queue.handle();

    let mut state = ProbeState::new(probe_point, probe_origin, spawn_probe);
    display.get_registry(&qh, ());

    for _ in 0..8 {
        event_queue
            .roundtrip(&mut state)
            .map_err(|err| format!("Wayland roundtrip failed: {err}"))?;
        if state.all_outputs_done() {
            break;
        }
    }

    if state.error.is_none() {
        if state.color_manager.is_none() {
            state.fail("wp_color_management unavailable".to_string());
        } else if !state.all_outputs_done() {
            state.fail("Wayland outputs did not become ready".to_string());
        } else {
            state.begin_output_query(&qh);
        }
    }

    for _ in 0..8 {
        if matches!(state.phase, ProbePhase::Done) {
            break;
        }
        event_queue
            .roundtrip(&mut state)
            .map_err(|err| format!("Wayland roundtrip failed: {err}"))?;
        if matches!(state.phase, ProbePhase::WaitImageDescription)
            && (state.image_state.ready || state.image_state.failed)
            && !state.image_state.info_requested
            && state.error.is_none()
        {
            state.request_image_information(&qh);
        }
        if matches!(state.phase, ProbePhase::WaitImageInfo) && state.image_state.info_done {
            state.finish_probe();
        }
    }

    if !matches!(state.phase, ProbePhase::Done) {
        state.fail("Wayland HDR probe timed out".to_string());
        state.finish_probe();
    }

    Ok(state)
}

#[cfg(target_os = "linux")]
pub fn spawn_monitor_hdr_status(
    saved_window_top_left: Option<[i32; 2]>,
) -> Result<SpawnMonitorHdrProbe, String> {
    let (point, origin) = resolve_spawn_probe_point(saved_window_top_left, None);
    log::info!(
        "[HDR] spawn-monitor Wayland probe: origin={origin} point={point:?} \
         saved_window_top_left={saved_window_top_left:?}"
    );

    let state = run_probe(point, origin, true)?;
    state
        .spawn_result
        .unwrap_or_else(|| Err("Wayland spawn monitor probe did not produce a result".to_string()))
}

#[cfg(target_os = "linux")]
pub fn active_monitor_hdr_status(
    viewport_outer_rect_screen_px: Option<[i32; 4]>,
) -> Result<HdrMonitorSelection, String> {
    let (point, origin) = resolve_active_probe_point(viewport_outer_rect_screen_px);
    log::debug!(
        "[HDR] active-monitor Wayland probe: origin={origin} point={point:?} \
         viewport_outer_rect={viewport_outer_rect_screen_px:?}"
    );

    let state = run_probe(point, origin, false)?;
    state
        .result
        .unwrap_or_else(|| Err("Wayland active monitor probe did not produce a result".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hdr::monitor::LinuxExplicitHdrState;

    #[test]
    fn st2084_is_hdr() {
        assert!(hdr_supported_from_wayland_probe(
            WaylandTransferFunction::St2084
        ));
    }

    #[test]
    fn hlg_is_not_treated_as_native_hdr() {
        assert!(!hdr_supported_from_wayland_probe(
            WaylandTransferFunction::Hlg
        ));
        assert_eq!(
            native_surface_encoding_from_transfer(WaylandTransferFunction::Hlg),
            None
        );
    }

    #[test]
    fn srgb_is_not_hdr() {
        assert!(!hdr_supported_from_wayland_probe(
            WaylandTransferFunction::Srgb
        ));
    }

    #[test]
    fn gamma22_with_high_peak_luminance_is_not_hdr_without_st2084() {
        assert!(!hdr_supported_from_wayland_probe(
            WaylandTransferFunction::Gamma22
        ));
        assert_eq!(
            native_surface_encoding_from_transfer(WaylandTransferFunction::Gamma22),
            None
        );
    }

    #[test]
    fn gamma22_maps_from_protocol() {
        assert_eq!(
            transfer_function_from_protocol(TransferFunction::Gamma22),
            WaylandTransferFunction::Gamma22
        );
    }

    #[test]
    fn wayland_output_selection_builds_hdr_monitor_selection() {
        let hdr = wayland_output_selection(
            "HDR-1".to_string(),
            WaylandTransferFunction::St2084,
            Some(1000.0),
            Some(203.0),
            None,
        );
        assert!(hdr.hdr_supported);
        assert_eq!(hdr.label, "HDR-1");
        assert_eq!(hdr.max_luminance_nits, Some(1000.0));
        assert_eq!(hdr.hdr_capacity_source, Some("Wayland wp_color_management"));

        let sdr = wayland_output_selection(
            "SDR-1".to_string(),
            WaylandTransferFunction::Srgb,
            None,
            None,
            None,
        );
        assert!(!sdr.hdr_supported);
        assert_eq!(sdr.hdr_capacity_source, None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn spawn_hdr_supported_uses_explicit_kde_enabled_state() {
        let mut selection = wayland_output_selection(
            "HDMI-A-1".to_string(),
            WaylandTransferFunction::Gamma22,
            Some(1800.0),
            Some(203.0),
            None,
        );
        assert!(!selection.hdr_supported);
        selection.linux_explicit_hdr_state = Some(LinuxExplicitHdrState::Enabled);
        assert!(spawn_hdr_supported(&selection));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn spawn_hdr_supported_uses_explicit_kde_disabled_veto() {
        let mut selection = wayland_output_selection(
            "HDMI-A-1".to_string(),
            WaylandTransferFunction::St2084,
            Some(1000.0),
            Some(203.0),
            None,
        );
        assert!(selection.hdr_supported);
        selection.linux_explicit_hdr_state = Some(LinuxExplicitHdrState::Disabled);
        assert!(!spawn_hdr_supported(&selection));
    }

    #[test]
    fn spawn_probe_point_prefers_saved_window_position() {
        let (point, origin) = resolve_spawn_probe_point(Some([100, 200]), Some([0, 0]));
        assert_eq!(point, [120, 220]);
        assert_eq!(origin, "saved_window_position");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn map_wayland_primaries_classifies_protocol_variants() {
        assert_eq!(
            map_wayland_primaries(Some(Primaries::Srgb)),
            LinuxWaylandColorPrimaries::Narrow
        );
        assert_eq!(
            map_wayland_primaries(Some(Primaries::Bt2020)),
            LinuxWaylandColorPrimaries::Wide
        );
    }

    #[test]
    fn output_selection_picks_smallest_matching_monitor() {
        let outputs = vec![
            WaylandOutputRect {
                label: "wide".to_string(),
                x: 0,
                y: 0,
                width: 3840,
                height: 1080,
            },
            WaylandOutputRect {
                label: "narrow".to_string(),
                x: 3840,
                y: 0,
                width: 1920,
                height: 1080,
            },
        ];
        let index = select_output_index_at_point(&outputs, [3900, 100]).unwrap();
        assert_eq!(index, 1);
    }
}
