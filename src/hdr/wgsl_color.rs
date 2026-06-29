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

macro_rules! hdr_wgsl_color_helpers {
    () => {
        r#"
const INPUT_COLOR_SPACE_REC2020_LINEAR: u32 = 2u;
const INPUT_COLOR_SPACE_ACES2065_1: u32 = 3u;
const INPUT_COLOR_SPACE_XYZ: u32 = 4u;
/// Must match HdrColorSpace::DisplayP3Linear as u32.
const INPUT_COLOR_SPACE_DISPLAY_P3_LINEAR: u32 = 6u;
const INPUT_TRANSFER_LINEAR: u32 = 0u;
const INPUT_TRANSFER_SRGB: u32 = 1u;
const INPUT_TRANSFER_PQ: u32 = 2u;
const INPUT_TRANSFER_HLG: u32 = 3u;
/// Must match HdrTransferFunction::Bt709 as u32; Gamma/Unknown omit dedicated WGSL branches.
const INPUT_TRANSFER_BT709: u32 = 6u;
const PQ_M1: f32 = 2610.0 / 16384.0;
const PQ_M2: f32 = 2523.0 / 32.0;
const PQ_C1: f32 = 3424.0 / 4096.0;
const PQ_C2: f32 = 2413.0 / 128.0;
const PQ_C3: f32 = 2392.0 / 128.0;

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

// BT.709 / SMPTE 170-style nonlinear code -> nominal linear-light (ITU-R BT.709 annex 1 OETF inverse).
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

// BT.2100 HLG EOTF inverse (input decode only). No matching scene_linear_to_hlg
// OETF or NativeHdrHlg swap-chain path; see hdr/monitor/wayland.rs.
fn hlg_to_scene_linear(rgb: vec3<f32>) -> vec3<f32> {
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
"#
    };
}

pub(crate) use hdr_wgsl_color_helpers;

#[cfg(test)]
mod tests {
    #[test]
    fn shared_color_helpers_keep_traceability_comments() {
        let helpers = hdr_wgsl_color_helpers!();

        assert!(helpers.contains("Must match HdrColorSpace::DisplayP3Linear as u32"));
        assert!(helpers.contains("HdrTransferFunction::Bt709"));
        assert!(helpers.contains("BT.709 / SMPTE 170-style nonlinear code"));
        assert!(helpers.contains("BT.2100 HLG EOTF inverse"));
    }
}
