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

use super::layout::Rgbe8Pixel;

use std::io::{Cursor, Read};
use std::sync::LazyLock;

/// Ward-style RGBE scale `2^(e-128-8)` for each exponent byte (index 0 is always 0).
static RGBE_EXPONENT_SCALE: LazyLock<[f32; 256]> = LazyLock::new(|| {
    let mut table = [0.0_f32; 256];
    for exponent in 1..=255u8 {
        table[exponent as usize] = 2.0_f32.powi(i32::from(exponent) - 128 - 8);
    }
    table
});

pub(crate) fn read_scanline<R: Read>(
    reader: &mut R,
    scanline: &mut [Rgbe8Pixel],
) -> Result<(), String> {
    if scanline.is_empty() {
        return Ok(());
    }

    let first = read_rgbe(reader)?;
    if first.rgb[0] == 2 && first.rgb[1] == 2 && first.rgb[2] < 128 {
        decode_component(reader, scanline.len(), |offset, value| {
            scanline[offset].rgb[0] = value;
        })?;
        decode_component(reader, scanline.len(), |offset, value| {
            scanline[offset].rgb[1] = value;
        })?;
        decode_component(reader, scanline.len(), |offset, value| {
            scanline[offset].rgb[2] = value;
        })?;
        decode_component(reader, scanline.len(), |offset, value| {
            scanline[offset].exponent = value;
        })?;
    } else {
        decode_old_rle(reader, first, scanline)?;
    }
    Ok(())
}

pub(crate) fn skip_scanline(reader: &mut Cursor<&[u8]>, width: usize) -> Result<(), String> {
    if width == 0 {
        return Ok(());
    }

    let first = read_rgbe(reader)?;
    if first.rgb[0] == 2 && first.rgb[1] == 2 && first.rgb[2] < 128 {
        for _ in 0..4 {
            skip_component(reader, width)?;
        }
    } else {
        skip_old_rle(reader, first, width)?;
    }
    Ok(())
}

fn skip_component(reader: &mut Cursor<&[u8]>, width: usize) -> Result<(), String> {
    let mut pos = 0usize;
    while pos < width {
        let run = read_byte(reader)?;
        if run <= 128 {
            let count = run as usize;
            if pos + count > width {
                return Err(format!(
                    "Wrong Radiance HDR scanline length: got {}, expected {width}",
                    pos + count
                ));
            }
            let next = reader
                .position()
                .checked_add(count as u64)
                .ok_or_else(|| "Radiance HDR scanline offset overflow".to_string())?;
            if next > reader.get_ref().len() as u64 {
                return Err("EOF in Radiance HDR scanline".to_string());
            }
            reader.set_position(next);
            pos += count;
        } else {
            let count = (run - 128) as usize;
            if pos + count > width {
                return Err(format!(
                    "Wrong Radiance HDR scanline length: got {}, expected {width}",
                    pos + count
                ));
            }
            read_byte(reader)?;
            pos += count;
        }
    }
    Ok(())
}

fn skip_old_rle(reader: &mut Cursor<&[u8]>, first: Rgbe8Pixel, width: usize) -> Result<(), String> {
    let mut x = 1usize;
    let mut run_multiplier = 1usize;
    let mut previous = first;

    while x < width {
        let pixel = read_rgbe(reader)?;
        if pixel.rgb == [1, 1, 1] {
            if run_multiplier > width {
                return Err("Radiance HDR old RLE run multiplier overflow".to_string());
            }
            let count = (pixel.exponent as usize)
                .checked_mul(run_multiplier)
                .ok_or_else(|| "Radiance HDR old RLE run count overflow".to_string())?;
            run_multiplier = run_multiplier
                .checked_mul(256)
                .ok_or_else(|| "Radiance HDR old RLE run multiplier overflow".to_string())?;
            if x + count > width {
                return Err(format!(
                    "Wrong Radiance HDR scanline length: got {}, expected {width}",
                    x + count
                ));
            }
            x += count;
        } else {
            run_multiplier = 1;
            previous = pixel;
            x += 1;
        }
    }
    let _ = previous;
    Ok(())
}

fn decode_component<R: Read, F: FnMut(usize, u8)>(
    reader: &mut R,
    width: usize,
    mut set_component: F,
) -> Result<(), String> {
    let mut pos = 0usize;
    let mut buf = [0u8; 128];
    while pos < width {
        let run = read_byte(reader)?;
        if run <= 128 {
            let count = run as usize;
            if pos + count > width {
                return Err(format!(
                    "Wrong Radiance HDR scanline length: got {}, expected {width}",
                    pos + count
                ));
            }
            reader
                .read_exact(&mut buf[..count])
                .map_err(|err| err.to_string())?;
            for (offset, value) in buf[..count].iter().copied().enumerate() {
                set_component(pos + offset, value);
            }
            pos += count;
        } else {
            let count = (run - 128) as usize;
            if pos + count > width {
                return Err(format!(
                    "Wrong Radiance HDR scanline length: got {}, expected {width}",
                    pos + count
                ));
            }
            let value = read_byte(reader)?;
            for offset in 0..count {
                set_component(pos + offset, value);
            }
            pos += count;
        }
    }
    Ok(())
}

fn decode_old_rle<R: Read>(
    reader: &mut R,
    first: Rgbe8Pixel,
    scanline: &mut [Rgbe8Pixel],
) -> Result<(), String> {
    scanline[0] = first;
    let mut x = 1usize;
    let mut run_multiplier = 1usize;
    let mut previous = first;

    while x < scanline.len() {
        let pixel = read_rgbe(reader)?;
        if pixel.rgb == [1, 1, 1] {
            let line_len = scanline.len();
            if run_multiplier > line_len {
                return Err("Radiance HDR old RLE run multiplier overflow".to_string());
            }
            let count = (pixel.exponent as usize)
                .checked_mul(run_multiplier)
                .ok_or_else(|| "Radiance HDR old RLE run count overflow".to_string())?;
            run_multiplier = run_multiplier
                .checked_mul(256)
                .ok_or_else(|| "Radiance HDR old RLE run multiplier overflow".to_string())?;
            if x + count > line_len {
                return Err(format!(
                    "Wrong Radiance HDR scanline length: got {}, expected {}",
                    x + count,
                    scanline.len()
                ));
            }
            for dst in &mut scanline[x..x + count] {
                *dst = previous;
            }
            x += count;
        } else {
            run_multiplier = 1;
            previous = pixel;
            scanline[x] = pixel;
            x += 1;
        }
    }
    Ok(())
}

fn read_rgbe<R: Read>(reader: &mut R) -> Result<Rgbe8Pixel, String> {
    let mut bytes = [0u8; 4];
    reader
        .read_exact(&mut bytes)
        .map_err(|err| err.to_string())?;
    Ok(Rgbe8Pixel {
        rgb: [bytes[0], bytes[1], bytes[2]],
        exponent: bytes[3],
    })
}

fn read_byte<R: Read>(reader: &mut R) -> Result<u8, String> {
    let mut byte = [0u8; 1];
    reader
        .read_exact(&mut byte)
        .map_err(|err| err.to_string())?;
    Ok(byte[0])
}

impl Rgbe8Pixel {
    /// Runs once per decoded Radiance RGBE pixel. Scale comes from a precomputed exponent LUT
    /// (bit-identical to `2^(e-128-8)` via `powi`).
    pub(crate) fn to_rgb_f32(self) -> [f32; 3] {
        if self.exponent == 0 {
            return [0.0; 3];
        }
        let scale = RGBE_EXPONENT_SCALE[self.exponent as usize];
        [
            f32::from(self.rgb[0]) * scale,
            f32::from(self.rgb[1]) * scale,
            f32::from(self.rgb[2]) * scale,
        ]
    }
}

#[cfg(test)]
mod rgbe_scale_tests {
    use super::*;

    #[test]
    fn rgbe_exponent_scale_lut_matches_powi() {
        for exponent in 0..=255u8 {
            let lut = RGBE_EXPONENT_SCALE[exponent as usize];
            let expected = if exponent == 0 {
                0.0
            } else {
                2.0_f32.powi(i32::from(exponent) - 128 - 8)
            };
            assert_eq!(lut, expected, "exponent={exponent}");
        }
    }

    #[test]
    fn rgbe_to_rgb_f32_sample_pixels() {
        let pixel = Rgbe8Pixel {
            rgb: [128, 128, 128],
            exponent: 129,
        };
        let rgb = pixel.to_rgb_f32();
        let scale = 2.0_f32.powi(129 - 128 - 8);
        assert!((rgb[0] - 128.0 * scale).abs() < f32::EPSILON);
        assert!((rgb[1] - 128.0 * scale).abs() < f32::EPSILON);
        assert!((rgb[2] - 128.0 * scale).abs() < f32::EPSILON);
    }
}
