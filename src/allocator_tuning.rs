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

#[cfg(feature = "mimalloc-allocator")]
mod mimalloc_policy {
    use libmimalloc_sys as ffi;
    use std::ffi::c_long;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct MimallocOptionSetting {
        option: ffi::mi_option_t,
        value: c_long,
    }

    // Option ids from bundled mimalloc v3.3.2 in libmimalloc-sys 0.1.49.
    // The Rust sys crate exposes only a subset of constants, so keep these ids
    // covered by tests and revisit them when upgrading mimalloc.
    const MI_OPTION_EAGER_COMMIT: ffi::mi_option_t = 3;
    const MI_OPTION_ARENA_EAGER_COMMIT: ffi::mi_option_t = 4;
    const MI_OPTION_PURGE_DECOMMITS: ffi::mi_option_t = 5;
    const MI_OPTION_ABANDONED_PAGE_PURGE: ffi::mi_option_t = 12;
    const MI_OPTION_PURGE_DELAY: ffi::mi_option_t = 15;
    const MI_OPTION_ARENA_PURGE_MULT: ffi::mi_option_t = 24;
    const MI_OPTION_PAGE_RECLAIM_ON_FREE: ffi::mi_option_t = 35;
    const MI_OPTION_PAGE_FULL_RETAIN: ffi::mi_option_t = 36;

    const IMAGE_VIEWER_MIMALLOC_SETTINGS: &[MimallocOptionSetting] = &[
        // Large image decode/upload bursts allocate many short-lived buffers.
        // Avoid eager OS commit so address-space reservations do not immediately
        // turn into committed working set.
        MimallocOptionSetting {
            option: MI_OPTION_EAGER_COMMIT,
            value: 0,
        },
        MimallocOptionSetting {
            option: MI_OPTION_ARENA_EAGER_COMMIT,
            value: 0,
        },
        // Prefer returning unused pages to the OS quickly after a navigation burst.
        // A 100ms delay is short enough to reduce post-flip high-water memory, but
        // avoids forcing synchronous collection in the hot path.
        MimallocOptionSetting {
            option: MI_OPTION_PURGE_DECOMMITS,
            value: 1,
        },
        MimallocOptionSetting {
            option: MI_OPTION_ABANDONED_PAGE_PURGE,
            value: 1,
        },
        MimallocOptionSetting {
            option: MI_OPTION_PURGE_DELAY,
            value: 100,
        },
        // Keep arena purge thresholds aggressive for this workload; image viewers
        // prefer lower retained memory over maximum allocator cache reuse.
        MimallocOptionSetting {
            option: MI_OPTION_ARENA_PURGE_MULT,
            value: 1,
        },
        // Do not retain fully free pages just in case another image needs them.
        // Background preloading already limits concurrency, so allocator retention
        // is more harmful than useful after large file flips.
        MimallocOptionSetting {
            option: MI_OPTION_PAGE_RECLAIM_ON_FREE,
            value: 0,
        },
        MimallocOptionSetting {
            option: MI_OPTION_PAGE_FULL_RETAIN,
            value: 0,
        },
    ];

    pub(super) fn configure() {
        unsafe {
            for setting in IMAGE_VIEWER_MIMALLOC_SETTINGS {
                ffi::mi_option_set_default(setting.option, setting.value);
            }
        }
    }

    pub(super) fn version() -> i32 {
        unsafe { ffi::mi_version() }
    }

    #[cfg(test)]
    mod tests {
        use super::{
            IMAGE_VIEWER_MIMALLOC_SETTINGS, MI_OPTION_ABANDONED_PAGE_PURGE,
            MI_OPTION_ARENA_EAGER_COMMIT, MI_OPTION_ARENA_PURGE_MULT, MI_OPTION_EAGER_COMMIT,
            MI_OPTION_PAGE_FULL_RETAIN, MI_OPTION_PAGE_RECLAIM_ON_FREE, MI_OPTION_PURGE_DECOMMITS,
            MI_OPTION_PURGE_DELAY, MimallocOptionSetting,
        };

        #[test]
        fn image_viewer_mimalloc_settings_match_bundled_v3_option_ids() {
            assert_eq!(MI_OPTION_EAGER_COMMIT, 3);
            assert_eq!(MI_OPTION_ARENA_EAGER_COMMIT, 4);
            assert_eq!(MI_OPTION_PURGE_DECOMMITS, 5);
            assert_eq!(MI_OPTION_ABANDONED_PAGE_PURGE, 12);
            assert_eq!(MI_OPTION_PURGE_DELAY, 15);
            assert_eq!(MI_OPTION_ARENA_PURGE_MULT, 24);
            assert_eq!(MI_OPTION_PAGE_RECLAIM_ON_FREE, 35);
            assert_eq!(MI_OPTION_PAGE_FULL_RETAIN, 36);
        }

        #[test]
        fn image_viewer_mimalloc_settings_are_memory_conservative() {
            assert!(
                IMAGE_VIEWER_MIMALLOC_SETTINGS.contains(&MimallocOptionSetting {
                    option: MI_OPTION_PURGE_DELAY,
                    value: 100,
                })
            );
            assert!(
                IMAGE_VIEWER_MIMALLOC_SETTINGS.contains(&MimallocOptionSetting {
                    option: MI_OPTION_PURGE_DECOMMITS,
                    value: 1,
                })
            );
            assert!(
                IMAGE_VIEWER_MIMALLOC_SETTINGS.contains(&MimallocOptionSetting {
                    option: MI_OPTION_PAGE_FULL_RETAIN,
                    value: 0,
                })
            );
        }
    }
}

pub(crate) fn configure_mimalloc_for_image_viewer() {
    #[cfg(feature = "mimalloc-allocator")]
    mimalloc_policy::configure();
}

pub(crate) fn mimalloc_version() -> i32 {
    #[cfg(feature = "mimalloc-allocator")]
    {
        return mimalloc_policy::version();
    }
    #[cfg(not(feature = "mimalloc-allocator"))]
    0
}
