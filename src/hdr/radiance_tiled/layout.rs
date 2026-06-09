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

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Rgbe8Pixel {
    pub(crate) rgb: [u8; 3],
    pub(crate) exponent: u8,
}

/// Axes in the Radiance resolution line (`+X`, `-Y`, …). Data is stored as `outer` scanlines of
/// `inner_len` RGBE pixels; see [`RadianceRasterLayout::logical_xy`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RadianceScanAxis {
    X,
    Y,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RadianceScanSign {
    Positive,
    Negative,
}

/// Parsed resolution line (`-Y H +X W`, `+X W -Y H`, …): display `width`×`height` in top-left origin,
/// `+y` downward, `+x` rightward. File order is `(outer_idx, inner_idx)` with mapping per Greg Ward /
/// RFC-style HDR semantics.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RadianceRasterLayout {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) outer_axis: RadianceScanAxis,
    pub(crate) outer_sign: RadianceScanSign,
    pub(crate) outer_len: u32,
    pub(crate) inner_axis: RadianceScanAxis,
    pub(crate) inner_sign: RadianceScanSign,
    pub(crate) inner_len: u32,
}

impl RadianceRasterLayout {
    /// Map file-order indices to logical image `(x,y)`.
    ///
    /// Hot decode paths use [`Self::stride_plan`] instead; this stays as the spec reference for
    /// tests ([`Self::file_indices_for_logical_xy`] inverse).
    #[allow(dead_code)]
    pub(crate) fn logical_xy(self, outer_a: u32, inner_b: u32) -> (u32, u32) {
        let w = self.width;
        let h = self.height;
        let x = match self.outer_axis {
            RadianceScanAxis::X => {
                if self.outer_sign == RadianceScanSign::Positive {
                    outer_a
                } else {
                    w - 1 - outer_a
                }
            }
            RadianceScanAxis::Y => {
                if self.inner_sign == RadianceScanSign::Positive {
                    inner_b
                } else {
                    w - 1 - inner_b
                }
            }
        };
        let y = match self.outer_axis {
            RadianceScanAxis::Y => {
                if self.outer_sign == RadianceScanSign::Negative {
                    outer_a
                } else {
                    h - 1 - outer_a
                }
            }
            RadianceScanAxis::X => {
                if self.inner_sign == RadianceScanSign::Negative {
                    inner_b
                } else {
                    h - 1 - inner_b
                }
            }
        };
        (x, y)
    }

    /// Inverse of [`Self::logical_xy`]: which file scanline and in-line index hold logical `(lx,ly)`.
    pub(crate) fn file_indices_for_logical_xy(self, lx: u32, ly: u32) -> (u32, u32) {
        match self.outer_axis {
            RadianceScanAxis::Y => {
                let outer_a = if self.outer_sign == RadianceScanSign::Negative {
                    ly
                } else {
                    self.height - 1 - ly
                };
                let inner_b = if self.inner_sign == RadianceScanSign::Positive {
                    lx
                } else {
                    self.width - 1 - lx
                };
                (outer_a, inner_b)
            }
            RadianceScanAxis::X => {
                let outer_a = if self.outer_sign == RadianceScanSign::Positive {
                    lx
                } else {
                    self.width - 1 - lx
                };
                let inner_b = if self.inner_sign == RadianceScanSign::Negative {
                    ly
                } else {
                    self.height - 1 - ly
                };
                (outer_a, inner_b)
            }
        }
    }

    /// `-Y … +X …` without flips — file scanlines match display rows left-to-right, top-to-bottom.
    pub(crate) fn is_row_major_top_left(self) -> bool {
        matches!(
            (
                self.outer_axis,
                self.outer_sign,
                self.inner_axis,
                self.inner_sign,
            ),
            (
                RadianceScanAxis::Y,
                RadianceScanSign::Negative,
                RadianceScanAxis::X,
                RadianceScanSign::Positive,
            )
        )
    }

    /// Starts and ±1 strides for stepping logical `(x,y)` without branches in the pixel hot loop,
    /// matching the resolution-line semantics (`outer_axis`/signs lifted out of inner loops).
    pub(crate) fn stride_plan(self) -> RadianceStridePlan {
        let w_i = self.width as i32;
        let h_i = self.height as i32;
        if self.outer_axis == RadianceScanAxis::Y {
            let y_start = if self.outer_sign == RadianceScanSign::Negative {
                0
            } else {
                h_i - 1
            };
            let y_step = if self.outer_sign == RadianceScanSign::Negative {
                1
            } else {
                -1
            };
            let x_start = if self.inner_sign == RadianceScanSign::Positive {
                0
            } else {
                w_i - 1
            };
            let x_step = if self.inner_sign == RadianceScanSign::Positive {
                1
            } else {
                -1
            };
            RadianceStridePlan {
                outer_major_is_y: true,
                outer_len: self.outer_len,
                inner_len: self.inner_len,
                x_start,
                x_step,
                y_start,
                y_step,
            }
        } else {
            let x_start = if self.outer_sign == RadianceScanSign::Positive {
                0
            } else {
                w_i - 1
            };
            let x_step = if self.outer_sign == RadianceScanSign::Positive {
                1
            } else {
                -1
            };
            let y_start = if self.inner_sign == RadianceScanSign::Negative {
                0
            } else {
                h_i - 1
            };
            let y_step = if self.inner_sign == RadianceScanSign::Negative {
                1
            } else {
                -1
            };
            RadianceStridePlan {
                outer_major_is_y: false,
                outer_len: self.outer_len,
                inner_len: self.inner_len,
                x_start,
                x_step,
                y_start,
                y_step,
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct RadianceStridePlan {
    pub(crate) outer_major_is_y: bool,
    pub(crate) outer_len: u32,
    pub(crate) inner_len: u32,
    pub(crate) x_start: i32,
    pub(crate) x_step: i32,
    pub(crate) y_start: i32,
    pub(crate) y_step: i32,
}

/// Indices `inner ∈ [imin,imax]` whose `coord = coord_start + inner * coord_step` lie in `[c0,c1]` (inclusive),
/// assuming `coord_step ∈ { -1, +1 }`.
pub(crate) fn inner_range_covering_coord_inclusive(
    coord_start: i32,
    coord_step: i32,
    inner_len: u32,
    c0: i32,
    c1: i32,
) -> Option<(u32, u32)> {
    debug_assert!(coord_step == 1 || coord_step == -1);
    let last = coord_start + coord_step * (inner_len as i32 - 1);
    let (vmin, vmax) = if coord_start <= last {
        (coord_start, last)
    } else {
        (last, coord_start)
    };
    let c0 = c0.max(vmin);
    let c1 = c1.min(vmax);
    if c1 < c0 {
        return None;
    }

    fn solve(inner_start: i32, step: i32, target: i32) -> i32 {
        (target - inner_start).div_euclid(step)
    }

    let i0 = solve(coord_start, coord_step, c0).clamp(0, inner_len as i32 - 1) as u32;
    let i1 = solve(coord_start, coord_step, c1).clamp(0, inner_len as i32 - 1) as u32;
    let imin = i0.min(i1);
    let imax = i0.max(i1);
    Some((imin, imax))
}

/// File `outer` indices whose coordinate `coord = outer_origin + outer * outer_step` falls in `[c0,c1]` (inclusive),
/// clipped to `[0, outer_len)`.
pub(crate) fn outer_range_covering_coord_inclusive(
    outer_origin: i32,
    outer_step: i32,
    outer_len: u32,
    c0: i32,
    c1: i32,
) -> Option<(u32, u32)> {
    debug_assert!(outer_step == 1 || outer_step == -1);
    let last_outer = outer_len as i32 - 1;
    let first_coord = outer_origin;
    let last_coord = outer_origin + outer_step * last_outer;
    let (vmin, vmax) = if first_coord <= last_coord {
        (first_coord, last_coord)
    } else {
        (last_coord, first_coord)
    };
    let c0 = c0.max(vmin);
    let c1 = c1.min(vmax);
    if c1 < c0 {
        return None;
    }

    fn solve(outer_orig: i32, step: i32, target: i32) -> i32 {
        (target - outer_orig).div_euclid(step)
    }

    let o0 = solve(outer_origin, outer_step, c0).clamp(0, last_outer) as u32;
    let o1 = solve(outer_origin, outer_step, c1).clamp(0, last_outer) as u32;
    let omin = o0.min(o1);
    let omax = o0.max(o1);
    Some((omin, omax))
}
