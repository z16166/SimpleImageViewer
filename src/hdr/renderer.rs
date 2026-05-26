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

#[cfg(feature = "heif-native")]
#[path = "apple_compose_gpu.rs"]
mod apple_compose_gpu;

#[cfg(feature = "heif-native")]
use super::heif_apple_gain_map::apple_gain_map_display_weight;
#[cfg(feature = "heif-native")]
use super::heif_apple_gain_map_gpu::apple_heic_deferred_from_metadata;
use super::types::{
    HdrColorSpace, HdrImageBuffer, HdrImageMetadata, HdrPixelFormat, HdrReference,
    HdrToneMapSettings, HdrTransferFunction,
};
use eframe::{
    egui,
    egui_wgpu::{self, CallbackResources, CallbackTrait},
};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use wgpu::util::DeviceExt;

pub const HDR_IMAGE_PLANE_TEXTURE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba32Float;

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HdrRenderOutputMode {
    SdrToneMapped = 0,
    /// Linear scRGB / EDR (`Rgba16Float`, `Rgba32Float`).
    NativeHdr = 1,
    /// PQ HDR10 (`Rgb10a2Unorm` + compositor ST 2084).
    NativeHdrPq = 2,
    /// Gamma 2.2 electrical for KWin KMS HDR offload (`Rgb10a2Unorm`).
    NativeHdrGamma22 = 3,
}

impl HdrRenderOutputMode {
    pub fn for_target_format(
        target_format: wgpu::TextureFormat,
        native_surface_encoding: Option<crate::hdr::monitor::HdrNativeSurfaceEncoding>,
    ) -> Self {
        use crate::hdr::monitor::HdrNativeSurfaceEncoding;
        match target_format {
            wgpu::TextureFormat::Rgb10a2Unorm => match native_surface_encoding {
                Some(HdrNativeSurfaceEncoding::PqHdr10) => Self::NativeHdrPq,
                Some(HdrNativeSurfaceEncoding::Gamma22Electrical) => Self::NativeHdrGamma22,
                Some(HdrNativeSurfaceEncoding::LinearScRgb) => Self::NativeHdrGamma22,
                None => Self::SdrToneMapped,
            },
            wgpu::TextureFormat::Rgba16Float | wgpu::TextureFormat::Rgba32Float => Self::NativeHdr,
            format if crate::hdr::surface::is_native_hdr_surface_format(Some(format)) => {
                Self::NativeHdr
            }
            _ => Self::SdrToneMapped,
        }
    }

    pub fn is_native_hdr(self) -> bool {
        matches!(
            self,
            Self::NativeHdr | Self::NativeHdrPq | Self::NativeHdrGamma22
        )
    }

    pub fn as_diagnostic_label(self) -> &'static str {
        match self {
            Self::NativeHdr => "native_hdr",
            Self::NativeHdrPq => "native_hdr_pq",
            Self::NativeHdrGamma22 => "native_hdr_gamma22",
            Self::SdrToneMapped => "sdr_tone_mapped",
        }
    }

    pub fn rgb10a2_uses_pq_shader(self) -> bool {
        matches!(self, Self::NativeHdrPq)
    }
}

/// When [`HdrRenderOutputMode::SdrToneMapped`] composites into **`Rgba8Unorm` / `Bgra8Unorm`**, the GPU stores
/// fragment output **literally** in 8‑bit channels (`encode_sdr` must apply IEC 61966‑2‑1 / ~gamma OETF in WGSL).
///
/// **`Rgba8UnormSrgb` / `Bgra8UnormSrgb`** treat fragment output as **linear display RGB** and **apply sRGB encode on write**
/// ([`wgpu` texture conventions](https://github.com/gfx-rs/wgpu/wiki/Texture-Color-Formats-and-Srgb-conversions)). Emitting pre‑encoded
/// values from WGSL (**double‑OETF**) lifts mids / washes contrast (**「灰蒙蒙」** vs Chrome on SDR canvases).
pub(crate) fn hdr_sdr_framebuffer_needs_manual_srgb_oetf(format: wgpu::TextureFormat) -> bool {
    matches!(
        format,
        wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Bgra8Unorm
    )
}

pub fn hdr_render_output_diagnostics(target_format: Option<wgpu::TextureFormat>) -> [String; 2] {
    let output_mode =
        target_format.map(|format| HdrRenderOutputMode::for_target_format(format, None));
    [
        format!("[HDR] render_target_format={target_format:?}"),
        format!(
            "[HDR] shader_output_mode={}",
            output_mode
                .map(HdrRenderOutputMode::as_diagnostic_label)
                .unwrap_or("unknown")
        ),
    ]
}

pub fn hdr_egui_overlay_diagnostics(target_format: Option<wgpu::TextureFormat>) -> [String; 2] {
    let shader_entry_point = target_format.map(|format| {
        let rgb10a2_pq = matches!(format, wgpu::TextureFormat::Rgb10a2Unorm)
            && HdrRenderOutputMode::for_target_format(format, None)
                == HdrRenderOutputMode::NativeHdrPq;
        egui_wgpu::egui_framebuffer_shader_entry_point(format, rgb10a2_pq)
    });
    [
        format!("[HDR] egui_overlay_target_format={target_format:?}"),
        format!(
            "[HDR] egui_overlay_framebuffer_shader={}",
            shader_entry_point.unwrap_or("unknown")
        ),
    ]
}

#[allow(dead_code)]
pub const HDR_IMAGE_PLANE_SHADER: &str = r#"
// Largest finite half-float value; caps extreme HDR values before tone mapping.
const MAX_FINITE_HDR_VALUE: f32 = 65504.0;
// Current SDR fallback approximates standard display gamma encoding.
const INVERSE_DISPLAY_GAMMA: f32 = 1.0 / 2.2;
const PQ_REFERENCE_LUMINANCE_NITS: f32 = 10000.0;
// Keeps generated UVs inside the texture for the fullscreen triangle edge.
const MAX_UV_CLAMP: f32 = 0.999999;
const OUTPUT_MODE_NATIVE_HDR: u32 = 1u;
const OUTPUT_MODE_NATIVE_HDR_PQ: u32 = 2u;
const OUTPUT_MODE_NATIVE_HDR_GAMMA22: u32 = 3u;
const INVERSE_GAMMA22: f32 = 1.0 / 2.2;
const INPUT_COLOR_SPACE_REC2020_LINEAR: u32 = 2u;
const INPUT_COLOR_SPACE_ACES2065_1: u32 = 3u;
const INPUT_COLOR_SPACE_XYZ: u32 = 4u;
// Must match HdrColorSpace::DisplayP3Linear as u32.
const INPUT_COLOR_SPACE_DISPLAY_P3_LINEAR: u32 = 6u;
const INPUT_TRANSFER_LINEAR: u32 = 0u;
const INPUT_TRANSFER_SRGB: u32 = 1u;
const INPUT_TRANSFER_PQ: u32 = 2u;
const INPUT_TRANSFER_HLG: u32 = 3u;
/// Must match [`HdrTransferFunction::Bt709`] as `u32` (not **4**/`5` — **`Gamma`/`Unknown`** omit dedicated WGSL branches).
const INPUT_TRANSFER_BT709: u32 = 6u;
// Must stay aligned with `HdrReference` discriminants pushed into ToneMapUniform.
const INPUT_REFERENCE_SCENE_LINEAR: u32 = 0u;
const HDR_DOWNSCALE_SAMPLE_GRID: u32 = 4u;
const HDR_DOWNSCALE_MAX_FOOTPRINT: f32 = 8.0;

struct ToneMapSettings {
    exposure_ev: f32,
    sdr_white_nits: f32,
    max_display_nits: f32,
    // 1.0 except libavif tone-mapped display-referred linear (matches encode_sdr peak scaler).
    native_display_scale: f32,
    rotation_steps: u32,
    alpha: f32,
    output_mode: u32,
    input_color_space: u32,
    input_transfer_function: u32,
    input_reference: u32,
    /// `1`: `Rgba8Unorm` / `Bgra8Unorm` target — WGSL emits **IEC 61966‑2‑1** codes in `encode_sdr`.
    /// `0`: `*UnormSrgb` (**GPU encodes**) or float/native paths — WGSL emits **linear** (~0–1) for writes.
    sdr_manual_srgb_encode: u32,
    // WGSL aligns vec2<f32> to 8 bytes; implicit padding before uv_min.
    _wgsl_pad_before_uv: u32,
    uv_min: vec2<f32>,
    uv_max: vec2<f32>,
    /// `1` when [`AppleHeicGainMapGpuSource`] planes are bound (GPU gain-map compose).
    apple_compose: u32,
    headroom_span: f32,
    weight: f32,
    gain_width: u32,
    gain_height: u32,
    primary_width: u32,
    primary_height: u32,
    _apple_pad: u32,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@group(0) @binding(0) var hdr_texture: texture_2d<f32>;
@group(0) @binding(1) var gain_map_texture: texture_2d<f32>;
@group(0) @binding(2) var<uniform> tone_map: ToneMapSettings;

fn reinhard_tone_map(rgb: vec3<f32>) -> vec3<f32> {
    return rgb / (vec3<f32>(1.0) + rgb);
}

fn sanitize_hdr_rgb(rgb: vec3<f32>) -> vec3<f32> {
    // NaN is the only value where `c != c`; clamp finite range (±Inf → ±MAX_FINITE_HDR_VALUE).
    var safe = rgb;
    if (safe.r != safe.r) {
        safe.r = 0.0;
    }
    if (safe.g != safe.g) {
        safe.g = 0.0;
    }
    if (safe.b != safe.b) {
        safe.b = 0.0;
    }
    return clamp(
        safe,
        vec3<f32>(-MAX_FINITE_HDR_VALUE),
        vec3<f32>(MAX_FINITE_HDR_VALUE),
    );
}

// Scene/display linear after exposure and optional display-referred peak scaler (libavif capped path).
fn exposed_linear_rgb(rgb: vec3<f32>, settings: ToneMapSettings) -> vec3<f32> {
    let exposure_scale = exp2(settings.exposure_ev);
    return sanitize_hdr_rgb(rgb * exposure_scale * settings.native_display_scale);
}

fn rec2020_to_linear_srgb(rgb: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        1.6605 * rgb.r - 0.5876 * rgb.g - 0.0728 * rgb.b,
        -0.1246 * rgb.r + 1.1329 * rgb.g - 0.0083 * rgb.b,
        -0.0182 * rgb.r - 0.1006 * rgb.g + 1.1187 * rgb.b,
    );
}

fn display_p3_linear_to_linear_srgb(rgb: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        1.2249401 * rgb.r - 0.2249402 * rgb.g,
        -0.0420569 * rgb.r + 1.0420571 * rgb.g,
        -0.0196376 * rgb.r - 0.0786507 * rgb.g + 1.0982884 * rgb.b,
    );
}

fn aces2065_1_to_linear_srgb(rgb: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        2.5216 * rgb.r - 1.1369 * rgb.g - 0.3849 * rgb.b,
        -0.2762 * rgb.r + 1.3697 * rgb.g - 0.0935 * rgb.b,
        -0.0159 * rgb.r - 0.1478 * rgb.g + 1.1638 * rgb.b,
    );
}

fn xyz_to_linear_srgb(xyz: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        3.2404 * xyz.x - 1.5371 * xyz.y - 0.4985 * xyz.z,
        -0.9692 * xyz.x + 1.8760 * xyz.y + 0.0415 * xyz.z,
        0.0556 * xyz.x - 0.2040 * xyz.y + 1.0572 * xyz.z,
    );
}

fn srgb_to_linear(rgb: vec3<f32>) -> vec3<f32> {
    let low = rgb / vec3<f32>(12.92);
    let high = pow((rgb + vec3<f32>(0.055)) / vec3<f32>(1.055), vec3<f32>(2.4));
    return select(high, low, rgb <= vec3<f32>(0.04045));
}

// BT.709 / SMPTE 170–style nonlinear code → nominal linear‑light (**ITU‑R BT.709** annex 1 OETF inverse).
fn bt709_nonlinear_to_linear(rgb: vec3<f32>) -> vec3<f32> {
    let low = rgb / vec3<f32>(4.5);
    let high = pow((rgb + vec3<f32>(0.099)) / vec3<f32>(1.099), vec3<f32>(1.0 / 0.45));
    return select(high, low, rgb < vec3<f32>(0.081));
}

fn pq_to_display_linear(rgb: vec3<f32>, settings: ToneMapSettings) -> vec3<f32> {
    let m1 = 2610.0 / 16384.0;
    let m2 = 2523.0 / 32.0;
    let c1 = 3424.0 / 4096.0;
    let c2 = 2413.0 / 128.0;
    let c3 = 2392.0 / 128.0;
    let code = pow(clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)), vec3<f32>(1.0 / m2));
    let numerator = max(code - vec3<f32>(c1), vec3<f32>(0.0));
    let denominator = max(vec3<f32>(c2) - vec3<f32>(c3) * code, vec3<f32>(0.000001));
    let absolute_nits = vec3<f32>(10000.0) * pow(numerator / denominator, vec3<f32>(1.0 / m1));
    return absolute_nits / max(settings.sdr_white_nits, 1.0);
}

fn hlg_to_scene_linear(rgb: vec3<f32>) -> vec3<f32> {
    // BT.2100 HLG EOTF inverse (input decode only). No matching `scene_linear_to_hlg`
    // OETF or `NativeHdrHlg` swap-chain path — see `hdr/monitor/wayland.rs`.
    let a = 0.17883277;
    let b = 0.28466892;
    let c = 0.55991073;
    let low = (rgb * rgb) / vec3<f32>(3.0);
    let high = (exp((rgb - vec3<f32>(c)) / vec3<f32>(a)) + vec3<f32>(b)) / vec3<f32>(12.0);
    return select(high, low, rgb <= vec3<f32>(0.5));
}

fn decode_input_transfer(rgb: vec3<f32>, input_transfer_function: u32, settings: ToneMapSettings) -> vec3<f32> {
    if input_transfer_function == INPUT_TRANSFER_SRGB {
        return srgb_to_linear(rgb);
    }
    if input_transfer_function == INPUT_TRANSFER_BT709 {
        return bt709_nonlinear_to_linear(rgb);
    }
    if input_transfer_function == INPUT_TRANSFER_PQ {
        return pq_to_display_linear(rgb, settings);
    }
    if input_transfer_function == INPUT_TRANSFER_HLG {
        return hlg_to_scene_linear(rgb);
    }
    return rgb;
}

fn convert_input_to_linear_srgb(rgb: vec3<f32>, input_color_space: u32) -> vec3<f32> {
    if input_color_space == INPUT_COLOR_SPACE_REC2020_LINEAR {
        return rec2020_to_linear_srgb(rgb);
    }
    if input_color_space == INPUT_COLOR_SPACE_DISPLAY_P3_LINEAR {
        return display_p3_linear_to_linear_srgb(rgb);
    }
    if input_color_space == INPUT_COLOR_SPACE_ACES2065_1 {
        return aces2065_1_to_linear_srgb(rgb);
    }
    if input_color_space == INPUT_COLOR_SPACE_XYZ {
        return xyz_to_linear_srgb(rgb);
    }
    return rgb;
}

fn rotate_uv_for_display(uv: vec2<f32>, rotation_steps: u32) -> vec2<f32> {
    switch rotation_steps % 4u {
        case 1u: {
            return vec2<f32>(uv.y, 1.0 - uv.x);
        }
        case 2u: {
            return vec2<f32>(1.0 - uv.x, 1.0 - uv.y);
        }
        case 3u: {
            return vec2<f32>(1.0 - uv.y, uv.x);
        }
        default: {
            return uv;
        }
    }
}

fn load_hdr_texel(texel: vec2<i32>, texture_size: vec2<i32>) -> vec4<f32> {
    return textureLoad(
        hdr_texture,
        clamp(texel, vec2<i32>(0), texture_size - vec2<i32>(1)),
        0,
    );
}

fn sample_hdr_for_display(uv: vec2<f32>) -> vec4<f32> {
    let texture_size_u = textureDimensions(hdr_texture);
    let texture_size = vec2<f32>(texture_size_u);
    let texture_size_i = vec2<i32>(texture_size_u);
    let footprint = min(
        max(
            max(abs(dpdx(uv)) * texture_size, abs(dpdy(uv)) * texture_size),
            vec2<f32>(1.0),
        ),
        vec2<f32>(HDR_DOWNSCALE_MAX_FOOTPRINT),
    );

    if max(footprint.x, footprint.y) <= 1.25 {
        return load_hdr_texel(vec2<i32>(uv * texture_size), texture_size_i);
    }

    var sum = vec4<f32>(0.0);
    for (var y = 0u; y < HDR_DOWNSCALE_SAMPLE_GRID; y = y + 1u) {
        for (var x = 0u; x < HDR_DOWNSCALE_SAMPLE_GRID; x = x + 1u) {
            let sample_uv = (vec2<f32>(f32(x), f32(y)) + vec2<f32>(0.5)) / f32(HDR_DOWNSCALE_SAMPLE_GRID);
            let offset = (sample_uv - vec2<f32>(0.5)) * footprint;
            sum += load_hdr_texel(vec2<i32>(uv * texture_size + offset), texture_size_i);
        }
    }
    return sum / f32(HDR_DOWNSCALE_SAMPLE_GRID * HDR_DOWNSCALE_SAMPLE_GRID);
}

// IEC 61966-2-1 sRGB opto-electronic transfer function (scalar, output 0..1).
fn linear_srgb_scalar_to_encoded_srgb(linear: f32) -> f32 {
    let c = clamp(linear, 0.0, 1.0);
    if c <= 0.0031308 {
        return c * 12.92;
    }
    return 1.055 * pow(c, 1.0 / 2.4) - 0.055;
}

fn encode_sdr(rgb: vec3<f32>, settings: ToneMapSettings) -> vec3<f32> {
    let exposure_scale = exp2(settings.exposure_ev);
    // Nielsen / IEC-style HEIF stills: transfer is sRGB PQ codes → treat like Chrome / unmanaged sRGB —
    // Reinhard + 2.2 here washed mid-tones on SDR float-plane path (osd: "线性 sRGB…SDR色彩映射").
    let use_piecewise_srgb = settings.input_transfer_function == INPUT_TRANSFER_SRGB &&
        settings.input_reference != INPUT_REFERENCE_SCENE_LINEAR;
    let manual_oetf = settings.sdr_manual_srgb_encode != 0u;

    if use_piecewise_srgb {
        let lr = sanitize_scalar_for_linear_srgb_encode(rgb.r * exposure_scale);
        let lg = sanitize_scalar_for_linear_srgb_encode(rgb.g * exposure_scale);
        let lb = sanitize_scalar_for_linear_srgb_encode(rgb.b * exposure_scale);
        let linear_clamped = clamp(vec3<f32>(lr, lg, lb), vec3<f32>(0.0), vec3<f32>(1.0));
        if (!manual_oetf) {
            return linear_clamped;
        }
        return vec3<f32>(
            linear_srgb_scalar_to_encoded_srgb(linear_clamped.r),
            linear_srgb_scalar_to_encoded_srgb(linear_clamped.g),
            linear_srgb_scalar_to_encoded_srgb(linear_clamped.b),
        );
    }

    let display_scale = settings.sdr_white_nits / max(settings.max_display_nits, settings.sdr_white_nits);
    let exposed = sanitize_hdr_rgb(rgb * exposure_scale * display_scale);
    let mapped = reinhard_tone_map(exposed);
    let clamped = clamp(mapped, vec3<f32>(0.0), vec3<f32>(1.0));
    if (!manual_oetf) {
        return clamped;
    }
    return pow(clamped, vec3<f32>(INVERSE_DISPLAY_GAMMA));
}

fn sanitize_scalar_for_linear_srgb_encode(value: f32) -> f32 {
    if value != value {
        return 0.0;
    }
    if value <= 0.0 {
        return 0.0;
    }
    return min(value, MAX_FINITE_HDR_VALUE);
}

fn display_linear_to_pq(rgb: vec3<f32>, settings: ToneMapSettings) -> vec3<f32> {
    let m1 = 2610.0 / 16384.0;
    let m2 = 2523.0 / 32.0;
    let c1 = 3424.0 / 4096.0;
    let c2 = 2413.0 / 128.0;
    let c3 = 2392.0 / 128.0;
    let nits = max(rgb * settings.sdr_white_nits, vec3<f32>(0.0));
    let normalized = nits / vec3<f32>(PQ_REFERENCE_LUMINANCE_NITS);
    let lm1 = pow(normalized, vec3<f32>(m1));
    let num = vec3<f32>(c1) + vec3<f32>(c2) * lm1;
    let den = vec3<f32>(1.0) + vec3<f32>(c3) * lm1;
    return pow(num / den, vec3<f32>(m2));
}

// Scene-referred linear → display-referred before KWin gamma 2.2 OETF (same Reinhard as encode_sdr).
fn scene_linear_to_display_referred(scrgb: vec3<f32>) -> vec3<f32> {
    return reinhard_tone_map(scrgb);
}

fn encode_native_hdr(rgb: vec3<f32>, settings: ToneMapSettings) -> vec3<f32> {
    // scRGB / EDR: linear (1.0 = SDR white). Compositor tone-maps to panel (Windows / macOS).
    return exposed_linear_rgb(rgb, settings);
}

fn encode_native_hdr_pq(rgb: vec3<f32>, settings: ToneMapSettings) -> vec3<f32> {
    // SMPTE ST 2084 (PQ) for HDR10 swap chains.
    return display_linear_to_pq(exposed_linear_rgb(rgb, settings), settings);
}

fn gamma22_from_linear_rgb(rgb: vec3<f32>) -> vec3<f32> {
    return pow(max(rgb, vec3<f32>(0.0)), vec3<f32>(INVERSE_GAMMA22));
}

fn encode_native_hdr_gamma22(rgb: vec3<f32>, settings: ToneMapSettings) -> vec3<f32> {
    // KWin gamma 2.2 electrical framebuffer: map SDR white to panel peak, then IEC 61966-2-2 OETF.
    let display_scale =
        settings.sdr_white_nits / max(settings.max_display_nits, settings.sdr_white_nits);
    let exposed = exposed_linear_rgb(rgb, settings);
    var peak_linear: vec3<f32>;
    if (settings.input_transfer_function == INPUT_TRANSFER_LINEAR) {
        peak_linear = scene_linear_to_display_referred(exposed) * display_scale;
    } else {
        peak_linear = exposed * display_scale;
    }
    return gamma22_from_linear_rgb(clamp(peak_linear, vec3<f32>(0.0), vec3<f32>(1.0)));
}

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    let positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    let uvs = array<vec2<f32>, 3>(
        vec2<f32>(0.0, 1.0),
        vec2<f32>(2.0, 1.0),
        vec2<f32>(0.0, -1.0),
    );

    var output: VertexOutput;
    output.position = vec4<f32>(positions[vertex_index], 0.0, 1.0);
    output.uv = uvs[vertex_index];
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let rotated_uv = rotate_uv_for_display(input.uv, tone_map.rotation_steps);
    let sampled_uv = tone_map.uv_min + rotated_uv * (tone_map.uv_max - tone_map.uv_min);
    let clamped_uv = clamp(sampled_uv, vec2<f32>(0.0), vec2<f32>(MAX_UV_CLAMP));
    let hdr = sample_hdr_for_display(clamped_uv);
    let decoded_rgb = decode_input_transfer(hdr.rgb, tone_map.input_transfer_function, tone_map);
    let source_rgb = convert_input_to_linear_srgb(decoded_rgb, tone_map.input_color_space);
    let src_a = clamp(hdr.a, 0.0, 1.0);
    var rgb: vec3<f32>;
    if tone_map.output_mode == OUTPUT_MODE_NATIVE_HDR_PQ {
        rgb = encode_native_hdr_pq(source_rgb, tone_map);
    } else if tone_map.output_mode == OUTPUT_MODE_NATIVE_HDR_GAMMA22 {
        rgb = encode_native_hdr_gamma22(source_rgb, tone_map);
    } else if tone_map.output_mode == OUTPUT_MODE_NATIVE_HDR {
        rgb = encode_native_hdr(source_rgb, tone_map);
    } else {
        rgb = encode_sdr(source_rgb, tone_map);
    }
    let has_linear_signal = max(max(abs(source_rgb.r), abs(source_rgb.g)), abs(source_rgb.b)) > 1e-6;
    let a_out = select(src_a, 1.0, src_a == 0.0 && has_linear_signal);
    return vec4<f32>(rgb, a_out * tone_map.alpha);
}
"#;

#[allow(dead_code)]
pub struct UploadedHdrImage {
    pub size: wgpu::Extent3d,
    pub format: wgpu::TextureFormat,
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub sampler: wgpu::Sampler,
}

pub struct HdrImageRenderer {
    pub tone_map: HdrToneMapSettings,
    uploaded_image: Option<UploadedHdrImage>,
}

impl HdrImageRenderer {
    pub fn new() -> Self {
        Self {
            tone_map: HdrToneMapSettings::default(),
            uploaded_image: None,
        }
    }

    #[allow(dead_code)]
    pub fn uploaded_image(&self) -> Option<&UploadedHdrImage> {
        self.uploaded_image.as_ref()
    }

    #[allow(dead_code)]
    pub fn clear_uploaded_image(&mut self) {
        self.uploaded_image = None;
    }

    #[allow(dead_code)]
    pub fn upload_image(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        image: &HdrImageBuffer,
    ) -> Result<(), String> {
        let layout = validate_upload_layout(image, device.limits().max_texture_dimension_2d)?;
        let (upload_bytes, bytes_per_row) = pack_rows_for_texture_copy(
            rgba32f_as_bytes(image.rgba_f32.as_slice()),
            image.width,
            image.height,
            std::mem::size_of::<f32>() as u32 * 4,
        )
        .map_err(|err| format!("HDR upload: {err}"))?;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("simple-image-viewer-hdr-image-plane"),
            size: layout.size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: layout.format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &upload_bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(layout.size.height),
            },
            layout.size,
        );

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("simple-image-viewer-hdr-image-plane-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        self.uploaded_image = Some(UploadedHdrImage {
            size: layout.size,
            format: layout.format,
            texture,
            view,
            sampler,
        });

        Ok(())
    }
}

#[allow(dead_code)]
pub fn hdr_image_plane_callback(
    rect: egui::Rect,
    image: Arc<HdrImageBuffer>,
    tone_map: HdrToneMapSettings,
    target_format: wgpu::TextureFormat,
    output_mode: HdrRenderOutputMode,
    rotation_steps: u32,
    alpha: f32,
) -> egui::Shape {
    hdr_image_plane_callback_with_uv(
        rect,
        image,
        tone_map,
        target_format,
        output_mode,
        rotation_steps,
        alpha,
        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
    )
}

pub fn hdr_image_plane_callback_with_uv(
    rect: egui::Rect,
    image: Arc<HdrImageBuffer>,
    tone_map: HdrToneMapSettings,
    target_format: wgpu::TextureFormat,
    output_mode: HdrRenderOutputMode,
    rotation_steps: u32,
    alpha: f32,
    uv_rect: egui::Rect,
) -> egui::Shape {
    egui::Shape::Callback(egui_wgpu::Callback::new_paint_callback(
        rect,
        HdrImagePlaneCallback {
            image,
            tone_map,
            target_format,
            output_mode,
            rotation_steps: rotation_steps % 4,
            alpha,
            uv_rect,
        },
    ))
}

#[allow(dead_code)]
pub fn hdr_tile_plane_callback(
    rect: egui::Rect,
    tile: Arc<crate::hdr::tiled::HdrTileBuffer>,
    tone_map: HdrToneMapSettings,
    target_format: wgpu::TextureFormat,
    output_mode: HdrRenderOutputMode,
    rotation_steps: u32,
    alpha: f32,
) -> egui::Shape {
    hdr_tile_plane_callback_with_uv(
        rect,
        tile,
        tone_map,
        target_format,
        output_mode,
        rotation_steps,
        alpha,
        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
    )
}

#[allow(dead_code)]
pub fn hdr_tile_plane_callback_with_uv(
    rect: egui::Rect,
    tile: Arc<crate::hdr::tiled::HdrTileBuffer>,
    tone_map: HdrToneMapSettings,
    target_format: wgpu::TextureFormat,
    output_mode: HdrRenderOutputMode,
    rotation_steps: u32,
    alpha: f32,
    uv_rect: egui::Rect,
) -> egui::Shape {
    egui::Shape::Callback(egui_wgpu::Callback::new_paint_callback(
        rect,
        HdrTilePlaneCallback {
            tile,
            tone_map,
            target_format,
            output_mode,
            rotation_steps: rotation_steps % 4,
            alpha,
            uv_rect,
        },
    ))
}

struct HdrImagePlaneCallback {
    image: Arc<HdrImageBuffer>,
    tone_map: HdrToneMapSettings,
    target_format: wgpu::TextureFormat,
    output_mode: HdrRenderOutputMode,
    rotation_steps: u32,
    alpha: f32,
    uv_rect: egui::Rect,
}

#[allow(dead_code)]
struct HdrTilePlaneCallback {
    tile: Arc<crate::hdr::tiled::HdrTileBuffer>,
    tone_map: HdrToneMapSettings,
    target_format: wgpu::TextureFormat,
    output_mode: HdrRenderOutputMode,
    rotation_steps: u32,
    alpha: f32,
    uv_rect: egui::Rect,
}

impl CallbackTrait for HdrImagePlaneCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        _egui_encoder: &mut wgpu::CommandEncoder,
        callback_resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let needs_resources = callback_resources
            .get::<HdrCallbackResources>()
            .map_or(true, |resources| {
                resources.target_format != self.target_format
            });
        if needs_resources {
            callback_resources.insert(create_callback_resources(device, self.target_format));
        }

        let Some(resources) = callback_resources.get_mut::<HdrCallbackResources>() else {
            return Vec::new();
        };

        let native_display_scale = libavif_tone_map_native_display_scale(
            &self.image.metadata,
            self.image.color_space,
            &self.tone_map,
        );

        let image_key = HdrImageKey::from_image(&self.image);
        #[cfg(feature = "heif-native")]
        let apple_deferred = apple_heic_deferred_from_metadata(&self.image.metadata);
        #[cfg(not(feature = "heif-native"))]
        let apple_deferred: Option<&crate::hdr::types::AppleHeicGainMapGpuSource> = None;
        let target_capacity_bits = self.tone_map.target_hdr_capacity().to_bits();
        let needs_upload = resources.uploaded_image_key != Some(image_key);
        #[cfg(feature = "heif-native")]
        let needs_compose = apple_deferred.is_some()
            && (needs_upload
                || resources.baked_apple_image_key != Some(image_key)
                || resources.baked_apple_weight_bits != Some(target_capacity_bits));
        #[cfg(not(feature = "heif-native"))]
        let needs_compose = false;

        if needs_upload {
            match upload_image_plane(device, queue, &self.image) {
                Ok(uploaded) => {
                    resources.uploaded_image_key = Some(image_key);
                    resources.uploaded_texture = Some(uploaded.base.texture);
                    resources.uploaded_view = Some(uploaded.base.view);
                    #[cfg(feature = "heif-native")]
                    {
                        resources.uploaded_display_storage_view = uploaded.base.storage_view;
                    }
                    if let Some(gain) = uploaded.gain {
                        resources.uploaded_gain_texture = Some(gain.texture);
                        resources.uploaded_gain_view = Some(gain.view);
                        #[cfg(feature = "heif-native")]
                        if apple_deferred.is_some() {
                            resources.baked_apple_image_key = None;
                            resources.baked_apple_weight_bits = None;
                            resources.encoded_primary_source_ptr = None;
                        }
                    } else {
                        resources.uploaded_gain_texture = None;
                        resources.uploaded_gain_view = None;
                        #[cfg(feature = "heif-native")]
                        {
                            resources.uploaded_display_storage_view = None;
                        }
                        resources.baked_apple_image_key = None;
                        resources.baked_apple_weight_bits = None;
                        #[cfg(feature = "heif-native")]
                        {
                            resources.encoded_primary_source_ptr = None;
                        }
                    }
                }
                Err(err) => {
                    log::warn!("[HDR] Skipping HDR image plane upload: {err}");
                    resources.uploaded_image_key = None;
                    resources.uploaded_texture = None;
                    resources.uploaded_view = None;
                    resources.uploaded_gain_texture = None;
                    resources.uploaded_gain_view = None;
                    #[cfg(feature = "heif-native")]
                    {
                        resources.uploaded_display_storage_view = None;
                        resources.encoded_primary_source_ptr = None;
                    }
                    resources.baked_apple_image_key = None;
                    resources.baked_apple_weight_bits = None;
                    resources.bind_group = None;
                    return Vec::new();
                }
            }
        }

        #[cfg(feature = "heif-native")]
        let mut compose_command_buffers = Vec::new();
        #[cfg(not(feature = "heif-native"))]
        let compose_command_buffers: Vec<wgpu::CommandBuffer> = Vec::new();
        #[cfg(feature = "heif-native")]
        if needs_compose {
            if resources.uploaded_view.is_none()
                || resources.uploaded_gain_view.is_none()
                || resources.uploaded_display_storage_view.is_none()
            {
                resources.bind_group = None;
                return compose_command_buffers;
            }
            if let Some(deferred) = apple_deferred {
                let primary_ptr = std::sync::Arc::as_ptr(&self.image.rgba_f32) as usize;
                let upload_primary =
                    needs_upload || resources.encoded_primary_source_ptr != Some(primary_ptr);
                if let Err(err) = apple_compose_gpu::ensure_encoded_primary_buffer(
                    device,
                    resources,
                    self.image.width,
                    device.limits().max_storage_buffer_binding_size,
                ) {
                    log::warn!("[HDR] Apple GPU compose primary buffer allocation failed: {err}");
                    resources.bind_group = None;
                } else {
                    let gain_view = resources.uploaded_gain_view.as_ref().expect("gain view");
                    let display_storage = resources
                        .uploaded_display_storage_view
                        .as_ref()
                        .expect("display storage view");
                    let encoded_primary_buffer = resources
                        .encoded_primary_buffer
                        .as_ref()
                        .expect("encoded primary buffer");
                    compose_command_buffers.push(apple_compose_gpu::encode_compose_compute_pass(
                        device,
                        queue,
                        resources,
                        &self.image,
                        deferred,
                        &self.tone_map,
                        encoded_primary_buffer,
                        gain_view,
                        display_storage,
                        upload_primary,
                    ));
                    if upload_primary {
                        resources.encoded_primary_source_ptr = Some(primary_ptr);
                    }
                    resources.baked_apple_image_key = Some(image_key);
                    resources.baked_apple_weight_bits = Some(target_capacity_bits);
                }
            }
        }

        let apple_gpu_composed = apple_deferred.is_some()
            && resources.baked_apple_image_key == Some(image_key)
            && resources.uploaded_view.is_some();

        // Never display encoded primary for deferred HEIC (causes HDR zoom stains). Wait for GPU compose.
        if apple_deferred.is_some() && !apple_gpu_composed {
            resources.bind_group = None;
            return compose_command_buffers;
        }

        let uniform = image_tone_map_uniform(
            &self.image,
            self.tone_map,
            self.rotation_steps,
            self.alpha,
            self.output_mode,
            self.target_format,
            self.uv_rect,
            native_display_scale,
            apple_gpu_composed,
        );
        queue.write_buffer(&resources.tone_map_buffer, 0, bytemuck::bytes_of(&uniform));

        let gain_view = if apple_gpu_composed {
            &resources.dummy_gain_view
        } else {
            resources
                .uploaded_gain_view
                .as_ref()
                .unwrap_or(&resources.dummy_gain_view)
        };
        let hdr_view = resources.uploaded_view.as_ref();
        if let Some(hdr_view) = hdr_view {
            resources.bind_group = Some(create_hdr_image_plane_bind_group(
                device,
                &resources.bind_group_layout,
                hdr_view,
                gain_view,
                &resources.tone_map_buffer,
            ));
        } else {
            resources.bind_group = None;
        }

        compose_command_buffers
    }

    fn paint(
        &self,
        info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        callback_resources: &CallbackResources,
    ) {
        let Some(resources) = callback_resources.get::<HdrCallbackResources>() else {
            return;
        };
        let Some(bind_group) = resources.bind_group.as_ref() else {
            return;
        };

        // egui-wgpu already sets this viewport before invoking callbacks; repeat it
        // here so the fullscreen triangle is explicitly scoped to the image rect.
        let viewport = info.viewport_in_pixels();
        render_pass.set_viewport(
            viewport.left_px as f32,
            viewport.top_px as f32,
            viewport.width_px as f32,
            viewport.height_px as f32,
            0.0,
            1.0,
        );
        let scissor = info.clip_rect_in_pixels();
        render_pass.set_scissor_rect(
            scissor.left_px.max(0) as u32,
            scissor.top_px.max(0) as u32,
            scissor.width_px.max(0) as u32,
            scissor.height_px.max(0) as u32,
        );
        render_pass.set_pipeline(&resources.pipeline);
        render_pass.set_bind_group(0, bind_group, &[]);
        render_pass.draw(0..3, 0..1);
    }
}

impl CallbackTrait for HdrTilePlaneCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        _egui_encoder: &mut wgpu::CommandEncoder,
        callback_resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let needs_resources = callback_resources
            .get::<HdrCallbackResources>()
            .map_or(true, |resources| {
                resources.target_format != self.target_format
            });
        if needs_resources {
            callback_resources.insert(create_callback_resources(device, self.target_format));
        }

        let Some(resources) = callback_resources.get_mut::<HdrCallbackResources>() else {
            return Vec::new();
        };

        let native_display_scale = libavif_tone_map_native_display_scale(
            &self.tile.metadata,
            self.tile.color_space,
            &self.tone_map,
        );
        let uniform = tile_tone_map_uniform(
            self.tone_map,
            self.rotation_steps,
            self.alpha,
            self.output_mode,
            self.target_format,
            self.tile.metadata.color_space_hint(),
            self.tile.metadata.transfer_function,
            self.tile.metadata.reference,
            self.uv_rect,
            native_display_scale,
        );

        let tile_key = HdrTileKey::from_tile_with_uv(&self.tile, self.uv_rect);
        if !resources.tile_bindings.contains(tile_key) {
            match upload_callback_tile(device, queue, &self.tile) {
                Ok(uploaded) => {
                    resources.uploaded_image_key = None;
                    let tone_map_buffer =
                        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                            label: Some("simple-image-viewer-hdr-tile-plane-tone-map-buffer"),
                            contents: bytemuck::bytes_of(&uniform),
                            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                        });
                    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("simple-image-viewer-hdr-tile-plane-bind-group"),
                        layout: &resources.bind_group_layout,
                        entries: &[
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: wgpu::BindingResource::TextureView(&uploaded.view),
                            },
                            wgpu::BindGroupEntry {
                                binding: 1,
                                resource: wgpu::BindingResource::TextureView(
                                    &resources.dummy_gain_view,
                                ),
                            },
                            wgpu::BindGroupEntry {
                                binding: 2,
                                resource: tone_map_buffer.as_entire_binding(),
                            },
                        ],
                    });
                    resources.tile_bindings.insert(
                        tile_key,
                        uploaded.texture,
                        uploaded.view,
                        tone_map_buffer,
                        bind_group,
                    );
                }
                Err(err) => {
                    log::warn!("[HDR] Skipping HDR tile plane upload: {err}");
                    resources.tile_bindings.remove(tile_key);
                }
            }
        }
        if let Some(binding) = resources.tile_bindings.binding_mut(tile_key) {
            if let Some(buffer) = binding.tone_map_buffer.as_ref() {
                queue.write_buffer(buffer, 0, bytemuck::bytes_of(&uniform));
            }
        }

        Vec::new()
    }

    fn paint(
        &self,
        info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        callback_resources: &CallbackResources,
    ) {
        let Some(resources) = callback_resources.get::<HdrCallbackResources>() else {
            return;
        };
        let tile_key = HdrTileKey::from_tile_with_uv(&self.tile, self.uv_rect);
        let Some(bind_group) = resources.tile_bindings.bind_group(tile_key) else {
            return;
        };

        let viewport = info.viewport_in_pixels();
        render_pass.set_viewport(
            viewport.left_px as f32,
            viewport.top_px as f32,
            viewport.width_px as f32,
            viewport.height_px as f32,
            0.0,
            1.0,
        );
        let scissor = info.clip_rect_in_pixels();
        render_pass.set_scissor_rect(
            scissor.left_px.max(0) as u32,
            scissor.top_px.max(0) as u32,
            scissor.width_px.max(0) as u32,
            scissor.height_px.max(0) as u32,
        );
        render_pass.set_pipeline(&resources.pipeline);
        render_pass.set_bind_group(0, bind_group, &[]);
        render_pass.draw(0..3, 0..1);
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ToneMapUniform {
    exposure_ev: f32,
    sdr_white_nits: f32,
    max_display_nits: f32,
    native_display_scale: f32,
    rotation_steps: u32,
    alpha: f32,
    output_mode: u32,
    input_color_space: u32,
    input_transfer_function: u32,
    input_reference: u32,
    /// See WGSL [`ToneMapSettings::sdr_manual_srgb_encode`].
    sdr_manual_srgb_encode: u32,
    /// Matches WGSL uniform layout: `uv_min` starts at byte 48 (8-byte aligned).
    _wgsl_pad_before_uv: u32,
    uv_min: [f32; 2],
    uv_max: [f32; 2],
    apple_compose: u32,
    headroom_span: f32,
    weight: f32,
    gain_width: u32,
    gain_height: u32,
    primary_width: u32,
    primary_height: u32,
    _apple_pad: u32,
}

unsafe impl bytemuck::Zeroable for ToneMapUniform {}
unsafe impl bytemuck::Pod for ToneMapUniform {}

const _: () = assert!(std::mem::size_of::<ToneMapUniform>() == 96);

impl ToneMapUniform {
    fn from_settings(
        settings: HdrToneMapSettings,
        rotation_steps: u32,
        alpha: f32,
        output_mode: HdrRenderOutputMode,
        framebuffer_format: wgpu::TextureFormat,
        input_color_space: HdrColorSpace,
        input_transfer_function: HdrTransferFunction,
        input_reference: HdrReference,
        uv_rect: egui::Rect,
        native_display_scale: f32,
        apple: Option<(&crate::hdr::types::AppleHeicGainMapGpuSource, u32, u32, f32)>,
    ) -> Self {
        let manual_srgb = output_mode == HdrRenderOutputMode::SdrToneMapped
            && hdr_sdr_framebuffer_needs_manual_srgb_oetf(framebuffer_format);
        let (
            apple_compose,
            headroom_span,
            weight,
            gain_width,
            gain_height,
            primary_width,
            primary_height,
        ) = if let Some((deferred, primary_w, primary_h, target_capacity)) = apple {
            (
                1,
                deferred.headroom_span,
                #[cfg(feature = "heif-native")]
                apple_gain_map_display_weight(target_capacity, deferred.stops),
                #[cfg(not(feature = "heif-native"))]
                0.0_f32,
                deferred.gain_width,
                deferred.gain_height,
                primary_w,
                primary_h,
            )
        } else {
            (0, 0.0, 0.0, 0, 0, 0, 0)
        };
        Self {
            exposure_ev: settings.exposure_ev,
            sdr_white_nits: settings.sdr_white_nits,
            max_display_nits: settings.max_display_nits,
            native_display_scale: native_display_scale.clamp(0.0, f32::MAX),
            rotation_steps: rotation_steps % 4,
            alpha: alpha.clamp(0.0, 1.0),
            output_mode: output_mode as u32,
            input_color_space: input_color_space as u32,
            input_transfer_function: input_transfer_function as u32,
            input_reference: input_reference as u32,
            sdr_manual_srgb_encode: manual_srgb as u32,
            _wgsl_pad_before_uv: 0,
            uv_min: [uv_rect.min.x, uv_rect.min.y],
            uv_max: [uv_rect.max.x, uv_rect.max.y],
            apple_compose,
            headroom_span,
            weight,
            gain_width,
            gain_height,
            primary_width,
            primary_height,
            _apple_pad: 0,
        }
    }
}

/// Peak scaler for **libavif** `avifImageApplyGainMap` output: display-referred linear in ~0–1,
/// same factor as the first step of `encode_sdr` so Native HDR is not hotter than the SDR path.
fn libavif_tone_map_native_display_scale(
    metadata: &HdrImageMetadata,
    color_space: HdrColorSpace,
    tone: &HdrToneMapSettings,
) -> f32 {
    let capped = metadata
        .gain_map
        .as_ref()
        .is_some_and(|g| g.capped_display_referred);
    if !capped {
        return 1.0;
    }
    if metadata.transfer_function != HdrTransferFunction::Linear
        || color_space != HdrColorSpace::LinearSrgb
    {
        return 1.0;
    }
    tone.sdr_white_nits / tone.max_display_nits.max(tone.sdr_white_nits)
}

fn tile_tone_map_uniform(
    settings: HdrToneMapSettings,
    rotation_steps: u32,
    alpha: f32,
    output_mode: HdrRenderOutputMode,
    framebuffer_format: wgpu::TextureFormat,
    input_color_space: HdrColorSpace,
    input_transfer_function: HdrTransferFunction,
    input_reference: HdrReference,
    uv_rect: egui::Rect,
    native_display_scale: f32,
) -> ToneMapUniform {
    ToneMapUniform::from_settings(
        settings,
        rotation_steps,
        alpha,
        output_mode,
        framebuffer_format,
        input_color_space,
        input_transfer_function,
        input_reference,
        uv_rect,
        native_display_scale,
        None,
    )
}

fn image_tone_map_uniform(
    image: &HdrImageBuffer,
    settings: HdrToneMapSettings,
    rotation_steps: u32,
    alpha: f32,
    output_mode: HdrRenderOutputMode,
    framebuffer_format: wgpu::TextureFormat,
    uv_rect: egui::Rect,
    native_display_scale: f32,
    apple_gpu_composed: bool,
) -> ToneMapUniform {
    if apple_gpu_composed {
        return ToneMapUniform::from_settings(
            settings,
            rotation_steps,
            alpha,
            output_mode,
            framebuffer_format,
            HdrColorSpace::LinearSrgb,
            HdrTransferFunction::Linear,
            HdrReference::Unknown,
            uv_rect,
            native_display_scale,
            None,
        );
    }

    ToneMapUniform::from_settings(
        settings,
        rotation_steps,
        alpha,
        output_mode,
        framebuffer_format,
        image.metadata.color_space_hint(),
        image.metadata.transfer_function,
        image.metadata.reference,
        uv_rect,
        native_display_scale,
        None,
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HdrImageKey {
    width: u32,
    height: u32,
    format: HdrPixelFormat,
    rgba_ptr: usize,
    rgba_len: usize,
    apple_deferred_ptr: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct HdrTileKey {
    cache_id: u64,
    width: u32,
    height: u32,
    rgba_len: usize,
    uv_min_bits: [u32; 2],
    uv_max_bits: [u32; 2],
}

impl HdrTileKey {
    #[allow(dead_code)]
    fn from_tile(tile: &crate::hdr::tiled::HdrTileBuffer) -> Self {
        Self::from_tile_with_uv(
            tile,
            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
        )
    }

    fn from_tile_with_uv(tile: &crate::hdr::tiled::HdrTileBuffer, uv_rect: egui::Rect) -> Self {
        Self {
            cache_id: tile.cache_id,
            width: tile.width,
            height: tile.height,
            rgba_len: tile.rgba_f32.len(),
            uv_min_bits: [uv_rect.min.x.to_bits(), uv_rect.min.y.to_bits()],
            uv_max_bits: [uv_rect.max.x.to_bits(), uv_rect.max.y.to_bits()],
        }
    }
}

impl HdrImageKey {
    fn from_image(image: &HdrImageBuffer) -> Self {
        Self {
            width: image.width,
            height: image.height,
            format: image.format,
            rgba_ptr: std::sync::Arc::as_ptr(&image.rgba_f32) as usize,
            rgba_len: image.rgba_f32.len(),
            apple_deferred_ptr: image
                .metadata
                .gain_map
                .as_ref()
                .and_then(|gm| gm.apple_heic_deferred.as_ref())
                .map(|d| std::sync::Arc::as_ptr(&d.gain_rgba) as usize),
        }
    }
}

struct HdrCallbackResources {
    target_format: wgpu::TextureFormat,
    bind_group_layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
    tone_map_buffer: wgpu::Buffer,
    #[allow(dead_code)]
    dummy_gain_texture: wgpu::Texture,
    dummy_gain_view: wgpu::TextureView,
    uploaded_image_key: Option<HdrImageKey>,
    tile_bindings: HdrTileBindings,
    uploaded_texture: Option<wgpu::Texture>,
    uploaded_view: Option<wgpu::TextureView>,
    uploaded_gain_texture: Option<wgpu::Texture>,
    uploaded_gain_view: Option<wgpu::TextureView>,
    #[cfg(feature = "heif-native")]
    uploaded_display_storage_view: Option<wgpu::TextureView>,
    #[cfg(feature = "heif-native")]
    compose_bind_group_layout: wgpu::BindGroupLayout,
    #[cfg(feature = "heif-native")]
    compose_pipeline: wgpu::ComputePipeline,
    #[cfg(feature = "heif-native")]
    compose_tone_map_buffer: wgpu::Buffer,
    #[cfg(feature = "heif-native")]
    encoded_primary_buffer: Option<wgpu::Buffer>,
    #[cfg(feature = "heif-native")]
    encoded_primary_buffer_bytes: usize,
    #[cfg(feature = "heif-native")]
    encoded_primary_source_ptr: Option<usize>,
    baked_apple_image_key: Option<HdrImageKey>,
    baked_apple_weight_bits: Option<u32>,
    bind_group: Option<wgpu::BindGroup>,
}

struct CallbackUpload {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    #[cfg(feature = "heif-native")]
    storage_view: Option<wgpu::TextureView>,
}

struct ImagePlaneUpload {
    base: CallbackUpload,
    gain: Option<CallbackUpload>,
}

const HDR_APPLE_GAIN_TEXTURE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

fn create_dummy_gain_texture(device: &wgpu::Device) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("simple-image-viewer-hdr-dummy-gain-texture"),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: HDR_APPLE_GAIN_TEXTURE_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

fn create_callback_resources(
    device: &wgpu::Device,
    target_format: wgpu::TextureFormat,
) -> HdrCallbackResources {
    let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("simple-image-viewer-hdr-image-plane-bind-group-layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("simple-image-viewer-hdr-image-plane-pipeline-layout"),
        bind_group_layouts: &[Some(&bind_group_layout)],
        immediate_size: 0,
    });
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("simple-image-viewer-hdr-image-plane-shader"),
        source: wgpu::ShaderSource::Wgsl(HDR_IMAGE_PLANE_SHADER.into()),
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("simple-image-viewer-hdr-image-plane-pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            polygon_mode: wgpu::PolygonMode::Fill,
            unclipped_depth: false,
            conservative: false,
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: target_format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    });
    let tone_map_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("simple-image-viewer-hdr-image-plane-tone-map-buffer"),
        contents: bytemuck::bytes_of(&ToneMapUniform::from_settings(
            HdrToneMapSettings::default(),
            0,
            1.0,
            HdrRenderOutputMode::SdrToneMapped,
            target_format,
            HdrColorSpace::LinearSrgb,
            HdrTransferFunction::Linear,
            HdrReference::Unknown,
            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
            1.0,
            None,
        )),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    });
    let (dummy_gain_texture, dummy_gain_view) = create_dummy_gain_texture(device);

    #[cfg(feature = "heif-native")]
    let (compose_bind_group_layout, compose_pipeline, compose_tone_map_buffer) =
        apple_compose_gpu::create_compose_compute_resources(device);

    HdrCallbackResources {
        target_format,
        bind_group_layout,
        pipeline,
        tone_map_buffer,
        dummy_gain_texture,
        dummy_gain_view,
        uploaded_image_key: None,
        tile_bindings: HdrTileBindings::default(),
        uploaded_texture: None,
        uploaded_view: None,
        uploaded_gain_texture: None,
        uploaded_gain_view: None,
        #[cfg(feature = "heif-native")]
        uploaded_display_storage_view: None,
        #[cfg(feature = "heif-native")]
        compose_bind_group_layout,
        #[cfg(feature = "heif-native")]
        compose_pipeline,
        #[cfg(feature = "heif-native")]
        compose_tone_map_buffer,
        #[cfg(feature = "heif-native")]
        encoded_primary_buffer: None,
        #[cfg(feature = "heif-native")]
        encoded_primary_buffer_bytes: 0,
        #[cfg(feature = "heif-native")]
        encoded_primary_source_ptr: None,
        baked_apple_image_key: None,
        baked_apple_weight_bits: None,
        bind_group: None,
    }
}

struct HdrTileBindings {
    entries: HashMap<HdrTileKey, HdrTileBinding>,
    lru: VecDeque<HdrTileKey>,
    protected_recent: HashSet<HdrTileKey>,
    protected_order: VecDeque<HdrTileKey>,
    current_bytes: usize,
    max_bytes: usize,
}

const HDR_TILE_BINDING_RECENT_PROTECTION_COUNT: usize = 512;

impl Default for HdrTileBindings {
    fn default() -> Self {
        Self::with_budget(crate::hdr::tiled::configured_hdr_tile_cache_max_bytes())
    }
}

impl HdrTileBindings {
    fn with_budget(max_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            lru: VecDeque::new(),
            protected_recent: HashSet::new(),
            protected_order: VecDeque::new(),
            current_bytes: 0,
            max_bytes,
        }
    }

    fn contains(&mut self, key: HdrTileKey) -> bool {
        if self.entries.contains_key(&key) {
            self.touch(key);
            self.protect_recent(key);
            true
        } else {
            false
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }

    fn insert(
        &mut self,
        key: HdrTileKey,
        texture: wgpu::Texture,
        view: wgpu::TextureView,
        tone_map_buffer: wgpu::Buffer,
        bind_group: wgpu::BindGroup,
    ) {
        self.protect_recent(key);
        self.insert_binding(
            key,
            HdrTileBinding {
                _texture: Some(texture),
                _view: Some(view),
                tone_map_buffer: Some(tone_map_buffer),
                bind_group: Some(bind_group),
                estimated_bytes: 0,
            },
        );
    }

    fn insert_binding(&mut self, key: HdrTileKey, binding: HdrTileBinding) {
        if let Some(old_binding) = self.entries.remove(&key) {
            self.current_bytes = self
                .current_bytes
                .saturating_sub(old_binding.estimated_bytes);
            self.lru.retain(|existing| *existing != key);
        }

        let bytes = hdr_tile_key_bytes(key);
        while !self.lru.is_empty() && self.current_bytes.saturating_add(bytes) > self.max_bytes {
            let evict_pos = self
                .lru
                .iter()
                .position(|existing| !self.protected_recent.contains(existing));
            let Some(evict_pos) = evict_pos else {
                break;
            };
            let Some(evicted_key) = self.lru.remove(evict_pos) else {
                break;
            };
            self.protected_recent.remove(&evicted_key);
            self.protected_order
                .retain(|existing| *existing != evicted_key);
            if let Some(evicted_binding) = self.entries.remove(&evicted_key) {
                self.current_bytes = self
                    .current_bytes
                    .saturating_sub(evicted_binding.estimated_bytes);
            }
        }

        if self.current_bytes.saturating_add(bytes) <= self.max_bytes
            || self.protected_recent.contains(&key)
        {
            let mut binding = binding;
            binding.estimated_bytes = bytes;
            self.entries.insert(key, binding);
            self.lru.push_back(key);
            self.current_bytes += bytes;
        }
    }

    fn protect_recent(&mut self, key: HdrTileKey) {
        self.protected_order.retain(|existing| *existing != key);
        self.protected_order.push_back(key);
        self.protected_recent.insert(key);
        while self.protected_order.len() > HDR_TILE_BINDING_RECENT_PROTECTION_COUNT {
            if let Some(expired) = self.protected_order.pop_front() {
                self.protected_recent.remove(&expired);
            }
        }
    }

    fn touch(&mut self, key: HdrTileKey) {
        if let Some(pos) = self.lru.iter().position(|existing| *existing == key) {
            self.lru.remove(pos);
        }
        self.lru.push_back(key);
    }

    #[cfg(test)]
    fn current_bytes(&self) -> usize {
        self.current_bytes
    }

    #[cfg(test)]
    fn insert_placeholder(&mut self, key: HdrTileKey) {
        self.insert_binding(
            key,
            HdrTileBinding {
                _texture: None,
                _view: None,
                tone_map_buffer: None,
                bind_group: None,
                estimated_bytes: 0,
            },
        );
    }

    #[cfg(test)]
    fn insert_protected_placeholder(&mut self, key: HdrTileKey) {
        self.protect_recent(key);
        self.insert_placeholder(key);
    }

    fn remove(&mut self, key: HdrTileKey) {
        if let Some(binding) = self.entries.remove(&key) {
            self.current_bytes = self.current_bytes.saturating_sub(binding.estimated_bytes);
        }
        self.lru.retain(|existing| *existing != key);
        self.protected_recent.remove(&key);
        self.protected_order.retain(|existing| *existing != key);
    }

    fn bind_group(&self, key: HdrTileKey) -> Option<&wgpu::BindGroup> {
        self.entries
            .get(&key)
            .and_then(|entry| entry.bind_group.as_ref())
    }

    fn binding_mut(&mut self, key: HdrTileKey) -> Option<&mut HdrTileBinding> {
        self.entries.get_mut(&key)
    }
}

struct HdrTileBinding {
    _texture: Option<wgpu::Texture>,
    _view: Option<wgpu::TextureView>,
    tone_map_buffer: Option<wgpu::Buffer>,
    bind_group: Option<wgpu::BindGroup>,
    estimated_bytes: usize,
}

fn hdr_tile_key_bytes(key: HdrTileKey) -> usize {
    key.rgba_len * std::mem::size_of::<f32>()
}

#[allow(dead_code)]
fn upload_callback_tile(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    tile: &crate::hdr::tiled::HdrTileBuffer,
) -> Result<CallbackUpload, String> {
    let layout = validate_tile_upload_layout(tile, device.limits().max_texture_dimension_2d)?;
    let (upload_bytes, bytes_per_row) = pack_rows_for_texture_copy(
        rgba32f_as_bytes(tile.rgba_f32.as_slice()),
        tile.width,
        tile.height,
        std::mem::size_of::<f32>() as u32 * 4,
    )
    .map_err(|err| format!("HDR tile upload: {err}"))?;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("simple-image-viewer-hdr-tile-plane-callback-texture"),
        size: layout.size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: layout.format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });

    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &upload_bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(bytes_per_row),
            rows_per_image: Some(layout.size.height),
        },
        layout.size,
    );

    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    Ok(CallbackUpload {
        texture,
        view,
        #[cfg(feature = "heif-native")]
        storage_view: None,
    })
}

fn upload_callback_image(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    image: &HdrImageBuffer,
) -> Result<CallbackUpload, String> {
    let layout = validate_upload_layout(image, device.limits().max_texture_dimension_2d)?;
    let (upload_bytes, bytes_per_row) = pack_rows_for_texture_copy(
        rgba32f_as_bytes(image.rgba_f32.as_slice()),
        image.width,
        image.height,
        std::mem::size_of::<f32>() as u32 * 4,
    )
    .map_err(|err| format!("HDR upload: {err}"))?;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("simple-image-viewer-hdr-image-plane-callback-texture"),
        size: layout.size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: layout.format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });

    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &upload_bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(bytes_per_row),
            rows_per_image: Some(layout.size.height),
        },
        layout.size,
    );

    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    Ok(CallbackUpload {
        texture,
        view,
        #[cfg(feature = "heif-native")]
        storage_view: None,
    })
}

fn wgpu_copy_bytes_per_row(unpadded_bytes_per_row: u32) -> u32 {
    wgpu::util::align_to(unpadded_bytes_per_row, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
}

/// Pack tightly laid-out RGBA rows into the pitch required by [`wgpu::Queue::write_texture`].
fn pack_rows_for_texture_copy(
    tight: &[u8],
    width: u32,
    height: u32,
    bytes_per_pixel: u32,
) -> Result<(Vec<u8>, u32), String> {
    let unpadded_bytes_per_row = width
        .checked_mul(bytes_per_pixel)
        .ok_or_else(|| format!("row byte count overflows for width {width}"))?;
    let bytes_per_row = wgpu_copy_bytes_per_row(unpadded_bytes_per_row);
    let expected_len = unpadded_bytes_per_row
        .checked_mul(height)
        .map(|len| len as usize)
        .ok_or_else(|| format!("tight buffer length overflows for {width}x{height}"))?;
    if tight.len() != expected_len {
        return Err(format!(
            "Malformed tight buffer: expected {expected_len} bytes for {width}x{height}, got {}",
            tight.len()
        ));
    }
    if bytes_per_row == unpadded_bytes_per_row {
        return Ok((tight.to_vec(), bytes_per_row));
    }

    let mut padded = vec![0u8; (bytes_per_row * height) as usize];
    for y in 0..height as usize {
        let src_start = y * unpadded_bytes_per_row as usize;
        let dst_start = y * bytes_per_row as usize;
        padded[dst_start..dst_start + unpadded_bytes_per_row as usize]
            .copy_from_slice(&tight[src_start..src_start + unpadded_bytes_per_row as usize]);
    }
    Ok((padded, bytes_per_row))
}

fn upload_rgba8_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    label: &str,
    width: u32,
    height: u32,
    rgba: &[u8],
    format: wgpu::TextureFormat,
    max_texture_dimension_2d: u32,
) -> Result<CallbackUpload, String> {
    let layout =
        validate_rgba8_upload_layout(width, height, rgba.len(), max_texture_dimension_2d, label)?;
    let (upload_bytes, bytes_per_row) = pack_rows_for_texture_copy(rgba, width, height, 4)
        .map_err(|err| format!("{label}: {err}"))?;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: layout.size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &upload_bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(bytes_per_row),
            rows_per_image: Some(layout.size.height),
        },
        layout.size,
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    Ok(CallbackUpload {
        texture,
        view,
        #[cfg(feature = "heif-native")]
        storage_view: None,
    })
}

fn upload_image_plane(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    image: &HdrImageBuffer,
) -> Result<ImagePlaneUpload, String> {
    #[cfg(feature = "heif-native")]
    if let Some(deferred) = apple_heic_deferred_from_metadata(&image.metadata) {
        let base = create_empty_rgba32f_texture(device, image.width, image.height)?;
        let gain = upload_rgba8_texture(
            device,
            queue,
            "simple-image-viewer-hdr-image-plane-apple-gain-texture",
            deferred.gain_width,
            deferred.gain_height,
            deferred.gain_rgba.as_slice(),
            HDR_APPLE_GAIN_TEXTURE_FORMAT,
            device.limits().max_texture_dimension_2d,
        )?;
        return Ok(ImagePlaneUpload {
            base,
            gain: Some(gain),
        });
    }

    let base = upload_callback_image(device, queue, image)?;
    Ok(ImagePlaneUpload { base, gain: None })
}

#[cfg(feature = "heif-native")]
fn create_empty_rgba32f_texture(
    device: &wgpu::Device,
    width: u32,
    height: u32,
) -> Result<CallbackUpload, String> {
    let layout = validate_rgba32f_upload_layout(
        width,
        height,
        width as usize * height as usize * 4,
        device.limits().max_texture_dimension_2d,
        "HDR deferred display texture",
    )?;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("simple-image-viewer-hdr-image-plane-callback-texture"),
        size: layout.size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: layout.format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let storage_view = texture.create_view(&wgpu::TextureViewDescriptor {
        label: Some("simple-image-viewer-hdr-deferred-display-storage-view"),
        format: Some(wgpu::TextureFormat::Rgba32Float),
        dimension: Some(wgpu::TextureViewDimension::D2),
        aspect: wgpu::TextureAspect::All,
        usage: Some(wgpu::TextureUsages::STORAGE_BINDING),
        ..Default::default()
    });
    Ok(CallbackUpload {
        texture,
        view,
        storage_view: Some(storage_view),
    })
}

fn create_hdr_image_plane_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    hdr_view: &wgpu::TextureView,
    gain_view: &wgpu::TextureView,
    tone_map_buffer: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("simple-image-viewer-hdr-image-plane-bind-group"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(hdr_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(gain_view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: tone_map_buffer.as_entire_binding(),
            },
        ],
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HdrUploadLayout {
    size: wgpu::Extent3d,
    bytes_per_row: u32,
    format: wgpu::TextureFormat,
}

fn validate_upload_layout(
    image: &HdrImageBuffer,
    max_texture_dimension_2d: u32,
) -> Result<HdrUploadLayout, String> {
    if image.format != HdrPixelFormat::Rgba32Float {
        return Err(format!(
            "HDR upload currently supports only Rgba32Float buffers, got {:?}",
            image.format
        ));
    }

    validate_rgba32f_upload_layout(
        image.width,
        image.height,
        image.rgba_f32.len(),
        max_texture_dimension_2d,
        "HDR upload",
    )
}

#[allow(dead_code)]
fn validate_tile_upload_layout(
    tile: &crate::hdr::tiled::HdrTileBuffer,
    max_texture_dimension_2d: u32,
) -> Result<HdrUploadLayout, String> {
    validate_rgba32f_upload_layout(
        tile.width,
        tile.height,
        tile.rgba_f32.len(),
        max_texture_dimension_2d,
        "HDR tile upload",
    )
}

fn validate_rgba32f_upload_layout(
    width: u32,
    height: u32,
    actual_len: usize,
    max_texture_dimension_2d: u32,
    label: &str,
) -> Result<HdrUploadLayout, String> {
    if width == 0 || height == 0 {
        return Err(format!(
            "{label} requires non-zero dimensions, got {width}x{height}"
        ));
    }

    if width > max_texture_dimension_2d || height > max_texture_dimension_2d {
        return Err(format!(
            "{label} dimensions {width}x{height} exceed device max_texture_dimension_2d {max_texture_dimension_2d}",
        ));
    }

    let expected_len = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .map(|len| len as usize)
        .ok_or_else(|| format!("{label} dimensions overflow: {width}x{height}"))?;

    if actual_len != expected_len {
        return Err(format!(
            "Malformed {label} buffer: expected {expected_len} floats for {width}x{height} RGBA, got {actual_len}",
        ));
    }

    let bytes_per_row = wgpu_copy_bytes_per_row(
        width
            .checked_mul(4)
            .and_then(|channels| channels.checked_mul(std::mem::size_of::<f32>() as u32))
            .ok_or_else(|| format!("{label} row byte count overflows for width {width}"))?,
    );

    Ok(HdrUploadLayout {
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        bytes_per_row,
        format: HDR_IMAGE_PLANE_TEXTURE_FORMAT,
    })
}

fn validate_rgba8_upload_layout(
    width: u32,
    height: u32,
    actual_len: usize,
    max_texture_dimension_2d: u32,
    label: &str,
) -> Result<HdrUploadLayout, String> {
    if width == 0 || height == 0 {
        return Err(format!(
            "{label} requires non-zero dimensions, got {width}x{height}"
        ));
    }

    if width > max_texture_dimension_2d || height > max_texture_dimension_2d {
        return Err(format!(
            "{label} dimensions {width}x{height} exceed device max_texture_dimension_2d {max_texture_dimension_2d}",
        ));
    }

    let expected_len = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .map(|len| len as usize)
        .ok_or_else(|| format!("{label} dimensions overflow: {width}x{height}"))?;

    if actual_len != expected_len {
        return Err(format!(
            "Malformed {label} buffer: expected {expected_len} bytes for {width}x{height} RGBA, got {actual_len}",
        ));
    }

    let bytes_per_row = wgpu_copy_bytes_per_row(
        width
            .checked_mul(4)
            .ok_or_else(|| format!("{label} row byte count overflows for width {width}"))?,
    );

    Ok(HdrUploadLayout {
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        bytes_per_row,
        format: wgpu::TextureFormat::Rgba8Unorm,
    })
}

fn rgba32f_as_bytes(values: &[f32]) -> &[u8] {
    bytemuck::cast_slice(values)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hdr::tiled::HdrTileBuffer;
    use crate::hdr::types::{
        HdrColorSpace, HdrGainMapMetadata, HdrImageBuffer, HdrImageMetadata, HdrPixelFormat,
        HdrReference, HdrToneMapSettings, HdrTransferFunction,
    };
    use std::sync::Arc;

    #[test]
    fn renderer_starts_without_uploaded_image_state() {
        let renderer = HdrImageRenderer::new();

        assert!(renderer.uploaded_image().is_none());
        assert_eq!(
            HDR_IMAGE_PLANE_TEXTURE_FORMAT,
            wgpu::TextureFormat::Rgba32Float
        );
    }

    #[test]
    fn upload_layout_matches_rgba32f_rows() {
        let image = hdr_image(3, 2, HdrPixelFormat::Rgba32Float, vec![0.0; 3 * 2 * 4]);

        let layout = validate_upload_layout(&image, 4096).expect("valid upload layout");

        assert_eq!(layout.size.width, 3);
        assert_eq!(layout.size.height, 2);
        assert_eq!(
            layout.bytes_per_row,
            wgpu::util::align_to(
                3 * 4 * std::mem::size_of::<f32>() as u32,
                wgpu::COPY_BYTES_PER_ROW_ALIGNMENT
            )
        );
        assert_eq!(layout.format, wgpu::TextureFormat::Rgba32Float);
    }

    #[test]
    fn rgba8_upload_layout_aligns_row_pitch_to_wgpu_copy_requirement() {
        let width = 3024;
        let height = 4032;
        let layout = validate_rgba8_upload_layout(
            width,
            height,
            width as usize * height as usize * 4,
            8192,
            "HEIC base upload",
        )
        .expect("valid rgba8 upload layout");

        assert_eq!(
            layout.bytes_per_row,
            wgpu::util::align_to(width * 4, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        );
        assert_eq!(layout.bytes_per_row % wgpu::COPY_BYTES_PER_ROW_ALIGNMENT, 0);
    }

    #[test]
    fn pack_rows_for_texture_copy_inserts_row_padding_when_required() {
        let width = 3024;
        let height = 2;
        let unpadded = (width * 4) as usize;
        let mut tight = vec![0u8; unpadded * height as usize];
        for y in 0..height {
            tight[y as usize * unpadded] = 100 + y as u8;
        }

        let (padded, bytes_per_row) =
            pack_rows_for_texture_copy(&tight, width, height, 4).expect("pack rows");

        assert_eq!(
            bytes_per_row,
            wgpu::util::align_to(width * 4, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        );
        assert_eq!(padded.len(), bytes_per_row as usize * height as usize);
        assert_eq!(padded[0], 100);
        assert_eq!(padded[bytes_per_row as usize], 101);
    }

    #[test]
    fn pack_rows_rgba32f_round_trip_preserves_data() {
        // bytes_per_pixel=16 → unpadded row = width * 16 bytes.
        // Test widths that are / are not multiples of 16 (alignment boundary).
        for &(width, height) in &[(13, 7), (16, 4), (17, 11), (64, 64), (4033, 3)] {
            let pixel_count = width as usize * height as usize * 4;
            let original: Vec<f32> = (0..pixel_count)
                .map(|i| (i as f32 * 0.12345 + 0.001).sin() * 2.0)
                .collect();
            let tight: &[u8] = bytemuck::cast_slice(&original);
            assert_eq!(tight.len(), width as usize * height as usize * 16);

            let (padded, bytes_per_row) =
                pack_rows_for_texture_copy(tight, width, height, 16).expect("pack rows");

            assert_eq!(bytes_per_row % wgpu::COPY_BYTES_PER_ROW_ALIGNMENT, 0);
            assert!(bytes_per_row >= width * 16);
            assert_eq!(padded.len(), bytes_per_row as usize * height as usize);

            // Simulate what `write_texture` does: extract each row's data (width * 16 bytes)
            // from the padded buffer, skipping padding.
            let unpadded_row = (width * 16) as usize;
            let mut unpacked = Vec::with_capacity(tight.len());
            for y in 0..height as usize {
                let src_start = y * bytes_per_row as usize;
                unpacked.extend_from_slice(&padded[src_start..src_start + unpadded_row]);
            }
            assert_eq!(unpacked, tight, "round-trip failed for {width}x{height}");
        }
    }

    #[test]
    fn upload_layout_rejects_zero_dimensions() {
        let image = hdr_image(0, 1, HdrPixelFormat::Rgba32Float, Vec::new());

        let err = validate_upload_layout(&image, 4096).expect_err("reject zero-width upload");

        assert!(err.contains("non-zero"));
    }

    #[test]
    fn upload_layout_rejects_malformed_buffer_length() {
        let image = hdr_image(2, 1, HdrPixelFormat::Rgba32Float, vec![0.0; 7]);

        let err = validate_upload_layout(&image, 4096).expect_err("reject malformed upload");

        assert!(err.contains("expected 8 floats"));
        assert!(err.contains("got 7"));
    }

    #[test]
    fn upload_layout_rejects_unsupported_cpu_format() {
        let image = hdr_image(1, 1, HdrPixelFormat::Rgba16Float, vec![0.0; 4]);

        let err = validate_upload_layout(&image, 4096).expect_err("reject unsupported format");

        assert!(err.contains("Rgba32Float"));
    }

    #[test]
    fn upload_layout_rejects_device_texture_limit_overflow() {
        let image = hdr_image(
            2049,
            1024,
            HdrPixelFormat::Rgba32Float,
            vec![0.0; 2049 * 1024 * 4],
        );

        let err = validate_upload_layout(&image, 2048).expect_err("reject texture limit overflow");

        assert!(err.contains("exceed"));
        assert!(err.contains("2048"));
    }

    #[test]
    fn tile_upload_layout_matches_rgba32f_rows() {
        let tile = hdr_tile(7, 5, vec![0.0; 7 * 5 * 4]);

        let layout = validate_tile_upload_layout(&tile, 4096).expect("valid tile upload layout");

        assert_eq!(layout.size.width, 7);
        assert_eq!(layout.size.height, 5);
        assert_eq!(
            layout.bytes_per_row,
            wgpu::util::align_to(
                7 * 4 * std::mem::size_of::<f32>() as u32,
                wgpu::COPY_BYTES_PER_ROW_ALIGNMENT
            )
        );
        assert_eq!(layout.format, wgpu::TextureFormat::Rgba32Float);
    }

    #[test]
    fn tile_upload_layout_rejects_malformed_buffer_length() {
        let tile = hdr_tile(2, 2, vec![0.0; 15]);

        let err = validate_tile_upload_layout(&tile, 4096).expect_err("reject malformed tile");

        assert!(err.contains("expected 16 floats"));
        assert!(err.contains("got 15"));
    }

    #[test]
    fn hdr_tile_plane_callback_returns_paint_callback_shape() {
        let tile = Arc::new(hdr_tile(1, 1, vec![1.0, 1.0, 1.0, 1.0]));
        let shape = hdr_tile_plane_callback(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(10.0, 10.0)),
            tile,
            HdrToneMapSettings::default(),
            wgpu::TextureFormat::Rgba16Float,
            HdrRenderOutputMode::NativeHdr,
            0,
            1.0,
        );

        assert!(matches!(shape, egui::Shape::Callback(_)));
    }

    #[test]
    fn tone_map_uniform_byte_size_matches_wgpu_shader() {
        assert_eq!(std::mem::size_of::<ToneMapUniform>(), 96);
    }

    #[test]
    fn libavif_tone_map_native_display_scale_matches_encode_sdr_peak_scaler() {
        let mut metadata = HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb);
        metadata.gain_map = Some(HdrGainMapMetadata {
            source: "AVIF",
            target_hdr_capacity: Some(4.0),
            diagnostic: String::new(),
            capped_display_referred: true,
            apple_heic_deferred: None,
        });
        let tone = HdrToneMapSettings {
            sdr_white_nits: 203.0,
            max_display_nits: 1000.0,
            ..HdrToneMapSettings::default()
        };
        let s = libavif_tone_map_native_display_scale(&metadata, HdrColorSpace::LinearSrgb, &tone);
        assert!((s - 203.0 / 1000.0).abs() < 1e-5);
    }

    #[test]
    fn tile_tone_map_uniform_carries_rotation() {
        let uniform = tile_tone_map_uniform(
            HdrToneMapSettings::default(),
            6,
            0.5,
            HdrRenderOutputMode::NativeHdr,
            wgpu::TextureFormat::Rgba16Float,
            HdrColorSpace::LinearSrgb,
            HdrTransferFunction::Linear,
            HdrReference::Unknown,
            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
            1.0,
        );

        assert_eq!(uniform.rotation_steps, 2);
        assert_eq!(uniform.alpha, 0.5);
        assert_eq!(uniform.output_mode, HdrRenderOutputMode::NativeHdr as u32);
    }

    #[test]
    fn tile_tone_map_uniform_carries_uv_subrect() {
        let uniform = tile_tone_map_uniform(
            HdrToneMapSettings::default(),
            0,
            1.0,
            HdrRenderOutputMode::NativeHdr,
            wgpu::TextureFormat::Rgba16Float,
            HdrColorSpace::LinearSrgb,
            HdrTransferFunction::Linear,
            HdrReference::Unknown,
            egui::Rect::from_min_max(egui::Pos2::new(0.25, 0.5), egui::Pos2::new(0.75, 1.0)),
            1.0,
        );

        assert_eq!(uniform.uv_min, [0.25, 0.5]);
        assert_eq!(uniform.uv_max, [0.75, 1.0]);
    }

    #[test]
    fn image_and_tile_uniforms_share_transform_output_and_color_space_logic() {
        let settings = HdrToneMapSettings {
            exposure_ev: 1.0,
            sdr_white_nits: 203.0,
            max_display_nits: 1000.0,
        };

        let image = HdrImageBuffer {
            width: 1,
            height: 1,
            format: HdrPixelFormat::Rgba32Float,
            color_space: HdrColorSpace::Rec2020Linear,
            metadata: HdrImageMetadata {
                transfer_function: HdrTransferFunction::Linear,
                reference: HdrReference::Unknown,
                ..HdrImageMetadata::from_color_space(HdrColorSpace::Rec2020Linear)
            },
            rgba_f32: Arc::new(vec![1.0, 0.0, 0.0, 1.0]),
        };

        let image_uniform = image_tone_map_uniform(
            &image,
            settings,
            5,
            0.75,
            HdrRenderOutputMode::SdrToneMapped,
            wgpu::TextureFormat::Bgra8UnormSrgb,
            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
            1.0,
            false,
        );
        let tile_uniform = tile_tone_map_uniform(
            settings,
            5,
            0.75,
            HdrRenderOutputMode::SdrToneMapped,
            wgpu::TextureFormat::Bgra8UnormSrgb,
            HdrColorSpace::Rec2020Linear,
            HdrTransferFunction::Linear,
            HdrReference::Unknown,
            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
            1.0,
        );

        assert_eq!(image_uniform.rotation_steps, tile_uniform.rotation_steps);
        assert_eq!(image_uniform.alpha, tile_uniform.alpha);
        assert_eq!(image_uniform.output_mode, tile_uniform.output_mode);
        assert_eq!(
            image_uniform.input_color_space,
            tile_uniform.input_color_space
        );
        assert_eq!(
            image_uniform.output_mode,
            HdrRenderOutputMode::SdrToneMapped as u32
        );
        assert_eq!(image_uniform.sdr_manual_srgb_encode, 0);
        assert_eq!(tile_uniform.sdr_manual_srgb_encode, 0);
    }

    #[test]
    fn tone_map_manual_srgb_oetf_plain_unorm_only() {
        assert!(
            crate::hdr::renderer::hdr_sdr_framebuffer_needs_manual_srgb_oetf(
                wgpu::TextureFormat::Bgra8Unorm
            )
        );
        assert!(
            crate::hdr::renderer::hdr_sdr_framebuffer_needs_manual_srgb_oetf(
                wgpu::TextureFormat::Rgba8Unorm
            )
        );
        assert!(
            !crate::hdr::renderer::hdr_sdr_framebuffer_needs_manual_srgb_oetf(
                wgpu::TextureFormat::Bgra8UnormSrgb
            )
        );
        assert!(
            !crate::hdr::renderer::hdr_sdr_framebuffer_needs_manual_srgb_oetf(
                wgpu::TextureFormat::Rgba8UnormSrgb
            )
        );
    }

    #[test]
    fn render_output_diagnostics_distinguish_native_hdr_and_sdr_tone_mapping() {
        assert_eq!(
            hdr_render_output_diagnostics(Some(wgpu::TextureFormat::Rgba16Float)),
            [
                "[HDR] render_target_format=Some(Rgba16Float)",
                "[HDR] shader_output_mode=native_hdr",
            ]
        );
        assert_eq!(
            hdr_render_output_diagnostics(Some(wgpu::TextureFormat::Bgra8Unorm)),
            [
                "[HDR] render_target_format=Some(Bgra8Unorm)",
                "[HDR] shader_output_mode=sdr_tone_mapped",
            ]
        );
        assert_eq!(
            hdr_render_output_diagnostics(None),
            [
                "[HDR] render_target_format=None",
                "[HDR] shader_output_mode=unknown",
            ]
        );
    }

    #[test]
    fn egui_overlay_diagnostics_report_linear_sdr_ui_on_hdr_float_target() {
        assert_eq!(
            hdr_egui_overlay_diagnostics(Some(wgpu::TextureFormat::Rgba16Float)),
            [
                "[HDR] egui_overlay_target_format=Some(Rgba16Float)",
                "[HDR] egui_overlay_framebuffer_shader=fs_main_linear_framebuffer",
            ]
        );
        assert_eq!(
            hdr_egui_overlay_diagnostics(Some(wgpu::TextureFormat::Bgra8Unorm)),
            [
                "[HDR] egui_overlay_target_format=Some(Bgra8Unorm)",
                "[HDR] egui_overlay_framebuffer_shader=fs_main_gamma_framebuffer",
            ]
        );
    }

    #[test]
    fn hdr_tile_keys_distinguish_equal_size_tile_buffers() {
        let first = hdr_tile(1, 1, vec![1.0, 0.0, 0.0, 1.0]);
        let second = hdr_tile(1, 1, vec![0.0, 1.0, 0.0, 1.0]);

        assert_ne!(
            HdrTileKey::from_tile(&first),
            HdrTileKey::from_tile(&second)
        );
    }

    #[test]
    fn hdr_tile_keys_distinguish_logical_tiles_even_when_rgba_allocation_matches() {
        let rgba = Arc::new(vec![1.0, 0.0, 0.0, 1.0]);
        let first = HdrTileBuffer::new(1, 1, HdrColorSpace::LinearSrgb, Arc::clone(&rgba));
        let second = HdrTileBuffer::new(1, 1, HdrColorSpace::LinearSrgb, rgba);

        assert_ne!(
            HdrTileKey::from_tile(&first),
            HdrTileKey::from_tile(&second)
        );
    }

    #[test]
    fn hdr_tile_keys_distinguish_uv_subrects() {
        let tile = hdr_tile(2, 2, vec![1.0; 2 * 2 * 4]);
        let full = HdrTileKey::from_tile_with_uv(
            &tile,
            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
        );
        let clipped = HdrTileKey::from_tile_with_uv(
            &tile,
            egui::Rect::from_min_max(egui::Pos2::new(0.5, 0.0), egui::Pos2::new(1.0, 1.0)),
        );

        assert_ne!(full, clipped);
    }

    #[test]
    fn callback_resources_store_independent_tile_bind_groups() {
        let first = HdrTileKey::from_tile(&hdr_tile(1, 1, vec![1.0, 0.0, 0.0, 1.0]));
        let second = HdrTileKey::from_tile(&hdr_tile(1, 1, vec![0.0, 1.0, 0.0, 1.0]));
        let mut resources = HdrTileBindings::default();

        resources.insert_placeholder(first);
        resources.insert_placeholder(second);

        assert!(resources.contains(first));
        assert!(resources.contains(second));
        assert_eq!(resources.len(), 2);
    }

    #[test]
    fn callback_resources_evict_lru_tile_bindings_when_over_budget() {
        let first = HdrTileKey::from_tile(&hdr_tile(1, 1, vec![1.0, 0.0, 0.0, 1.0]));
        let second = HdrTileKey::from_tile(&hdr_tile(1, 1, vec![0.0, 1.0, 0.0, 1.0]));
        let third = HdrTileKey::from_tile(&hdr_tile(1, 1, vec![0.0, 0.0, 1.0, 1.0]));
        let mut resources = HdrTileBindings::with_budget(2 * hdr_tile_key_bytes(first));

        resources.insert_placeholder(first);
        resources.insert_placeholder(second);
        resources.insert_placeholder(third);

        assert!(!resources.contains(first));
        assert!(resources.contains(second));
        assert!(resources.contains(third));
        assert_eq!(resources.len(), 2);
        assert!(resources.current_bytes() <= 2 * hdr_tile_key_bytes(first));
    }

    #[test]
    fn callback_resources_keep_recently_prepared_tile_bindings_over_budget() {
        let first = HdrTileKey::from_tile(&hdr_tile(1, 1, vec![1.0, 0.0, 0.0, 1.0]));
        let second = HdrTileKey::from_tile(&hdr_tile(1, 1, vec![0.0, 1.0, 0.0, 1.0]));
        let third = HdrTileKey::from_tile(&hdr_tile(1, 1, vec![0.0, 0.0, 1.0, 1.0]));
        let mut resources = HdrTileBindings::with_budget(2 * hdr_tile_key_bytes(first));

        resources.insert_protected_placeholder(first);
        resources.insert_protected_placeholder(second);
        resources.insert_protected_placeholder(third);

        assert!(resources.contains(first));
        assert!(resources.contains(second));
        assert!(resources.contains(third));
        assert_eq!(resources.len(), 3);
        assert!(resources.current_bytes() > 2 * hdr_tile_key_bytes(first));
    }

    #[test]
    fn callback_resources_refresh_lru_on_existing_tile_binding() {
        let first = HdrTileKey::from_tile(&hdr_tile(1, 1, vec![1.0, 0.0, 0.0, 1.0]));
        let second = HdrTileKey::from_tile(&hdr_tile(1, 1, vec![0.0, 1.0, 0.0, 1.0]));
        let third = HdrTileKey::from_tile(&hdr_tile(1, 1, vec![0.0, 0.0, 1.0, 1.0]));
        let mut resources = HdrTileBindings::with_budget(2 * hdr_tile_key_bytes(first));

        resources.insert_placeholder(first);
        resources.insert_placeholder(second);
        assert!(resources.contains(first));
        resources.insert_placeholder(third);

        assert!(resources.contains(first));
        assert!(!resources.contains(second));
        assert!(resources.contains(third));
    }

    #[test]
    fn shader_sanitizes_non_finite_hdr_rgb_before_tone_mapping() {
        assert!(HDR_IMAGE_PLANE_SHADER.contains("fn sanitize_hdr_rgb"));
        assert!(HDR_IMAGE_PLANE_SHADER.contains("safe.r != safe.r"));
        assert!(HDR_IMAGE_PLANE_SHADER.contains("const MAX_FINITE_HDR_VALUE: f32"));
        assert!(HDR_IMAGE_PLANE_SHADER.contains("clamp("));
    }

    #[test]
    fn shader_names_tone_map_numeric_constants() {
        assert!(HDR_IMAGE_PLANE_SHADER.contains("const INVERSE_DISPLAY_GAMMA: f32"));
        assert!(HDR_IMAGE_PLANE_SHADER.contains("const MAX_UV_CLAMP: f32"));
        assert!(HDR_IMAGE_PLANE_SHADER.contains("vec3<f32>(INVERSE_DISPLAY_GAMMA)"));
        assert!(HDR_IMAGE_PLANE_SHADER.contains("vec2<f32>(MAX_UV_CLAMP)"));
    }

    #[test]
    fn rgba32f_byte_view_does_not_allocate_or_copy() {
        let values = [1.0, -2.5, 0.25, f32::INFINITY];

        let bytes = rgba32f_as_bytes(&values);

        assert_eq!(bytes.len(), values.len() * std::mem::size_of::<f32>());
        assert_eq!(bytes.as_ptr(), values.as_ptr().cast::<u8>());
        assert_eq!(&bytes[0..4], &1.0_f32.to_ne_bytes());
    }

    #[test]
    fn tone_map_uniform_carries_rotation_and_alpha() {
        let uniform = ToneMapUniform::from_settings(
            HdrToneMapSettings::default(),
            5,
            0.25,
            HdrRenderOutputMode::SdrToneMapped,
            wgpu::TextureFormat::Bgra8Unorm,
            HdrColorSpace::LinearSrgb,
            HdrTransferFunction::Linear,
            HdrReference::Unknown,
            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
            1.0,
            None,
        );

        assert_eq!(uniform.rotation_steps, 1);
        assert_eq!(uniform.alpha, 0.25);
        assert_eq!(uniform.sdr_manual_srgb_encode, 1);
    }

    #[test]
    fn render_mode_uses_native_hdr_for_float_and_pq_targets() {
        use crate::hdr::monitor::HdrNativeSurfaceEncoding;
        assert_eq!(
            HdrRenderOutputMode::for_target_format(wgpu::TextureFormat::Rgba16Float, None,),
            HdrRenderOutputMode::NativeHdr
        );
        assert_eq!(
            HdrRenderOutputMode::for_target_format(wgpu::TextureFormat::Rgba32Float, None,),
            HdrRenderOutputMode::NativeHdr
        );
        assert_eq!(
            HdrRenderOutputMode::for_target_format(
                wgpu::TextureFormat::Rgb10a2Unorm,
                Some(HdrNativeSurfaceEncoding::PqHdr10),
            ),
            HdrRenderOutputMode::NativeHdrPq
        );
        assert_eq!(
            HdrRenderOutputMode::for_target_format(
                wgpu::TextureFormat::Rgb10a2Unorm,
                Some(HdrNativeSurfaceEncoding::Gamma22Electrical),
            ),
            HdrRenderOutputMode::NativeHdrGamma22
        );
        assert_eq!(
            HdrRenderOutputMode::for_target_format(wgpu::TextureFormat::Rgb10a2Unorm, None,),
            HdrRenderOutputMode::SdrToneMapped
        );
        assert_eq!(
            HdrRenderOutputMode::for_target_format(wgpu::TextureFormat::Bgra8Unorm, None,),
            HdrRenderOutputMode::SdrToneMapped
        );
    }

    #[test]
    fn tone_map_uniform_carries_output_mode() {
        let uniform = ToneMapUniform::from_settings(
            HdrToneMapSettings::default(),
            0,
            1.0,
            HdrRenderOutputMode::NativeHdr,
            wgpu::TextureFormat::Bgra8Unorm,
            HdrColorSpace::Rec2020Linear,
            HdrTransferFunction::Pq,
            HdrReference::DisplayReferred,
            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
            1.0,
            None,
        );

        assert_eq!(uniform.output_mode, HdrRenderOutputMode::NativeHdr as u32);
        assert_eq!(uniform.sdr_manual_srgb_encode, 0);
        assert_eq!(
            uniform.input_color_space,
            HdrColorSpace::Rec2020Linear as u32
        );
        assert_eq!(
            uniform.input_transfer_function,
            HdrTransferFunction::Pq as u32
        );
        assert_eq!(
            uniform.input_reference,
            HdrReference::DisplayReferred as u32
        );
    }

    #[test]
    fn shader_converts_rec2020_input_to_linear_srgb() {
        assert!(HDR_IMAGE_PLANE_SHADER.contains("INPUT_COLOR_SPACE_REC2020_LINEAR"));
        assert!(HDR_IMAGE_PLANE_SHADER.contains("INPUT_COLOR_SPACE_DISPLAY_P3_LINEAR"));
        assert!(HDR_IMAGE_PLANE_SHADER.contains("fn convert_input_to_linear_srgb"));
        assert!(HDR_IMAGE_PLANE_SHADER.contains("1.6605"));
    }

    #[test]
    fn shader_converts_aces2065_1_input_to_linear_srgb() {
        assert!(HDR_IMAGE_PLANE_SHADER.contains("INPUT_COLOR_SPACE_ACES2065_1"));
        assert!(HDR_IMAGE_PLANE_SHADER.contains("fn aces2065_1_to_linear_srgb"));
        assert!(HDR_IMAGE_PLANE_SHADER.contains("2.5216"));
    }

    #[test]
    fn shader_converts_xyz_input_to_linear_srgb() {
        assert!(HDR_IMAGE_PLANE_SHADER.contains("INPUT_COLOR_SPACE_XYZ"));
        assert!(HDR_IMAGE_PLANE_SHADER.contains("fn xyz_to_linear_srgb"));
        assert!(HDR_IMAGE_PLANE_SHADER.contains("3.2404"));
    }

    #[test]
    fn shader_decodes_hdr_transfer_functions_before_color_conversion() {
        assert!(HDR_IMAGE_PLANE_SHADER.contains("INPUT_TRANSFER_PQ"));
        assert!(HDR_IMAGE_PLANE_SHADER.contains("INPUT_TRANSFER_HLG"));
        assert!(HDR_IMAGE_PLANE_SHADER.contains("INPUT_TRANSFER_BT709"));
        assert!(HDR_IMAGE_PLANE_SHADER.contains("fn pq_to_display_linear"));
        assert!(HDR_IMAGE_PLANE_SHADER.contains("fn bt709_nonlinear_to_linear"));
        assert!(HDR_IMAGE_PLANE_SHADER.contains("fn hlg_to_scene_linear"));
        assert!(HDR_IMAGE_PLANE_SHADER.contains("fn decode_input_transfer"));
        assert!(HDR_IMAGE_PLANE_SHADER.contains("sdr_manual_srgb_encode"));
        assert!(HDR_IMAGE_PLANE_SHADER.contains("manual_oetf"));
    }

    #[test]
    fn shader_outputs_straight_alpha_for_standard_blending() {
        assert!(HDR_IMAGE_PLANE_SHADER.contains("fn encode_native_hdr"));
        assert!(
            HDR_IMAGE_PLANE_SHADER.contains("if tone_map.output_mode == OUTPUT_MODE_NATIVE_HDR")
        );
        assert!(HDR_IMAGE_PLANE_SHADER.contains("src_a = clamp(hdr.a, 0.0, 1.0)"));
        assert!(HDR_IMAGE_PLANE_SHADER.contains("a_out * tone_map.alpha"));
        assert!(!HDR_IMAGE_PLANE_SHADER.contains("encode_sdr(hdr.rgb, tone_map) * tone_map.alpha"));
    }

    #[test]
    fn apple_heic_display_never_uses_per_fragment_compose() {
        assert!(!HDR_IMAGE_PLANE_SHADER.contains("tone_map.apple_compose != 0u"));
        assert!(!HDR_IMAGE_PLANE_SHADER.contains("fn sample_apple_gain_encoded_at_primary_pixel"));
        assert!(HDR_IMAGE_PLANE_SHADER.contains("fn sample_hdr_for_display"));
    }

    #[test]
    #[cfg(feature = "heif-native")]
    fn apple_gain_map_gpu_compose_entry_point_exists() {
        use super::apple_compose_gpu::APPLE_GAIN_COMPOSE_SHADER;

        assert!(APPLE_GAIN_COMPOSE_SHADER.contains("fn cs_compose_apple_gain"));
        assert!(APPLE_GAIN_COMPOSE_SHADER.contains("var<storage, read> encoded_primary"));
        assert!(APPLE_GAIN_COMPOSE_SHADER.contains("compose_row_offset"));
        assert!(
            APPLE_GAIN_COMPOSE_SHADER
                .contains("compose_apple_at_primary_pixel(px, py, local_py, tone_map)")
        );
    }

    #[test]
    fn native_hdr_pq_shader_encodes_pq_for_rgb10a2_target() {
        assert!(HDR_IMAGE_PLANE_SHADER.contains("OUTPUT_MODE_NATIVE_HDR_PQ"));
        assert!(HDR_IMAGE_PLANE_SHADER.contains("fn encode_native_hdr_pq"));
        assert!(HDR_IMAGE_PLANE_SHADER.contains("fn display_linear_to_pq"));
        assert!(
            HDR_IMAGE_PLANE_SHADER.contains("const PQ_REFERENCE_LUMINANCE_NITS: f32 = 10000.0")
        );
        assert!(HDR_IMAGE_PLANE_SHADER.contains("nits / vec3<f32>(PQ_REFERENCE_LUMINANCE_NITS)"));
        assert!(HDR_IMAGE_PLANE_SHADER.contains("OUTPUT_MODE_NATIVE_HDR_GAMMA22"));
        assert!(HDR_IMAGE_PLANE_SHADER.contains("fn encode_native_hdr_gamma22"));
    }

    #[test]
    fn native_hdr_encoders_share_exposed_linear_rgb() {
        assert!(HDR_IMAGE_PLANE_SHADER.contains("fn exposed_linear_rgb"));
        assert!(HDR_IMAGE_PLANE_SHADER.contains("return exposed_linear_rgb(rgb, settings);"));
        assert!(HDR_IMAGE_PLANE_SHADER.contains("display_linear_to_pq(exposed_linear_rgb"));
        assert!(
            HDR_IMAGE_PLANE_SHADER.contains("exposed_linear_rgb(rgb, settings) * display_scale")
                || HDR_IMAGE_PLANE_SHADER
                    .contains("scene_linear_to_display_referred(exposed) * display_scale")
        );
        assert!(!HDR_IMAGE_PLANE_SHADER.contains("fn encode_scene_linear_kwin_gamma22"));
        assert!(!HDR_IMAGE_PLANE_SHADER.contains("fn compress_scene_linear_highlights"));
        assert!(!HDR_IMAGE_PLANE_SHADER.contains("reinhard_tone_map_luminance_preserved"));
        assert!(HDR_IMAGE_PLANE_SHADER.contains("fn scene_linear_to_display_referred"));
        assert!(
            HDR_IMAGE_PLANE_SHADER
                .contains("scene_linear_to_display_referred(exposed) * display_scale")
        );
        assert!(
            HDR_IMAGE_PLANE_SHADER
                .contains("if (settings.input_transfer_function == INPUT_TRANSFER_LINEAR)"),
            "scene-linear needs display-referred mapping before KWin gamma 2.2 OETF"
        );
    }

    #[test]
    fn native_hdr_shader_outputs_linear_scrgb_without_gamma_encoding() {
        // scRGB native HDR is linear; γ2.2 inflates shadows and destroys SDR contrast on
        // physically SDR displays advertising HDR support (conformance `bench_oriented_brg`).
        assert!(HDR_IMAGE_PLANE_SHADER.contains("fn encode_native_hdr"));
        assert!(
            !HDR_IMAGE_PLANE_SHADER.contains("let sdr_base ="),
            "encode_native_hdr must not γ-encode for scRGB output"
        );
        assert!(
            !HDR_IMAGE_PLANE_SHADER.contains("return max(sdr_base, exposed);"),
            "encode_native_hdr must return exposed linear value, no γ-blend"
        );
    }

    #[test]
    fn shader_averages_hdr_texels_when_downscaling() {
        assert!(HDR_IMAGE_PLANE_SHADER.contains("fn sample_hdr_for_display"));
        assert!(HDR_IMAGE_PLANE_SHADER.contains("HDR_DOWNSCALE_SAMPLE_GRID"));
        assert!(HDR_IMAGE_PLANE_SHADER.contains("dpdx(uv)"));
        assert!(HDR_IMAGE_PLANE_SHADER.contains("sum += load_hdr_texel"));
    }

    #[test]
    fn shader_uses_wgsl_if_statement_for_output_mode_selection() {
        assert!(
            !HDR_IMAGE_PLANE_SHADER.contains("let rgb = if "),
            "WGSL/Naga rejects Rust-style if expressions in shader code"
        );
        assert!(HDR_IMAGE_PLANE_SHADER.contains("var rgb: vec3<f32>;"));
    }

    #[test]
    fn hdr_image_plane_shader_parses_as_wgsl() {
        naga::front::wgsl::parse_str(HDR_IMAGE_PLANE_SHADER)
            .expect("HDR image plane shader must parse before runtime pipeline creation");
    }

    fn hdr_image(
        width: u32,
        height: u32,
        format: HdrPixelFormat,
        rgba_f32: Vec<f32>,
    ) -> HdrImageBuffer {
        HdrImageBuffer {
            width,
            height,
            format,
            color_space: HdrColorSpace::LinearSrgb,
            metadata: HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
            rgba_f32: Arc::new(rgba_f32),
        }
    }

    fn hdr_tile(width: u32, height: u32, rgba_f32: Vec<f32>) -> HdrTileBuffer {
        HdrTileBuffer::new(width, height, HdrColorSpace::LinearSrgb, Arc::new(rgba_f32))
    }
}
