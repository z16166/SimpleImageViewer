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

// Use mimalloc for all platforms — the default Windows HeapAlloc has severe
// lock contention when many threads concurrently allocate/free ~68KB buffers
// (one per PSB row decode). mimalloc uses per-thread heaps to eliminate this.
// Unit/integration tests use the system allocator: mimalloc + static CRT on
// Windows CI has been observed to fault the test harness before any case runs.
#[cfg(not(test))]
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
mod formats;
mod hdr;
mod hotkeys;
mod ipc;
mod libtiff_loader;
mod loader;
#[cfg(target_os = "macos")]
mod macos_image_io;
mod metadata_utils;
mod mmap_util;
mod path_location;
mod pixel_inspector;
pub mod print;
mod psb_reader;
mod raw_processor;
mod scanner;
#[cfg(target_os = "windows")]
mod seh_handler;
mod settings;
pub mod theme;
mod tile_cache;
mod ui;
#[cfg(target_os = "windows")]
mod wic;

#[cfg(all(target_os = "windows", not(feature = "legacy_win7")))]
mod wgpu_preprobe_cache;

mod startup;
#[cfg(target_os = "windows")]
mod windows_utils;

fn main() -> eframe::Result {
    startup::run()
}
