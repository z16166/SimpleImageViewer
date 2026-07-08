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

use crate::context_menu::model::{ContextMenuCommand, quote_arg};
use std::path::Path;

impl ContextMenuCommand {
    pub fn spawn_for_image(&self, image_path: &Path) -> Result<(), String> {
        #[cfg(target_os = "windows")]
        {
            return self.spawn_for_image_windows(image_path);
        }
        #[cfg(not(target_os = "windows"))]
        {
            return self.spawn_for_image_unix(image_path);
        }
    }

    #[cfg(target_os = "windows")]
    fn spawn_for_image_windows(&self, image_path: &Path) -> Result<(), String> {
        match self {
            Self::Executable { path } => {
                let exe = path.trim().trim_matches('"');
                if exe.is_empty() {
                    return Err("executable path is empty".to_string());
                }
                let parameters = format_shell_execute_parameters(&[path_to_string(image_path)]);
                shell_execute(exe, Some(&parameters))
            }
            Self::CommandLine { .. } => {
                let Some(line) = self.command_line_for_image(image_path) else {
                    return Err("invalid command template".to_string());
                };
                let argv = parse_windows_command_line(&line)?;
                let Some(program) = argv.first() else {
                    return Err("command line has no executable".to_string());
                };
                let parameters = if argv.len() > 1 {
                    Some(format_shell_execute_parameters(&argv[1..]))
                } else {
                    None
                };
                shell_execute(program, parameters.as_deref())
            }
        }
    }

    #[cfg(not(target_os = "windows"))]
    fn spawn_for_image_unix(&self, image_path: &Path) -> Result<(), String> {
        match self {
            Self::Executable { path: exe } => {
                let exe = exe.trim();
                if exe.is_empty() {
                    return Err("executable path is empty".to_string());
                }
                std::process::Command::new(exe)
                    .arg(image_path)
                    .spawn()
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            }
            Self::CommandLine { .. } => {
                let Some(line) = self.command_line_for_image(image_path) else {
                    return Err("invalid command template".to_string());
                };
                std::process::Command::new("sh")
                    .arg("-c")
                    .arg(line)
                    .spawn()
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            }
        }
    }
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn format_shell_execute_parameters(args: &[String]) -> String {
    args.iter()
        .map(|arg| quote_arg(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(target_os = "windows")]
fn shell_execute(file: &str, parameters: Option<&str>) -> Result<(), String> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;
    use winapi::um::shellapi::ShellExecuteW;
    use winapi::um::winuser::SW_SHOWNORMAL;

    let to_wide = |value: &str| -> Vec<u16> {
        OsStr::new(value)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    };

    let file_w = to_wide(file);
    let verb_w = to_wide("open");
    let params_w = parameters.map(|value| to_wide(value));

    let result = unsafe {
        ShellExecuteW(
            ptr::null_mut(),
            verb_w.as_ptr(),
            file_w.as_ptr(),
            params_w
                .as_ref()
                .map_or(ptr::null(), |value| value.as_ptr()),
            ptr::null(),
            SW_SHOWNORMAL,
        )
    };

    if (result as isize) <= 32 {
        Err(format!(
            "ShellExecuteW failed with code {}",
            result as isize
        ))
    } else {
        Ok(())
    }
}

#[cfg(target_os = "windows")]
fn parse_windows_command_line(line: &str) -> Result<Vec<String>, String> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use winapi::shared::minwindef::HLOCAL;
    use winapi::um::shellapi::CommandLineToArgvW;
    use winapi::um::winbase::LocalFree;

    struct LocalArgvGuard(HLOCAL);

    impl LocalArgvGuard {
        fn new(argv: HLOCAL) -> Result<Self, String> {
            if argv.is_null() {
                Err("CommandLineToArgvW failed".to_string())
            } else {
                Ok(Self(argv))
            }
        }

        fn as_argv_ptr(&self) -> *mut winapi::shared::ntdef::LPWSTR {
            self.0 as _
        }
    }

    impl Drop for LocalArgvGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    let _ = LocalFree(self.0);
                }
            }
        }
    }

    let wide: Vec<u16> = OsStr::new(line)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let mut argc = 0;
    let argv =
        LocalArgvGuard::new(unsafe { CommandLineToArgvW(wide.as_ptr(), &mut argc) as HLOCAL })?;

    let mut parsed = Vec::new();
    unsafe {
        let argv_ptr = argv.as_argv_ptr();
        for index in 0..argc as usize {
            let arg_ptr = *argv_ptr.add(index);
            let len = (0..)
                .take_while(|&offset| *arg_ptr.add(offset) != 0)
                .count();
            let slice = std::slice::from_raw_parts(arg_ptr, len);
            parsed.push(String::from_utf16_lossy(slice));
        }
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context_menu::model::CommandTemplate;

    #[test]
    fn format_shell_execute_parameters_quotes_each_argument() {
        let params = format_shell_execute_parameters(&[
            "D:/Work Images/photo 1.jpg".to_string(),
            "--flag".to_string(),
        ]);
        assert_eq!(params, "\"D:/Work Images/photo 1.jpg\" \"--flag\"");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn parse_windows_command_line_splits_mspaint_template() {
        let parsed = parse_windows_command_line("mspaint \"F:/HDR/ref.png\"")
            .expect("parse mspaint command line");
        assert_eq!(parsed, vec!["mspaint", "F:/HDR/ref.png"]);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn parse_windows_command_line_splits_quoted_executable() {
        let parsed = parse_windows_command_line(
            "\"C:/Program Files/App/App.exe\" \"D:/Work Images/photo 1.jpg\" --flag",
        )
        .expect("parse quoted executable command line");
        assert_eq!(
            parsed,
            vec![
                "C:/Program Files/App/App.exe",
                "D:/Work Images/photo 1.jpg",
                "--flag",
            ]
        );
    }

    #[test]
    fn command_line_template_builds_mspaint_arguments() {
        let image = Path::new("F:/HDR/conformance/testcases/cmyk_layers/ref.png");
        let command = ContextMenuCommand::CommandLine {
            template: CommandTemplate::new("mspaint %1".to_string()),
        };
        let line = command.command_line_for_image(image).expect("command line");
        assert_eq!(
            line,
            "mspaint \"F:/HDR/conformance/testcases/cmyk_layers/ref.png\""
        );

        #[cfg(target_os = "windows")]
        {
            let parsed = parse_windows_command_line(&line).expect("parse command line");
            assert_eq!(parsed[0], "mspaint");
            assert_eq!(
                parsed[1],
                "F:/HDR/conformance/testcases/cmyk_layers/ref.png"
            );
        }
    }
}
