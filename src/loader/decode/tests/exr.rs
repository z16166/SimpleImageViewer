//! OpenEXR routing, corpus loads, and EXR-vs-fallback probes.

use std::path::{Path, PathBuf};

use crate::hdr::types::HdrToneMapSettings;
use crate::loader::ImageData;
use crate::loader::decode::detect::load_via_content_detection;
use crate::loader::decode::hdr_formats::{load_hdr, try_load_disk_backed_exr_hdr};
use crate::loader::decode::load_image_file;

use super::support::{lock_tiled_threshold_for_test, TiledThresholdOverride};

fn openexr_images_root() -> Option<PathBuf> {
    std::env::var_os("SIV_OPENEXR_IMAGES_DIR")
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from(r"F:\HDR\openexr-images")))
        .filter(|path| path.is_dir())
}

fn assert_gray_ramp_loads_with_visible_fallback(root: &Path, relative_path: &str) {
    let path = root.join(relative_path);
    assert!(
        path.is_file(),
        "OpenEXR sample file is missing: {}",
        path.display()
    );

    let image_data = load_hdr(&path, 1.0, HdrToneMapSettings::default())
        .unwrap_or_else(|err| panic!("load {}: {err}", path.display()));
    let (hdr_max_rgb, fallback_pixels) = match image_data {
        ImageData::Hdr { hdr, fallback } => (
            max_hdr_rgb(hdr.rgba_f32.as_slice()),
            fallback.rgba().to_vec(),
        ),
        ImageData::HdrTiled { .. } => panic!(
            "{} is small enough for static HDR and should not route through tiled rendering",
            path.display()
        ),
        _ => panic!(
            "expected {} to load as static HDR image data",
            path.display()
        ),
    };
    let fallback_max_rgb = max_rgba8_rgb(&fallback_pixels);

    assert!(
        fallback_max_rgb > 0,
        "fallback display pixels should not be all black for {} (hdr_max_rgb={hdr_max_rgb:?})",
        path.display(),
    );
}

fn max_hdr_rgb(rgba_f32: &[f32]) -> Option<f32> {
    rgba_f32
        .chunks_exact(4)
        .map(|pixel| pixel[0].max(pixel[1]).max(pixel[2]))
        .reduce(f32::max)
}

fn max_rgba8_rgb(pixels: &[u8]) -> u8 {
    pixels
        .chunks_exact(4)
        .map(|pixel| pixel[0].max(pixel[1]).max(pixel[2]))
        .max()
        .unwrap_or(0)
}

fn collect_exr_files(root: &Path, files: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(root).unwrap_or_else(|err| {
        panic!("read OpenEXR corpus directory {}: {err}", root.display())
    });
    for entry in entries {
        let path = entry.expect("read OpenEXR corpus entry").path();
        if path.is_dir() {
            collect_exr_files(&path, files);
        } else if path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("exr"))
        {
            files.push(path);
        }
    }
}

#[test]
fn gray_ramps_load_with_visible_fallback_pixels() {
    let _threshold_lock = lock_tiled_threshold_for_test();
    let _threshold_override = TiledThresholdOverride::set(u64::MAX);
    let Some(root) = openexr_images_root() else {
        eprintln!(
            "skipping OpenEXR GrayRamps loader regression test; set SIV_OPENEXR_IMAGES_DIR to openexr-images"
        );
        return;
    };

    assert_gray_ramp_loads_with_visible_fallback(&root, "TestImages/GrayRampsDiagonal.exr");
    assert_gray_ramp_loads_with_visible_fallback(&root, "TestImages/GrayRampsHorizontal.exr");
}

#[test]
fn openexr_standard_corpus_loads_every_exr_sample() {
    let Some(root) = openexr_images_root() else {
        eprintln!(
            "skipping OpenEXR corpus load test; set SIV_OPENEXR_IMAGES_DIR to openexr-images"
        );
        return;
    };

    let mut files = Vec::new();
    collect_exr_files(&root, &mut files);
    files.sort();
    assert!(!files.is_empty(), "OpenEXR corpus contains no EXR files");

    let failures: Vec<String> = files
        .iter()
        .filter_map(|path| {
            load_hdr(path, 1.0, HdrToneMapSettings::default())
                .err()
                .map(|err| {
                    let relative = path.strip_prefix(&root).unwrap_or(path);
                    format!("{}: {err}", relative.display())
                })
        })
        .collect();

    assert!(
        failures.is_empty(),
        "OpenEXR corpus load failures ({}/{}):\n{}",
        failures.len(),
        files.len(),
        failures.join("\n")
    );
}

#[test]
fn deep_openexr_standard_passes_decode_without_placeholder() {
    let root = std::path::PathBuf::from(r"F:\HDR\openexr-images");
    if !root.is_dir() {
        eprintln!(
            "skipping OpenEXR deep sample test; set up F:\\HDR\\openexr-images or SIV_OPENEXR_IMAGES_DIR"
        );
        return;
    }

    for relative_path in [
        "v2/LeftView/Balls.exr",
        "v2/LeftView/Ground.exr",
        "v2/LeftView/Leaves.exr",
        "v2/LeftView/Trunks.exr",
        "v2/LowResLeftView/Balls.exr",
        "v2/LowResLeftView/Ground.exr",
        "v2/LowResLeftView/Leaves.exr",
        "v2/LowResLeftView/Trunks.exr",
        "v2/Stereo/Balls.exr",
        "v2/Stereo/Ground.exr",
        "v2/Stereo/Leaves.exr",
        "v2/Stereo/Trunks.exr",
    ] {
        let path = root.join(relative_path);
        assert!(
            path.is_file(),
            "OpenEXR deep sample file is missing: {}",
            path.display()
        );

        let hdr = crate::hdr::exr_tiled::decode_deep_exr_image(&path).unwrap_or_else(|err| {
            panic!(
                "decode deep OpenEXR sample failed for {}: {err}",
                path.display()
            )
        });
        assert_eq!(
            hdr.rgba_f32.len(),
            hdr.width as usize * hdr.height as usize * 4
        );
        assert!(
            hdr.rgba_f32.iter().all(|value| value.is_finite()),
            "deep EXR decode should produce finite float samples: {}",
            path.display()
        );
    }
}

#[test]
fn deep_openexr_standard_sample_loads_hdr_float_content() {
    let path = std::path::PathBuf::from(r"F:\HDR\openexr-images\v2\LowResLeftView\Balls.exr");
    if !path.is_file() {
        eprintln!(
            "skipping OpenEXR deep sample test; set up F:\\HDR\\openexr-images or SIV_OPENEXR_IMAGES_DIR"
        );
        return;
    }

    let image_data =
        load_hdr(&path, 1.0, HdrToneMapSettings::default()).expect("load deep OpenEXR sample");
    let ImageData::Hdr { hdr, .. } = image_data else {
        panic!("unexpected deep EXR image data");
    };
    assert!(
        hdr.rgba_f32
            .chunks_exact(4)
            .any(|pixel| pixel[0] > 0.0 || pixel[1] > 0.0 || pixel[2] > 0.0),
        "deep EXR HDR buffer should contain visible RGB content"
    );
    assert!(
        hdr.rgba_f32.chunks_exact(4).any(|pixel| pixel[3] > 0.0),
        "deep EXR HDR buffer should contain visible alpha"
    );
}

#[test]
fn disk_backed_exr_probe_accepts_subsampled_yc_sample() {
    let path = std::path::PathBuf::from(r"F:\HDR\openexr-images\Chromaticities\Rec709_YC.exr");
    if !path.is_file() {
        eprintln!(
            "skipping OpenEXR YC sample test; set up F:\\HDR\\openexr-images or SIV_OPENEXR_IMAGES_DIR"
        );
        return;
    }

    let image_data = try_load_disk_backed_exr_hdr(&path, 1.0, HdrToneMapSettings::default())
        .expect("probe should load subsampled YC EXR");

    assert!(matches!(image_data, Some(ImageData::HdrTiled { .. })));
}

#[test]
fn exr_extension_short_circuits_to_openexr_core_loader() {
    let path = std::env::temp_dir().join(format!(
        "simple_image_viewer_loader_exr_short_circuit_{}.exr",
        std::process::id()
    ));
    std::fs::write(&path, b"not an exr file").expect("write invalid EXR probe");
    let (tx, _rx) = crossbeam_channel::unbounded();
    let (refine_tx, _refine_rx) = crossbeam_channel::unbounded();

    let result = load_image_file(
        1,
        0,
        &path,
        tx,
        refine_tx,
        false,
        HdrToneMapSettings::default().target_hdr_capacity(),
        HdrToneMapSettings::default(),
    );
    let err = match result.result {
        Ok(_) => panic!("invalid EXR should fail in the OpenEXRCore loader"),
        Err(err) => err,
    };
    let _ = std::fs::remove_file(&path);

    assert!(
        err.contains("OpenEXRCore"),
        "EXR extension must not fall through to image-rs/static fallback: {err}"
    );
}

#[test]
fn exr_magic_short_circuits_to_openexr_core_loader_even_with_wrong_extension() {
    let path = std::env::temp_dir().join(format!(
        "simple_image_viewer_loader_exr_magic_short_circuit_{}.png",
        std::process::id()
    ));
    std::fs::write(&path, [0x76, 0x2f, 0x31, 0x01, 0, 0, 0, 0])
        .expect("write invalid EXR magic probe");

    let result = load_via_content_detection(
        &path,
        HdrToneMapSettings::default().target_hdr_capacity(),
        HdrToneMapSettings::default(),
    );
    let err = match result {
        Ok(_) => panic!("invalid EXR magic should fail in the OpenEXRCore loader"),
        Err(err) => err,
    };
    let _ = std::fs::remove_file(&path);

    assert!(
        err.contains("OpenEXRCore"),
        "EXR magic must route to OpenEXRCore even when extension is wrong: {err}"
    );
}
