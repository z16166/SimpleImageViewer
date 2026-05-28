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
    let days = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        / 86_400;
    iso_date_from_unix_days(days as i64)
}

fn iso_date_from_unix_days(days: i64) -> String {
    // Civil date conversion from Howard Hinnant's days_from_civil inverse.
    // It keeps the persisted setting human-readable without adding a time crate.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    let year = y + if m <= 2 { 1 } else { 0 };
    format!("{year:04}-{m:02}-{d:02}")
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
    let tx_for_spawn_error = tx.clone();
    if let Err(err) = std::thread::Builder::new()
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
        })
    {
        let _ = tx_for_spawn_error.send(UpdateCheckMessage::Failed(format!(
            "failed to start update check thread: {err}"
        )));
    }
}

#[cfg(test)]
mod tests {
    use super::iso_date_from_unix_days;

    #[test]
    fn unix_days_convert_to_iso_utc_date() {
        assert_eq!(iso_date_from_unix_days(0), "1970-01-01");
        assert_eq!(iso_date_from_unix_days(20_602), "2026-05-29");
    }
}
