use super::*;

fn gain_map_metadata_parses_paris_xmp_headroom_as_log2() {
    let gain_map_jpeg = minimal_jpeg_with_app1_xmp(
        r#"
        <rdf:Description
          xmlns:hdrgm="http://ns.adobe.com/hdr-gain-map/1.0/"
          hdrgm:Version="1.0"
          hdrgm:GainMapMax="3.7"
          hdrgm:HDRCapacityMin="0"
          hdrgm:HDRCapacityMax="3.5"/>
    "#,
    );

    let metadata = gain_map_metadata(&gain_map_jpeg).expect("parse paris-class XMP");
    assert!((metadata.hdr_capacity_min - 1.0).abs() < 0.001);
    assert!((metadata.hdr_capacity_max - 2.0_f32.powf(3.5)).abs() < 0.001);

    let tone = crate::hdr::types::HdrToneMapSettings {
        max_display_nits: 450.0,
        ..Default::default()
    };
    let weight = gain_map_weight(metadata, tone.target_hdr_capacity());
    assert!(
        weight < 0.4,
        "Paris-class headroom should not apply ~full gain-map weight on 450 nit display (got {weight})"
    );
}

#[test]
fn gain_map_metadata_diagnostic_reports_recovery_parameters() {
    let metadata = GainMapMetadata {
        gain_map_min: [0.1, 0.2, 0.3],
        gain_map_max: [1.0, 2.0, 3.0],
        gamma: [1.0, 1.5, 2.0],
        offset_sdr: [0.01, 0.02, 0.03],
        offset_hdr: [0.04, 0.05, 0.06],
        hdr_capacity_min: 1.25,
        hdr_capacity_max: 4.5,
        backward_direction: false,
    };

    let diagnostic = gain_map_metadata_diagnostic(metadata, 3.0);

    assert!(diagnostic.contains("GainMapMin=[0.100,0.200,0.300]"));
    assert!(diagnostic.contains("GainMapMax=[1.000,2.000,3.000]"));
    assert!(diagnostic.contains("Gamma=[1.000,1.500,2.000]"));
    assert!(diagnostic.contains("OffsetSDR=[0.010,0.020,0.030]"));
    assert!(diagnostic.contains("OffsetHDR=[0.040,0.050,0.060]"));
    assert!(diagnostic.contains("HDRCapacity=[1.250,4.500]"));
    assert!(diagnostic.contains("target=3.000"));
}

#[test]
fn gain_map_metadata_sets_backward_for_hdr_base_rendition() {
    let gain_map_jpeg = minimal_jpeg_with_app1_xmp(
        r#"
        <rdf:Description
          xmlns:hdrgm="http://ns.adobe.com/hdr-gain-map/1.0/"
          hdrgm:Version="1.0"
          hdrgm:GainMapMax="3.0"
          hdrgm:BaseRenditionIsHDR="True"/>
    "#,
    );

    let metadata =
        gain_map_metadata(&gain_map_jpeg).expect("HDR base gain map metadata should parse");

    assert!(metadata.backward_direction);
}

#[test]
fn gain_map_metadata_parses_iso_backward_direction() {
    let mut iso = Vec::new();
    write_iso_common_denominator_metadata(
        &mut iso,
        10,
        20,
        0,
        &[(0, 30, 10, 0, 0), (1, 31, 11, 1, 1), (2, 32, 12, 2, 2)],
    );
    iso[4] = 0b0000_1100; // backward + common denominator
    let gain_map_jpeg = minimal_jpeg_with_app1_xmp_and_app2_iso(
        r#"
        <rdf:Description
          xmlns:hdrgm="http://ns.adobe.com/hdr-gain-map/1.0/"
          hdrgm:Version="1.0"
          hdrgm:GainMapMax="1.0"/>
    "#,
        &iso,
    );

    let metadata = gain_map_metadata(&gain_map_jpeg).expect("parse ISO backward metadata");
    assert!(metadata.backward_direction);
}

#[test]
fn attach_iso_hdr_base_skips_iso_deferred() {
    let mut iso = Vec::new();
    write_iso_common_denominator_metadata(
        &mut iso,
        10,
        20,
        0,
        &[(0, 30, 10, 0, 0), (1, 31, 11, 1, 1), (2, 32, 12, 2, 2)],
    );
    iso[4] = 0b0000_1100;
    let metadata = gain_map_metadata(&minimal_jpeg_with_app1_xmp_and_app2_iso(
        r#"<rdf:Description xmlns:hdrgm="http://ns.adobe.com/hdr-gain-map/1.0/" hdrgm:Version="1.0"/>"#,
        &iso,
    ))
    .expect("parse ISO backward metadata");

    let hdr = attach_iso_gain_map_hdr_base_from_primary_rgba8(
        "JPEG_R",
        1,
        1,
        vec![255, 128, 64, 255],
        metadata,
    )
    .expect("attach hdr base");

    assert_eq!(hdr.rgba_f32.len(), 4);
    assert!(iso_deferred_from_metadata(&hdr.metadata).is_none());
    assert_eq!(hdr.metadata.transfer_function, HdrTransferFunction::Linear);
}

#[test]
fn gain_map_metadata_prefers_iso_over_xmp() {
    let mut iso = Vec::new();
    write_iso_common_denominator_metadata(
        &mut iso,
        10,
        0,
        20,
        &[(0, 30, 10, 0, 0), (1, 31, 11, 1, 1), (2, 32, 12, 2, 2)],
    );
    let gain_map_jpeg = minimal_jpeg_with_app1_xmp_and_app2_iso(
        r#"
        <rdf:Description
          xmlns:hdrgm="http://ns.adobe.com/hdr-gain-map/1.0/"
          hdrgm:Version="1.0"
          hdrgm:GainMapMax="1.0"
          hdrgm:HDRCapacityMax="1.0"/>
    "#,
        &iso,
    );

    let metadata = gain_map_metadata(&gain_map_jpeg).expect("parse ISO gain map metadata");

    assert_eq!(metadata.gain_map_min, [0.0, 0.1, 0.2]);
    assert_eq!(metadata.gain_map_max, [3.0, 3.1, 3.2]);
    assert_eq!(metadata.gamma, [1.0, 1.1, 1.2]);
    assert_eq!(metadata.offset_sdr, [0.0, 0.1, 0.2]);
    assert_eq!(metadata.offset_hdr, [0.0, 0.1, 0.2]);
    assert_eq!(metadata.hdr_capacity_min, 1.0);
    assert_eq!(metadata.hdr_capacity_max, 4.0);
}

#[test]
fn gain_map_metadata_parses_ordered_rgb_values() {
    let gain_map_jpeg = minimal_jpeg_with_app1_xmp(
        r#"
        <rdf:Description
          xmlns:hdrgm="http://ns.adobe.com/hdr-gain-map/1.0/"
          hdrgm:Version="1.0"
          hdrgm:HDRCapacityMax="4.0">
          <hdrgm:GainMapMin>
            <rdf:Seq><rdf:li>0.1</rdf:li><rdf:li>0.2</rdf:li><rdf:li>0.3</rdf:li></rdf:Seq>
          </hdrgm:GainMapMin>
          <hdrgm:GainMapMax>
            <rdf:Seq><rdf:li>1.0</rdf:li><rdf:li>2.0</rdf:li><rdf:li>3.0</rdf:li></rdf:Seq>
          </hdrgm:GainMapMax>
          <hdrgm:Gamma>
            <rdf:Seq><rdf:li>1.0</rdf:li><rdf:li>2.0</rdf:li><rdf:li>4.0</rdf:li></rdf:Seq>
          </hdrgm:Gamma>
        </rdf:Description>
    "#,
    );

    let metadata = gain_map_metadata(&gain_map_jpeg).expect("parse RGB gain map metadata");

    assert_eq!(metadata.gain_map_min, [0.1, 0.2, 0.3]);
    assert_eq!(metadata.gain_map_max, [1.0, 2.0, 3.0]);
    assert_eq!(metadata.gamma, [1.0, 2.0, 4.0]);
}

#[test]
fn gain_map_metadata_rejects_non_positive_gamma() {
    let gain_map_jpeg = minimal_jpeg_with_app1_xmp(
        r#"
        <rdf:Description
          xmlns:hdrgm="http://ns.adobe.com/hdr-gain-map/1.0/"
          hdrgm:Version="1.0"
          hdrgm:GainMapMax="3.0"
          hdrgm:Gamma="0.0"/>
    "#,
    );

    let err = gain_map_metadata(&gain_map_jpeg).expect_err("reject non-positive gamma");

    assert!(err.contains("Gamma"));
}

#[test]
fn gain_map_offsets_and_gamma_affect_recovered_hdr_pixel() {
    let metadata = GainMapMetadata {
        gain_map_min: [0.0; 3],
        gain_map_max: [4.0; 3],
        gamma: [2.0; 3],
        offset_sdr: [0.25; 3],
        offset_hdr: [0.10; 3],
        hdr_capacity_min: 0.0,
        hdr_capacity_max: 2.0,
        backward_direction: false,
    };

    let recovered = recover_hdr_channel_from_sdr_and_gain(255, 0.25, metadata, 0, 2.0);

    assert!((recovered - 4.9).abs() < 0.001);
}

#[test]
fn gain_map_sampling_preserves_rgb_channels() {
    let gain_rgba = vec![0, 64, 128, 255];

    let sampled = sample_gain_map_rgb(&gain_rgba, 1, 1, 0, 0, 1, 1);

    assert!((sampled[0] - 0.0).abs() < 0.001);
    assert!((sampled[1] - 64.0 / 255.0).abs() < 0.001);
    assert!((sampled[2] - 128.0 / 255.0).abs() < 0.001);
}

#[test]
fn hdr_orientation_rotates_float_buffer_like_exif_orientation() {
    let hdr = HdrImageBuffer {
        width: 2,
        height: 1,
        format: HdrPixelFormat::Rgba32Float,
        color_space: HdrColorSpace::LinearSrgb,
        metadata: HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
        rgba_f32: Arc::new(vec![
            1.0, 0.0, 0.0, 1.0, //
            0.0, 1.0, 0.0, 1.0,
        ]),
    };

    let oriented = apply_orientation_to_hdr_buffer(hdr, 6);

    assert_eq!((oriented.width, oriented.height), (1, 2));
    assert_eq!(
        oriented.rgba_f32.as_slice(),
        &[
            1.0, 0.0, 0.0, 1.0, //
            0.0, 1.0, 0.0, 1.0,
        ]
    );
}

#[test]
fn display_to_physical_maps_orientation_six() {
    assert_eq!(display_to_physical_pixel(0, 0, 2, 1, 6), (0, 0));
    assert_eq!(display_to_physical_pixel(0, 1, 2, 1, 6), (1, 0));
}

#[test]
fn hdr_capacity_scales_gain_map_application() {
    let metadata = GainMapMetadata {
        gain_map_min: [0.0; 3],
        gain_map_max: [2.0; 3],
        gamma: [1.0; 3],
        offset_sdr: [0.0; 3],
        offset_hdr: [0.0; 3],
        // Ratios 2^0 .. 2^2 so log₂ headroom interpolates like libavif `avifGetGainMapWeight`.
        hdr_capacity_min: 1.0,
        hdr_capacity_max: 4.0,
        backward_direction: false,
    };

    assert_eq!(gain_map_weight(metadata, 0.5), 0.0);
    assert!((gain_map_weight(metadata, 2.0) - 0.5).abs() < 0.001);
    assert_eq!(gain_map_weight(metadata, 4.0), 1.0);
}

#[test]
fn hdr_capacity_weight_changes_recovered_hdr_pixel() {
    let metadata = GainMapMetadata {
        gain_map_min: [0.0; 3],
        gain_map_max: [2.0; 3],
        gamma: [1.0; 3],
        offset_sdr: [0.0; 3],
        offset_hdr: [0.0; 3],
        hdr_capacity_min: 1.0,
        hdr_capacity_max: 4.0,
        backward_direction: false,
    };
    let sdr = [255, 255, 255, 255];

    let low = recover_hdr_channel_from_sdr_and_gain(255, 1.0, metadata, 0, 1.0);
    let mid = recover_hdr_channel_from_sdr_and_gain(255, 1.0, metadata, 0, 2.0);
    let high = recover_hdr_channel_from_sdr_and_gain(255, 1.0, metadata, 0, 4.0);

    assert!((low - 1.0).abs() < 0.001);
    assert!(mid > low && mid < high);
    assert!((high - 4.0).abs() < 0.001);

    let mut rgba = Vec::new();
    append_hdr_pixel_from_sdr_and_gain(&mut rgba, &sdr, [1.0; 3], metadata, 2.0);
    assert!((rgba[0] - mid).abs() < 0.001);
}

#[test]
fn per_channel_metadata_changes_recovered_hdr_channels() {
    let metadata = GainMapMetadata {
        gain_map_min: [0.0; 3],
        gain_map_max: [1.0, 2.0, 3.0],
        gamma: [1.0; 3],
        offset_sdr: [0.0; 3],
        offset_hdr: [0.0; 3],
        hdr_capacity_min: 1.0,
        hdr_capacity_max: 8.0,
        backward_direction: false,
    };
    let mut rgba = Vec::new();

    append_hdr_pixel_from_sdr_and_gain(&mut rgba, &[255, 255, 255, 255], [1.0; 3], metadata, 8.0);

    assert!((rgba[0] - 2.0).abs() < 0.001);
    assert!((rgba[1] - 4.0).abs() < 0.001);
    assert!((rgba[2] - 8.0).abs() < 0.001);
}

/// `cargo test probe_paris_gainmap -- --ignored --nocapture`
#[test]
#[ignore = "manual probe against libavif paris gain-map JPEGs"]
fn probe_paris_gainmap_jpegs() {
    use crate::hdr::decode::linear_srgb_linear_to_srgb_u8;
    use crate::hdr::gain_map::gain_map_metadata_diagnostic;
    use std::path::Path;

    let tone = crate::hdr::types::HdrToneMapSettings {
        max_display_nits: 450.0,
        ..Default::default()
    };
    let capacity = tone.target_hdr_capacity();

    for name in [
        "paris_exif_xmp_gainmap_bigendian.jpg",
        "paris_exif_xmp_gainmap_littleendian.jpg",
        "paris_exif_xmp_icc_gainmap_bigendian.jpg",
        "paris_exif_xmp_icc.jpg",
    ] {
        let path = Path::new("/home/happy/Downloads/HDR/libavif/tests/data").join(name);
        if !path.is_file() {
            eprintln!("skip {}", path.display());
            continue;
        }
        let bytes = std::fs::read(&path).expect("read");
        if let Ok(info) = inspect_ultra_hdr_jpeg_bytes(&bytes) {
            eprintln!("{name}: ultra_hdr={}", info.is_ultra_hdr);
        }
        if let Ok(gm_jpeg) = extract_gain_map_jpeg_bytes(&bytes) {
            let meta = gain_map_metadata(&gm_jpeg).expect("gain meta");
            eprintln!(
                "{name}: gain meta: {}",
                gain_map_metadata_diagnostic(meta, capacity)
            );
        }
        if let Ok(hdr) = decode_ultra_hdr_jpeg_bytes_with_cpu_compose(&bytes, capacity) {
            let cx = hdr.width as usize / 2;
            let cy = hdr.height as usize / 2;
            let i = (cy * hdr.width as usize + cx) * 4;
            let rgb = [hdr.rgba_f32[i], hdr.rgba_f32[i + 1], hdr.rgba_f32[i + 2]];
            let sdr = [
                linear_srgb_linear_to_srgb_u8(rgb[0]),
                linear_srgb_linear_to_srgb_u8(rgb[1]),
                linear_srgb_linear_to_srgb_u8(rgb[2]),
            ];
            eprintln!("{name}: hdr linear center {rgb:?} sdr8 {sdr:?}");
            if name.contains("gainmap") {
                assert!(
                    rgb[0] < 1.5 && rgb[1] < 1.5 && rgb[2] < 1.5,
                    "{name} center linear should stay in display range after headroom fix"
                );
            }
            continue;
        }
        let (_, _, rgba) = libjpeg_turbo::decode_to_rgba(&bytes).expect("sdr decode");
        let cx = rgba.len() / 4 / 2;
        eprintln!(
            "{name}: baseline sdr center [{}, {}, {}]",
            rgba[cx * 4],
            rgba[cx * 4 + 1],
            rgba[cx * 4 + 2]
        );
    }
}

#[test]
fn ultra_hdr_decode_uses_target_hdr_capacity() {
    let Some(root) = ultra_hdr_samples_root() else {
        eprintln!(
            "skipping Ultra HDR corpus test; set SIV_ULTRA_HDR_SAMPLES_DIR to Ultra_HDR_Samples"
        );
        return;
    };
    let path = sample_path(&root, "Originals/Ultra_HDR_Samples_Originals_01.jpg");
    if !path.is_file() {
        eprintln!("skipping Ultra HDR target capacity test; sample missing");
        return;
    }
    let file = std::fs::File::open(&path).expect("open Ultra HDR sample");
    let bytes = unsafe { memmap2::Mmap::map(&file).expect("mmap Ultra HDR sample") };

    let low = decode_ultra_hdr_jpeg_bytes_with_cpu_compose(&bytes, 1.0)
        .expect("decode low-capacity Ultra HDR");
    let high = decode_ultra_hdr_jpeg_bytes_with_cpu_compose(&bytes, 8.0)
        .expect("decode high-capacity Ultra HDR");

    let low_peak = low
        .rgba_f32
        .chunks_exact(4)
        .map(|pixel| pixel[0].max(pixel[1]).max(pixel[2]))
        .fold(0.0_f32, f32::max);
    let high_peak = high
        .rgba_f32
        .chunks_exact(4)
        .map(|pixel| pixel[0].max(pixel[1]).max(pixel[2]))
        .fold(0.0_f32, f32::max);

    assert!(
        high_peak > low_peak,
        "higher target HDR capacity should recover brighter JPEG_R highlights"
    );
}

fn minimal_jpeg_with_app1_xmp(xmp: &str) -> Vec<u8> {
    let payload = format!("http://ns.adobe.com/xap/1.0/\0{xmp}");
    let len = u16::try_from(payload.len() + 2).expect("test XMP fits in JPEG segment");
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&[0xFF, 0xD8, 0xFF, 0xE1]);
    bytes.extend_from_slice(&len.to_be_bytes());
    bytes.extend_from_slice(payload.as_bytes());
    bytes.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x02, 0xFF, 0xD9]);
    bytes
}

fn minimal_jpeg_with_app1_xmp_and_app2_iso(xmp: &str, iso_metadata: &[u8]) -> Vec<u8> {
    let mut bytes = minimal_jpeg_with_app1_xmp(xmp);
    bytes.truncate(bytes.len() - 6);
    let mut payload = b"urn:iso:std:iso:ts:21496:-1\0".to_vec();
    payload.extend_from_slice(iso_metadata);
    let len = u16::try_from(payload.len() + 2).expect("test ISO metadata fits in JPEG segment");
    bytes.extend_from_slice(&[0xFF, 0xE2]);
    bytes.extend_from_slice(&len.to_be_bytes());
    bytes.extend_from_slice(&payload);
    bytes.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x02, 0xFF, 0xD9]);
    bytes
}

fn write_iso_common_denominator_metadata(
    out: &mut Vec<u8>,
    denominator: u32,
    base_hdr_headroom_n: u32,
    alternate_hdr_headroom_n: u32,
    channels: &[(i32, i32, u32, i32, i32); 3],
) {
    out.extend_from_slice(&0_u16.to_be_bytes());
    out.extend_from_slice(&0_u16.to_be_bytes());
    out.push(0x80 | 0x08);
    out.extend_from_slice(&denominator.to_be_bytes());
    out.extend_from_slice(&base_hdr_headroom_n.to_be_bytes());
    out.extend_from_slice(&alternate_hdr_headroom_n.to_be_bytes());
    for (gain_min, gain_max, gamma, offset_sdr, offset_hdr) in channels {
        out.extend_from_slice(&gain_min.to_be_bytes());
        out.extend_from_slice(&gain_max.to_be_bytes());
        out.extend_from_slice(&gamma.to_be_bytes());
        out.extend_from_slice(&offset_sdr.to_be_bytes());
        out.extend_from_slice(&offset_hdr.to_be_bytes());
    }
}
