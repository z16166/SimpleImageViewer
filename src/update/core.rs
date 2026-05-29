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

use std::path::Path;

pub const GITHUB_LATEST_RELEASE_API: &str =
    "https://api.github.com/repos/z16166/SimpleImageViewer/releases/latest";
pub const GITHUB_RELEASES_PAGE: &str = "https://github.com/z16166/SimpleImageViewer/releases";
pub const SHA256SUMS_ASSET: &str = "SHA256SUMS.txt";
pub const CHANGELOG_ASSET_PREFIX: &str = "CHANGELOG.";
pub const CHANGELOG_ASSET_SUFFIX: &str = ".md";
pub const UPDATE_USER_AGENT: &str = "SimpleImageViewer-update-checker";

pub fn github_token_for_request(token: &str) -> Option<&str> {
    let trimmed = token.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
#[derive(Clone, Copy)]
pub enum PlatformKind {
    Windows,
    Macos,
    Linux,
}

#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
#[derive(Clone, Copy)]
pub enum CpuArch {
    X86_64,
    Aarch64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxyType {
    Http,
    Socks5,
    Socks5h,
}

impl ProxyType {
    pub const ALL: [Self; 3] = [Self::Http, Self::Socks5, Self::Socks5h];

    pub fn label_key(self) -> &'static str {
        match self {
            Self::Http => "update.proxy_type_http",
            Self::Socks5 => "update.proxy_type_socks5_local_dns",
            Self::Socks5h => "update.proxy_type_socks5h_proxy_dns",
        }
    }
}

impl Default for ProxyType {
    fn default() -> Self {
        Self::Http
    }
}

#[derive(Clone)]
pub struct ProxyConfig {
    pub enabled: bool,
    pub proxy_type: ProxyType,
    pub host: String,
    pub port: u16,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub struct GithubRelease {
    pub tag_name: String,
    pub html_url: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub published_at: String,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub assets: Vec<GithubAsset>,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub struct GithubAsset {
    pub name: String,
    pub browser_download_url: String,
    #[serde(default)]
    pub size: u64,
}

#[derive(Clone, Debug)]
pub struct UpdateCandidate {
    pub version: String,
    pub release_page_url: String,
    pub release_notes: String,
    pub localized_changelog_url: Option<String>,
    pub published_at: String,
    pub asset_name: String,
    pub asset_url: String,
    pub asset_size: u64,
    pub checksum_url: Option<String>,
}

pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

pub fn normalize_version(raw: &str) -> Option<semver::Version> {
    let trimmed = raw.trim().trim_start_matches('v').trim_start_matches('V');
    semver::Version::parse(trimmed).ok()
}

pub fn is_newer_version(current: &str, latest: &str) -> bool {
    match (normalize_version(current), normalize_version(latest)) {
        (Some(current), Some(latest)) => latest > current,
        _ => false,
    }
}

pub fn should_check_today(last_check_date_utc: Option<&str>, today_utc: &str) -> bool {
    last_check_date_utc != Some(today_utc)
}

/// Returns `Some(())` when proxy is disabled or fully configured.
pub fn validate_proxy_config(config: &ProxyConfig) -> Result<(), ()> {
    if !config.enabled {
        return Ok(());
    }
    if config.host.trim().is_empty() || config.port == 0 {
        return Err(());
    }
    Ok(())
}

pub fn proxy_url(config: &ProxyConfig) -> Option<String> {
    if validate_proxy_config(config).is_err() {
        return None;
    }
    let scheme = match config.proxy_type {
        ProxyType::Http => "http",
        ProxyType::Socks5 => "socks5",
        ProxyType::Socks5h => "socks5h",
    };
    Some(format!(
        "{}://{}:{}",
        scheme,
        config.host.trim(),
        config.port
    ))
}

pub fn expected_asset_name(platform: PlatformKind, arch: CpuArch, legacy_win7: bool) -> String {
    if legacy_win7 && matches!(platform, PlatformKind::Windows) && matches!(arch, CpuArch::X86_64) {
        return "SimpleImageViewer-win7-x64.zip".to_string();
    }
    let target = match (platform, arch) {
        (PlatformKind::Windows, CpuArch::X86_64) => "x86_64-pc-windows-msvc",
        (PlatformKind::Windows, CpuArch::Aarch64) => "aarch64-pc-windows-msvc",
        (PlatformKind::Macos, CpuArch::X86_64) => "x86_64-apple-darwin",
        (PlatformKind::Macos, CpuArch::Aarch64) => "aarch64-apple-darwin",
        (PlatformKind::Linux, CpuArch::X86_64) => "x86_64-unknown-linux-gnu",
        (PlatformKind::Linux, CpuArch::Aarch64) => "aarch64-unknown-linux-gnu",
    };
    let ext = match platform {
        PlatformKind::Linux => "tar.gz",
        PlatformKind::Windows | PlatformKind::Macos => "zip",
    };
    format!("SimpleImageViewer-{target}.{ext}")
}

pub fn parse_sha256sums(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let hash = parts.next()?;
            let file = parts.next()?;
            if hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()) {
                Some((
                    file.trim_start_matches('*').to_string(),
                    hash.to_ascii_lowercase(),
                ))
            } else {
                None
            }
        })
        .collect()
}

pub fn checksum_for_asset(text: &str, asset_name: &str) -> Option<String> {
    parse_sha256sums(text)
        .into_iter()
        .find_map(|(file, hash)| (file == asset_name).then_some(hash))
}

pub fn changelog_asset_names_for_locale(locale: &str) -> Vec<String> {
    let normalized = locale.trim();
    let mut locales = Vec::new();
    if !normalized.is_empty() {
        locales.push(normalized.to_string());
        let lower = normalized.to_ascii_lowercase();
        match lower.as_str() {
            "zh-cn" | "zh-hans" | "zh-sg" => locales.push("zh-CN".to_string()),
            "zh-tw" | "zh-hant" => locales.push("zh-TW".to_string()),
            "zh-hk" | "zh-mo" => locales.push("zh-HK".to_string()),
            _ if lower.starts_with("zh") => locales.push("zh-CN".to_string()),
            _ => {}
        }
    }
    locales.push("en".to_string());
    locales.dedup();
    locales
        .into_iter()
        .map(|locale| format!("{CHANGELOG_ASSET_PREFIX}{locale}{CHANGELOG_ASSET_SUFFIX}"))
        .collect()
}

pub fn localized_changelog_url_for_release(
    release: &GithubRelease,
    locale: &str,
) -> Option<String> {
    changelog_asset_names_for_locale(locale)
        .into_iter()
        .find_map(|name| {
            release
                .assets
                .iter()
                .find(|asset| asset.name == name)
                .map(|asset| asset.browser_download_url.clone())
        })
}

pub fn candidate_from_release(
    release: &GithubRelease,
    current_version: &str,
    ignored_version: Option<&str>,
    platform: PlatformKind,
    arch: CpuArch,
    legacy_win7: bool,
    locale: &str,
) -> Option<UpdateCandidate> {
    if release.draft || release.prerelease {
        return None;
    }
    if !is_newer_version(current_version, &release.tag_name) {
        return None;
    }
    let version = normalize_version(&release.tag_name)?.to_string();
    if ignored_version
        .and_then(normalize_version)
        .is_some_and(|ignored| ignored.to_string() == version)
    {
        return None;
    }
    let asset_name = expected_asset_name(platform, arch, legacy_win7);
    let asset = release
        .assets
        .iter()
        .find(|asset| asset.name == asset_name)?;
    let checksum_url = release
        .assets
        .iter()
        .find(|asset| asset.name == SHA256SUMS_ASSET)
        .map(|asset| asset.browser_download_url.clone());
    Some(UpdateCandidate {
        version,
        release_page_url: release.html_url.clone(),
        release_notes: release.body.clone(),
        localized_changelog_url: localized_changelog_url_for_release(release, locale),
        published_at: release.published_at.clone(),
        asset_name: asset.name.clone(),
        asset_url: asset.browser_download_url.clone(),
        asset_size: asset.size,
        checksum_url,
    })
}

pub fn is_safe_archive_path(path: &str) -> bool {
    // Callers normalize `\` to `/` first, then we only allow plain path components.
    // This rejects absolute paths, prefixes, `.` and `..` traversal. Windows device
    // names such as `CON` are not accepted as special cases here because release
    // archives are expected to contain only app-owned filenames checked later.
    let p = Path::new(path);
    !p.is_absolute()
        && p.components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_compare_accepts_v_prefix() {
        assert!(is_newer_version("2.2.1", "v2.2.2"));
        assert!(!is_newer_version("2.2.2", "v2.2.2"));
        assert!(!is_newer_version("2.2.3", "v2.2.2"));
    }

    #[test]
    fn daily_gate_checks_only_once_per_utc_date() {
        assert!(should_check_today(None, "2026-05-29"));
        assert!(should_check_today(Some("2026-05-28"), "2026-05-29"));
        assert!(!should_check_today(Some("2026-05-29"), "2026-05-29"));
    }

    #[test]
    fn github_token_for_request_ignores_blank_values() {
        assert_eq!(github_token_for_request(""), None);
        assert_eq!(github_token_for_request("   "), None);
        assert_eq!(github_token_for_request("ghp_test"), Some("ghp_test"));
    }

    #[test]
    fn proxy_url_builds_from_split_fields() {
        let config = ProxyConfig {
            enabled: true,
            proxy_type: ProxyType::Socks5,
            host: "127.0.0.1".to_string(),
            port: 1080,
        };
        assert_eq!(
            proxy_url(&config),
            Some("socks5://127.0.0.1:1080".to_string())
        );
    }

    #[test]
    fn validate_proxy_config_rejects_incomplete_settings() {
        assert!(
            validate_proxy_config(&ProxyConfig {
                enabled: false,
                proxy_type: ProxyType::Http,
                host: String::new(),
                port: 0,
            })
            .is_ok()
        );
        assert!(
            validate_proxy_config(&ProxyConfig {
                enabled: true,
                proxy_type: ProxyType::Http,
                host: String::new(),
                port: 0,
            })
            .is_err()
        );
        assert!(
            validate_proxy_config(&ProxyConfig {
                enabled: true,
                proxy_type: ProxyType::Http,
                host: "127.0.0.1".to_string(),
                port: 0,
            })
            .is_err()
        );
        assert!(
            validate_proxy_config(&ProxyConfig {
                enabled: true,
                proxy_type: ProxyType::Http,
                host: String::new(),
                port: 1080,
            })
            .is_err()
        );
    }

    #[test]
    fn proxy_url_supports_socks5h_remote_dns() {
        let config = ProxyConfig {
            enabled: true,
            proxy_type: ProxyType::Socks5h,
            host: "127.0.0.1".to_string(),
            port: 1080,
        };
        assert_eq!(
            proxy_url(&config),
            Some("socks5h://127.0.0.1:1080".to_string())
        );
    }

    #[test]
    fn asset_name_matches_platform_and_arch() {
        assert_eq!(
            expected_asset_name(PlatformKind::Windows, CpuArch::X86_64, false),
            "SimpleImageViewer-x86_64-pc-windows-msvc.zip"
        );
        assert_eq!(
            expected_asset_name(PlatformKind::Linux, CpuArch::Aarch64, false),
            "SimpleImageViewer-aarch64-unknown-linux-gnu.tar.gz"
        );
        assert_eq!(
            expected_asset_name(PlatformKind::Windows, CpuArch::X86_64, true),
            "SimpleImageViewer-win7-x64.zip"
        );
    }

    #[test]
    fn checksum_parser_finds_asset_hash() {
        let sums = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef  SimpleImageViewer-x86_64-pc-windows-msvc.zip\n";
        assert_eq!(
            checksum_for_asset(sums, "SimpleImageViewer-x86_64-pc-windows-msvc.zip"),
            Some("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string())
        );
    }

    #[test]
    fn archive_path_validation_rejects_escape_paths() {
        assert!(is_safe_archive_path("SimpleImageViewer.exe"));
        assert!(is_safe_archive_path("assets/screenshot.jpg"));
        assert!(!is_safe_archive_path("../SimpleImageViewer.exe"));
        assert!(!is_safe_archive_path("/tmp/SimpleImageViewer.exe"));
        assert!(!is_safe_archive_path("assets/../SimpleImageViewer.exe"));
    }

    #[test]
    fn github_release_candidate_selects_matching_asset() {
        let release = GithubRelease {
            tag_name: "v2.2.2".to_string(),
            html_url: "https://github.com/z16166/SimpleImageViewer/releases/tag/v2.2.2".to_string(),
            body: "Release notes".to_string(),
            published_at: "2026-05-29T00:00:00Z".to_string(),
            draft: false,
            prerelease: false,
            assets: vec![
                GithubAsset {
                    name: "SimpleImageViewer-x86_64-pc-windows-msvc.zip".to_string(),
                    browser_download_url: "https://example.invalid/win.zip".to_string(),
                    size: 123,
                },
                GithubAsset {
                    name: SHA256SUMS_ASSET.to_string(),
                    browser_download_url: "https://example.invalid/SHA256SUMS.txt".to_string(),
                    size: 64,
                },
            ],
        };

        let candidate = candidate_from_release(
            &release,
            "2.2.1",
            None,
            PlatformKind::Windows,
            CpuArch::X86_64,
            false,
            "en",
        )
        .expect("new release candidate");

        assert_eq!(candidate.version, "2.2.2");
        assert_eq!(
            candidate.asset_name,
            "SimpleImageViewer-x86_64-pc-windows-msvc.zip"
        );
        assert_eq!(
            candidate.checksum_url,
            Some("https://example.invalid/SHA256SUMS.txt".to_string())
        );
    }

    #[test]
    fn github_release_candidate_respects_ignored_version() {
        let release = GithubRelease {
            tag_name: "v2.2.2".to_string(),
            html_url: String::new(),
            body: String::new(),
            published_at: String::new(),
            draft: false,
            prerelease: false,
            assets: vec![GithubAsset {
                name: "SimpleImageViewer-x86_64-pc-windows-msvc.zip".to_string(),
                browser_download_url: "https://example.invalid/win.zip".to_string(),
                size: 123,
            }],
        };

        assert!(
            candidate_from_release(
                &release,
                "2.2.1",
                Some("v2.2.2"),
                PlatformKind::Windows,
                CpuArch::X86_64,
                false,
                "en",
            )
            .is_none()
        );
    }

    #[test]
    fn changelog_asset_names_prefer_locale_then_english() {
        assert_eq!(
            changelog_asset_names_for_locale("zh-CN"),
            vec![
                "CHANGELOG.zh-CN.md".to_string(),
                "CHANGELOG.en.md".to_string()
            ]
        );
        assert_eq!(
            changelog_asset_names_for_locale("zh-Hans"),
            vec![
                "CHANGELOG.zh-Hans.md".to_string(),
                "CHANGELOG.zh-CN.md".to_string(),
                "CHANGELOG.en.md".to_string()
            ]
        );
    }

    #[test]
    fn github_release_candidate_selects_localized_changelog_asset() {
        let release = GithubRelease {
            tag_name: "v2.2.2".to_string(),
            html_url: String::new(),
            body: "Release body fallback".to_string(),
            published_at: String::new(),
            draft: false,
            prerelease: false,
            assets: vec![
                GithubAsset {
                    name: "SimpleImageViewer-x86_64-pc-windows-msvc.zip".to_string(),
                    browser_download_url: "https://example.invalid/win.zip".to_string(),
                    size: 123,
                },
                GithubAsset {
                    name: "CHANGELOG.en.md".to_string(),
                    browser_download_url: "https://example.invalid/CHANGELOG.en.md".to_string(),
                    size: 456,
                },
                GithubAsset {
                    name: "CHANGELOG.zh-CN.md".to_string(),
                    browser_download_url: "https://example.invalid/CHANGELOG.zh-CN.md".to_string(),
                    size: 456,
                },
            ],
        };

        let candidate = candidate_from_release(
            &release,
            "2.2.1",
            None,
            PlatformKind::Windows,
            CpuArch::X86_64,
            false,
            "zh-CN",
        )
        .expect("new release candidate");

        assert_eq!(
            candidate.localized_changelog_url,
            Some("https://example.invalid/CHANGELOG.zh-CN.md".to_string())
        );
        assert_eq!(candidate.release_notes, "Release body fallback");
    }
}
