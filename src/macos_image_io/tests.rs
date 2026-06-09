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

mod exif_mapping_tests {
    use crate::macos_image_io::exif_display_to_physical_pixel;

    /// `exif_display_to_physical_pixel` must invert the forward map in `apply_orientation_buffer`.
    #[test]
    fn display_to_physical_inverts_forward_map() {
        let pw = 23u32;
        let ph = 17u32;
        for orientation in 1u32..=8u32 {
            let (lw, lh) = if (5..=8).contains(&orientation) {
                (ph, pw)
            } else {
                (pw, ph)
            };
            for ly in 0..lh {
                for lx in 0..lw {
                    let Some((px, py)) =
                        exif_display_to_physical_pixel(lx, ly, orientation, pw, ph)
                    else {
                        panic!("no inverse for o={orientation} ({lx},{ly})");
                    };
                    let (nx, ny) = match orientation {
                        2 => (pw - 1 - px, py),
                        3 => (pw - 1 - px, ph - 1 - py),
                        4 => (px, ph - 1 - py),
                        5 => (py, px),
                        6 => (ph - 1 - py, px),
                        7 => (ph - 1 - py, pw - 1 - px),
                        8 => (py, pw - 1 - px),
                        _ => (px, py),
                    };
                    assert_eq!(
                        (nx, ny),
                        (lx, ly),
                        "orientation={orientation} physical=({px},{py})"
                    );
                }
            }
        }
    }
}
