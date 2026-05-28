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

use super::core::{
    CpuArch, PlatformKind, UpdateCandidate, candidate_from_release, current_version,
};
use crate::settings::UpdateSettings;

#[derive(Clone, Debug)]
pub enum UpdateCheckMessage {
    Checking,
    UpToDate,
    Available(UpdateCandidate),
    Failed(String),
}

pub fn today_utc_string() -> String {
    // UTC day number is enough for once-per-day gating and avoids a time crate dependency.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}", now.as_secs() / 86_400)
}

pub fn platform_kind() -> PlatformKind {
    if cfg!(target_os = "windows") {
        PlatformKind::Windows
    } else if cfg!(target_os = "macos") {
        PlatformKind::Macos
    } else {
        PlatformKind::Linux
    }
}

pub fn cpu_arch() -> CpuArch {
    if cfg!(target_arch = "aarch64") {
        CpuArch::Aarch64
    } else {
        CpuArch::X86_64
    }
}

pub fn spawn_update_check(
    settings: UpdateSettings,
    tx: crossbeam_channel::Sender<UpdateCheckMessage>,
) {
    let _ = tx.send(UpdateCheckMessage::Checking);
    let _ = std::thread::Builder::new()
        .name("siv-update-check".to_string())
        .spawn(move || {
            let proxy = settings.proxy.to_proxy_config();
            let proxy = proxy.enabled.then_some(proxy);
            let result = crate::update::net::fetch_latest_release(proxy.as_ref())
                .map(|release| {
                    candidate_from_release(
                        &release,
                        current_version(),
                        settings.ignored_version.as_deref(),
                        platform_kind(),
                        cpu_arch(),
                        cfg!(feature = "legacy_win7"),
                    )
                })
                .map_err(UpdateCheckMessage::Failed);

            match result {
                Ok(Some(candidate)) => {
                    let _ = tx.send(UpdateCheckMessage::Available(candidate));
                }
                Ok(None) => {
                    let _ = tx.send(UpdateCheckMessage::UpToDate);
                }
                Err(message) => {
                    let _ = tx.send(message);
                }
            }
        });
}
