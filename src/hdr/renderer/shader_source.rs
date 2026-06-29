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
const HDR_DOWNSCALE_LIGHT_SAMPLE_GRID: u32 = 2u;
const HDR_DOWNSCALE_HEAVY_SAMPLE_GRID: u32 = 4u;
const HDR_DOWNSCALE_HEAVY_FOOTPRINT: f32 = 2.25;
const HDR_DOWNSCALE_MAX_FOOTPRINT: f32 = 8.0;
const PQ_M1: f32 = 2610.0 / 16384.0;
const PQ_M2: f32 = 2523.0 / 32.0;
const PQ_C1: f32 = 3424.0 / 4096.0;
const PQ_C2: f32 = 2413.0 / 128.0;
const PQ_C3: f32 = 2392.0 / 128.0;

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
    ripple_center: vec2<f32>,
    ripple_radius: f32,
    ripple_enabled: u32,
    pixels_per_point: f32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
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
    let code = pow(clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)), vec3<f32>(1.0 / PQ_M2));
    let numerator = max(code - vec3<f32>(PQ_C1), vec3<f32>(0.0));
    let denominator = max(vec3<f32>(PQ_C2) - vec3<f32>(PQ_C3) * code, vec3<f32>(0.000001));
    let absolute_nits = vec3<f32>(10000.0) * pow(numerator / denominator, vec3<f32>(1.0 / PQ_M1));
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

fn premultiply_hdr_rgba(rgba: vec4<f32>) -> vec4<f32> {
    return vec4<f32>(rgba.rgb * rgba.a, rgba.a);
}

fn unpremultiply_hdr_rgba(premul: vec4<f32>) -> vec4<f32> {
    if premul.a <= 0.0 {
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }
    return vec4<f32>(premul.rgb / premul.a, premul.a);
}

// Rgba32Float is not filterable in WebGPU; emulate bilinear in shader (4 loads).
// Premultiplied interpolation avoids RGB halos on alpha edges (animated icos4d, etc.).
fn bilinear_load_hdr(uv: vec2<f32>, texture_size_i: vec2<i32>) -> vec4<f32> {
    let texel_f = uv * vec2<f32>(texture_size_i) - vec2<f32>(0.5);
    let base = vec2<i32>(i32(floor(texel_f.x)), i32(floor(texel_f.y)));
    let frac = texel_f - vec2<f32>(base);
    let p00 = premultiply_hdr_rgba(load_hdr_texel(base, texture_size_i));
    let p10 = premultiply_hdr_rgba(load_hdr_texel(vec2<i32>(base.x + 1, base.y), texture_size_i));
    let p01 = premultiply_hdr_rgba(load_hdr_texel(vec2<i32>(base.x, base.y + 1), texture_size_i));
    let p11 = premultiply_hdr_rgba(load_hdr_texel(vec2<i32>(base.x + 1, base.y + 1), texture_size_i));
    let top = mix(p00, p10, frac.x);
    let bot = mix(p01, p11, frac.x);
    return unpremultiply_hdr_rgba(mix(top, bot, frac.y));
}

fn downsample_grid_hdr(
    uv: vec2<f32>,
    texture_size: vec2<f32>,
    texture_size_i: vec2<i32>,
    footprint: vec2<f32>,
    sample_grid: u32,
) -> vec4<f32> {
    var sum = vec4<f32>(0.0);
    for (var y = 0u; y < sample_grid; y = y + 1u) {
        for (var x = 0u; x < sample_grid; x = x + 1u) {
            let sample_uv = (vec2<f32>(f32(x), f32(y)) + vec2<f32>(0.5)) / f32(sample_grid);
            let offset = (sample_uv - vec2<f32>(0.5)) * footprint;
            sum += premultiply_hdr_rgba(load_hdr_texel(vec2<i32>(uv * texture_size + offset), texture_size_i));
        }
    }
    return unpremultiply_hdr_rgba(sum / f32(sample_grid * sample_grid));
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
        return bilinear_load_hdr(uv, texture_size_i);
    }

    if max(footprint.x, footprint.y) <= HDR_DOWNSCALE_HEAVY_FOOTPRINT {
        return downsample_grid_hdr(uv, texture_size, texture_size_i, footprint, HDR_DOWNSCALE_LIGHT_SAMPLE_GRID);
    }
    return downsample_grid_hdr(uv, texture_size, texture_size_i, footprint, HDR_DOWNSCALE_HEAVY_SAMPLE_GRID);
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
    let nits = max(rgb * settings.sdr_white_nits, vec3<f32>(0.0));
    let normalized = nits / vec3<f32>(PQ_REFERENCE_LUMINANCE_NITS);
    let lm1 = pow(normalized, vec3<f32>(PQ_M1));
    let num = vec3<f32>(PQ_C1) + vec3<f32>(PQ_C2) * lm1;
    let den = vec3<f32>(1.0) + vec3<f32>(PQ_C3) * lm1;
    return pow(num / den, vec3<f32>(PQ_M2));
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
    // ripple_enabled: 0 = disabled, 1 = inside clip (RIPPLE_CLIP_INSIDE), 2 = outside clip (RIPPLE_CLIP_OUTSIDE).
    // Avoid using other values (e.g., 3) to prevent unexpected behaviors or bugs.
    if (tone_map.ripple_enabled != 0u) {
        let frag_logical = input.position.xy / tone_map.pixels_per_point;
        let diff = frag_logical - tone_map.ripple_center;
        let dist_sq = dot(diff, diff);
        let radius_sq = tone_map.ripple_radius * tone_map.ripple_radius;
        if (tone_map.ripple_enabled == 1u) {
            if (dist_sq > radius_sq) {
                discard;
            }
        } else if (tone_map.ripple_enabled == 2u) {
            if (dist_sq <= radius_sq) {
                discard;
            }
        }
    }
    let rotated_uv = rotate_uv_for_display(input.uv, tone_map.rotation_steps);
    let sampled_uv = tone_map.uv_min + rotated_uv * (tone_map.uv_max - tone_map.uv_min);
    let clamped_uv = clamp(sampled_uv, vec2<f32>(0.0), vec2<f32>(MAX_UV_CLAMP));
    let hdr = sample_hdr_for_display(clamped_uv);
    let src_a = clamp(hdr.a, 0.0, 1.0);
    let display_referred_srgb = tone_map.input_transfer_function == INPUT_TRANSFER_SRGB &&
        tone_map.input_reference != INPUT_REFERENCE_SCENE_LINEAR;
    let decoded_rgb = decode_input_transfer(hdr.rgb, tone_map.input_transfer_function, tone_map);
    var source_rgb = convert_input_to_linear_srgb(decoded_rgb, tone_map.input_color_space);
    if (src_a <= 0.0) {
        source_rgb = vec3<f32>(0.0);
    }
    var source_rgb_exposed = source_rgb;
    if (display_referred_srgb) {
        let exposure_scale = exp2(tone_map.exposure_ev);
        source_rgb_exposed = sanitize_hdr_rgb(source_rgb * exposure_scale);
        if (src_a <= 0.0) {
            source_rgb_exposed = vec3<f32>(0.0);
        }
    }
    var rgb: vec3<f32>;
    if tone_map.output_mode == OUTPUT_MODE_NATIVE_HDR_PQ {
        if (display_referred_srgb) {
            rgb = display_linear_to_pq(source_rgb_exposed * tone_map.native_display_scale, tone_map);
        } else {
            rgb = encode_native_hdr_pq(source_rgb, tone_map);
        }
    } else if tone_map.output_mode == OUTPUT_MODE_NATIVE_HDR_GAMMA22 {
        if (display_referred_srgb) {
            let display_scale =
                tone_map.sdr_white_nits / max(tone_map.max_display_nits, tone_map.sdr_white_nits);
            let peak_linear = source_rgb_exposed * display_scale;
            rgb = gamma22_from_linear_rgb(clamp(peak_linear, vec3<f32>(0.0), vec3<f32>(1.0)));
        } else {
            rgb = encode_native_hdr_gamma22(source_rgb, tone_map);
        }
    } else if tone_map.output_mode == OUTPUT_MODE_NATIVE_HDR {
        if (display_referred_srgb) {
            rgb = source_rgb_exposed * tone_map.native_display_scale;
        } else {
            rgb = encode_native_hdr(source_rgb, tone_map);
        }
    } else {
        rgb = encode_sdr(source_rgb, tone_map);
    }
    let a_out = src_a;
    return vec4<f32>(rgb, a_out * tone_map.alpha);
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hdr_image_plane_shader_parses_as_wgsl() {
        naga::front::wgsl::parse_str(HDR_IMAGE_PLANE_SHADER)
            .expect("HDR image plane shader must parse before runtime pipeline creation");
    }

    #[test]
    fn hdr_downscale_shader_has_light_and_heavy_sampling_paths() {
        assert!(HDR_IMAGE_PLANE_SHADER.contains("HDR_DOWNSCALE_LIGHT_SAMPLE_GRID"));
        assert!(HDR_IMAGE_PLANE_SHADER.contains("HDR_DOWNSCALE_HEAVY_SAMPLE_GRID"));
        assert!(HDR_IMAGE_PLANE_SHADER.contains("HDR_DOWNSCALE_HEAVY_FOOTPRINT"));
    }

    #[test]
    fn pq_constants_are_declared_once_at_module_scope() {
        assert_eq!(HDR_IMAGE_PLANE_SHADER.matches("const PQ_M1").count(), 1);
        assert_eq!(HDR_IMAGE_PLANE_SHADER.matches("const PQ_M2").count(), 1);
        assert!(!HDR_IMAGE_PLANE_SHADER.contains("let m1 = 2610.0 / 16384.0"));
        assert!(!HDR_IMAGE_PLANE_SHADER.contains("let m2 = 2523.0 / 32.0"));
    }
}
