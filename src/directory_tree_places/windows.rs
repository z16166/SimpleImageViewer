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

//! Windows Places: SHGetKnownFolderPath + FOLDERID_ComputerFolder drive enumeration.

use std::path::PathBuf;

use windows::Win32::Foundation::S_FALSE;
use windows::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize,
};
use windows::Win32::System::SystemServices::{SFGAO_FILESYSTEM, SFGAO_FLAGS, SFGAO_FOLDER};
use windows::Win32::UI::Shell::{
    BHID_EnumItems, FOLDERID_ComputerFolder, FOLDERID_Desktop, FOLDERID_Documents,
    FOLDERID_Downloads, FOLDERID_Music, FOLDERID_Pictures, FOLDERID_Profile, FOLDERID_Videos,
    IEnumShellItems, IShellItem, KF_FLAG_DEFAULT, SHGetKnownFolderItem, SHGetKnownFolderPath,
    SIGDN_FILESYSPATH, SIGDN_NORMALDISPLAY,
};
use windows::core::{GUID, HRESULT, PWSTR};

use super::fs::path_is_accessible_directory;
use super::types::{
    DirectoryTreePlaces, DriveEntry, KnownFolderEntry, KnownFolderKind, known_folder_tree_path,
};

const RPC_E_CHANGED_MODE: HRESULT = HRESULT(0x8001_0106_u32 as i32);

struct ComSession {
    should_uninitialize: bool,
    shell_usable: bool,
}

impl ComSession {
    fn new() -> Self {
        unsafe {
            let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            if hr.is_ok() {
                return Self {
                    should_uninitialize: hr != S_FALSE,
                    shell_usable: true,
                };
            }
            if hr == RPC_E_CHANGED_MODE {
                return Self {
                    should_uninitialize: false,
                    shell_usable: true,
                };
            }
            log::warn!(
                "[DirectoryTreePlaces] CoInitializeEx failed with unexpected HRESULT: {hr:?}"
            );
        }
        Self {
            should_uninitialize: false,
            shell_usable: false,
        }
    }
}

impl Drop for ComSession {
    fn drop(&mut self) {
        if self.should_uninitialize {
            unsafe {
                CoUninitialize();
            }
        }
    }
}

pub(super) fn load() -> DirectoryTreePlaces {
    let com = ComSession::new();
    if !com.shell_usable {
        log::error!("[DirectoryTreePlaces] COM unavailable; returning empty places");
        return DirectoryTreePlaces {
            known_folders: Vec::new(),
            drives: Vec::new(),
            network_locations: Vec::new(),
            this_pc_label: rust_i18n::t!("directory_tree.this_pc").to_string(),
            network_label: rust_i18n::t!("directory_tree.network").to_string(),
        };
    }
    let _com = com;
    DirectoryTreePlaces {
        known_folders: load_known_folders(),
        drives: enumerate_filesystem_drives(),
        // Network shell enumeration can block for many seconds on startup; UNC paths
        // mount lazily via `ensure_network_visible` when needed.
        network_locations: Vec::new(),
        this_pc_label: rust_i18n::t!("directory_tree.this_pc").to_string(),
        network_label: rust_i18n::t!("directory_tree.network").to_string(),
    }
}

fn load_known_folders() -> Vec<KnownFolderEntry> {
    const SPECS: [(KnownFolderKind, GUID, &str); 7] = [
        (
            KnownFolderKind::Desktop,
            FOLDERID_Desktop,
            "directory_tree.place_desktop",
        ),
        (
            KnownFolderKind::Documents,
            FOLDERID_Documents,
            "directory_tree.place_documents",
        ),
        (
            KnownFolderKind::Pictures,
            FOLDERID_Pictures,
            "directory_tree.place_pictures",
        ),
        (
            KnownFolderKind::Downloads,
            FOLDERID_Downloads,
            "directory_tree.place_downloads",
        ),
        (
            KnownFolderKind::Music,
            FOLDERID_Music,
            "directory_tree.place_music",
        ),
        (
            KnownFolderKind::Videos,
            FOLDERID_Videos,
            "directory_tree.place_videos",
        ),
        (
            KnownFolderKind::Profile,
            FOLDERID_Profile,
            "directory_tree.place_profile",
        ),
    ];

    SPECS
        .into_iter()
        .filter_map(|(kind, folder_id, i18n_key)| {
            let filesystem_path = known_folder_path(&folder_id)?;
            if !path_is_accessible_directory(&filesystem_path) {
                return None;
            }
            Some(KnownFolderEntry {
                kind,
                display_name: rust_i18n::t!(i18n_key).to_string(),
                tree_path: known_folder_tree_path(kind),
                filesystem_path,
            })
        })
        .collect()
}

fn known_folder_path(folder_id: &GUID) -> Option<PathBuf> {
    unsafe {
        let pw = SHGetKnownFolderPath(folder_id, KF_FLAG_DEFAULT, None).ok()?;
        let path = pw.to_string().ok().map(PathBuf::from);
        CoTaskMemFree(Some(pw.0.cast()));
        path
    }
}

fn enumerate_filesystem_drives() -> Vec<DriveEntry> {
    enumerate_shell_filesystem_children(&FOLDERID_ComputerFolder)
}

fn enumerate_shell_filesystem_children(folder_id: &GUID) -> Vec<DriveEntry> {
    unsafe {
        let root: IShellItem = match SHGetKnownFolderItem(folder_id, KF_FLAG_DEFAULT, None) {
            Ok(item) => item,
            Err(_) => return Vec::new(),
        };

        let enum_items: IEnumShellItems = match root.BindToHandler(None, &BHID_EnumItems) {
            Ok(items) => items,
            Err(_) => return Vec::new(),
        };

        let mut entries = Vec::new();
        let mut batch = [None];

        loop {
            if enum_items.Next(&mut batch, None).is_err() {
                break;
            }
            let Some(item) = batch[0].take() else {
                break;
            };

            let mask = SFGAO_FOLDER | SFGAO_FILESYSTEM;
            let attrs = match item.GetAttributes(mask) {
                Ok(value) => value,
                Err(_) => continue,
            };
            if !has_shell_flag(attrs, SFGAO_FOLDER) || !has_shell_flag(attrs, SFGAO_FILESYSTEM) {
                continue;
            }

            let display_name = match item.GetDisplayName(SIGDN_NORMALDISPLAY) {
                Ok(pw) => take_pwstr(pw),
                Err(_) => continue,
            };
            let path = match item.GetDisplayName(SIGDN_FILESYSPATH) {
                Ok(pw) => take_pwstr(pw).map(PathBuf::from),
                Err(_) => continue,
            };
            let (Some(display_name), Some(path)) = (display_name, path) else {
                continue;
            };
            if !path_is_accessible_directory(&path) {
                continue;
            }

            entries.push(DriveEntry { display_name, path });
        }

        entries
    }
}

fn has_shell_flag(attrs: SFGAO_FLAGS, flag: SFGAO_FLAGS) -> bool {
    (attrs & flag) == flag
}

fn take_pwstr(pw: PWSTR) -> Option<String> {
    unsafe {
        if pw.0.is_null() {
            return None;
        }
        let text = pw.to_string().ok()?;
        CoTaskMemFree(Some(pw.0.cast()));
        Some(text)
    }
}
