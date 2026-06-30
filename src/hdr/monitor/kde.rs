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

use super::types::LinuxExplicitHdrState;
use std::collections::HashMap;

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) const KDE_KSCREEN_HDR_STATE_SOURCE: &str = "KDE KScreen";
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const KSCREEN_DOCTOR_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(250);
#[cfg(target_os = "linux")]
const KSCREEN_CACHE_TTL: std::time::Duration = std::time::Duration::from_millis(750);

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn parse_kscreen_hdr_state_for_output(
    outputs: &str,
    output_label: &str,
) -> Option<LinuxExplicitHdrState> {
    parse_kscreen_hdr_states(outputs).remove(output_label)
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_kscreen_hdr_states(outputs: &str) -> HashMap<String, LinuxExplicitHdrState> {
    let mut states = HashMap::new();
    let mut current_output_name: Option<String> = None;
    for line in outputs.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Output:") {
            current_output_name = output_name_from_header(trimmed).map(str::to_string);
            continue;
        }
        if let (Some(output_name), Some(value)) =
            (current_output_name.as_ref(), trimmed.strip_prefix("HDR:"))
            && let Some(state) = parse_hdr_state(value.trim())
        {
            states.insert(output_name.clone(), state);
        }
    }
    states
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn output_name_from_header(header: &str) -> Option<&str> {
    let mut parts = header.split_whitespace();
    (parts.next()? == "Output:").then_some(())?;
    parts.next()?;
    parts.next()
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_hdr_state(value: &str) -> Option<LinuxExplicitHdrState> {
    let state = value.to_ascii_lowercase();
    if state.starts_with("enabled") || state.starts_with("active") || state.starts_with("on") {
        Some(LinuxExplicitHdrState::Enabled)
    } else if state.starts_with("disabled") || state.starts_with("off") {
        Some(LinuxExplicitHdrState::Disabled)
    } else if state.starts_with("incapable")
        || state.starts_with("unsupported")
        || state.starts_with("unavailable")
    {
        Some(LinuxExplicitHdrState::Incapable)
    } else {
        None
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn explicit_hdr_state_for_output(output_label: &str) -> Option<LinuxExplicitHdrState> {
    if !is_kde_session() {
        log_kde_probe_if_changed("skipped: KDE session environment not detected".to_string());
        return None;
    }
    cached_explicit_hdr_state_for_output(output_label)
}

#[cfg(target_os = "linux")]
pub(crate) fn explicit_hdr_state_for_output_blocking(
    output_label: &str,
) -> Option<LinuxExplicitHdrState> {
    if !is_kde_session() {
        log_kde_probe_if_changed("skipped: KDE session environment not detected".to_string());
        return None;
    }
    refresh_kscreen_cache_blocking(output_label)
}

#[cfg(target_os = "linux")]
#[derive(Default)]
struct KscreenCache {
    states: HashMap<String, LinuxExplicitHdrState>,
    last_attempt: Option<std::time::Instant>,
    refresh_in_flight: bool,
}

#[cfg(target_os = "linux")]
fn kscreen_cache() -> &'static parking_lot::Mutex<KscreenCache> {
    use std::sync::OnceLock;

    static CACHE: OnceLock<parking_lot::Mutex<KscreenCache>> = OnceLock::new();
    CACHE.get_or_init(|| parking_lot::Mutex::new(KscreenCache::default()))
}

#[cfg(target_os = "linux")]
fn cached_explicit_hdr_state_for_output(output_label: &str) -> Option<LinuxExplicitHdrState> {
    let now = std::time::Instant::now();
    let (state, refresh_in_flight, should_spawn_refresh) = {
        let mut cache = kscreen_cache().lock();
        let state = cache.states.get(output_label).copied();
        let stale = cache
            .last_attempt
            .is_none_or(|last| now.duration_since(last) >= KSCREEN_CACHE_TTL);
        let should_spawn_refresh = stale && !cache.refresh_in_flight;
        if should_spawn_refresh {
            cache.refresh_in_flight = true;
            cache.last_attempt = Some(now);
        }
        (state, cache.refresh_in_flight, should_spawn_refresh)
    };
    if should_spawn_refresh {
        spawn_kscreen_cache_refresh(output_label.to_string());
    }
    match state {
        Some(state) => log_kde_probe_if_changed(format!(
            "output={output_label} explicit_hdr_state={state:?}"
        )),
        None if refresh_in_flight => log_kde_probe_if_changed(format!(
            "output={output_label} explicit HDR state refresh pending"
        )),
        None => log_kde_probe_if_changed(format!(
            "output={output_label} HDR state not found in cached KScreen output"
        )),
    }
    state
}

#[cfg(target_os = "linux")]
fn refresh_kscreen_cache_blocking(output_label: &str) -> Option<LinuxExplicitHdrState> {
    let outputs = match run_kscreen_doctor_outputs(KSCREEN_DOCTOR_TIMEOUT) {
        Ok(outputs) => outputs,
        Err(err) => {
            log_kde_probe_if_changed(format!("failed: {err}"));
            let mut cache = kscreen_cache().lock();
            cache.last_attempt = Some(std::time::Instant::now());
            cache.refresh_in_flight = false;
            return None;
        }
    };
    let states = parse_kscreen_hdr_states(&outputs);
    let state = states.get(output_label).copied();
    {
        let mut cache = kscreen_cache().lock();
        cache.states = states;
        cache.last_attempt = Some(std::time::Instant::now());
        cache.refresh_in_flight = false;
    }
    match state {
        Some(state) => {
            log_kde_probe_if_changed(format!(
                "output={output_label} explicit_hdr_state={state:?}"
            ));
            Some(state)
        }
        None => {
            log_kde_probe_if_changed(format!(
                "output={output_label} HDR state not found in kscreen-doctor output"
            ));
            None
        }
    }
}

#[cfg(target_os = "linux")]
fn spawn_kscreen_cache_refresh(output_label: String) {
    if let Err(err) = std::thread::Builder::new()
        .name("siv-kde-hdr-probe".to_string())
        .spawn(move || {
            let _ = refresh_kscreen_cache_blocking(&output_label);
        })
    {
        let mut cache = kscreen_cache().lock();
        cache.refresh_in_flight = false;
        log_kde_probe_if_changed(format!("failed: spawn refresh thread failed: {err}"));
    }
}

#[cfg(target_os = "linux")]
fn is_kde_session() -> bool {
    fn env_contains(name: &str, needle: &str) -> bool {
        std::env::var_os(name)
            .and_then(|value| value.into_string().ok())
            .is_some_and(|value| value.to_ascii_lowercase().contains(needle))
    }
    env_contains("XDG_CURRENT_DESKTOP", "kde")
        || env_contains("XDG_SESSION_DESKTOP", "kde")
        || env_contains("DESKTOP_SESSION", "plasma")
        || std::env::var_os("KDE_FULL_SESSION").is_some_and(|value| value == "true")
}

#[cfg(target_os = "linux")]
fn log_kde_probe_if_changed(message: String) {
    use std::sync::OnceLock;

    static LAST_MESSAGE: OnceLock<parking_lot::Mutex<Option<String>>> = OnceLock::new();
    let last = LAST_MESSAGE.get_or_init(|| parking_lot::Mutex::new(None));
    let mut guard = last.lock();
    if guard.as_ref() == Some(&message) {
        return;
    }
    log::info!("[HDR] KDE KScreen explicit HDR probe: {message}");
    *guard = Some(message);
}

#[cfg(target_os = "linux")]
fn run_kscreen_doctor_outputs(timeout: std::time::Duration) -> Result<String, String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::time::Instant;

    let mut child = Command::new("kscreen-doctor")
        .arg("--outputs")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("kscreen-doctor spawn failed: {err}"))?;

    let start = Instant::now();
    loop {
        match child
            .try_wait()
            .map_err(|err| format!("kscreen-doctor wait failed: {err}"))?
        {
            Some(status) => {
                let mut stdout = String::new();
                if let Some(mut pipe) = child.stdout.take() {
                    let _ = pipe.read_to_string(&mut stdout);
                }
                if status.success() {
                    return Ok(stdout);
                }
                let mut stderr = String::new();
                if let Some(mut pipe) = child.stderr.take() {
                    let _ = pipe.read_to_string(&mut stderr);
                }
                return Err(format!(
                    "kscreen-doctor --outputs failed with {status}: {}",
                    stderr.trim()
                ));
            }
            None if start.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("kscreen-doctor --outputs timed out".to_string());
            }
            None => std::thread::sleep(std::time::Duration::from_millis(10)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KSCREEN_OUTPUTS: &str = "\
Output: 1 HDMI-A-3 85ee5fc7-ab2e-479b-94d3-5abdb155c6d9
        enabled
        connected
        HDR: incapable
        Wide Color Gamut: incapable
Output: 2 HDMI-A-1 16a46789-a40c-4208-a7e9-183d638dd8d1
        enabled
        connected
        HDR: enabled
        Wide Color Gamut: enabled
Output: 3 DP-1
        enabled
        connected
        HDR: disabled
        Wide Color Gamut: enabled
";

    #[test]
    fn parses_enabled_hdr_state_for_matching_output() {
        assert_eq!(
            parse_kscreen_hdr_state_for_output(KSCREEN_OUTPUTS, "HDMI-A-1"),
            Some(LinuxExplicitHdrState::Enabled)
        );
    }

    #[test]
    fn parses_incapable_hdr_state_for_matching_output() {
        assert_eq!(
            parse_kscreen_hdr_state_for_output(KSCREEN_OUTPUTS, "HDMI-A-3"),
            Some(LinuxExplicitHdrState::Incapable)
        );
    }

    #[test]
    fn parses_disabled_hdr_state_for_matching_output() {
        assert_eq!(
            parse_kscreen_hdr_state_for_output(KSCREEN_OUTPUTS, "DP-1"),
            Some(LinuxExplicitHdrState::Disabled)
        );
    }

    #[test]
    fn ignores_non_matching_outputs() {
        assert_eq!(
            parse_kscreen_hdr_state_for_output(KSCREEN_OUTPUTS, "DP-2"),
            None
        );
    }
}
