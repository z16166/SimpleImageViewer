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

/// Headroom on the containing monitor’s **physical** long edge when capping HQ preview / refine
/// (`ceil(max(phys_w, phys_h) * k_zoom)` vs tier and `hq_preview_max_side` in the loader module).
pub const HQ_PREVIEW_MONITOR_HEADROOM: f32 = 1.1;

/// The absolute fallback limit for GPU texture dimensions (usually 8192 or 16384).
/// We cap it at 8192 to be safe across different frameworks and platforms.
pub const ABSOLUTE_MAX_TEXTURE_SIDE: u32 = 8192;

/// Hard ceiling for a single WIC frame side. Larger claims are treated as corrupt headers.
/// Allows tiled wide/tall images beyond GPU texture side, but bounds DoS from absurd sizes.
pub const WIC_ABSOLUTE_MAX_SIDE: u32 = ABSOLUTE_MAX_TEXTURE_SIDE * 8;

/// Maximum pixel count for static full-image decode into a contiguous RGBA buffer.
/// Malformed headers that claim larger sizes are rejected before allocation (DoS protection).
/// 256 megapixels * 4 bytes/pixel ~= 1 GiB RGBA upper bound.
pub const MAX_STATIC_FULL_DECODE_PIXELS: u64 = 256 * 1024 * 1024;

/// Reject zero / overflowing / oversized static decode dimensions before allocating pixels.
///
/// Returns `Ok(total_pixel_count)` on success — the caller can use the returned pixel count
/// to pre-allocate buffers without re-computing `width * height`.
#[inline]
pub fn validate_static_decode_dimensions(width: u32, height: u32) -> Result<u64, String> {
    if width == 0 || height == 0 {
        return Err(format!("image dimensions {width}x{height} are zero"));
    }
    let Some(pixels) = (width as u64).checked_mul(height as u64) else {
        return Err(format!("image dimensions {width}x{height} overflow"));
    };
    if pixels > MAX_STATIC_FULL_DECODE_PIXELS {
        return Err(format!(
            "image dimensions {width}x{height} ({pixels} pixels) exceed maximum {MAX_STATIC_FULL_DECODE_PIXELS} pixels"
        ));
    }
    Ok(pixels)
}

/// RGBA8 buffer length for `width` x `height`; rejects dimension overflow.
#[inline]
pub fn checked_rgba8_len_u32(width: u32, height: u32) -> Option<usize> {
    (width as u64)
        .checked_mul(height as u64)?
        .checked_mul(RGBA_CHANNELS as u64)?
        .try_into()
        .ok()
}

/// RGBA8 row stride (`width * 4`); rejects overflow.
#[inline]
pub fn checked_rgba8_stride_u32(width: u32) -> Option<u32> {
    width.checked_mul(RGBA_CHANNELS as u32)
}

/// Standard number of color channels for RGB images.
pub const RGB_CHANNELS: usize = 3;
/// Standard number of color channels for RGBA images.
pub const RGBA_CHANNELS: usize = 4;

/// Computes `width * channels` without overflow.
#[inline]
pub fn checked_pixel_row_len(width: usize, channels: usize) -> Option<usize> {
    width.checked_mul(channels)
}

/// Computes the element count for an RGBA row without overflow.
#[inline]
pub fn checked_rgba_row_len(width: usize) -> Option<usize> {
    checked_pixel_row_len(width, RGBA_CHANNELS)
}

/// Computes the element count for an RGBA image without overflow.
#[inline]
pub fn checked_rgba_buffer_len(width: usize, height: usize) -> Option<usize> {
    checked_rgba_row_len(width).and_then(|row_len| row_len.checked_mul(height))
}

/// Computes `width * height` without overflow.
#[inline]
pub fn checked_pixel_area(width: usize, height: usize) -> Option<usize> {
    width.checked_mul(height)
}

/// Standard bit depth for 8-bit image formats.
pub const BIT_DEPTH_8: usize = 8;
/// Maximum value for a single 8-bit color channel.
#[allow(dead_code)]
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

/// Cooldown period (2s) between audio backend initialization attempts.
/// Prevents hot-looping when the hardware is busy or in exclusive mode.
pub const AUDIO_RECOVERY_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(2);

/// Dimensions and layout for the Music HUD (OSD).
pub const MUSIC_HUD_WIDTH: f32 = 400.0;
pub const MUSIC_HUD_HEIGHT: f32 = 42.0;
pub const MUSIC_HUD_BOTTOM_OFFSET: f32 = -100.0;

/// Number of idle seconds before the Music HUD auto-hides.
pub const MUSIC_HUD_IDLE_SECONDS: u64 = 5;

/// Soft cap on tracked short-lived background threads (`BackgroundThreadJoiner`).
/// Beyond this, new work is run on the rayon pool instead of spawning more OS threads.
pub const BACKGROUND_THREAD_SOFT_LIMIT: usize = 64;

/// Low-frequency wake interval while music is playing (track name / HUD idle hide).
pub const MUSIC_PLAYING_REPAINT_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(500);

/// Background wake interval while the window is minimized / hidden to tray.
pub const MINIMIZED_REPAINT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

/// Minimum seconds between Next/Prev from arrow hotkeys when no transition is active.
/// Key auto-repeat otherwise enqueues unbounded decode work and large `LoaderOutput`
/// payloads in the channel. ~5 navigations per second is already extreme for manual
/// input; matches wheel debounce. While a navigation transition (or pending start) is
/// in flight, Next/Prev are blocked until it settles -- see `keyboard_nav_allowed`.
pub const KEYBOARD_NAV_MIN_INTERVAL_SECS: f64 = 0.2;

/// Minimum interval between background YAML writes from the async saver threads.
/// Authoritative persistence happens in `ImageViewerApp::on_exit`; runtime saves are best-effort.
pub const BACKGROUND_YAML_SAVE_MIN_INTERVAL: std::time::Duration =
    std::time::Duration::from_secs(5);

/// Color brightness multiplier for HUD text to ensure contrast in light themes.
pub const MUSIC_HUD_CONTRAST_BOOST: f32 = 2.5;

/// Threshold for character count before truncating track titles in the OSD.
pub const MUSIC_HUD_MAX_CHARS: usize = 45;
pub const MUSIC_HUD_TRUNCATE_LEN: usize = 42;

/// Shared egui data ID for pending seek operations.
pub const ID_PENDING_SEEK: &str = "pending_seek";

/// Default sample rate for audio decoding (44.1 kHz).
pub const DEFAULT_SAMPLE_RATE: u32 = 44100;
/// Default number of audio channels (Stereo).
pub const DEFAULT_CHANNELS: u16 = 2;

/// The number of samples per decoding chunk for the background audio buffer.
/// A value of 4096 provides a good balance between memory overhead and
/// synchronization granularity (approx. 46ms at 44.1kHz stereo).
pub const AUDIO_CHUNK_SIZE: usize = 4096;

/// The number of audio chunks to keep in the background decoding queue.
/// 16 chunks of 4096 samples equals approx 1.5 seconds of audio buffer.
pub const AUDIO_BUFFER_QUEUE_DEPTH: usize = 16;

/// Supported music file extensions used by file scan and picker filters.
pub const MUSIC_SUPPORTED_EXTENSIONS: &[&str] =
    &["mp3", "flac", "ogg", "wav", "aac", "m4a", "ape", "m3u"];

/// Returns true if `ext` is one of [`MUSIC_SUPPORTED_EXTENSIONS`].
pub fn is_supported_music_extension(ext: &str) -> bool {
    MUSIC_SUPPORTED_EXTENSIONS
        .iter()
        .any(|supported| ext.eq_ignore_ascii_case(supported))
}

/// Filename for the emergency diagnostic crash report.
pub const CRASH_REPORT_FILENAME: &str = "crash_report.txt";

/// Filename for the minidump generated by the Windows SEH handler.
#[cfg(target_os = "windows")]
pub const CRASH_DUMP_FILENAME: &str = "crash_dump.dmp";

/// Filename for the first-chance native exception probe emitted by the Windows
/// vectored exception handler.
#[cfg(target_os = "windows")]
pub const CRASH_PROBE_FILENAME: &str = "crash_probe.txt";

/// Default fallback title for the error dialog when i18n is not yet available.
pub const CRASH_DIALOG_FALLBACK_TITLE: &str = "Application Error";

/// Default fallback message for the error dialog when i18n is not yet available.
pub const CRASH_DIALOG_FALLBACK_MSG: &str = "An unexpected error occurred.\n\nDiagnostic info has been copied to the clipboard and saved to the crash report file.";

/// Default position for the settings window.
/// Golden ratio φ — default settings window uses width / height = φ (landscape).
pub const GOLDEN_RATIO: f32 = 1.618_034;
/// Minimum width for the settings window.
pub const SETTINGS_WINDOW_MIN_WIDTH: f32 = 550.0;
/// Default width for the settings window.
pub const SETTINGS_WINDOW_DEFAULT_WIDTH: f32 = 580.0;
/// Maximum width for the settings window.
pub const SETTINGS_WINDOW_MAX_WIDTH: f32 = 800.0;
/// Default height for the settings window (`DEFAULT_WIDTH / φ`).
pub const SETTINGS_WINDOW_DEFAULT_HEIGHT: f32 = SETTINGS_WINDOW_DEFAULT_WIDTH / GOLDEN_RATIO;
/// Minimum height for the settings window (`MIN_WIDTH / φ`).
pub const SETTINGS_WINDOW_MIN_HEIGHT: f32 = SETTINGS_WINDOW_MIN_WIDTH / GOLDEN_RATIO;

/// Margin for standard OSD elements.
pub const OSD_MARGIN: f32 = 12.0;
/// Pixels between main OSD line and optional HDR line (above main, toward image).
pub const OSD_HDR_LINE_GAP: f32 = 3.0;
/// Text size for OSD status information.
pub const OSD_TEXT_SIZE: f32 = 12.0;
/// Text size for OSD error messages.
pub const OSD_ERROR_TEXT_SIZE: f32 = 13.0;
/// Vertical offset for OSD error messages (to avoid overlapping status text).
pub const OSD_ERROR_OFFSET: f32 = 32.0;
/// Extra offset when a second (HDR) OSD line is present.
pub const OSD_ERROR_EXTRA_WHEN_HDR_LINE: f32 = 16.0;
/// Gap between the bottom OSD stack and the hotkeys issue overlay.
pub const HOTKEYS_ISSUE_GAP_ABOVE_OSD: f32 = 10.0;

/// Width deduction for the progress slider in the OSD/HUD to account for labels.
pub const SLIDER_WIDTH_LABEL_OFFSET: f32 = 40.0;

/// Text size for large loading hints.
pub const LOADING_HINT_TEXT_SIZE: f32 = 16.0;

/// Common spacing between items in vertical layouts (e.g. dialogs).
pub const UI_ITEM_SPACING_X: f32 = 8.0;
pub const UI_ITEM_SPACING_Y: f32 = 6.0;

/// The name used for the Local Socket (IPC) channel.
pub const IPC_SOCKET_NAME: &str = "siv_ipc_sock_v1";

/// Maximum allowed size for an IPC payload (8KB) to prevent DoS.
pub const MAX_IPC_PAYLOAD_SIZE: u64 = 8 * 1024;

/// Total deadline for the client instance to connect and forward arguments.
pub const IPC_CLIENT_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(1500);

/// Deadline for the underlying connection attempt.
pub const IPC_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

/// Maximum wait for PSD v1 async full decode when building a directory-tree strip preview.
/// Strip workers run on a 4-thread pool; this must stay well below main-loader decode
/// deadlines to avoid thread-pool starvation.
pub const PSD_V1_ASYNC_DECODE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Row strip height when probing oversized PSB disk-tiled flat Image Data for absolute blank.
pub const PSB_DISK_TILED_BLANK_PROBE_STRIP_ROWS: u32 = 64;

/// Maximum size for the log file (10MB) before rotation.
#[cfg(not(feature = "preload-debug"))]
pub const LOG_FILE_SIZE_LIMIT: u64 = 10 * 1024 * 1024;

/// Larger rotation threshold when built with `--features preload-debug`.
/// Preload diagnostics emit many info lines per frame; 10 MiB fills in minutes and
/// pushes strip / directory-tree logs into rotated files that are soon deleted.
#[cfg(feature = "preload-debug")]
pub const LOG_FILE_SIZE_LIMIT_PRELOAD_DEBUG: u64 = 100 * 1024 * 1024;

/// Number of rotated log files flexi_logger retains (default builds).
#[cfg(not(feature = "preload-debug"))]
pub const LOG_FILE_KEEP_COUNT: usize = 3;

/// Retain more numbered logs in preload-debug builds so a long repro session stays grep-able.
#[cfg(feature = "preload-debug")]
pub const LOG_FILE_KEEP_COUNT_PRELOAD_DEBUG: usize = 8;

/// The clipping threshold for LibRaw's auto-brightness adjustment.
/// A value of 0.01 (1%) provides robust normalization for high-dynamic range images
/// that would otherwise be rendered as all-black due to high sensor black levels.
pub const RAW_AUTO_BRIGHT_THR: f32 = 0.01;

/// Standard SMPTE ST 2084 / ITU-R BT.2100 PQ transfer function coefficients.
pub const PQ_M1: f32 = 2610.0 / 16384.0;
pub const PQ_M2: f32 = 2523.0 / 32.0;
pub const PQ_C1: f32 = 3424.0 / 4096.0;
pub const PQ_C2: f32 = 2413.0 / 128.0;
pub const PQ_C3: f32 = 2392.0 / 128.0;

/// Maximum number of tags allowed in an ICC profile to prevent malformed profile processing loops.
pub const MAX_ICC_TAG_COUNT: usize = 4096;

/// Iteration cap for JXL decoder event loops on probes to ensure early termination on bad inputs.
pub const JXL_PROBE_ITERATION_CAP: usize = 4096;

/// JPEG XL jhgm (gain map) box size limit for DoS protection.
pub const JXL_MAX_GAIN_MAP_BOX_SIZE: u64 = 32 * 1024 * 1024;

/// Minimum on-disk size that can hold a still-image container header (ISO BMFF `ftyp` is 12 bytes).
pub const MIN_IMAGE_FILE_BYTES: u64 = 12;

/// Default buffer/profile size for building ICC profiles in unit tests.
#[cfg(test)]
pub const MOCK_ICC_PROFILE_SIZE: usize = 4096;

/// Pixel region warning dimension threshold (warn when width or height is larger than this).
pub const PIXEL_REGION_WARN_DIM: u32 = 64;
/// Maximum allowed dimension for pixel region inspection.
pub const PIXEL_REGION_MAX_DIM: u32 = 128;
/// Pixel inspector tooltip offset relative to the mouse pointer.
pub const PIXEL_TOOLTIP_OFFSET: f32 = 16.0;
/// Width of the pixel inspector hover tooltip in logical pixels.
pub const PIXEL_TOOLTIP_WIDTH: f32 = 132.0;
/// Height of the pixel inspector hover tooltip in logical pixels.
pub const PIXEL_TOOLTIP_HEIGHT: f32 = 32.0;
/// Square of threshold of pointer movement under which the pointer is considered stationary.
pub const PIXEL_POINTER_STATIONARY_THRESHOLD_SQ: f32 = 0.01;
/// Horizontal inner padding of the pixel inspector hover tooltip in logical pixels.
pub const PIXEL_TOOLTIP_PADDING_X: f32 = 6.0;
/// Vertical inner padding of the pixel inspector hover tooltip in logical pixels.
pub const PIXEL_TOOLTIP_PADDING_Y: f32 = 4.0;

/// Maximum zoom factor multiplier (applied on top of fit-to-window or original-size scale).
pub const ZOOM_FACTOR_MAX: f32 = 20.0;
/// Minimum zoom factor multiplier.
pub const ZOOM_FACTOR_MIN: f32 = 0.05;

#[cfg(test)]
mod decode_dim_tests {
    use super::{
        MAX_STATIC_FULL_DECODE_PIXELS, checked_rgba8_len_u32, validate_static_decode_dimensions,
    };

    #[test]
    fn validate_static_decode_dimensions_rejects_zero_and_oversize() {
        assert!(validate_static_decode_dimensions(0, 10).is_err());
        assert!(validate_static_decode_dimensions(10, 0).is_err());
        assert_eq!(validate_static_decode_dimensions(8, 8).unwrap(), 64);
        let side = ((MAX_STATIC_FULL_DECODE_PIXELS as f64).sqrt() as u32) + 1;
        assert!(validate_static_decode_dimensions(side, side).is_err());
    }

    #[test]
    fn checked_rgba8_len_u32_matches_area_times_four() {
        assert_eq!(checked_rgba8_len_u32(3, 2), Some(24));
        assert_eq!(checked_rgba8_len_u32(u32::MAX, u32::MAX), None);
    }
}
