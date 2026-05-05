fn main() {
    unsafe {
        std::env::set_var("VCPKG_ALL_STATIC", "1");
    }

    let manifest_dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let workspace_root = manifest_dir.parent().unwrap().parent().unwrap();
    let installed_dir = workspace_root.join("vcpkg_installed");

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let vcpkg_triplet = std::env::var("VCPKG_DEFAULT_TRIPLET").unwrap_or_else(|_| {
        match (target_os.as_str(), target_arch.as_str()) {
            ("windows", "x86_64") => "x64-windows-static".to_string(),
            ("windows", "x86") => "x86-windows-static".to_string(),
            ("windows", "aarch64") => "arm64-windows-static".to_string(),
            ("macos", "x86_64") => "x64-osx".to_string(),
            ("macos", "aarch64") => "arm64-osx".to_string(),
            ("linux", "x86_64") => "x64-linux".to_string(),
            ("linux", "aarch64") => "arm64-linux".to_string(),
            _ => "x64-windows-static".to_string(),
        }
    });

    if installed_dir.exists() {
        unsafe {
            std::env::set_var("VCPKG_INSTALLED_DIR", &installed_dir);
            std::env::set_var("VCPKG_TARGET_TRIPLET", &vcpkg_triplet);
        }
    }

    let mut config = vcpkg::Config::new();
    config.cargo_metadata(true);

    let include_dirs = match config.find_package("openexr") {
        Ok(lib) => {
            for include in &lib.include_paths {
                println!("cargo:include={}", include.display());
            }
            lib.include_paths.clone()
        }
        Err(e) => {
            let lib_dir = installed_dir.join(&vcpkg_triplet).join("lib");
            let include_dir = installed_dir.join(&vcpkg_triplet).join("include");
            if !(lib_dir.exists() && include_dir.exists()) {
                panic!("Could not find OpenEXR via vcpkg: {e:?}");
            }

            println!("cargo:rustc-link-search=native={}", lib_dir.display());
            println!("cargo:include={}", include_dir.display());

            if target_os == "windows" {
                println!("cargo:rustc-link-lib=static=OpenEXRCore-3_4");
                println!("cargo:rustc-link-lib=static=OpenEXR-3_4");
                println!("cargo:rustc-link-lib=static=Iex-3_4");
                println!("cargo:rustc-link-lib=static=IlmThread-3_4");
                println!("cargo:rustc-link-lib=static=Imath-3_2");
                println!("cargo:rustc-link-lib=static=openjph.0.27");
                println!("cargo:rustc-link-lib=static=deflatestatic");
                println!("cargo:rustc-link-lib=static=zlibstatic");
            } else {
                println!("cargo:rustc-link-lib=static=OpenEXRCore-3_4");
                println!("cargo:rustc-link-lib=static=OpenEXR-3_4");
                println!("cargo:rustc-link-lib=static=Iex-3_4");
                println!("cargo:rustc-link-lib=static=IlmThread-3_4");
                println!("cargo:rustc-link-lib=static=Imath-3_2");
                println!("cargo:rustc-link-lib=static=openjph");
                println!("cargo:rustc-link-lib=static=deflate");
                println!("cargo:rustc-link-lib=static=z");
                if target_os == "macos" {
                    println!("cargo:rustc-link-lib=dylib=c++");
                } else if target_os == "linux" {
                    println!("cargo:rustc-link-lib=dylib=stdc++");
                    println!("cargo:rustc-link-lib=dylib=m");
                }
            }
            vec![include_dir]
        }
    };

    let cpp = manifest_dir.join("src/deep_flatten.cpp");
    println!("cargo:rerun-if-changed={}", cpp.display());
    let mut build = cc::Build::new();
    build.cpp(true);
    build.file(&cpp);
    for inc in &include_dirs {
        build.include(inc);
        let imath = inc.join("Imath");
        if imath.exists() {
            build.include(imath);
        }
        let openexr = inc.join("OpenEXR");
        if openexr.exists() {
            build.include(openexr);
        }
    }
    if target_os == "windows" {
        build.flag("/std:c++17");
        build.flag("/EHsc");
    } else {
        build.flag("-std=c++17");
    }
    build.compile("siv_openexr_deep_flatten");
}
