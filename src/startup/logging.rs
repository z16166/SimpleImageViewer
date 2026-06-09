use parking_lot::Mutex;
use std::time::Instant;

use super::phases::{startup_capture_phase, StartupPhases};

const LOG_LEVEL_ENV: &str = "SIV_LOG_LEVEL";
const LOG_FILE_ENV: &str = "SIV_LOG_FILE";

static LOGGER_HANDLE: Mutex<Option<flexi_logger::LoggerHandle>> = Mutex::new(None);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggingConfig {
    pub level: String,
    pub enable_file: bool,
}

pub(crate) fn parse_env_bool(value: Option<&str>) -> bool {
    matches!(
        value.map(str::trim).map(str::to_ascii_lowercase).as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

pub(crate) fn logging_config_from_env(log_level: Option<&str>, log_file: Option<&str>) -> LoggingConfig {
    LoggingConfig {
        level: log_level
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("off")
            .to_string(),
        enable_file: parse_env_bool(log_file),
    }
}

fn logging_config() -> LoggingConfig {
    let level = std::env::var(LOG_LEVEL_ENV).ok();
    let file = std::env::var(LOG_FILE_ENV).ok();
    logging_config_from_env(level.as_deref(), file.as_deref())
}

pub fn init_logging() -> StartupPhases {
    #[cfg(feature = "startup-timing")]
    let t0 = Instant::now();
    #[cfg(feature = "startup-timing")]
    let mut prev = t0;
    #[cfg(feature = "startup-timing")]
    let mut phases = Vec::new();

    let log_dir = std::env::temp_dir();
    #[cfg(feature = "startup-timing")]
    startup_capture_phase(&mut phases, &mut prev, t0, "init_logging: temp_dir");

    let logging = logging_config();
    let logger = flexi_logger::Logger::try_with_env_or_str(&logging.level)
        .expect("Failed to initialize logger");
    #[cfg(feature = "startup-timing")]
    startup_capture_phase(
        &mut phases,
        &mut prev,
        t0,
        "init_logging: try_with_env_or_str",
    );

    let logger = if logging.enable_file {
        logger.log_to_file(
            flexi_logger::FileSpec::default()
                .directory(log_dir)
                .basename("simple_image_viewer"),
        )
    } else {
        logger
    };
    #[cfg(feature = "startup-timing")]
    startup_capture_phase(&mut phases, &mut prev, t0, "init_logging: log_to_file");

    #[cfg(windows)]
    let logger = logger.use_windows_line_ending();
    #[cfg(feature = "startup-timing")]
    startup_capture_phase(
        &mut phases,
        &mut prev,
        t0,
        "init_logging: windows_line_ending",
    );

    let logger = logger.write_mode(flexi_logger::WriteMode::Async).rotate(
        flexi_logger::Criterion::Size(crate::constants::LOG_FILE_SIZE_LIMIT),
        flexi_logger::Naming::Numbers,
        flexi_logger::Cleanup::KeepLogFiles(3),
    );
    #[cfg(feature = "startup-timing")]
    startup_capture_phase(
        &mut phases,
        &mut prev,
        t0,
        "init_logging: write_mode + rotate",
    );

    #[cfg(debug_assertions)]
    let logger = logger.duplicate_to_stderr(flexi_logger::Duplicate::All);
    #[cfg(feature = "startup-timing")]
    startup_capture_phase(
        &mut phases,
        &mut prev,
        t0,
        "init_logging: duplicate_to_stderr",
    );

    let handle = logger.start().expect("Failed to start logger");
    LOGGER_HANDLE.lock().replace(handle);
    #[cfg(feature = "startup-timing")]
    startup_capture_phase(&mut phases, &mut prev, t0, "init_logging: start");

    #[cfg(not(feature = "startup-timing"))]
    let phases = ();
    phases
}

pub(crate) fn shutdown_logger() {
    if let Some(handle) = LOGGER_HANDLE.lock().take() {
        handle.shutdown();
    }
}

pub fn log_env_info() -> String {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();

    let total_memory = sys.total_memory();
    let memory_gb = total_memory as f64 / 1024.0 / 1024.0 / 1024.0;

    #[cfg(windows)]
    let env_desc = {
        use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
        use windows::core::PCSTR;

        #[repr(C)]
        #[allow(non_snake_case)]
        struct OSVERSIONINFOEXW {
            dwOSVersionInfoSize: u32,
            dwMajorVersion: u32,
            dwMinorVersion: u32,
            dwBuildNumber: u32,
            dwPlatformId: u32,
            szCSDVersion: [u16; 128],
            wServicePackMajor: u16,
            wServicePackMinor: u16,
            wSuiteMask: u16,
            wProductType: u8,
            wReserved: u8,
        }

        unsafe fn get_win_env(memory_gb: f64) -> Option<String> {
            let h_ntdll = unsafe { GetModuleHandleW(windows::core::w!("ntdll.dll")).ok()? };
            let proc = unsafe { GetProcAddress(h_ntdll, PCSTR(b"RtlGetVersion\0".as_ptr()))? };
            let rtl_get_version: extern "system" fn(*mut OSVERSIONINFOEXW) -> i32 =
                unsafe { std::mem::transmute(proc) };

            let mut osi: OSVERSIONINFOEXW = unsafe { std::mem::zeroed() };
            osi.dwOSVersionInfoSize = std::mem::size_of::<OSVERSIONINFOEXW>() as u32;

            if rtl_get_version(&mut osi) == 0 {
                let major = osi.dwMajorVersion;
                let minor = osi.dwMinorVersion;
                let build = osi.dwBuildNumber;
                let is_server = osi.wProductType != 1;

                let service_pack = String::from_utf16_lossy(&osi.szCSDVersion);
                let service_pack = service_pack.trim_matches('\0').trim().to_string();

                use winreg::RegKey;
                use winreg::enums::HKEY_LOCAL_MACHINE;
                let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);

                let marketing_name = match (major, minor) {
                    (10, 0) => {
                        if is_server {
                            if build >= 26100 {
                                "Server 2025"
                            } else if build >= 20348 {
                                "Server 2022"
                            } else if build >= 17763 {
                                "Server 2019"
                            } else if build >= 14393 {
                                "Server 2016"
                            } else {
                                "Server"
                            }
                        } else {
                            if build >= 22000 { "11" } else { "10" }
                        }
                    }
                    (6, 3) => {
                        if is_server {
                            "Server 2012 R2"
                        } else {
                            "8.1"
                        }
                    }
                    (6, 2) => {
                        if is_server {
                            "Server 2012"
                        } else {
                            "8"
                        }
                    }
                    (6, 1) => {
                        if is_server {
                            "Server 2008 R2"
                        } else {
                            "7"
                        }
                    }
                    (6, 0) => {
                        if is_server {
                            "Server 2008"
                        } else {
                            "Vista"
                        }
                    }
                    (5, 2) => {
                        if is_server {
                            "Server 2003"
                        } else {
                            "XP"
                        }
                    }
                    (5, 1) => "XP",
                    _ => "Unknown",
                };

                let mut display_name = format!("Windows {}", marketing_name);
                let mut display_version = String::new();
                let mut edition_id = String::new();
                let mut ubr: u32 = 0;

                if let Ok(key) = hklm.open_subkey(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion") {
                    display_version = key
                        .get_value("DisplayVersion")
                        .or_else(|_| key.get_value("ReleaseId"))
                        .unwrap_or_default();
                    edition_id = key.get_value("EditionID").unwrap_or_default();
                    ubr = key.get_value("UBR").unwrap_or(0);
                }

                if !edition_id.is_empty() {
                    display_name.push_str(" ");
                    display_name.push_str(&edition_id);
                }
                if !display_version.is_empty() {
                    display_name.push_str(" ");
                    display_name.push_str(&display_version);
                }
                if !service_pack.is_empty() {
                    display_name.push_str(" ");
                    display_name.push_str(&service_pack);
                }

                let full_version = if ubr > 0 {
                    format!("{}.{}.{}.{}", major, minor, build, ubr)
                } else {
                    format!("{}.{}.{}", major, minor, build)
                };

                return Some(format!(
                    "{} [{}] (RAM: {:.2} GB)",
                    display_name, full_version, memory_gb
                ));
            }
            None
        }

        unsafe { get_win_env(memory_gb) }
    };

    #[cfg(not(windows))]
    let env_desc: Option<String> = None;

    let final_desc = env_desc.unwrap_or_else(|| {
        let os_name = sysinfo::System::name().unwrap_or_else(|| "Unknown".to_string());
        let os_version = sysinfo::System::os_version().unwrap_or_else(|| "Unknown".to_string());
        format!("{} [{}] (RAM: {:.2} GB)", os_name, os_version, memory_gb)
    });

    // Always emit when logging is enabled (SIV_LOG_LEVEL / SIV_LOG_FILE), even without
    // the local-only startup-timing feature, so support can collect OS/RAM context.
    log::info!(
        "Simple Image Viewer v{} | Environment: {}",
        env!("CARGO_PKG_VERSION"),
        final_desc
    );

    #[cfg(feature = "legacy_win7")]
    log::info!("Build Type: Windows 7 Legacy Compatibility Edition (x64)");

    final_desc
}

#[cfg(test)]
mod tests {
    use super::{logging_config_from_env, parse_env_bool};

    #[test]
    fn logging_env_defaults_to_no_file_and_off_level() {
        let config = logging_config_from_env(None, None);

        assert_eq!(config.level, "off");
        assert!(!config.enable_file);
    }

    #[test]
    fn logging_env_uses_explicit_level_and_file_flag() {
        let config = logging_config_from_env(Some("debug"), Some("1"));

        assert_eq!(config.level, "debug");
        assert!(config.enable_file);
    }

    #[test]
    fn logging_env_treats_blank_level_as_off() {
        let config = logging_config_from_env(Some("   "), Some("false"));

        assert_eq!(config.level, "off");
        assert!(!config.enable_file);
    }

    #[test]
    fn log_file_env_accepts_common_true_values_only() {
        for value in ["1", "true", "TRUE", "yes", "on"] {
            assert!(parse_env_bool(Some(value)), "{value}");
        }
        for value in [
            None,
            Some("0"),
            Some("false"),
            Some("no"),
            Some("off"),
            Some(""),
        ] {
            assert!(!parse_env_bool(value), "{value:?}");
        }
    }
}
