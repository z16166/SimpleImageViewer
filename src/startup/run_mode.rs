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

//! Launch-time run mode parsing (normal viewer vs screensaver host protocols).

use std::ffi::OsString;
use std::path::PathBuf;

/// Platform-native parent window handle for preview/config embedding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformWindowHandle {
    #[cfg(target_os = "windows")]
    Win32(isize),
}

/// Screensaver host phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaverPhase {
    /// Full-screen slideshow (CLI kiosk or Windows `/s`).
    Run,
    /// Configuration UI (CLI config or Windows `/c`).
    Config,
    /// Lightweight preview (Windows `/p`).
    Preview,
}

/// Application run mode selected from argv / platform protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppRunMode {
    Normal {
        image_path: Option<PathBuf>,
    },
    Screensaver {
        phase: SaverPhase,
        parent: Option<PlatformWindowHandle>,
        /// Optional CLI overrides applied on top of screensaver settings.
        cli: ScreensaverCliOverrides,
    },
}

/// Cross-platform screensaver CLI overrides (Phase 0).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScreensaverCliOverrides {
    pub source: Option<PathBuf>,
    /// Interval seconds; kept as integer millis-friendly f32 string parse result stored as f32 bits via string in Eq: use Option with ordered f32 via bits.
    pub interval_secs: Option<OrderedF32>,
    pub random: Option<bool>,
    pub recursive: Option<bool>,
    pub display: Option<ScreensaverDisplayPolicy>,
    pub exit_on_input: Option<bool>,
    pub power_save: Option<bool>,
}

/// Display coverage policy for screensaver run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScreensaverDisplayPolicy {
    #[default]
    All,
    Primary,
}

/// `f32` wrapper that implements `Eq` via bit identity (CLI parse only).
#[derive(Debug, Clone, Copy)]
pub struct OrderedF32(pub f32);

impl PartialEq for OrderedF32 {
    fn eq(&self, other: &Self) -> bool {
        self.0.to_bits() == other.0.to_bits()
    }
}

impl Eq for OrderedF32 {}

impl AppRunMode {
    #[inline]
    pub fn is_screensaver(&self) -> bool {
        matches!(self, Self::Screensaver { .. })
    }

    #[inline]
    pub fn is_screensaver_run(&self) -> bool {
        matches!(
            self,
            Self::Screensaver {
                phase: SaverPhase::Run,
                ..
            }
        )
    }

    #[inline]
    pub fn is_screensaver_config(&self) -> bool {
        matches!(
            self,
            Self::Screensaver {
                phase: SaverPhase::Config,
                ..
            }
        )
    }

    #[inline]
    pub fn is_screensaver_preview(&self) -> bool {
        matches!(
            self,
            Self::Screensaver {
                phase: SaverPhase::Preview,
                ..
            }
        )
    }

    /// Single-instance IPC must stay off for every screensaver host phase so a
    /// running viewer cannot swallow `/s`, `/c`, `/p`, or CLI kiosk launches.
    #[inline]
    pub fn bypass_single_instance_ipc(&self) -> bool {
        self.is_screensaver()
    }

    /// Tray icon / minimize-to-tray must never arm in screensaver hosts.
    #[inline]
    pub fn bypass_tray(&self) -> bool {
        self.is_screensaver()
    }

    /// Runtime settings / window placement must not pollute the normal viewer YAML.
    #[inline]
    pub fn settings_writeback_blocked(&self) -> bool {
        matches!(
            self,
            Self::Screensaver {
                phase: SaverPhase::Run | SaverPhase::Preview,
                ..
            }
        )
    }

    #[inline]
    pub fn image_path(&self) -> Option<&PathBuf> {
        match self {
            Self::Normal { image_path } => image_path.as_ref(),
            Self::Screensaver { .. } => None,
        }
    }

    #[inline]
    pub fn screensaver_cli(&self) -> Option<&ScreensaverCliOverrides> {
        match self {
            Self::Screensaver { cli, .. } => Some(cli),
            Self::Normal { .. } => None,
        }
    }
}

/// Parse process argv into an [`AppRunMode`].
///
/// Priority:
/// 1. Windows screensaver protocol (`/s`, `/c`, `/p`)
/// 2. Cross-platform `--mode=screensaver` (and `--mode screensaver`)
/// 3. Normal viewer (optional first file path)
pub fn parse_launch_mode<I, S>(args: I) -> AppRunMode
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let args: Vec<OsString> = args.into_iter().map(Into::into).collect();
    // Drop argv[0] (program name) when present.
    let args = if args.is_empty() {
        args
    } else {
        args[1..].to_vec()
    };

    if let Some(mode) = parse_windows_screensaver_protocol(&args) {
        return mode;
    }
    if let Some(mode) = parse_screensaver_cli(&args) {
        return mode;
    }
    parse_normal_mode(&args)
}

fn parse_normal_mode(args: &[OsString]) -> AppRunMode {
    let mut image_path = None;
    for arg in args {
        let path = PathBuf::from(arg);
        if path.is_file() {
            image_path = Some(path);
            break;
        }
    }
    AppRunMode::Normal { image_path }
}

fn parse_screensaver_cli(args: &[OsString]) -> Option<AppRunMode> {
    let mut mode_screensaver = false;
    let mut phase = SaverPhase::Run;
    let mut cli = ScreensaverCliOverrides::default();
    let mut i = 0usize;
    while i < args.len() {
        let raw = args[i].to_string_lossy();
        let arg = raw.as_ref();

        if let Some(value) = arg.strip_prefix("--mode=") {
            if eq_ignore_ascii_case(value, "screensaver") {
                mode_screensaver = true;
            } else if eq_ignore_ascii_case(value, "normal") {
                return None;
            } else {
                // Unknown mode token: treat as non-screensaver so normal path can still run.
                return None;
            }
            i += 1;
            continue;
        }
        if arg == "--mode" {
            if let Some(value) = args.get(i + 1) {
                let v = value.to_string_lossy();
                if eq_ignore_ascii_case(v.as_ref(), "screensaver") {
                    mode_screensaver = true;
                    i += 2;
                    continue;
                }
                if eq_ignore_ascii_case(v.as_ref(), "normal") {
                    return None;
                }
            }
            return None;
        }

        if let Some(value) = arg.strip_prefix("--phase=") {
            phase = parse_phase_token(value).unwrap_or(SaverPhase::Run);
            i += 1;
            continue;
        }
        if arg == "--phase"
            && let Some(value) = args.get(i + 1)
        {
            phase = parse_phase_token(&value.to_string_lossy()).unwrap_or(SaverPhase::Run);
            i += 2;
            continue;
        }

        if let Some(value) = arg.strip_prefix("--source=") {
            let path = PathBuf::from(value);
            if !path.as_os_str().is_empty() {
                cli.source = Some(path);
            }
            i += 1;
            continue;
        }
        if arg == "--source"
            && let Some(value) = args.get(i + 1)
        {
            let path = PathBuf::from(value);
            if !path.as_os_str().is_empty() {
                cli.source = Some(path);
            }
            i += 2;
            continue;
        }

        if let Some(value) = arg.strip_prefix("--interval=") {
            if let Ok(secs) = value.parse::<f32>()
                && secs.is_finite()
                && secs > 0.0
            {
                cli.interval_secs = Some(OrderedF32(secs));
            }
            i += 1;
            continue;
        }
        if arg == "--interval"
            && let Some(value) = args.get(i + 1)
            && let Ok(secs) = value.to_string_lossy().parse::<f32>()
            && secs.is_finite()
            && secs > 0.0
        {
            cli.interval_secs = Some(OrderedF32(secs));
            i += 2;
            continue;
        }

        if let Some(value) = arg.strip_prefix("--display=") {
            cli.display = Some(parse_display_token(value));
            i += 1;
            continue;
        }
        if arg == "--display"
            && let Some(value) = args.get(i + 1)
        {
            cli.display = Some(parse_display_token(&value.to_string_lossy()));
            i += 2;
            continue;
        }

        if arg == "--random" {
            cli.random = Some(true);
            i += 1;
            continue;
        }
        if arg == "--no-random" {
            cli.random = Some(false);
            i += 1;
            continue;
        }
        if arg == "--recursive" {
            cli.recursive = Some(true);
            i += 1;
            continue;
        }
        if arg == "--no-recursive" {
            cli.recursive = Some(false);
            i += 1;
            continue;
        }
        if arg == "--exit-on-input" {
            cli.exit_on_input = Some(true);
            i += 1;
            continue;
        }
        if arg == "--no-exit-on-input" {
            cli.exit_on_input = Some(false);
            i += 1;
            continue;
        }
        if arg == "--power-save" {
            cli.power_save = Some(true);
            i += 1;
            continue;
        }
        if arg == "--no-power-save" {
            cli.power_save = Some(false);
            i += 1;
            continue;
        }
        // Compatibility aliases from the design doc (always true for screensaver hosts).
        if arg == "--no-single-instance" || arg == "--no-tray" || arg == "--ui=minimal" {
            i += 1;
            continue;
        }
        if let Some(value) = arg.strip_prefix("--ui=") {
            let _ = value;
            i += 1;
            continue;
        }

        i += 1;
    }

    if !mode_screensaver {
        return None;
    }

    Some(AppRunMode::Screensaver {
        phase,
        parent: None,
        cli,
    })
}

fn parse_phase_token(value: &str) -> Option<SaverPhase> {
    if eq_ignore_ascii_case(value, "run") {
        Some(SaverPhase::Run)
    } else if eq_ignore_ascii_case(value, "config") {
        Some(SaverPhase::Config)
    } else if eq_ignore_ascii_case(value, "preview") {
        Some(SaverPhase::Preview)
    } else {
        None
    }
}

fn parse_display_token(value: &str) -> ScreensaverDisplayPolicy {
    if eq_ignore_ascii_case(value, "primary") {
        ScreensaverDisplayPolicy::Primary
    } else {
        ScreensaverDisplayPolicy::All
    }
}

/// Windows Control Panel / desk.cpl protocol.
///
/// Accepts `/s`, `-s`, `/c`, `/c:HWND`, `/c HWND`, `/p HWND`, `/p:HWND` (case-insensitive).
fn parse_windows_screensaver_protocol(args: &[OsString]) -> Option<AppRunMode> {
    if args.is_empty() {
        return None;
    }

    // Only engage when the first non-empty token looks like a Windows screensaver switch.
    // This keeps Linux/macOS path args and `--mode` free of false positives on `/s` files.
    let first = args[0].to_string_lossy();
    let first_trim = first.trim();
    if first_trim.is_empty() {
        return None;
    }
    let first_bytes = first_trim.as_bytes();
    let looks_like_switch = first_bytes[0] == b'/' || first_bytes[0] == b'-';
    if !looks_like_switch {
        return None;
    }

    // Collect only switch-like tokens; reject if mixed with a file path that is not HWND.
    let mut phase: Option<SaverPhase> = None;
    let mut parent: Option<PlatformWindowHandle> = None;
    let mut i = 0usize;
    while i < args.len() {
        let raw = args[i].to_string_lossy();
        let token = raw.trim();
        if token.is_empty() {
            i += 1;
            continue;
        }

        let (switch, inline_hwnd) = split_windows_switch(token)?;
        let switch_l = switch.to_ascii_lowercase();
        match switch_l.as_str() {
            "s" => {
                phase = Some(SaverPhase::Run);
                i += 1;
            }
            "c" => {
                phase = Some(SaverPhase::Config);
                if let Some(hwnd) = inline_hwnd {
                    parent = parse_hwnd_token(&hwnd);
                } else if let Some(next) = args.get(i + 1) {
                    let next_s = next.to_string_lossy();
                    if looks_like_hwnd_token(next_s.as_ref()) {
                        parent = parse_hwnd_token(next_s.as_ref());
                        i += 1;
                    }
                }
                i += 1;
            }
            "p" => {
                phase = Some(SaverPhase::Preview);
                if let Some(hwnd) = inline_hwnd {
                    parent = parse_hwnd_token(&hwnd);
                } else if let Some(next) = args.get(i + 1) {
                    parent = parse_hwnd_token(&next.to_string_lossy());
                    i += 1;
                }
                i += 1;
            }
            // `/a` password change is obsolete; ignore but stay in protocol mode if already selected.
            "a" => {
                i += 1;
            }
            _ => {
                // Not a recognized Windows screensaver switch.
                phase?;
                i += 1;
            }
        }
    }

    let phase = phase?;
    Some(AppRunMode::Screensaver {
        phase,
        parent,
        cli: ScreensaverCliOverrides::default(),
    })
}

fn split_windows_switch(token: &str) -> Option<(String, Option<String>)> {
    let bytes = token.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    if bytes[0] != b'/' && bytes[0] != b'-' {
        return None;
    }
    let body = &token[1..];
    if body.is_empty() {
        return None;
    }
    if let Some((sw, hwnd)) = body.split_once(':') {
        Some((sw.to_string(), Some(hwnd.to_string())))
    } else if let Some((sw, hwnd)) = body.split_once('=') {
        Some((sw.to_string(), Some(hwnd.to_string())))
    } else {
        Some((body.to_string(), None))
    }
}

fn looks_like_hwnd_token(s: &str) -> bool {
    let t = s.trim();
    if t.is_empty() {
        return false;
    }
    // decimal or 0x hex
    if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        return !hex.is_empty() && hex.chars().all(|c| c.is_ascii_hexdigit());
    }
    t.chars().all(|c| c.is_ascii_digit())
}

fn parse_hwnd_token(s: &str) -> Option<PlatformWindowHandle> {
    #[cfg(target_os = "windows")]
    {
        let t = s.trim();
        if t.is_empty() {
            return None;
        }
        let value = if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
            isize::from_str_radix(hex, 16).ok()?
        } else {
            t.parse::<isize>().ok()?
        };
        if value == 0 {
            return None;
        }
        Some(PlatformWindowHandle::Win32(value))
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = s;
        None
    }
}

fn eq_ignore_ascii_case(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(args: &[&str]) -> AppRunMode {
        let mut full = vec![OsString::from("SimpleImageViewer")];
        full.extend(args.iter().map(OsString::from));
        parse_launch_mode(full)
    }

    #[test]
    fn normal_without_args() {
        assert_eq!(mode(&[]), AppRunMode::Normal { image_path: None });
    }

    #[test]
    fn screensaver_cli_mode_flag() {
        let m = mode(&[
            "--mode=screensaver",
            "--source=C:\\Photos",
            "--interval=8",
            "--random",
        ]);
        match m {
            AppRunMode::Screensaver {
                phase: SaverPhase::Run,
                parent: None,
                cli,
            } => {
                assert_eq!(cli.source, Some(PathBuf::from("C:\\Photos")));
                assert_eq!(cli.interval_secs, Some(OrderedF32(8.0)));
                assert_eq!(cli.random, Some(true));
            }
            other => panic!("unexpected mode: {other:?}"),
        }
    }

    #[test]
    fn screensaver_cli_phase_config() {
        let m = mode(&["--mode", "screensaver", "--phase=config"]);
        assert!(matches!(
            m,
            AppRunMode::Screensaver {
                phase: SaverPhase::Config,
                ..
            }
        ));
    }

    #[test]
    fn windows_s_switch() {
        let m = mode(&["/s"]);
        assert!(matches!(
            m,
            AppRunMode::Screensaver {
                phase: SaverPhase::Run,
                parent: None,
                ..
            }
        ));
    }

    #[test]
    fn windows_c_with_inline_hwnd() {
        let m = mode(&["/c:12345"]);
        match m {
            AppRunMode::Screensaver {
                phase: SaverPhase::Config,
                parent,
                ..
            } => {
                #[cfg(target_os = "windows")]
                assert_eq!(parent, Some(PlatformWindowHandle::Win32(12345)));
                #[cfg(not(target_os = "windows"))]
                assert!(parent.is_none());
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn windows_p_with_separate_hwnd() {
        let m = mode(&["/p", "0xABC"]);
        match m {
            AppRunMode::Screensaver {
                phase: SaverPhase::Preview,
                parent,
                ..
            } => {
                #[cfg(target_os = "windows")]
                assert_eq!(parent, Some(PlatformWindowHandle::Win32(0xABC)));
                #[cfg(not(target_os = "windows"))]
                assert!(parent.is_none());
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn windows_switches_bypass_ipc_and_tray() {
        let m = mode(&["/s"]);
        assert!(m.bypass_single_instance_ipc());
        assert!(m.bypass_tray());
        assert!(m.settings_writeback_blocked());
    }

    #[test]
    fn config_phase_allows_writeback() {
        let m = mode(&["/c"]);
        assert!(m.bypass_single_instance_ipc());
        assert!(m.bypass_tray());
        assert!(!m.settings_writeback_blocked());
    }

    #[test]
    fn dash_s_is_windows_run() {
        let m = mode(&["-s"]);
        assert!(m.is_screensaver_run());
    }

    #[test]
    fn normal_image_path_not_swallowed_by_windows_protocol() {
        let m = mode(&[r"C:\Photos\a.jpg"]);
        assert!(matches!(m, AppRunMode::Normal { .. }));
    }

    #[test]
    fn preview_phase_blocks_writeback() {
        let m = mode(&["/p", "1"]);
        assert!(m.is_screensaver_preview());
        assert!(m.settings_writeback_blocked());
        assert!(m.bypass_single_instance_ipc());
        assert!(m.bypass_tray());
    }

    #[test]
    fn cli_no_random_and_display_primary() {
        let m = mode(&[
            "--mode=screensaver",
            "--no-random",
            "--display=primary",
            "--interval",
            "12.5",
        ]);
        match m {
            AppRunMode::Screensaver { cli, .. } => {
                assert_eq!(cli.random, Some(false));
                assert_eq!(cli.display, Some(ScreensaverDisplayPolicy::Primary));
                assert_eq!(cli.interval_secs, Some(OrderedF32(12.5)));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn windows_protocol_priority_over_mode_flag() {
        // First token is a Windows switch, so /s wins even if --mode is also present later.
        let m = mode(&["/s", "--mode=screensaver", "--phase=config"]);
        assert!(m.is_screensaver_run());
    }
}
