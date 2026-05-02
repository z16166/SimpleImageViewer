use std::fs;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn cargo_declares_native_hdr_codec_wrappers_and_features() {
    let cargo = fs::read_to_string(repo_root().join("Cargo.toml")).expect("read Cargo.toml");

    for dependency in ["libavif-sys", "libheif-sys", "libjxl-sys"] {
        assert!(
            cargo.contains(dependency),
            "Cargo.toml should declare {dependency}"
        );
    }
    for feature in ["avif-native", "heif-native", "jpegxl", "hdr-modern-formats"] {
        assert!(
            cargo.contains(feature),
            "Cargo.toml should declare feature {feature}"
        );
    }
}

#[test]
fn vcpkg_manifest_declares_industrial_hdr_codec_libraries() {
    let manifest = fs::read_to_string(repo_root().join("vcpkg.json")).expect("read vcpkg.json");

    for package in ["libavif", "dav1d", "libheif", "libde265", "libjxl"] {
        assert!(
            manifest.contains(package),
            "vcpkg.json should declare {package}"
        );
    }
    assert!(
        !manifest.contains("\"aom\""),
        "aom is intentionally not required in the manifest because this vcpkg port currently needs an extra Perl download during build"
    );
}

#[test]
fn native_hdr_codec_wrapper_crates_have_build_scripts() {
    for crate_dir in ["libavif-sys", "libheif-sys", "libjxl-sys"] {
        let path = repo_root().join("libraries").join(crate_dir);
        assert!(path.join("Cargo.toml").exists(), "{crate_dir} Cargo.toml");
        assert!(path.join("build.rs").exists(), "{crate_dir} build.rs");
        assert!(
            path.join("src").join("lib.rs").exists(),
            "{crate_dir} lib.rs"
        );
    }
}

#[test]
fn native_hdr_backends_are_wired_past_initial_stubs() {
    let hdr_dir = repo_root().join("src").join("hdr");
    let avif = fs::read_to_string(hdr_dir.join("avif.rs")).expect("read avif backend");
    let heif = fs::read_to_string(hdr_dir.join("heif.rs")).expect("read heif backend");
    let jxl = fs::read_to_string(hdr_dir.join("jpegxl.rs")).expect("read jpegxl backend");
    let loader =
        fs::read_to_string(repo_root().join("src").join("loader.rs")).expect("read loader");
    let raw = fs::read_to_string(repo_root().join("src").join("raw_processor.rs"))
        .expect("read raw processor");

    for (name, source) in [
        ("AVIF", avif.as_str()),
        ("HEIF", heif.as_str()),
        ("JPEG XL", jxl.as_str()),
    ] {
        assert!(
            !source.contains("not wired"),
            "{name} backend should no longer be a native decode stub"
        );
    }
    for decode_entry in ["decode_avif_hdr", "load_heif_hdr", "load_jxl_hdr"] {
        assert!(
            loader.contains(decode_entry),
            "loader should route through {decode_entry}"
        );
    }
    assert!(
        loader.contains("hdr_to_sdr_rgba8")
            || heif.contains("hdr_to_sdr_rgba8")
            || jxl.contains("hdr_to_sdr_rgba8"),
        "native HDR formats should provide SDR fallbacks from HDR pixels"
    );

    assert!(
        raw.contains("develop_scene_linear_hdr"),
        "RAW processor should expose a scene-linear HDR development path"
    );
    assert!(
        raw.contains("siv_libraw_set_gamma(self.data, 1.0, 1.0)"),
        "RAW HDR development should request linear gamma from LibRaw"
    );
}

#[test]
fn gain_map_tmap_support_is_wired_for_modern_hdr_codecs() {
    let hdr_dir = repo_root().join("src").join("hdr");
    let gain_map = fs::read_to_string(hdr_dir.join("gain_map.rs")).expect("read gain_map module");
    let avif = fs::read_to_string(hdr_dir.join("avif.rs")).expect("read avif backend");
    let heif = fs::read_to_string(hdr_dir.join("heif.rs")).expect("read heif backend");
    let jxl = fs::read_to_string(hdr_dir.join("jpegxl.rs")).expect("read jpegxl backend");
    let loader =
        fs::read_to_string(repo_root().join("src").join("loader.rs")).expect("read loader");

    assert!(gain_map.contains("parse_iso_gain_map_metadata"));
    assert!(gain_map.contains("append_hdr_pixel_from_sdr_and_gain"));
    assert!(avif.contains("avif_gain_map_to_metadata"));
    assert!(avif.contains("siv_avif_decoder_decode_all_content"));
    assert!(jxl.contains("read_jxl_gain_map_bundle"));
    assert!(jxl.contains("JXL_DEC_BOX"));
    assert!(heif.contains("classify_heif_auxiliary_type"));
    assert!(heif.contains("hdrgainmap"));
    assert!(heif.contains("tmap"));
    assert!(loader.contains("load_avif_with_target_capacity"));
    assert!(loader.contains("load_jxl_with_target_capacity"));
}
