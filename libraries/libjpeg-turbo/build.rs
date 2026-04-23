use std::env;
use std::path::{PathBuf};
use std::process::Command;

fn main() {
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let libjpeg_dir = manifest_dir.join("../../3rdparty/libjpeg-turbo");

    // 1. NASM: mandatory for x86_64 (SIMD requires it)
    let mut nasm_exe: Option<String> = None;
    if target_arch == "x86_64" {
        // Probe candidates: bare 'nasm' in PATH first, then $NASM env var, then well-known Windows path
        let candidates = [
            "nasm".to_string(),
            env::var("NASM").unwrap_or_default(),
            "C:\\Program Files\\NASM\\nasm.exe".to_string(),
        ];

        for candidate in &candidates {
            if candidate.is_empty() { continue; }
            if Command::new(candidate).arg("-v").output().is_ok() {
                nasm_exe = Some(candidate.clone());
                break;
            }
        }

        let nasm_path = nasm_exe.as_deref().unwrap_or_else(|| {
            panic!(
                "\n\n[ERROR] NASM is mandatory for x86_64 builds (SIMD acceleration).\n\
                Install NASM and add it to PATH, or set the NASM environment variable.\n\
                Checked: {:?}\n\n",
                &candidates
            )
        });
        println!("cargo:info=NASM found: {}", nasm_path);
    }

    // 2. Build with official CMake build system
    let mut config = cmake::Config::new(&libjpeg_dir);
    config
        .define("ENABLE_SHARED", "OFF")
        .define("ENABLE_STATIC", "ON")
        .define("WITH_SIMD", "ON")
        .define("WITH_TURBOJPEG", "OFF")
        .define("WITH_TOOLS", "OFF")
        .define("WITH_TESTS", "OFF");

    // Pass NASM path explicitly so CMake doesn't have to rely on PATH
    if let Some(ref nasm) = nasm_exe {
        config.define("CMAKE_ASM_NASM_COMPILER", nasm);
    }

    if target_arch == "aarch64" {
        config.define("CMAKE_SYSTEM_PROCESSOR", "aarch64");
    }

    let dst = config.build();


    // 3. Export paths for dependent crates (e.g. libraw-sys)
    let include_dir = dst.join("include");
    let lib_dir = dst.join("lib");
    let lib_dir_64 = dst.join("lib64");

    println!("cargo:include={}", include_dir.display());
    println!("cargo:src={}", libjpeg_dir.join("src").display()); // For some internal headers if needed
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-search=native={}", lib_dir_64.display());
    
    let lib_name = if target_os == "windows" {
        "jpeg-static"
    } else {
        "jpeg"
    };
    
    println!("cargo:rustc-link-lib=static={}", lib_name);
    println!("cargo:lib_name={}", lib_name);
}
