use super::*;

fn conformance_cmyk_layers_sdr_fallback_matches_ref_png_when_sample_present() {
    use crate::hdr::types::HdrToneMapSettings;
    let jxl_path = std::path::Path::new(r"F:\HDR\conformance\testcases\cmyk_layers\input.jxl");
    let ref_path = std::path::Path::new(r"F:\HDR\conformance\testcases\cmyk_layers\ref.png");
    if !jxl_path.is_file() || !ref_path.is_file() {
        return;
    }
    let bytes = std::fs::read(jxl_path).expect("read cmyk_layers/input.jxl");
    let tone = HdrToneMapSettings::default();
    let img = crate::loader::apply_exif_orientation_to_image_data(
        jxl_path,
        super::decode_jxl_bytes_to_image_data(
            &bytes,
            tone.target_hdr_capacity(),
            tone.target_hdr_capacity(),
            tone,
        )
        .expect("decode cmyk_layers"),
    );
    let crate::loader::ImageData::Hdr { fallback, hdr, .. } = img else {
        panic!("expected ImageData::Hdr");
    };
    let jxl_bytes = fallback.rgba().to_vec();
    let ref_img = image::open(ref_path).expect("decode ref.png").to_rgba8();
    assert_eq!(
        (hdr.width, hdr.height),
        (ref_img.width(), ref_img.height()),
        "ref.png dimensions must match cmyk_layers JXL"
    );
    let ref_bytes = ref_img.into_raw();
    assert_eq!(jxl_bytes.len(), ref_bytes.len());
    let n = (jxl_bytes.len() / 4) as i64;
    let (mut diff_r, mut diff_g, mut diff_b) = (0_i64, 0_i64, 0_i64);
    let (mut max_r, mut max_g, mut max_b) = (0_u32, 0_u32, 0_u32);
    for (j, r) in jxl_bytes.chunks_exact(4).zip(ref_bytes.chunks_exact(4)) {
        diff_r += i64::from(j[0]) - i64::from(r[0]);
        diff_g += i64::from(j[1]) - i64::from(r[1]);
        diff_b += i64::from(j[2]) - i64::from(r[2]);
        max_r = max_r.max((j[0] as i32 - r[0] as i32).unsigned_abs());
        max_g = max_g.max((j[1] as i32 - r[1] as i32).unsigned_abs());
        max_b = max_b.max((j[2] as i32 - r[2] as i32).unsigned_abs());
    }
    let bias_r = diff_r as f64 / n as f64;
    let bias_g = diff_g as f64 / n as f64;
    let bias_b = diff_b as f64 / n as f64;
    eprintln!(
        "cmyk_layers fallback vs ref.png:\n  mean signed diff = ({bias_r:+.2}, {bias_g:+.2}, {bias_b:+.2})\n  max abs diff = ({max_r}, {max_g}, {max_b})"
    );
    assert!(
        bias_r.abs() < 5.0 && bias_g.abs() < 5.0 && bias_b.abs() < 5.0,
        "lcms2 CMYK→sRGB SDR fallback bias too large: ({bias_r:+.2}, {bias_g:+.2}, {bias_b:+.2}) — \
         check JxlDecoderSetExtraChannelBuffer wiring + jxl_decoder_copy_target_original_icc + \
         apply_cmyk_to_srgb_via_lcms (libjxl CMYK convention 0=max ink, lcms2 0=no ink in 0..100)"
    );
}

#[cfg(feature = "jpegxl")]
#[test]
fn jxl_gain_map_bundle_rejects_malformed_payload() {
    let err = read_jxl_gain_map_bundle(&[0, 0, 1, 0]).expect_err("reject malformed jhgm");

    assert!(err.contains("jhgm"));
}

#[cfg(feature = "jpegxl")]
#[test]
fn jxl_gain_map_bundle_parses_metadata_and_embedded_codestream() {
    let metadata = [1_u8, 2, 3];
    let gain_map = [0xff_u8, 0x0a, 0x55];
    let mut bundle = Vec::new();
    bundle.push(0);
    bundle.extend_from_slice(&(metadata.len() as u16).to_be_bytes());
    bundle.extend_from_slice(&metadata);
    bundle.push(0); // no compressed color encoding
    bundle.extend_from_slice(&0_u32.to_be_bytes()); // no compressed ICC
    bundle.extend_from_slice(&gain_map);

    let parsed = read_jxl_gain_map_bundle(&bundle).expect("parse jhgm");

    assert_eq!(parsed.version, 0);
    assert_eq!(parsed.metadata, metadata);
    assert_eq!(parsed.gain_map, gain_map);
}

/// Conformance regression: `patches/input.jxl` ships TF=Linear in the
/// codestream (libjxl emits truly linear floats). Before fixing the SDR-
/// grade fallback to honor the actual TF reported by
/// `JxlDecoderGetColorAsEncodedProfile`, our renderer treated those linear
/// floats as if they were sRGB-encoded and quantized them directly,
/// producing a uniformly ~22-code darker image (mean signed diff
/// (-19.76, -22.22, -25.93), max diff 75) and effectively losing the gray
/// table grid lines that should be visible. After the fix every pixel
/// matches `ref.png` to within ≤3 codes.
#[cfg(feature = "jpegxl")]
#[test]
fn conformance_patches_sdr_fallback_matches_ref_png_when_sample_present() {
    use crate::hdr::types::HdrToneMapSettings;
    let jxl_path = std::path::Path::new(r"F:\HDR\conformance\testcases\patches\input.jxl");
    let ref_path = std::path::Path::new(r"F:\HDR\conformance\testcases\patches\ref.png");
    if !jxl_path.is_file() || !ref_path.is_file() {
        return;
    }
    let bytes = std::fs::read(jxl_path).expect("read jxl");
    let img = crate::loader::apply_exif_orientation_to_image_data(
        jxl_path,
        super::decode_jxl_bytes_to_image_data(
            &bytes,
            HdrToneMapSettings::default().target_hdr_capacity(),
            HdrToneMapSettings::default().target_hdr_capacity(),
            HdrToneMapSettings::default(),
        )
        .expect("decode jxl"),
    );
    let crate::loader::ImageData::Hdr { fallback, hdr } = img else {
        panic!("unexpected ImageData variant");
    };
    assert_eq!(
        hdr.metadata.transfer_function,
        HdrTransferFunction::Linear,
        "patches.jxl ships TF=Linear in the codestream — read_jxl_metadata must surface that"
    );
    let ours = fallback.rgba();
    let ref_img = image::open(ref_path).expect("decode ref.png").to_rgba8();
    assert_eq!((hdr.width, hdr.height), (ref_img.width(), ref_img.height()));
    let r = ref_img.into_raw();
    let n = (ours.len() / 4) as i64;
    let (mut sr, mut sg, mut sb) = (0_i64, 0_i64, 0_i64);
    let (mut mr, mut mg, mut mb) = (0_u32, 0_u32, 0_u32);
    for (j, p) in ours.chunks_exact(4).zip(r.chunks_exact(4)) {
        let dr = j[0] as i32 - p[0] as i32;
        let dg = j[1] as i32 - p[1] as i32;
        let db = j[2] as i32 - p[2] as i32;
        sr += dr as i64;
        sg += dg as i64;
        sb += db as i64;
        mr = mr.max(dr.unsigned_abs());
        mg = mg.max(dg.unsigned_abs());
        mb = mb.max(db.unsigned_abs());
    }
    let bias = (
        sr as f64 / n as f64,
        sg as f64 / n as f64,
        sb as f64 / n as f64,
    );
    assert!(
        bias.0.abs() < 2.0 && bias.1.abs() < 2.0 && bias.2.abs() < 2.0,
        "mean signed diff vs ref.png must stay within ±2 codes (was -19.76 / -22.22 / -25.93 \
         before treating TF=Linear codestream as truly linear); got {bias:?}"
    );
    assert!(
        mr <= 5 && mg <= 5 && mb <= 5,
        "max abs diff vs ref.png must stay within 5 codes (was 75 / 74 / 73 before the fix); \
         got ({mr}, {mg}, {mb})"
    );
}

/// Conformance regression: `patches_lossless/input.jxl`. Distinct from
/// `patches/input.jxl` in that the lossless variant ships a 2924-byte
/// **embedded ICC profile** (Display P3 primaries with a *linear* `rTRC`)
/// that libjxl can't represent as a `JxlColorEncoding` enum
/// (`JxlDecoderGetColorAsEncodedProfile` returns `JXL_DEC_ERROR` here).
/// libjxl emits truly linear floats per the codestream, so the SDR-grade
/// fallback must apply the sRGB OETF before quantizing. Before parsing
/// `rTRC`, we'd guess "non-sRGB primaries → PQ" for any non-sRGB ICC and
/// route the linear floats through the HDR tone-mapping pipeline, which
/// produced a uniformly ~16-code darker image (mean diff
/// (-14.26, -16.16, -19.21), max 51) and washed out the gray table grid
/// lines plus cell background grays.
#[cfg(feature = "jpegxl")]
#[test]
fn conformance_patches_lossless_sdr_fallback_matches_ref_png_when_sample_present() {
    use crate::hdr::types::HdrToneMapSettings;
    let jxl_path = std::path::Path::new(r"F:\HDR\conformance\testcases\patches_lossless\input.jxl");
    let ref_path = std::path::Path::new(r"F:\HDR\conformance\testcases\patches_lossless\ref.png");
    if !jxl_path.is_file() || !ref_path.is_file() {
        return;
    }
    let bytes = std::fs::read(jxl_path).expect("read jxl");
    let img = crate::loader::apply_exif_orientation_to_image_data(
        jxl_path,
        super::decode_jxl_bytes_to_image_data(
            &bytes,
            HdrToneMapSettings::default().target_hdr_capacity(),
            HdrToneMapSettings::default().target_hdr_capacity(),
            HdrToneMapSettings::default(),
        )
        .expect("decode jxl"),
    );
    let crate::loader::ImageData::Hdr { fallback, hdr } = img else {
        panic!("unexpected ImageData variant");
    };
    // The metadata TF is whatever the rTRC parser decides — it can be
    // `Srgb` (piecewise / parametric / LUT curve) or `Linear` (count=0 or
    // count=1@1.0). Either way the SDR-grade fallback must produce bytes
    // that match ref.png; the previous bug was routing through the HDR
    // tone-mapping pipeline (because the old code guessed PQ for any non-
    // sRGB primary ICC).
    assert!(
        !matches!(
            hdr.metadata.transfer_function,
            HdrTransferFunction::Pq | HdrTransferFunction::Hlg
        ),
        "patches_lossless is SDR — must not route through the HDR pipeline; \
         got transfer_function={:?}",
        hdr.metadata.transfer_function
    );
    let ours = fallback.rgba();
    let ref_img = image::open(ref_path).expect("decode ref.png").to_rgba8();
    assert_eq!((hdr.width, hdr.height), (ref_img.width(), ref_img.height()));
    let r = ref_img.into_raw();
    let n = (ours.len() / 4) as i64;
    let (mut sr, mut sg, mut sb) = (0_i64, 0_i64, 0_i64);
    let (mut mr, mut mg, mut mb) = (0_u32, 0_u32, 0_u32);
    for (j, p) in ours.chunks_exact(4).zip(r.chunks_exact(4)) {
        let dr = j[0] as i32 - p[0] as i32;
        let dg = j[1] as i32 - p[1] as i32;
        let db = j[2] as i32 - p[2] as i32;
        sr += dr as i64;
        sg += dg as i64;
        sb += db as i64;
        mr = mr.max(dr.unsigned_abs());
        mg = mg.max(dg.unsigned_abs());
        mb = mb.max(db.unsigned_abs());
    }
    let bias = (
        sr as f64 / n as f64,
        sg as f64 / n as f64,
        sb as f64 / n as f64,
    );
    assert!(
        bias.0.abs() < 2.0 && bias.1.abs() < 2.0 && bias.2.abs() < 2.0,
        "mean signed diff vs ref.png must stay within ±2 codes (was -14.26 / -16.16 / -19.21 \
         before parsing rTRC); got {bias:?}"
    );
    assert!(
        mr <= 5 && mg <= 5 && mb <= 5,
        "max abs diff vs ref.png must stay within 5 codes (was 51 / 49 / 49 before the fix); \
         got ({mr}, {mg}, {mb})"
    );
}

/// Unit coverage for the ICC `rTRC` classifier: synthetic `curveType`
/// payloads exercise the linear (count=0, count=1@1.0), gamma
/// (count=1@2.2), and LUT (count>=2) branches.
#[cfg(feature = "jpegxl")]
#[test]
fn icc_trc_kind_classifies_linear_gamma_and_lut() {
    // Build a minimal MOCK_ICC_PROFILE_SIZE-byte ICC profile with a single rTRC tag at a
    // known offset. We don't need a valid header — `icc_find_tag_element_offset`
    // only reads the tag-table at offset 128.
    fn make(count: u32, payload_after_count: &[u8]) -> Vec<u8> {
        let trc_offset = 256_u32;
        let mut icc = vec![0u8; crate::constants::MOCK_ICC_PROFILE_SIZE];
        icc[128..132].copy_from_slice(&1_u32.to_be_bytes()); // tag_count
        icc[132..136].copy_from_slice(b"rTRC");
        icc[136..140].copy_from_slice(&trc_offset.to_be_bytes());
        // size (unused by the parser but spec-correct):
        let size = (12 + payload_after_count.len()) as u32;
        icc[140..144].copy_from_slice(&size.to_be_bytes());
        let off = trc_offset as usize;
        icc[off..off + 4].copy_from_slice(b"curv");
        icc[off + 4..off + 8].fill(0); // reserved
        icc[off + 8..off + 12].copy_from_slice(&count.to_be_bytes());
        icc[off + 12..off + 12 + payload_after_count.len()].copy_from_slice(payload_after_count);
        icc
    }

    let linear_zero = make(0, &[]);
    assert_eq!(
        super::icc_trc_kind(&linear_zero),
        Some(HdrTransferFunction::Linear)
    );

    let linear_one = make(1, &[0x01, 0x00]); // u8.8 = 1.0
    assert_eq!(
        super::icc_trc_kind(&linear_one),
        Some(HdrTransferFunction::Linear)
    );

    let gamma_22 = make(1, &[0x02, 0x33]); // u8.8 ≈ 2.2
    assert_eq!(
        super::icc_trc_kind(&gamma_22),
        Some(HdrTransferFunction::Gamma)
    );

    let lut = make(1024, &[0u8; 2048]);
    assert_eq!(super::icc_trc_kind(&lut), Some(HdrTransferFunction::Srgb));
}

/// Conformance regression: `blendmodes/input.jxl` (12-bit Modular, multiple
/// blend modes, TF=sRGB codestream). The float buffer libjxl gives us is
/// already sRGB-encoded; the SDR-grade fallback must direct-quantize
/// (`value * 255`) without re-applying the OETF. The blend-mode arithmetic
/// libjxl uses for partially-transparent / HDR-alpha (>1.0) pixels can
/// disagree with the reference compositor by up to ~90 codes on the
/// diagonal-stripe regions, so we lock the global statistics rather than
/// pixel-exact equality. Any future regression that accidentally re-applies
/// the OETF or routes through the HDR pipeline will blow these bounds.
#[cfg(feature = "jpegxl")]
#[test]
fn conformance_blendmodes_sdr_fallback_close_to_ref_png_when_sample_present() {
    use crate::hdr::types::HdrToneMapSettings;
    let jxl_path = std::path::Path::new(r"F:\HDR\conformance\testcases\blendmodes\input.jxl");
    let ref_path = std::path::Path::new(r"F:\HDR\conformance\testcases\blendmodes\ref.png");
    if !jxl_path.is_file() || !ref_path.is_file() {
        return;
    }
    let bytes = std::fs::read(jxl_path).expect("read jxl");
    let img = crate::loader::apply_exif_orientation_to_image_data(
        jxl_path,
        super::decode_jxl_bytes_to_image_data(
            &bytes,
            HdrToneMapSettings::default().target_hdr_capacity(),
            HdrToneMapSettings::default().target_hdr_capacity(),
            HdrToneMapSettings::default(),
        )
        .expect("decode jxl"),
    );
    let crate::loader::ImageData::Hdr { fallback, hdr } = img else {
        panic!("unexpected ImageData variant");
    };
    assert_eq!(
        hdr.metadata.transfer_function,
        HdrTransferFunction::Srgb,
        "blendmodes.jxl ships TF=sRGB in the codestream — read_jxl_metadata must surface \
         that so the SDR-grade fallback direct-quantizes without re-applying the OETF"
    );
    let ours = fallback.rgba();
    let ref_img = image::open(ref_path).expect("decode ref.png").to_rgba8();
    assert_eq!((hdr.width, hdr.height), (ref_img.width(), ref_img.height()));
    let r = ref_img.into_raw();
    let total = (ours.len() / 4) as f64;
    let (mut sr, mut sg, mut sb) = (0_i64, 0_i64, 0_i64);
    let mut exact = 0_u32;
    let mut close_15 = 0_u32;
    for (j, p) in ours.chunks_exact(4).zip(r.chunks_exact(4)) {
        let dr = j[0] as i32 - p[0] as i32;
        let dg = j[1] as i32 - p[1] as i32;
        let db = j[2] as i32 - p[2] as i32;
        sr += dr as i64;
        sg += dg as i64;
        sb += db as i64;
        let m = dr
            .unsigned_abs()
            .max(dg.unsigned_abs())
            .max(db.unsigned_abs());
        if m == 0 {
            exact += 1;
        }
        if m <= 15 {
            close_15 += 1;
        }
    }
    let bias = (sr as f64 / total, sg as f64 / total, sb as f64 / total);
    assert!(
        bias.0.abs() < 5.0 && bias.1.abs() < 5.0 && bias.2.abs() < 5.0,
        "global mean RGB bias vs ref.png must stay within ±5 codes (we currently sit at \
         ~+1.55, -2.76, -0.75); got {bias:?}"
    );
    let exact_pct = exact as f64 / total;
    let close_15_pct = close_15 as f64 / total;
    assert!(
        exact_pct >= 0.30,
        "Icc core tests: ≥30% of pixels must match ref.png exactly (currently ~37%); got {:.1}%",
        exact_pct * 100.0
    );
    assert!(
        close_15_pct >= 0.55,
        "≥55% of pixels must be within 15 codes of ref.png; got {:.1}%",
        close_15_pct * 100.0
    );
}

/// Diagnostic harness for investigating SDR-fallback regressions on JXL
/// conformance files. For a given (jxl, ref_png) pair: dumps `JxlBasicInfo`
/// + extra-channel info, runs the live decode pipeline, and reports per-
/// channel bias / max-abs / pixel histogram of differences vs ref.png so
/// we can localize where rendering goes wrong (gamma, primaries, blend
/// modes, patches, ...). Skipped silently when the conformance corpus is
/// not present on the host.
#[cfg(feature = "jpegxl")]
fn diagnose_conformance_pair(name: &str, jxl_path: &std::path::Path, ref_path: &std::path::Path) {
    use crate::hdr::types::HdrToneMapSettings;
    if !jxl_path.is_file() || !ref_path.is_file() {
        eprintln!("[{name}] skipped — conformance file not present");
        return;
    }
    let bytes = std::fs::read(jxl_path).expect("read jxl");
    unsafe {
        let decoder = libjxl_sys::JxlDecoderCreate(std::ptr::null());
        assert!(!decoder.is_null());
        let subscribed = libjxl_sys::JXL_DEC_BASIC_INFO | libjxl_sys::JXL_DEC_COLOR_ENCODING;
        libjxl_sys::JxlDecoderSubscribeEvents(decoder, subscribed as i32);
        libjxl_sys::JxlDecoderSetInput(decoder, bytes.as_ptr(), bytes.len());
        libjxl_sys::JxlDecoderCloseInput(decoder);
        let mut info: libjxl_sys::JxlBasicInfo = std::mem::zeroed();
        loop {
            let st = libjxl_sys::JxlDecoderProcessInput(decoder);
            if st == libjxl_sys::JXL_DEC_BASIC_INFO {
                libjxl_sys::JxlDecoderGetBasicInfo(decoder, &mut info);
            } else if st == libjxl_sys::JXL_DEC_COLOR_ENCODING {
                eprintln!(
                    "[{name}] dims={}x{} bits={} float={} num_color={} num_extra={} have_anim={} intensity_target={} min_nits={} alpha_bits={}",
                    info.xsize,
                    info.ysize,
                    info.bits_per_sample,
                    info.exponent_bits_per_sample,
                    info.num_color_channels,
                    info.num_extra_channels,
                    info.have_animation,
                    info.intensity_target,
                    info.min_nits,
                    info.alpha_bits,
                );
                for i in 0..info.num_extra_channels {
                    let mut ec = std::mem::MaybeUninit::<libjxl_sys::JxlExtraChannelInfo>::zeroed();
                    if libjxl_sys::JxlDecoderGetExtraChannelInfo(
                        decoder.cast_const(),
                        i as usize,
                        ec.as_mut_ptr(),
                    ) == libjxl_sys::JXL_DEC_SUCCESS
                    {
                        let ec = ec.assume_init();
                        eprintln!(
                            "[{name}]   extra channel #{i}: type={} bits={}",
                            ec.type_, ec.bits_per_sample,
                        );
                    }
                }
                let mut orig_size = 0_usize;
                libjxl_sys::JxlDecoderGetICCProfileSize(
                    decoder.cast_const(),
                    libjxl_sys::JXL_COLOR_PROFILE_TARGET_ORIGINAL,
                    &mut orig_size,
                );
                let mut data_size = 0_usize;
                libjxl_sys::JxlDecoderGetICCProfileSize(
                    decoder.cast_const(),
                    libjxl_sys::JXL_COLOR_PROFILE_TARGET_DATA,
                    &mut data_size,
                );
                let mut enc: libjxl_sys::JxlColorEncoding = std::mem::zeroed();
                let enc_st = libjxl_sys::JxlDecoderGetColorAsEncodedProfile(
                    decoder.cast_const(),
                    libjxl_sys::JXL_COLOR_PROFILE_TARGET_DATA,
                    &mut enc,
                );
                eprintln!(
                    "[{name}] icc_orig={} icc_data={} encoded_st={} (cs={} wp={} prim={} tf={} ri={})",
                    orig_size,
                    data_size,
                    enc_st,
                    enc.color_space,
                    enc.white_point,
                    enc.primaries,
                    enc.transfer_function,
                    enc.rendering_intent
                );
                break;
            } else if st == libjxl_sys::JXL_DEC_ERROR || st == libjxl_sys::JXL_DEC_NEED_MORE_INPUT {
                break;
            }
        }
        libjxl_sys::JxlDecoderDestroy(decoder);
    }

    let tone = HdrToneMapSettings::default();
    let img = match super::decode_jxl_bytes_to_image_data(
        &bytes,
        tone.target_hdr_capacity(),
        tone.target_hdr_capacity(),
        tone,
    ) {
        Ok(img) => crate::loader::apply_exif_orientation_to_image_data(jxl_path, img),
        Err(e) => {
            eprintln!("[{name}] decode failed: {e}");
            return;
        }
    };
    let crate::loader::ImageData::Hdr { fallback, hdr, .. } = img else {
        eprintln!("[{name}] unexpected ImageData variant");
        return;
    };
    let jxl_bytes = fallback.rgba().to_vec();
    let ref_img = image::open(ref_path).expect("decode ref.png").to_rgba8();
    if (hdr.width, hdr.height) != (ref_img.width(), ref_img.height()) {
        eprintln!(
            "[{name}] dim mismatch jxl={}x{} ref={}x{}",
            hdr.width,
            hdr.height,
            ref_img.width(),
            ref_img.height()
        );
        return;
    }
    let ref_bytes = ref_img.into_raw();
    let n = (jxl_bytes.len() / 4) as i64;
    let (mut dr, mut dg, mut db, mut da) = (0_i64, 0_i64, 0_i64, 0_i64);
    let (mut mr, mut mg, mut mb, mut ma) = (0_u32, 0_u32, 0_u32, 0_u32);
    let mut buckets = [0_u32; 8]; // 0,1-3,4-7,8-15,16-31,32-63,64-127,128+
    for (j, r) in jxl_bytes.chunks_exact(4).zip(ref_bytes.chunks_exact(4)) {
        let cr = j[0] as i32 - r[0] as i32;
        let cg = j[1] as i32 - r[1] as i32;
        let cb = j[2] as i32 - r[2] as i32;
        let ca = j[3] as i32 - r[3] as i32;
        dr += cr as i64;
        dg += cg as i64;
        db += cb as i64;
        da += ca as i64;
        mr = mr.max(cr.unsigned_abs());
        mg = mg.max(cg.unsigned_abs());
        mb = mb.max(cb.unsigned_abs());
        ma = ma.max(ca.unsigned_abs());
        let max_abs = cr
            .unsigned_abs()
            .max(cg.unsigned_abs())
            .max(cb.unsigned_abs());
        let bin = match max_abs {
            0 => 0,
            1..=3 => 1,
            4..=7 => 2,
            8..=15 => 3,
            16..=31 => 4,
            32..=63 => 5,
            64..=127 => 6,
            _ => 7,
        };
        buckets[bin] += 1;
    }
    eprintln!(
        "[{name}] vs ref.png: bias=({:+.2},{:+.2},{:+.2},a:{:+.2}) max=({},{},{},a:{}) hist[==,1-3,4-7,8-15,16-31,32-63,64-127,>=128]={:?}",
        dr as f64 / n as f64,
        dg as f64 / n as f64,
        db as f64 / n as f64,
        da as f64 / n as f64,
        mr,
        mg,
        mb,
        ma,
        buckets,
    );
}

#[cfg(feature = "jpegxl")]
#[test]
