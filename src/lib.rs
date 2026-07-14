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

//! Library surface for isolated unit tests and shared modules.
pub mod constants;
pub mod simd_downsample;
pub mod simd_pixel_convert;
pub mod simd_swizzle;

// PSD/PSB SIMD helpers — registered here for `cargo test --lib` coverage
// (also declared in `main.rs` as private `mod` for the binary crate).
pub mod psb_blend_nonseparable;
pub mod psb_blend_nonseparable_full;
pub mod psb_blend_separable;
pub mod psb_downconvert_simd;
pub mod psb_hdr_blend;
pub mod psb_hdr_interleave_simd;
pub mod psb_layer_blend_simd;
pub mod psb_layer_rgba_simd;
pub mod psb_packbits_simd;
pub mod psb_simd_mul_div255;
