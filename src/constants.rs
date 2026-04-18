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

/// The maximum dimension (width or height) for a high-quality intermediate preview.
/// Used for cross-fading and providing sharp visuals before tiling completes.
pub const MAX_QUALITY_PREVIEW_SIZE: u32 = 4096;

/// The absolute fallback limit for GPU texture dimensions (usually 8192 or 16384).
/// We cap it at 8192 to be safe across different frameworks and platforms.
pub const ABSOLUTE_MAX_TEXTURE_SIDE: u32 = 8192;

/// Standard number of color channels for RGB images.
pub const RGB_CHANNELS: usize = 3;
/// Standard number of color channels for RGBA images.
pub const RGBA_CHANNELS: usize = 4;
/// Standard bit depth for 8-bit image formats.
pub const BIT_DEPTH_8: usize = 8;
/// Maximum value for a single 8-bit color channel.
pub const MAX_CHANNEL_VALUE: u8 = 255;

/// Number of bytes in one Megabyte.
pub const BYTES_PER_MB: u64 = 1024 * 1024;
/// Number of bytes in one Gigabyte.
pub const BYTES_PER_GB: u64 = 1024 * 1024 * 1024;

/// Default size for small on-demand previews (e.g. for tiled loading hints).
pub const DEFAULT_PREVIEW_SIZE: u32 = 512;

/// Standard fallback delay for animation frames (100ms).
pub const DEFAULT_ANIMATION_DELAY_MS: u32 = 100;
/// Minimum threshold for animation delays; values below this are often considered 
/// broken and should use the default fallback (standard browser behavior).
pub const MIN_ANIMATION_DELAY_THRESHOLD_MS: u32 = 10;

/// Default capacity for audio file read buffers (8 MB).
/// High capacity helps prevent stuttering on slow HDDs (like WD Green) 
/// when images are being loaded in parallel, as it reduces disk seek frequency.
pub const AUDIO_BUFFER_CAPACITY: usize = 8 * 1024 * 1024;

