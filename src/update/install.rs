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

use super::core::{UpdateCandidate, checksum_for_asset, is_safe_archive_path};
use crate::settings::UpdateSettings;
use crate::update::net::{MAX_SHA256SUMS_DOWNLOAD_BYTES, MAX_UPDATE_DOWNLOAD_BYTES};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub const MAIN_EXE_NAME: &str = "SimpleImageViewer.exe";
pub const UPDATER_EXE_NAME: &str = "update.exe";

#[derive(Clone, Debug)]
pub enum UpdateInstallMessage {
    Downloading(u8),
    ReadyToRestart,
    Failed(String),
}

pub fn spawn_windows_update_install(
    candidate: UpdateCandidate,
    settings: UpdateSettings,
    locale: String,
    tx: crossbeam_channel::Sender<UpdateInstallMessage>,
) {
    let tx_for_spawn_error = tx.clone();
    if let Err(err) = std::thread::Builder::new()
        .name("siv-update-install".to_string())
        .spawn(move || {
            let result = install_windows_update(&candidate, &settings, &locale, &tx);
            match result {
                Ok(()) => {
                    let _ = tx.send(UpdateInstallMessage::ReadyToRestart);
                }
                Err(err) => {
                    let _ = tx.send(UpdateInstallMessage::Failed(err));
                }
            }
        })
    {
        let _ = tx_for_spawn_error.send(UpdateInstallMessage::Failed(format!(
            "failed to start update install thread: {err}"
        )));
    }
}

#[cfg(target_os = "windows")]
fn install_windows_update(
    candidate: &UpdateCandidate,
    settings: &UpdateSettings,
    locale: &str,
    tx: &crossbeam_channel::Sender<UpdateInstallMessage>,
) -> Result<(), String> {
    let proxy = settings.proxy.to_proxy_config();
    let proxy = proxy.enabled.then_some(proxy);
    let checksum_url = candidate
        .checksum_url
        .as_deref()
        .ok_or_else(|| rust_i18n::t!("update.err_missing_sums").to_string())?;

    let _ = tx.send(UpdateInstallMessage::Downloading(0));
    let progress_tx = tx.clone();
    let archive = crate::update::net::download_bytes_with_progress(
        &candidate.asset_url,
        proxy.as_ref(),
        MAX_UPDATE_DOWNLOAD_BYTES,
        move |received, total| {
            if let Some(total) = total.filter(|total| *total > 0) {
                let percent = ((received.saturating_mul(70) / total).min(70)) as u8;
                let _ = progress_tx.send(UpdateInstallMessage::Downloading(percent));
            }
        },
    )?;
    let _ = tx.send(UpdateInstallMessage::Downloading(70));
    let sums = crate::update::net::download_bytes_with_progress(
        checksum_url,
        proxy.as_ref(),
        MAX_SHA256SUMS_DOWNLOAD_BYTES,
        |_, _| {},
    )?;
    let sums = String::from_utf8(sums).map_err(|err| err.to_string())?;
    let expected_hash = checksum_for_asset(&sums, &candidate.asset_name).ok_or_else(|| {
        rust_i18n::t!(
            "update.err_checksum_missing",
            asset = candidate.asset_name.clone()
        )
        .to_string()
    })?;
    verify_sha256(&archive, &expected_hash)?;

    let staging = staging_dir(&candidate.version)?;
    if staging.exists() {
        std::fs::remove_dir_all(&staging).map_err(|err| err.to_string())?;
    }
    std::fs::create_dir_all(&staging).map_err(|err| err.to_string())?;
    extract_zip_safely(&archive, &staging)?;
    let _ = tx.send(UpdateInstallMessage::Downloading(80));

    let old_exe = std::env::current_exe().map_err(|err| err.to_string())?;
    let old_dir = old_exe
        .parent()
        .ok_or_else(|| rust_i18n::t!("update.err_current_exe_parent").to_string())?;
    ensure_directory_writable(old_dir)?;
    let new_exe = staging.join(MAIN_EXE_NAME);
    if !new_exe.is_file() {
        return Err(rust_i18n::t!("update.err_missing_exe", exe = MAIN_EXE_NAME).to_string());
    }
    verify_staged_exe_version(&new_exe, &candidate.version)?;
    let helper = old_dir.join(UPDATER_EXE_NAME);
    if !helper.is_file() {
        return Err(rust_i18n::t!(
            "update.err_missing_helper",
            path = helper.display().to_string()
        )
        .to_string());
    }
    let backup = old_exe.with_extension(format!(
        "exe.old.{}.{}",
        candidate.version,
        unix_timestamp_secs()
    ));
    let log_path = staging.join("update.log");
    let success_marker = old_dir.join(".siv_update_success");
    launch_helper(
        &helper,
        &old_exe,
        &new_exe,
        &backup,
        &log_path,
        &success_marker,
        &candidate.version,
        locale,
    )?;
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn install_windows_update(
    _candidate: &UpdateCandidate,
    _settings: &UpdateSettings,
    _locale: &str,
    _tx: &crossbeam_channel::Sender<UpdateInstallMessage>,
) -> Result<(), String> {
    Err(rust_i18n::t!("update.err_win_only").to_string())
}

fn staging_dir(version: &str) -> Result<PathBuf, String> {
    Ok(std::env::temp_dir()
        .join("SimpleImageViewer-update")
        .join(version))
}

fn verify_sha256(bytes: &[u8], expected_hex: &str) -> Result<(), String> {
    let actual = to_hex_lower(&Sha256::digest(bytes));
    if actual == expected_hex {
        Ok(())
    } else {
        Err(rust_i18n::t!("update.err_checksum_mismatch").to_string())
    }
}

fn to_hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn extract_zip_safely(bytes: &[u8], dest: &Path) -> Result<(), String> {
    let cursor = std::io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor).map_err(|err| err.to_string())?;
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).map_err(|err| err.to_string())?;
        let name = file.name().replace('\\', "/");
        if !is_safe_archive_path(&name) {
            return Err(rust_i18n::t!("update.err_unsafe_archive_path", path = name).to_string());
        }
        let out = dest.join(&name);
        if file.is_dir() {
            std::fs::create_dir_all(&out).map_err(|err| err.to_string())?;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
        }
        let mut output = std::fs::File::create(&out).map_err(|err| err.to_string())?;
        std::io::copy(&mut file, &mut output).map_err(|err| err.to_string())?;
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn ensure_directory_writable(dir: &Path) -> Result<(), String> {
    let probe = dir.join(".siv-update-write-test");
    std::fs::write(&probe, b"test").map_err(|err| {
        rust_i18n::t!(
            "update.err_not_writable",
            path = dir.display().to_string(),
            err = err.to_string()
        )
        .to_string()
    })?;
    if let Err(err) = std::fs::remove_file(&probe) {
        log::warn!(
            "[update] failed to remove write-test probe {}: {}",
            probe.display(),
            err
        );
    }
    Ok(())
}

fn unix_timestamp_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(target_os = "windows")]
fn verify_staged_exe_version(exe: &Path, expected_version: &str) -> Result<(), String> {
    let staged = read_windows_file_version(exe)?;
    let staged = crate::update::core::normalize_version(&staged).ok_or_else(|| {
        rust_i18n::t!("update.err_bad_staged_version", version = staged.clone()).to_string()
    })?;
    let expected = crate::update::core::normalize_version(expected_version).ok_or_else(|| {
        rust_i18n::t!(
            "update.err_bad_release_version",
            version = expected_version.to_string()
        )
        .to_string()
    })?;
    if staged == expected {
        Ok(())
    } else {
        Err(rust_i18n::t!(
            "update.err_version_mismatch",
            staged = staged.to_string(),
            expected = expected.to_string()
        )
        .to_string())
    }
}

#[cfg(target_os = "windows")]
fn read_windows_file_version(exe: &Path) -> Result<String, String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::{
        GetFileVersionInfoSizeW, GetFileVersionInfoW, VS_FIXEDFILEINFO, VerQueryValueW,
    };
    use windows::core::PCWSTR;

    let wide: Vec<u16> = exe.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY:
    // - `wide` and `root` are NUL-terminated UTF-16 buffers and stay alive for each Win32 call.
    // - `data` owns the version-info bytes returned by `GetFileVersionInfoW`; `VerQueryValueW`
    //   returns `ptr` into that buffer, so `data` must remain alive until `info` is copied/read.
    // - For the root `\` query, `len` is the number of `VS_FIXEDFILEINFO` elements returned by
    //   the API shape used by Windows; checking `ptr` non-null and `len > 0` is sufficient here.
    unsafe {
        let size = GetFileVersionInfoSizeW(PCWSTR(wide.as_ptr()), None);
        if size == 0 {
            return Err(rust_i18n::t!(
                "update.err_version_read_failed",
                path = exe.display().to_string()
            )
            .to_string());
        }
        let mut data = vec![0u8; size as usize];
        GetFileVersionInfoW(
            PCWSTR(wide.as_ptr()),
            0,
            size,
            data.as_mut_ptr() as *mut std::ffi::c_void,
        )
        .map_err(|err| err.to_string())?;
        let root: Vec<u16> = "\\".encode_utf16().chain(Some(0)).collect();
        let mut ptr = std::ptr::null_mut();
        let mut len = 0u32;
        if !VerQueryValueW(
            data.as_ptr() as *const std::ffi::c_void,
            PCWSTR(root.as_ptr()),
            &mut ptr,
            &mut len,
        )
        .as_bool()
        {
            return Err(rust_i18n::t!(
                "update.err_version_read_failed",
                path = exe.display().to_string()
            )
            .to_string());
        }
        if ptr.is_null() || len == 0 {
            return Err(rust_i18n::t!(
                "update.err_version_read_failed",
                path = exe.display().to_string()
            )
            .to_string());
        }
        let info = &*(ptr as *const VS_FIXEDFILEINFO);
        let major = (info.dwFileVersionMS >> 16) & 0xffff;
        let minor = info.dwFileVersionMS & 0xffff;
        let patch = (info.dwFileVersionLS >> 16) & 0xffff;
        let build = info.dwFileVersionLS & 0xffff;
        if build == 0 {
            Ok(format!("{major}.{minor}.{patch}"))
        } else {
            Ok(format!("{major}.{minor}.{patch}.{build}"))
        }
    }
}

#[cfg(target_os = "windows")]
fn launch_helper(
    helper: &Path,
    old_exe: &Path,
    new_exe: &Path,
    backup: &Path,
    log_path: &Path,
    success_marker: &Path,
    version: &str,
    locale: &str,
) -> Result<(), String> {
    let pid = std::process::id().to_string();
    std::process::Command::new(helper)
        .arg("--pid")
        .arg(pid)
        .arg("--old-exe")
        .arg(old_exe)
        .arg("--new-exe")
        .arg(new_exe)
        .arg("--backup-exe")
        .arg(backup)
        .arg("--log")
        .arg(log_path)
        .arg("--success-marker")
        .arg(success_marker)
        .arg("--version")
        .arg(version)
        .arg("--locale")
        .arg(locale)
        .arg("--restart")
        .spawn()
        .map_err(|err| err.to_string())?;
    Ok(())
}

pub fn consume_success_marker() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let marker = exe.parent()?.join(".siv_update_success");
    let version = std::fs::read_to_string(&marker).ok()?.trim().to_string();
    let _ = std::fs::remove_file(marker);
    (!version.is_empty()).then_some(version)
}

pub fn cleanup_old_backups() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let Some(dir) = exe.parent() else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.starts_with("SimpleImageViewer.exe.old.") {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_sha256_accepts_matching_digest() {
        let bytes = b"abc";
        let digest = to_hex_lower(&Sha256::digest(bytes));

        assert!(verify_sha256(bytes, &digest).is_ok());
    }

    #[test]
    fn verify_sha256_rejects_mismatched_digest() {
        assert!(verify_sha256(b"abc", "0000").is_err());
    }

    #[test]
    fn staging_dir_uses_version_component() {
        let path = staging_dir("2.2.2").expect("staging dir");

        assert!(path.ends_with(Path::new("SimpleImageViewer-update").join("2.2.2")));
    }
}
