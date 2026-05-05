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

// LogL / LogLuv TIFF → linear display RGB (CCIR 709 matrix from libtiff `tif_luv.c` / Greg Ward).
// UV lattice tables: libtiff `uvcode.h` (SGI / Greg Ward Larson).

mod tables {
    #[derive(Clone, Copy)]
    pub(super) struct UvRow {
        pub(super) ustart: f64,
        #[allow(dead_code)]
        pub(super) nus: i16,
        pub(super) ncum: i16,
    }

    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/hdr/logluv_uv_row.rs"));
}

use tables::UV_ROW;

const UV_SQSIZ: f64 = 0.003500;
const UV_NDIVS: i32 = 16289;
const UV_VSTART: f64 = 0.016940;
const UV_NVS: usize = 163;

const U_NEU: f64 = 0.210526316;
const V_NEU: f64 = 0.473684211;
const UVSCALE: f64 = 410.0;

/// `COMPRESSION_SGILOG24` — libtiff 24-bit packed LogLuv (10+14); all other SGILog modes use LogLuv32.
pub const COMPRESSION_SGILOG24: u16 = 34677;

/// libtiff `uv_decode`: map 14-bit chroma index to CIE (u',v').
fn uv_decode(c: i32) -> Option<(f64, f64)> {
    if c < 0 || c >= UV_NDIVS {
        return None;
    }
    let mut lower: u32 = 0;
    let mut upper: u32 = UV_NVS as u32;
    while upper - lower > 1 {
        let vi = (lower + upper) >> 1;
        let ui = c - UV_ROW[vi as usize].ncum as i32;
        if ui > 0 {
            lower = vi;
        } else if ui < 0 {
            upper = vi;
        } else {
            lower = vi;
            break;
        }
    }
    let vi = lower as usize;
    let ui = c - UV_ROW[vi].ncum as i32;
    let u = UV_ROW[vi].ustart + ((ui as f64) + 0.5) * UV_SQSIZ;
    let v = UV_VSTART + (vi as f64 + 0.5) * UV_SQSIZ;
    Some((u, v))
}

fn log_l10_to_y(p10: u32) -> f64 {
    let p10 = p10 & 0x3ff;
    if p10 == 0 {
        return 0.0;
    }
    let e = (std::f64::consts::LN_2 / 64.0) * ((p10 as f64) + 0.5) - std::f64::consts::LN_2 * 12.0;
    e.exp()
}

/// Greg Ward `LogL16toY` (luminance from high 16 bits of LogLuv32 or raw `int16` LogL).
pub(crate) fn log_l16_to_y(p16: i32) -> f64 {
    let le = p16 & 0x7fff;
    if le == 0 {
        return 0.0;
    }
    let y =
        ((std::f64::consts::LN_2 / 256.0) * ((le as f64) + 0.5) - std::f64::consts::LN_2 * 64.0).exp();
    if (p16 & 0x8000) == 0 {
        y
    } else {
        -y
    }
}

/// `LogLuv24toXYZ` from `tif_luv.c`.
fn logluv24_to_xyz(p: u32) -> [f32; 3] {
    let l = log_l10_to_y(p >> 14);
    if l <= 0.0 {
        return [0.0, 0.0, 0.0];
    }
    let ce = (p & 0x3fff) as i32;
    let (u, v) = match uv_decode(ce) {
        Some(pair) => pair,
        None => (U_NEU, V_NEU),
    };
    let s = 1.0 / (6.0 * u - 16.0 * v + 12.0);
    let x = 9.0 * u * s;
    let y = 4.0 * v * s;
    [
        (x / y * l) as f32,
        l as f32,
        (((1.0 - x - y) / y) * l) as f32,
    ]
}

/// `LogLuv32toXYZ` from `tif_luv.c`.
fn logluv32_to_xyz(p: u32) -> [f32; 3] {
    let p_hi = (p >> 16) as u16 as i16 as i32;
    let l = log_l16_to_y(p_hi);
    if l <= 0.0 {
        return [0.0, 0.0, 0.0];
    }
    let u = (1.0 / UVSCALE) * (((p >> 8) & 0xff) as f64 + 0.5);
    let v = (1.0 / UVSCALE) * ((p & 0xff) as f64 + 0.5);
    let s = 1.0 / (6.0 * u - 16.0 * v + 12.0);
    let x = 9.0 * u * s;
    let y = 4.0 * v * s;
    [
        (x / y * l) as f32,
        l as f32,
        (((1.0 - x - y) / y) * l) as f32,
    ]
}

/// `XYZtoRGB24` linear part only — CCIR-709 primaries (`tif_luv.c`).
fn xyz_ccir709_to_linear_rgb(xyz: [f32; 3]) -> [f32; 3] {
    let x = xyz[0] as f64;
    let y = xyz[1] as f64;
    let z = xyz[2] as f64;
    let r = 2.690 * x + -1.276 * y + -0.414 * z;
    let g = -1.022 * x + 1.978 * y + 0.044 * z;
    let b = 0.061 * x + -0.224 * y + 1.163 * z;
    [r as f32, g as f32, b as f32]
}

/// One packed LogLuv/Luminance word → premultiplied **linear** RGB + alpha.
pub(crate) fn logluv_word_to_linear_rgba(compression: u16, word: u32) -> [f32; 4] {
    let xyz = if compression == COMPRESSION_SGILOG24 {
        logluv24_to_xyz(word)
    } else {
        logluv32_to_xyz(word)
    };
    let rgb = xyz_ccir709_to_linear_rgb(xyz);
    [rgb[0], rgb[1], rgb[2], 1.0]
}

/// Grayscale LogL (`PHOTO_LOGL`) from 16-bit encoded luminance.
pub(crate) fn logl_i16_to_linear_rgba(le: i16) -> [f32; 4] {
    let y = log_l16_to_y(le as i32).max(0.0) as f32;
    [y, y, y, 1.0]
}

pub(crate) fn logl_f32_y_to_linear_rgba(y: f32) -> [f32; 4] {
    let y = if y.is_finite() { y.max(0.0) } else { 0.0 };
    [y, y, y, 1.0]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logluv32_not_black() {
        // Synthetic non-zero upper bits
        let w = 0x4000_8080u32;
        let rgba = logluv_word_to_linear_rgba(34676, w);
        assert!(rgba[0].is_finite() && rgba[1].is_finite() && rgba[2].is_finite());
    }
}
