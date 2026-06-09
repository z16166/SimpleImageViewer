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

use std::time::Duration;

pub(crate) const AUDIO_HW_POS_ZERO_GRACE: Duration = Duration::from_millis(500);

#[cfg(windows)]
unsafe extern "C" {
    pub(crate) fn wasapi_monitor_init();
    pub(crate) fn wasapi_monitor_uninit();
    pub(crate) fn wasapi_is_device_available() -> bool;
    pub(crate) fn wasapi_poll_device_lost() -> bool;
}

#[cfg(not(windows))]
pub(crate) unsafe fn wasapi_monitor_init() {}

#[cfg(not(windows))]
pub(crate) unsafe fn wasapi_monitor_uninit() {}

#[cfg(not(windows))]
pub(crate) unsafe fn wasapi_is_device_available() -> bool {
    true
}

#[cfg(not(windows))]
pub(crate) unsafe fn wasapi_poll_device_lost() -> bool {
    false
}
