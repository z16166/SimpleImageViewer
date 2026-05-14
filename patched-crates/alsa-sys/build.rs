extern crate pkg_config;

use std::path::{Path, PathBuf};

#[cfg(feature = "use-bindgen")]
extern crate bindgen;

/// `pkg-config` 0.3: with `statik(true)`, `-lasound` is still emitted as **dynamic** when the
/// resolved `-L` is under `/usr` (`is_static_available` refuses “system” dirs). CI prepends vcpkg to
/// `PKG_CONFIG_PATH`; local cargo often does not, so we prepend `vcpkg_installed/<triplet>/lib/pkgconfig`
/// here when present so `alsa.pc` from vcpkg wins and static `libasound.a` is linked.
///
/// Uses `CARGO_CFG_TARGET_*` (the artifact target), not `cfg(target_os)` in this crate: build.rs is
/// built for the **host**, so `#[cfg(target_os = "linux")]` would break cross-compiles from Windows.
fn prepend_workspace_vcpkg_pkgconfig_path_for_linux_target() {
    if std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() != "linux" {
        return;
    }

    println!("cargo:rerun-if-env-changed=VCPKG_INSTALLED_DIR");
    println!("cargo:rerun-if-env-changed=VCPKG_DEFAULT_TRIPLET");
    println!("cargo:rerun-if-env-changed=VCPKGRS_TRIPLET");

    let Some(triplet) = linux_vcpkg_triplet() else {
        return;
    };
    let installed = vcpkg_installed_dir();
    let pc_dir = installed.join(&triplet).join("lib").join("pkgconfig");
    if !pc_dir.is_dir() {
        return;
    }

    let pc_path = pc_dir.display().to_string();
    let new_path = match std::env::var("PKG_CONFIG_PATH") {
        Ok(existing) if !existing.is_empty() => format!("{pc_path}:{existing}"),
        _ => pc_path,
    };
    std::env::set_var("PKG_CONFIG_PATH", new_path);
}

fn vcpkg_installed_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("VCPKG_INSTALLED_DIR") {
        return PathBuf::from(dir);
    }
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    manifest_dir
        .parent()
        .and_then(Path::parent)
        .map(|p| p.join("vcpkg_installed"))
        .unwrap_or_else(|| manifest_dir.join("vcpkg_installed"))
}

fn linux_vcpkg_triplet() -> Option<String> {
    if let Ok(t) = std::env::var("VCPKG_DEFAULT_TRIPLET") {
        return Some(t);
    }
    if let Ok(t) = std::env::var("VCPKGRS_TRIPLET") {
        return Some(t);
    }
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").ok()?;
    Some(match arch.as_str() {
        "x86_64" => "x64-linux".into(),
        "aarch64" => "arm64-linux-v8a".into(),
        _ => return None,
    })
}

fn main() {
    prepend_workspace_vcpkg_pkgconfig_path_for_linux_target();

    // Static link so the final binary uses vcpkg's libasound.a (glibc 2.28-compatible)
    // instead of the distro libasound.so (GLIBC_2.33+ / 2.34+ symbols on Ubuntu runners).
    match pkg_config::Config::new().statik(true).probe("alsa") {
        Err(pkg_config::Error::Failure { command, output }) => panic!(
            "Pkg-config failed - usually this is because alsa development headers are not installed.\n\n\
            For Fedora users:\n# dnf install alsa-lib-devel\n\n\
            For Debian/Ubuntu users:\n# apt-get install libasound2-dev\n\n\
            pkg_config details:\n{}\n", pkg_config::Error::Failure { command, output }),
        Err(e) => panic!("{}", e),
        Ok(_alsa_library) => {
            #[cfg(feature = "use-bindgen")]
            generate_bindings(&_alsa_library);
        } 
    };
}

#[cfg(feature = "use-bindgen")]
fn generate_bindings(alsa_library: &pkg_config::Library) {
    use std::env;
    use std::path::PathBuf;

    let clang_include_args = alsa_library.include_paths.iter().map(|include_path| {
        format!(
            "-I{}",
            include_path.to_str().expect("include path was not UTF-8")
        )
    });

    let mut codegen_config = bindgen::CodegenConfig::empty();
    codegen_config.insert(bindgen::CodegenConfig::FUNCTIONS);
    codegen_config.insert(bindgen::CodegenConfig::TYPES);

    let builder = bindgen::Builder::default()
        .use_core()
        .size_t_is_usize(true)
        .allowlist_recursively(false)
        .prepend_enum_name(false)
        .layout_tests(false)
        .allowlist_function("snd_.*")
        .allowlist_type("_?snd_.*")
        .allowlist_type(".*va_list.*")
        .with_codegen_config(codegen_config)
        .clang_args(clang_include_args)
        .header("wrapper.h")
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()));
    let bindings = builder.generate().expect("Unable to generate bindings");

    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());

    bindings
        .write_to_file(out_path.join("generated.rs"))
        .expect("Couldn't write bindings");
}
