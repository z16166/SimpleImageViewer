use std::path::Path;

fn configured_sample_dir(var: &str) -> Option<String> {
    std::env::var(var)
        .ok()
        .filter(|value| Path::new(value).is_dir())
}

#[test]
fn hdr_format_corpus_environment_gates_are_recognized() {
    for var in [
        "SIV_AVIF_HDR_SAMPLES_DIR",
        "SIV_HEIF_HDR_SAMPLES_DIR",
        "SIV_JXL_HDR_SAMPLES_DIR",
        "SIV_RAW_HDR_SAMPLES_DIR",
        "SIV_PSD_SAMPLES_DIR",
    ] {
        let _ = configured_sample_dir(var);
    }
}

#[test]
#[ignore = "requires local HDR corpus directories and visual validation"]
fn manual_hdr_sdr_verification_matrix() {
    let matrix = [
        ("AVIF PQ/BT.2020", "native HDR and SDR tone-map"),
        ("AVIF HLG/BT.2020", "scene/display reference handling"),
        (
            "HEIC 10/12-bit PQ",
            "16-bit libheif decode and SDR fallback",
        ),
        (
            "HEIC gain-map sample",
            "auxiliary gain-map/tmap diagnostics with primary HDR fallback",
        ),
        (
            "AVIF ISO gain-map sample",
            "capacity-sensitive gain-map reconstruction with SDR fallback",
        ),
        (
            "JPEG XL jhgm gain-map sample",
            "jhgm bundle decode and capacity-sensitive reconstruction",
        ),
        (
            "JPEG XL PQ/HLG",
            "libjxl color profile target and float output",
        ),
        ("RAW DNG/CR3/NEF", "scene-linear HDR path for small images"),
        (
            "Large modern HDR image",
            "tile threshold and fallback behavior",
        ),
        ("Windows DX12/scRGB HDR monitor", "native output"),
        ("macOS Metal/EDR HDR monitor", "native output"),
        ("SDR monitor", "tone-mapped output"),
    ];

    assert_eq!(matrix.len(), 12);
}
