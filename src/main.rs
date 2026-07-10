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

#![cfg_attr(all(not(debug_assertions), not(test)), windows_subsystem = "windows")]

// Default: mimalloc (`mimalloc-allocator` in Cargo.toml default features; matches CI/release).
// For Page Heap on Rust allocations locally, build with `system-allocator` instead
// (see Cargo.toml `system-allocator` feature comment).
// Unit/integration tests always use the system allocator (`not(test)` below).
#[cfg(all(not(test), feature = "mimalloc-allocator"))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

rust_i18n::i18n!("locales");

#[macro_use]
mod preload_debug;
mod allocator_tuning;
mod app;

mod audio;
mod constants;
mod context_menu;
mod directory_tree_places;
mod formats;
mod hdr;
mod hotkeys;
mod ipc;
mod libtiff_loader;
mod loader;
mod lru_order;
#[cfg(target_os = "macos")]
mod macos_image_io;
mod metadata_utils;
mod mmap_util;
mod path_location;
mod pixel_inspector;
pub mod print;
mod psb_cmyk_cms;
mod psb_cmyk_simd;
mod psb_downconvert_simd;
mod psb_layer_blend_gpu;
mod psb_layer_blend_simd;
mod psb_layer_clip;
pub mod psb_layer_composite;
mod psb_layer_decode_pool;
mod psb_packbits_simd;
mod psb_reader;
pub mod psb_section_index;
mod psb_zip;
mod raw_processor;
mod scanner;
#[cfg(target_os = "windows")]
mod seh_handler;
mod settings;
mod system_memory;
pub mod theme;
mod tile_cache;
mod ui;
#[cfg(target_os = "windows")]
mod wic;

mod wgpu_pipeline_cache;
#[cfg(all(target_os = "windows", not(feature = "legacy_win7")))]
mod wgpu_preprobe_cache;

mod startup;
#[cfg(target_os = "windows")]
mod windows_utils;

fn main() -> eframe::Result {
    startup::run()
}
