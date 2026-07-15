use super::{
    CompositeTiming, LAYER_PREFETCH_WINDOW, LayerChannel, LayerInfo, LayerRecord,
    SECTION_TYPE_BOUNDING_DIVIDER, SECTION_TYPE_CLOSED_FOLDER, SECTION_TYPE_OPEN_FOLDER,
    STRICT_LAYER_COMPOSITE_BLANK, StreamingPeakTracker, checked_layer_pixel_count,
    composite_layers_from_bytes_with_cancel, composite_layers_with_visibility_from_info,
    compute_effective_visibility, dimensions_within_limit, gpu_batch_eligible_decoded_bytes,
    layer_will_decode, parse_layer_records, run_composite_pass_cpu_streaming,
    scan_extra_tagged_blocks, strict_visibility_has_drawable_output,
};
use std::path::Path;

fn raw_channel_bytes(pixel: u8, pixel_count: usize) -> Vec<u8> {
    let mut data = vec![0u8, 0u8]; // compression = 0 (raw)
    data.extend(std::iter::repeat_n(pixel, pixel_count));
    data
}

/// Minimal spec for a synthetic composite test layer: full RGB channels
/// (raw/uncompressed) covering `[left, right) x [top, bottom)`, no alpha
/// or mask channel (alpha defaults to fully opaque).
struct TestLayerSpec {
    top: i32,
    left: i32,
    bottom: i32,
    right: i32,
    rgb: (u8, u8, u8),
    blend: [u8; 4],
    clipping: u8,
    opacity: u8,
}

/// Build `LayerRecord`s + a matching contiguous `channel_data` blob for
/// [`super::run_composite_pass_cpu_streaming`] /
/// [`crate::psb_layer_decode::decode_layers_for_composite`] tests, bypassing
/// the full on-disk PSD byte format.
fn build_test_layers(specs: &[TestLayerSpec]) -> (Vec<LayerRecord>, Vec<u8>) {
    let mut records = Vec::with_capacity(specs.len());
    let mut channel_data = Vec::new();
    for spec in specs {
        let width = (spec.right - spec.left) as u32;
        let height = (spec.bottom - spec.top) as u32;
        let pixel_count = (width * height) as usize;
        let mut channels = Vec::with_capacity(3);
        for (id, value) in [(0i16, spec.rgb.0), (1, spec.rgb.1), (2, spec.rgb.2)] {
            let bytes = raw_channel_bytes(value, pixel_count);
            channels.push(LayerChannel {
                id,
                data_len: bytes.len() as u32,
            });
            channel_data.extend_from_slice(&bytes);
        }
        records.push(LayerRecord {
            top: spec.top,
            left: spec.left,
            bottom: spec.bottom,
            right: spec.right,
            name: String::new(),
            layer_id: None,
            cmls_payload: None,
            channels,
            blend: spec.blend,
            opacity: spec.opacity,
            fill_opacity: None,
            clipping: spec.clipping,
            flags: 0,
            mask_size: 0,
            mask: None,
            real_mask: None,
            vector_mask: None,
            vector_mask_density: 255,
            vector_mask_feather: 0.0,
            is_section_divider: false,
            section_type: None,
        });
    }
    (records, channel_data)
}

fn mk_layer_info(
    width: u32,
    height: u32,
    records: Vec<LayerRecord>,
    channel_data: &[u8],
) -> LayerInfo<'_> {
    LayerInfo {
        records,
        channel_data,
        channel_data_shared: None,
        width,
        height,
        depth: 8,
        color_mode: 3,
        is_psb: false,
        cmyk_icc: Vec::new(),
    }
}

fn px(canvas: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
    let o = ((y * w + x) * 4) as usize;
    [canvas[o], canvas[o + 1], canvas[o + 2], canvas[o + 3]]
}

fn empty_timing() -> CompositeTiming {
    CompositeTiming {
        parse_ms: 0.0,
        unpack_ms: 0.0,
        cmyk_ms: 0.0,
        blend_ms: 0.0,
        readback_ms: 0.0,
        mode: "cpu",
        layers: 0,
    }
}

fn push_tagged_block(bytes: &mut Vec<u8>, key: &[u8; 4], payload: &[u8]) {
    bytes.extend_from_slice(b"8BIM");
    bytes.extend_from_slice(key);
    bytes.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    bytes.extend_from_slice(payload);
    if !payload.len().is_multiple_of(2) {
        bytes.push(0);
    }
}

fn minimal_psd_with_layer_extra(extra: Vec<u8>) -> Vec<u8> {
    let mut layer_record = Vec::new();
    layer_record.extend_from_slice(&0i32.to_be_bytes()); // top
    layer_record.extend_from_slice(&0i32.to_be_bytes()); // left
    layer_record.extend_from_slice(&1i32.to_be_bytes()); // bottom
    layer_record.extend_from_slice(&1i32.to_be_bytes()); // right
    layer_record.extend_from_slice(&0u16.to_be_bytes()); // channel count
    layer_record.extend_from_slice(b"8BIM");
    layer_record.extend_from_slice(b"norm");
    layer_record.extend_from_slice(&[255, 0, 0, 0]); // opacity, clipping, flags, filler
    layer_record.extend_from_slice(&(extra.len() as u32).to_be_bytes());
    layer_record.extend_from_slice(&extra);

    let mut layer_info = Vec::new();
    layer_info.extend_from_slice(&1i16.to_be_bytes());
    layer_info.extend_from_slice(&layer_record);

    let mut layer_mask_info = Vec::new();
    layer_mask_info.extend_from_slice(&(layer_info.len() as u32).to_be_bytes());
    layer_mask_info.extend_from_slice(&layer_info);

    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"8BPS");
    bytes.extend_from_slice(&1u16.to_be_bytes());
    bytes.extend_from_slice(&[0; 6]);
    bytes.extend_from_slice(&3u16.to_be_bytes());
    bytes.extend_from_slice(&1u32.to_be_bytes());
    bytes.extend_from_slice(&1u32.to_be_bytes());
    bytes.extend_from_slice(&8u16.to_be_bytes());
    bytes.extend_from_slice(&3u16.to_be_bytes());
    bytes.extend_from_slice(&0u32.to_be_bytes()); // color mode data
    bytes.extend_from_slice(&0u32.to_be_bytes()); // image resources
    bytes.extend_from_slice(&(layer_mask_info.len() as u32).to_be_bytes());
    bytes.extend_from_slice(&layer_mask_info);
    bytes.extend_from_slice(&0u16.to_be_bytes()); // image data compression
    bytes
}

fn layer_extra_with_pascal_name(name: &[u8]) -> Vec<u8> {
    let mut extra = Vec::new();
    extra.extend_from_slice(&0u32.to_be_bytes()); // mask data length
    extra.extend_from_slice(&0u32.to_be_bytes()); // blending ranges length
    extra.push(name.len() as u8);
    extra.extend_from_slice(name);
    while extra.len() % 4 != 0 {
        extra.push(0);
    }
    extra
}

#[test]
fn parse_lyid_and_luni_from_extra_block() {
    let mut extra = layer_extra_with_pascal_name(b"A");
    push_tagged_block(&mut extra, b"lyid", &42u32.to_be_bytes());
    let mut luni = Vec::new();
    luni.extend_from_slice(&5u32.to_be_bytes());
    for unit in "Hello".encode_utf16() {
        luni.extend_from_slice(&unit.to_be_bytes());
    }
    push_tagged_block(&mut extra, b"luni", &luni);

    let bytes = minimal_psd_with_layer_extra(extra);
    let info = parse_layer_records(&bytes).expect("parse layers");

    assert_eq!(info.records.len(), 1);
    assert_eq!(info.records[0].layer_id, Some(42));
    assert_eq!(info.records[0].name, "Hello");
}

#[test]
fn parse_shmd_stores_cmls_payload() {
    let cmls_payload = [0, 0, 0, 16, b'c', b'm', b'l', b's'];
    let mut shmd = Vec::new();
    shmd.extend_from_slice(&1u32.to_be_bytes());
    shmd.extend_from_slice(b"8BIM");
    shmd.extend_from_slice(b"cmls");
    shmd.push(1); // copy flag
    shmd.extend_from_slice(&[0; 3]);
    shmd.extend_from_slice(&(cmls_payload.len() as u32).to_be_bytes());
    shmd.extend_from_slice(&cmls_payload);

    let mut extra = layer_extra_with_pascal_name(b"A");
    push_tagged_block(&mut extra, b"shmd", &shmd);

    let bytes = minimal_psd_with_layer_extra(extra);
    let info = parse_layer_records(&bytes).expect("parse layers");

    assert_eq!(
        info.records[0].cmls_payload.as_deref(),
        Some(&cmls_payload[..])
    );
}

/// Build a minimal `LayerRecord` for `compute_effective_visibility` tests;
/// only `flags` (hidden bit), `is_section_divider`, and `section_type`
/// matter for that function.
fn mk_layer(hidden: bool, is_section_divider: bool, section_type: Option<u32>) -> LayerRecord {
    LayerRecord {
        top: 0,
        left: 0,
        bottom: 1,
        right: 1,
        name: String::new(),
        layer_id: None,
        cmls_payload: None,
        channels: Vec::new(),
        blend: *b"norm",
        opacity: 255,
        fill_opacity: None,
        clipping: 0,
        flags: if hidden { 2 } else { 0 },
        mask_size: 0,
        mask: None,
        real_mask: None,
        vector_mask: None,
        vector_mask_density: 255,
        vector_mask_feather: 0.0,
        is_section_divider,
        section_type,
    }
}

#[test]
fn dimensions_within_limit_rejects_oversized_dimensions() {
    assert!(dimensions_within_limit(1, 1));
    assert!(dimensions_within_limit(
        crate::psb_reader::PSD_MAX_DIMENSION,
        1
    ));
    assert!(dimensions_within_limit(
        1,
        crate::psb_reader::PSD_MAX_DIMENSION
    ));
    // Per-side max alone would allow 300k x 300k (~90GB); pixel cap must reject it.
    assert!(!dimensions_within_limit(
        crate::psb_reader::PSD_MAX_DIMENSION,
        crate::psb_reader::PSD_MAX_DIMENSION
    ));
    assert!(!dimensions_within_limit(
        crate::psb_reader::PSD_MAX_DIMENSION + 1,
        1
    ));
    assert!(!dimensions_within_limit(
        1,
        crate::psb_reader::PSD_MAX_DIMENSION + 1
    ));
    assert!(!dimensions_within_limit(u32::MAX, u32::MAX));
    // Pixel budget is exactly 32768^2 (= 1G); one pixel over must fail.
    assert!(dimensions_within_limit(32_768, 32_768));
    assert!(!dimensions_within_limit(32_769, 32_769));
}

#[test]
fn max_layer_records_constant_is_sane() {
    // i16::unsigned_abs max is 65535; our DoS cap must be tighter and
    // still allow complex legitimate comps.
    const {
        assert!(super::MAX_LAYER_RECORDS < 65_535);
        assert!(super::MAX_LAYER_RECORDS >= 1024);
    }
}

#[test]
fn checked_layer_pixel_count_uses_checked_mul() {
    // Defense in depth for decode_channel_image: do not rely solely on
    // upstream dimensions_within_limit.
    assert_eq!(checked_layer_pixel_count(2, 3), Some(6));
    assert!(checked_layer_pixel_count(u32::MAX, u32::MAX).is_none());
    assert!(checked_layer_pixel_count(32_769, 32_769).is_none());
}

// RED until accumulate_decoded_layer_bytes + MAX_COMPOSITE_DECODED_BYTES land.
#[test]
fn composite_decoded_byte_budget_rejects_many_large_layers() {
    // Three 32k^2 RGBA layers are 12 GiB; budget must reject before alloc.
    let mut total = 0u64;
    for _ in 0..3 {
        match super::accumulate_decoded_layer_bytes(total, 32_768, 32_768) {
            Ok(next) => total = next,
            Err(e) => {
                assert!(e.contains("decoded layer byte budget"), "err: {e}");
                return;
            }
        }
    }
    panic!("expected decoded-layer byte budget to reject");
}

#[test]
fn compute_effective_visibility_leaf_hidden_inside_visible_group() {
    // Records in file (bottom-to-top) order:
    //   0: outer bottom divider (type 3)
    //   1: inner bottom divider (type 3)
    //   2: leaf, hidden, inside inner group
    //   3: inner folder header (type 1), visible
    //   4: leaf, visible, inside outer group but outside inner group
    //   5: outer folder header (type 2), visible
    let records = vec![
        mk_layer(false, true, Some(SECTION_TYPE_BOUNDING_DIVIDER)),
        mk_layer(false, true, Some(SECTION_TYPE_BOUNDING_DIVIDER)),
        mk_layer(true, false, None),
        mk_layer(false, true, Some(SECTION_TYPE_OPEN_FOLDER)),
        mk_layer(false, false, None),
        mk_layer(false, true, Some(SECTION_TYPE_CLOSED_FOLDER)),
    ];

    let visible = compute_effective_visibility(&records);

    assert!(!visible[2], "leaf's own hidden flag must hide it");
    assert!(
        visible[4],
        "sibling leaf in the same visible group stays visible"
    );
    assert!(visible[3], "inner group header itself is visible");
    assert!(visible[5], "outer group header itself is visible");
}

#[test]
fn compute_effective_visibility_group_hidden_hides_descendants() {
    // Same nesting as above, but the *outer* group is hidden while every
    // leaf/inner-group flag is visible.
    let records = vec![
        mk_layer(false, true, Some(SECTION_TYPE_BOUNDING_DIVIDER)),
        mk_layer(false, true, Some(SECTION_TYPE_BOUNDING_DIVIDER)),
        mk_layer(false, false, None),
        mk_layer(false, true, Some(SECTION_TYPE_OPEN_FOLDER)),
        mk_layer(false, false, None),
        mk_layer(true, true, Some(SECTION_TYPE_CLOSED_FOLDER)),
    ];

    let strict = compute_effective_visibility(&records);
    assert!(
        !strict[2],
        "strict visibility: leaf inside a hidden ancestor group must be hidden"
    );
    assert!(!strict[4], "strict visibility: sibling leaf also hidden");
    assert!(
        !strict[5],
        "strict visibility: the hidden group header itself is hidden"
    );
}

#[test]
fn strict_visibility_has_drawable_output_rejects_hidden_and_offcanvas() {
    let mut on_canvas = mk_layer(false, false, None);
    on_canvas.left = 0;
    on_canvas.top = 0;
    on_canvas.right = 10;
    on_canvas.bottom = 10;
    let mut off_canvas = mk_layer(false, false, None);
    off_canvas.left = 100;
    off_canvas.top = 100;
    off_canvas.right = 110;
    off_canvas.bottom = 110;
    let mut hidden = mk_layer(true, false, None);
    hidden.left = 0;
    hidden.top = 0;
    hidden.right = 10;
    hidden.bottom = 10;

    let records = vec![on_canvas.clone()];
    let visible = compute_effective_visibility(&records);
    assert!(strict_visibility_has_drawable_output(
        50, 50, &records, &visible
    ));

    let records = vec![off_canvas];
    let visible = compute_effective_visibility(&records);
    assert!(!strict_visibility_has_drawable_output(
        50, 50, &records, &visible
    ));

    let records = vec![hidden];
    let visible = compute_effective_visibility(&records);
    assert!(!strict_visibility_has_drawable_output(
        50, 50, &records, &visible
    ));
}

#[test]
fn compute_effective_visibility_unpaired_divider_does_not_panic() {
    // A lone bounding divider (type 3) with no matching folder header
    // above it must not underflow the visibility stack.
    let records = vec![
        mk_layer(false, true, Some(SECTION_TYPE_BOUNDING_DIVIDER)),
        mk_layer(false, false, None),
    ];

    let visible = compute_effective_visibility(&records);

    assert_eq!(visible.len(), 2);
    assert!(visible[1], "leaf above the unpaired divider stays visible");
}

#[test]
fn composite_layers_all_hidden_returns_blank_error() {
    // Two top-level groups, both eye-off: strict composite must not invent
    // visibility and must report blank without pixel work.
    let path = Path::new(r"F:\BaiduNetdiskDownload\素材库\45套 psd企业画册模板\11\11.psd");
    if !path.is_file() {
        eprintln!("skipping composite_layers_all_hidden_returns_blank_error; sample missing");
        return;
    }
    let bytes = std::fs::read(path).expect("read");
    let err = composite_layers_from_bytes_with_cancel(&bytes, None, None)
        .expect_err("expected blank under strict visibility");
    assert!(err.is_no_drawable_visible_layers());
    assert_eq!(err.as_str(), STRICT_LAYER_COMPOSITE_BLANK);
}

#[test]
fn composite_with_visibility_forces_hidden_layer_when_mask_says_so() {
    // Both layers are hidden per their on-disk flags, so the default
    // strict-visibility path (`compute_effective_visibility`) would find
    // nothing drawable and return `NoDrawableVisibleLayers`. An explicit
    // `visible` override (as a future Layer Comp / max-bbox reveal pass
    // will supply) must be able to force them on regardless of flags.
    let (width, height) = (2u32, 2u32);
    let specs = [
        TestLayerSpec {
            top: 0,
            left: 0,
            bottom: 2,
            right: 2,
            rgb: (10, 20, 30),
            blend: *b"norm",
            clipping: 0,
            opacity: 255,
        },
        TestLayerSpec {
            top: 0,
            left: 0,
            bottom: 2,
            right: 2,
            rgb: (40, 50, 60),
            blend: *b"norm",
            clipping: 0,
            opacity: 255,
        },
    ];
    let (mut records, channel_data) = build_test_layers(&specs);
    for record in &mut records {
        record.flags = 2; // hidden bit set on every record
    }
    let default_visible = compute_effective_visibility(&records);
    assert!(
        default_visible.iter().all(|v| !v),
        "sanity: default strict visibility must hide every record here"
    );

    let info = mk_layer_info(width, height, records, &channel_data);
    let visible = vec![true, true];

    let composite = composite_layers_with_visibility_from_info(
        &info,
        &visible,
        0.0,
        std::time::Instant::now(),
        None,
        None,
    )
    .expect("explicit visibility override should produce a drawable composite");

    assert_eq!(composite.width, width);
    assert_eq!(composite.height, height);
    // Top (last) opaque layer wins under Normal blend.
    assert_eq!(px(&composite.pixels, width, 0, 0), [40, 50, 60, 255]);
}

#[test]
fn composite_with_visibility_length_mismatch_is_an_error() {
    let (width, height) = (2u32, 2u32);
    let specs = [TestLayerSpec {
        top: 0,
        left: 0,
        bottom: 2,
        right: 2,
        rgb: (1, 2, 3),
        blend: *b"norm",
        clipping: 0,
        opacity: 255,
    }];
    let (records, channel_data) = build_test_layers(&specs);
    let info = mk_layer_info(width, height, records, &channel_data);
    let visible = vec![true, true]; // wrong length: 2 vs 1 record

    let err = composite_layers_with_visibility_from_info(
        &info,
        &visible,
        0.0,
        std::time::Instant::now(),
        None,
        None,
    )
    .expect_err("mismatched visibility length must be rejected");
    assert!(!err.is_no_drawable_visible_layers());
    assert!(err.as_str().contains("visibility"));
}

#[test]
fn psb_8bim_lsct_uses_u32_length() {
    let mut block = Vec::new();
    block.extend_from_slice(b"8BIM");
    block.extend_from_slice(b"lsct");
    block.extend_from_slice(&4u32.to_be_bytes());
    block.extend_from_slice(&2u32.to_be_bytes());
    let mut cursor = std::io::Cursor::new(block.as_slice());

    let scan = scan_extra_tagged_blocks(&mut cursor, block.len() as u64, true).unwrap();

    assert!(scan.is_section_divider);
    assert_eq!(scan.section_type, Some(SECTION_TYPE_CLOSED_FOLDER));
}

#[test]
fn scan_extra_parses_iopa_fill_opacity() {
    let mut block = Vec::new();
    block.extend_from_slice(b"8BIM");
    block.extend_from_slice(b"iOpa");
    block.extend_from_slice(&1u32.to_be_bytes());
    block.push(128);
    block.push(0); // even pad
    let mut cursor = std::io::Cursor::new(block.as_slice());

    let scan = scan_extra_tagged_blocks(&mut cursor, block.len() as u64, false).unwrap();
    assert_eq!(scan.fill_opacity, Some(128));
}

#[test]
fn strict_visibility_empty_mask_honors_default_color() {
    let mut layer = mk_layer(false, false, None);
    layer.bottom = 4;
    layer.right = 4;
    layer.mask = Some(super::LayerMaskInfo {
        top: 0,
        left: 0,
        bottom: 0,
        right: 0,
        default_color: 255,
        disabled: false,
        has_parameters_applied: false,
        density: 255,
        feather: 0.0,
    });
    assert!(strict_visibility_has_drawable_output(
        4,
        4,
        &[layer.clone()],
        &[true]
    ));
    layer.mask.as_mut().unwrap().default_color = 0;
    assert!(!strict_visibility_has_drawable_output(
        4,
        4,
        &[layer],
        &[true]
    ));
}

#[test]
fn scan_lsct_skips_when_payload_truncated() {
    // data_len claims 4 bytes but only 2 remain after the length field.
    let mut block = Vec::new();
    block.extend_from_slice(b"8BIM");
    block.extend_from_slice(b"lsct");
    block.extend_from_slice(&4u32.to_be_bytes());
    block.extend_from_slice(&[0x00, 0x01]); // truncated
    let mut cursor = std::io::Cursor::new(block.as_slice());

    let scan = scan_extra_tagged_blocks(&mut cursor, block.len() as u64, false).unwrap();

    assert!(!scan.is_section_divider);
    assert_eq!(scan.section_type, None);
}

#[test]
fn scan_extra_finds_lsct_after_garbage() {
    let mut block = Vec::new();
    block.extend_from_slice(b"garbage before signature");
    block.extend_from_slice(b"8BIM");
    block.extend_from_slice(b"lsct");
    block.extend_from_slice(&4u32.to_be_bytes());
    block.extend_from_slice(&3u32.to_be_bytes());
    let mut cursor = std::io::Cursor::new(block.as_slice());

    let scan = scan_extra_tagged_blocks(&mut cursor, block.len() as u64, false).unwrap();

    assert!(scan.is_section_divider);
    assert_eq!(scan.section_type, Some(SECTION_TYPE_BOUNDING_DIVIDER));
}

#[test]
fn scan_extra_resync_budget_terminates() {
    let block = vec![0u8; 32 * 1024 * 1024];
    let mut cursor = std::io::Cursor::new(block.as_slice());
    let started = std::time::Instant::now();

    let scan = scan_extra_tagged_blocks(&mut cursor, block.len() as u64, false).unwrap();

    assert!(!scan.is_section_divider);
    assert_eq!(scan.section_type, None);
    assert!(
        started.elapsed() < std::time::Duration::from_millis(500),
        "signature-free scan should finish quickly"
    );
}

#[test]
fn scan_extra_resync_budget_stops_before_late_lsct() {
    let mut block = Vec::new();
    for _ in 0..=super::MAX_TAGGED_BLOCK_RESYNCS_PER_LAYER {
        block.extend_from_slice(b"8BIM");
        block.extend_from_slice(b"junk");
        block.extend_from_slice(&(u32::MAX).to_be_bytes());
    }
    block.extend_from_slice(b"8BIM");
    block.extend_from_slice(b"lsct");
    block.extend_from_slice(&4u32.to_be_bytes());
    block.extend_from_slice(&2u32.to_be_bytes());
    let mut cursor = std::io::Cursor::new(block.as_slice());

    let scan = scan_extra_tagged_blocks(&mut cursor, block.len() as u64, false).unwrap();

    assert!(!scan.is_section_divider);
    assert_eq!(scan.section_type, None);
}

#[test]
fn scan_extra_resync_budget_keeps_existing_lsct() {
    let mut block = Vec::new();
    block.extend_from_slice(b"8BIM");
    block.extend_from_slice(b"lsct");
    block.extend_from_slice(&4u32.to_be_bytes());
    block.extend_from_slice(&1u32.to_be_bytes());
    for _ in 0..=super::MAX_TAGGED_BLOCK_RESYNCS_PER_LAYER {
        block.extend_from_slice(b"8BIM");
        block.extend_from_slice(b"junk");
        block.extend_from_slice(&(u32::MAX).to_be_bytes());
    }
    let mut cursor = std::io::Cursor::new(block.as_slice());

    let scan = scan_extra_tagged_blocks(&mut cursor, block.len() as u64, false).unwrap();

    assert!(scan.is_section_divider);
    assert_eq!(scan.section_type, Some(SECTION_TYPE_OPEN_FOLDER));
}

#[test]
fn parse_layer_records_11_psd_corpus() {
    let path = Path::new(r"F:\BaiduNetdiskDownload\素材库\45套 psd企业画册模板\11\11.psd");
    if !path.is_file() {
        return;
    }
    let bytes = std::fs::read(path).unwrap();
    let layers = parse_layer_records(&bytes).unwrap();
    assert!(layers.records.len() >= 300);
    assert!(
        layers
            .records
            .iter()
            .any(|l| !l.is_empty_bounds() && !l.is_hidden())
    );
    assert!(layers.records.iter().any(|l| l.is_section_divider));
    assert!(!layers.channel_data.is_empty());
}

#[test]
fn streaming_composite_peak_live_layers_at_most_prefetch_window() {
    let (width, height) = (4u32, 4u32);
    let specs = [
        TestLayerSpec {
            top: 0,
            left: 0,
            bottom: 4,
            right: 4,
            rgb: (255, 0, 0),
            blend: *b"norm",
            clipping: 0,
            opacity: 255,
        },
        TestLayerSpec {
            top: 0,
            left: 0,
            bottom: 4,
            right: 4,
            rgb: (0, 255, 0),
            blend: *b"norm",
            clipping: 0,
            opacity: 255,
        },
        TestLayerSpec {
            top: 0,
            left: 0,
            bottom: 4,
            right: 4,
            rgb: (0, 0, 255),
            blend: *b"norm",
            clipping: 0,
            opacity: 255,
        },
    ];
    let (records, channel_data) = build_test_layers(&specs);
    let visible = vec![true; records.len()];
    let info = mk_layer_info(width, height, records, &channel_data);
    let mut canvas = vec![0u8; (width * height * 4) as usize];
    let mut timing = empty_timing();
    let tracker = StreamingPeakTracker::default();

    let composited = run_composite_pass_cpu_streaming(
        &info,
        &visible,
        &mut canvas,
        width,
        height,
        None,
        &mut timing,
        &tracker,
    )
    .expect("stream composite");

    assert_eq!(composited, 3);
    // Top (last) opaque layer wins under Normal blend.
    assert_eq!(px(&canvas, width, 0, 0), [0, 0, 255, 255]);
    let peak = tracker.peak();
    assert!(
        peak <= LAYER_PREFETCH_WINDOW,
        "peak live decoded layers {peak} exceeded window {LAYER_PREFETCH_WINDOW}"
    );
    assert!(peak >= 1, "expected at least one live layer to be observed");
}

#[test]
fn streaming_composite_skips_zero_opacity_and_invisible_layers() {
    let (width, height) = (2u32, 2u32);
    let full_rect = |rgb: (u8, u8, u8), opacity: u8| TestLayerSpec {
        top: 0,
        left: 0,
        bottom: 2,
        right: 2,
        rgb,
        blend: *b"norm",
        clipping: 0,
        opacity,
    };
    let specs = [
        full_rect((10, 20, 30), 255),
        // Zero opacity -- must be skipped, not painted white.
        full_rect((255, 255, 255), 0),
        // Not visible (e.g. hidden layer/ancestor group, as computed
        // upstream by `compute_effective_visibility`) -- also skipped.
        full_rect((0, 255, 0), 255),
    ];
    let (records, channel_data) = build_test_layers(&specs);
    let visible = vec![true, true, false];
    let info = mk_layer_info(width, height, records, &channel_data);
    let mut canvas = vec![0u8; (width * height * 4) as usize];
    let mut timing = empty_timing();
    let tracker = StreamingPeakTracker::default();

    let composited = run_composite_pass_cpu_streaming(
        &info,
        &visible,
        &mut canvas,
        width,
        height,
        None,
        &mut timing,
        &tracker,
    )
    .expect("stream composite");

    assert_eq!(composited, 1, "only the base layer should composite");
    assert_eq!(px(&canvas, width, 0, 0), [10, 20, 30, 255]);
}

#[test]
fn streaming_composite_matches_batch_with_clipping_and_screen_blend() {
    // Bottom red base, a Screen-blended clip on top of it (clipped to the
    // base's silhouette), and an unclipped green base above both. This
    // exercises both blend dispatch and clipping-group handling on the
    // streaming path, and must match the pre-existing batch API exactly.
    let (width, height) = (4u32, 4u32);
    let specs = [
        TestLayerSpec {
            top: 0,
            left: 0,
            bottom: 4,
            right: 4,
            rgb: (200, 0, 0),
            blend: *b"norm",
            clipping: 0,
            opacity: 255,
        },
        TestLayerSpec {
            top: 1,
            left: 1,
            bottom: 3,
            right: 3,
            rgb: (0, 0, 255),
            blend: *b"scrn",
            clipping: 1,
            opacity: 255,
        },
        TestLayerSpec {
            top: 0,
            left: 2,
            bottom: 2,
            right: 4,
            rgb: (0, 128, 0),
            blend: *b"norm",
            clipping: 0,
            opacity: 128,
        },
    ];
    let (records, channel_data) = build_test_layers(&specs);
    let visible = vec![true; records.len()];
    let info = mk_layer_info(width, height, records, &channel_data);

    let mut streamed = vec![0u8; (width * height * 4) as usize];
    let mut timing = empty_timing();
    let tracker = StreamingPeakTracker::default();
    run_composite_pass_cpu_streaming(
        &info,
        &visible,
        &mut streamed,
        width,
        height,
        None,
        &mut timing,
        &tracker,
    )
    .expect("stream composite");

    let decoded = crate::psb_layer_decode::decode_layers_for_composite(&info, &visible, None)
        .expect("decode");
    let clip_refs: Vec<crate::psb_layer_clip::ClipLayerRef<'_>> = decoded
        .iter()
        .map(|l| crate::psb_layer_clip::ClipLayerRef {
            left: l.left,
            top: l.top,
            width: l.width,
            height: l.height,
            blend: l.blend,
            clipping: l.clipping,
            rgba: &l.rgba,
            rgba_arc: Some(&l.rgba),
        })
        .collect();
    let mut batch = vec![0u8; (width * height * 4) as usize];
    crate::psb_layer_clip::blend_layers_with_clipping(&mut batch, width, height, &clip_refs, None)
        .expect("batch blend");

    assert_eq!(streamed, batch);
}

#[test]
fn gpu_batch_eligible_allows_clipping_with_separable_modes() {
    // Canvas must clear gpu_blend_worthwhile; layer rects stay tiny.
    let (width, height) = (512u32, 512u32);
    let channel_data_owned = Vec::new();
    let base_spec = |blend: [u8; 4], clipping: u8| TestLayerSpec {
        top: 0,
        left: 0,
        bottom: 4,
        right: 4,
        rgb: (10, 10, 10),
        blend,
        clipping,
        opacity: 255,
    };

    let (records, _) = build_test_layers(&[base_spec(*b"scrn", 0)]);
    let visible = vec![true; records.len()];
    let info = mk_layer_info(width, height, records, &channel_data_owned);
    assert!(
        gpu_batch_eligible_decoded_bytes(&info, &visible, width, height).is_some(),
        "Screen without clipping should be GPU batch eligible after P0"
    );

    for key in [*b"norm", *b"mul ", *b"lddg"] {
        let (records, _) = build_test_layers(&[base_spec(key, 0)]);
        let visible = vec![true; records.len()];
        let info = mk_layer_info(width, height, records, &channel_data_owned);
        assert!(
            gpu_batch_eligible_decoded_bytes(&info, &visible, width, height).is_some(),
            "separable mode {:?} should be eligible",
            key
        );
    }

    let (clipped_records, _) = build_test_layers(&[base_spec(*b"norm", 0), base_spec(*b"scrn", 1)]);
    let visible3 = vec![true; clipped_records.len()];
    let clipped_info = mk_layer_info(width, height, clipped_records, &channel_data_owned);
    assert!(
        gpu_batch_eligible_decoded_bytes(&clipped_info, &visible3, width, height).is_some(),
        "clipping with separable modes should be GPU batch eligible"
    );
}

#[test]
fn gpu_batch_eligible_rejects_small_canvas_before_decode() {
    let channel_data_owned = Vec::new();
    let (records, _) = build_test_layers(&[TestLayerSpec {
        top: 0,
        left: 0,
        bottom: 64,
        right: 64,
        rgb: (1, 2, 3),
        blend: *b"norm",
        clipping: 0,
        opacity: 255,
    }]);
    let visible = vec![true; records.len()];
    let info = mk_layer_info(64, 64, records, &channel_data_owned);
    assert!(
        gpu_batch_eligible_decoded_bytes(&info, &visible, 64, 64).is_none(),
        "tiny canvas must not enter GPU batch admission"
    );
}

#[test]
fn gpu_batch_peak_vram_counts_clip_scratch_and_rejects_huge_canvas() {
    use crate::psb_layer_decode::gpu_batch_peak_vram_bytes;
    let w = 32_768u32;
    let h = 32_768u32;
    // Canvas + readback alone are 8 GiB; clip scratch pushes over the budget.
    let peak = gpu_batch_peak_vram_bytes(0, w, h, true).expect("peak");
    assert!(peak > super::MAX_COMPOSITE_DECODED_BYTES);

    let channel_data_owned = Vec::new();
    let (records, _) = build_test_layers(&[
        TestLayerSpec {
            top: 0,
            left: 0,
            bottom: 4,
            right: 4,
            rgb: (1, 1, 1),
            blend: *b"norm",
            clipping: 0,
            opacity: 255,
        },
        TestLayerSpec {
            top: 0,
            left: 0,
            bottom: 4,
            right: 4,
            rgb: (2, 2, 2),
            blend: *b"scrn",
            clipping: 1,
            opacity: 255,
        },
    ]);
    let visible = vec![true; records.len()];
    let info = mk_layer_info(w, h, records, &channel_data_owned);
    assert!(
        gpu_batch_eligible_decoded_bytes(&info, &visible, w, h).is_none(),
        "max canvas with clip scratch must fall back to CPU streaming"
    );
}

#[test]
fn layer_will_decode_matches_should_decode_conditions() {
    // `layer_will_decode` trusts the caller-supplied `visible` flag (that
    // is where `is_hidden()`/group visibility is already folded in by
    // `compute_effective_visibility`); it only re-checks the remaining
    // decode-eligibility conditions.
    let mut normal = mk_layer(false, false, None);
    normal.right = 2;
    normal.bottom = 2;
    assert!(layer_will_decode(&normal, true));
    assert!(!layer_will_decode(&normal, false), "not visible");

    let mut zero_opacity = mk_layer(false, false, None);
    zero_opacity.right = 2;
    zero_opacity.bottom = 2;
    zero_opacity.opacity = 0;
    assert!(!layer_will_decode(&zero_opacity, true));

    let divider = mk_layer(false, true, Some(SECTION_TYPE_OPEN_FOLDER));
    assert!(!layer_will_decode(&divider, true));

    let mut oversized = mk_layer(false, false, None);
    oversized.right = crate::psb_reader::PSD_MAX_DIMENSION as i32 + 1;
    oversized.bottom = 1;
    assert!(!layer_will_decode(&oversized, true));
}

// ── apply_mask_density / apply_mask_feather ───────────────────────

#[test]
fn mask_density_255_is_noop() {
    let mut m = vec![0u8, 128, 200, 255];
    super::apply_mask_density(&mut m, 255);
    assert_eq!(m, vec![0u8, 128, 200, 255]);
}

#[test]
fn mask_density_0_clears_all() {
    let mut m = vec![0u8, 128, 200, 255];
    super::apply_mask_density(&mut m, 0);
    assert_eq!(m, vec![0u8, 0, 0, 0]);
}

#[test]
fn mask_density_half_scales() {
    let mut m = vec![0u8, 128, 200, 255];
    super::apply_mask_density(&mut m, 128);
    assert_eq!(m, vec![0, 64, 100, 128]);
}

#[test]
fn mask_feather_noop_when_radius_small() {
    let mut m = vec![0u8, 0, 255, 255].repeat(4);
    super::apply_mask_feather(&mut m, 2, 2, 0.3);
    assert_eq!(m, vec![0u8, 0, 255, 255].repeat(4));
}

#[test]
fn mask_feather_single_row() {
    // 2 rows × 8 cols: feather blurs horizontally. Top row is the gradient.
    let mut m = vec![
        0u8, 0, 0, 255, 255, 255, 255, 255, // row 0: hard step
        0, 0, 0, 255, 255, 255, 255, 255, // row 1: same
    ];
    let orig = m.clone();
    super::apply_mask_feather(&mut m, 8, 2, 4.0);
    assert_ne!(m, orig, "feather should change a 8×2 gradient");
    assert!(m[0] > 0, "left edge should blur inward: got {}", m[0]);
}

#[test]
fn mask_feather_single_column() {
    // 2 cols × 8 rows: feather blurs vertically.
    let mut m = vec![
        0u8, 0, 0, 0, 0, 0, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    ];
    let orig = m.clone();
    super::apply_mask_feather(&mut m, 2, 8, 4.0);
    assert_ne!(m, orig);
    assert!(m[0] > 0, "top edge should blur downward: got {}", m[0]);
}

#[test]
fn mask_density_and_feather_compose() {
    let mut m = vec![0u8, 128, 200, 255].repeat(8);
    super::apply_mask_density(&mut m, 128);
    assert_eq!(m[1], 64);
    super::apply_mask_feather(&mut m, 4, 2, 2.0);
    assert!(
        m[0] != 0 || m[2] != 100,
        "feather should change density-scaled values after density"
    );
}

// ── Large-mask tests that exercise SIMD interior paths ────────────

#[test]
fn mask_feather_large_horizontal_hits_simd() {
    // 32x8 mask with feather=8 => radius=ceil(2.5*4)=10.
    // wp=32 >= 2*10+4=24 => SSE41/AVX2 interior is reachable.
    let mut m = vec![0u8; 8 * 32];
    for col in 16..32 {
        m[col] = 255;
    }
    // Second row same
    for col in 16..32 {
        m[32 + col] = 255;
    }
    for row in 2..8 {
        let off = row * 32;
        for col in 16..32 {
            m[off + col] = 255;
        }
    }
    super::apply_mask_feather(&mut m, 32, 8, 8.0);
    // Leftmost pixel should have blurred inward from the hard edge at col 16.
    assert!(m[0] == 0, "far left should stay 0: got {}", m[0]);
    // Pixels near the edge should be between 0 and 255.
    assert!(
        m[14] > 0 && m[14] < 255,
        "edge pixel should be partial: got {}",
        m[14]
    );
    assert!(
        m[16] > 0 && m[16] < 255,
        "edge pixel should be partial: got {}",
        m[16]
    );
    // Center of the white area should be 255.
    assert!(m[24] >= 250, "far right should be near 255: got {}", m[24]);
}

#[test]
fn mask_feather_large_vertical_hits_simd() {
    // 8x32 mask: vertical test for SIMD path (row-major).
    let mut m = vec![0u8; 32 * 8];
    for row in 16..32 {
        let off = row * 8;
        for col in 0..8 {
            m[off + col] = 255;
        }
    }
    super::apply_mask_feather(&mut m, 8, 32, 8.0);
    // Top rows should be 0, bottom rows 255, edge rows partial.
    assert!(m[0] == 0, "top should stay 0: got {}", m[0]);
    let edge_idx = 14 * 8;
    assert!(
        m[edge_idx] > 0 && m[edge_idx] < 255,
        "vertical edge pixel should be partial: got {}",
        m[edge_idx]
    );
    let far_idx = 28 * 8;
    assert!(
        m[far_idx] == 255,
        "bottom should be 255: got {}",
        m[far_idx]
    );
}
