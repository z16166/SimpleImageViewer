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
    tx: crossbeam_channel::Sender<UpdateInstallMessage>,
) {
    let _ = std::thread::Builder::new()
        .name("siv-update-install".to_string())
        .spawn(move || {
            let result = install_windows_update(&candidate, &settings, &tx);
            match result {
                Ok(()) => {
                    let _ = tx.send(UpdateInstallMessage::ReadyToRestart);
                }
                Err(err) => {
                    let _ = tx.send(UpdateInstallMessage::Failed(err));
                }
            }
        });
}

#[cfg(target_os = "windows")]
fn install_windows_update(
    candidate: &UpdateCandidate,
    settings: &UpdateSettings,
    tx: &crossbeam_channel::Sender<UpdateInstallMessage>,
) -> Result<(), String> {
    let proxy = settings.proxy.to_proxy_config();
    let proxy = proxy.enabled.then_some(proxy);
    let checksum_url = candidate
        .checksum_url
        .as_deref()
        .ok_or_else(|| "release is missing SHA256SUMS.txt".to_string())?;

    let _ = tx.send(UpdateInstallMessage::Downloading(0));
    let archive = crate::update::net::download_bytes(&candidate.asset_url, proxy.as_ref())?;
    let _ = tx.send(UpdateInstallMessage::Downloading(50));
    let sums = crate::update::net::download_bytes(checksum_url, proxy.as_ref())?;
    let sums = String::from_utf8(sums).map_err(|err| err.to_string())?;
    let expected_hash = checksum_for_asset(&sums, &candidate.asset_name)
        .ok_or_else(|| format!("SHA256SUMS.txt does not list {}", candidate.asset_name))?;
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
        .ok_or_else(|| "current executable has no parent directory".to_string())?;
    ensure_directory_writable(old_dir)?;
    let new_exe = staging.join(MAIN_EXE_NAME);
    if !new_exe.is_file() {
        return Err(format!("update archive is missing {MAIN_EXE_NAME}"));
    }
    let helper = old_dir.join(UPDATER_EXE_NAME);
    if !helper.is_file() {
        return Err(format!(
            "{} was not found next to the app",
            helper.display()
        ));
    }
    let backup = old_exe.with_extension(format!("exe.old.{}", candidate.version));
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
    )?;
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn install_windows_update(
    _candidate: &UpdateCandidate,
    _settings: &UpdateSettings,
    _tx: &crossbeam_channel::Sender<UpdateInstallMessage>,
) -> Result<(), String> {
    Err("automatic installation is only available on Windows".to_string())
}

fn staging_dir(version: &str) -> Result<PathBuf, String> {
    Ok(std::env::temp_dir()
        .join("SimpleImageViewer-update")
        .join(version))
}

fn verify_sha256(bytes: &[u8], expected_hex: &str) -> Result<(), String> {
    let actual = to_hex_lower(&Sha256::digest(bytes));
    if actual.eq_ignore_ascii_case(expected_hex) {
        Ok(())
    } else {
        Err("downloaded update checksum did not match SHA256SUMS.txt".to_string())
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
            return Err(format!("unsafe path in update archive: {name}"));
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
        format!(
            "the application directory is not writable ({}): {}",
            dir.display(),
            err
        )
    })?;
    let _ = std::fs::remove_file(probe);
    Ok(())
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
        let err = verify_sha256(b"abc", "0000").unwrap_err();

        assert!(err.contains("checksum"));
    }

    #[test]
    fn staging_dir_uses_version_component() {
        let path = staging_dir("2.2.2").expect("staging dir");

        assert!(path.ends_with(Path::new("SimpleImageViewer-update").join("2.2.2")));
    }
}
