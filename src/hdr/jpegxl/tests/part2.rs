use super::*;

fn conformance_bench_oriented_brg_sdr_fallback_matches_ref_png_when_sample_present() {
    use crate::hdr::types::HdrToneMapSettings;
    let jxl_path =
        std::path::Path::new(r"F:\HDR\conformance\testcases\bench_oriented_brg\input.jxl");
    let ref_path = std::path::Path::new(r"F:\HDR\conformance\testcases\bench_oriented_brg\ref.png");
    if !jxl_path.is_file() || !ref_path.is_file() {
        return;
    }
    let bytes = std::fs::read(jxl_path).expect("read conformance jxl");
    let tone = HdrToneMapSettings::default();
    let img = crate::loader::apply_exif_orientation_to_image_data(
        jxl_path,
        super::decode_jxl_bytes_to_image_data(
            &bytes,
            tone.target_hdr_capacity(),
            tone.target_hdr_capacity(),
            tone,
        )
        .expect("decode jxl"),
    );
    let crate::loader::ImageData::Hdr { fallback, hdr, .. } = img else {
        panic!("expected ImageData::Hdr");
    };
    let jxl_w = hdr.width as usize;
    let jxl_h = hdr.height as usize;
    let jxl_bytes = fallback.rgba().to_vec();

    let ref_img = image::open(ref_path).expect("decode ref.png").to_rgba8();
    let ref_w = ref_img.width() as usize;
    let ref_h = ref_img.height() as usize;
    assert_eq!(
        (jxl_w, jxl_h),
        (ref_w, ref_h),
        "ref.png dimensions {ref_w}×{ref_h} must match JXL fallback {jxl_w}×{jxl_h}"
    );
    let ref_bytes = ref_img.into_raw();
    assert_eq!(jxl_bytes.len(), ref_bytes.len());

    let n_pixels = (ref_bytes.len() / 4) as u64;
    let (mut sum_jxl_r, mut sum_jxl_g, mut sum_jxl_b) = (0_u64, 0_u64, 0_u64);
    let (mut sum_ref_r, mut sum_ref_g, mut sum_ref_b) = (0_u64, 0_u64, 0_u64);
    let (mut diff_r, mut diff_g, mut diff_b) = (0_i64, 0_i64, 0_i64);
    let (mut max_diff_r, mut max_diff_g, mut max_diff_b) = (0_u32, 0_u32, 0_u32);
    for (j, r) in jxl_bytes.chunks_exact(4).zip(ref_bytes.chunks_exact(4)) {
        sum_jxl_r += u64::from(j[0]);
        sum_jxl_g += u64::from(j[1]);
        sum_jxl_b += u64::from(j[2]);
        sum_ref_r += u64::from(r[0]);
        sum_ref_g += u64::from(r[1]);
        sum_ref_b += u64::from(r[2]);
        diff_r += i64::from(j[0]) - i64::from(r[0]);
        diff_g += i64::from(j[1]) - i64::from(r[1]);
        diff_b += i64::from(j[2]) - i64::from(r[2]);
        max_diff_r = max_diff_r.max((j[0] as i32 - r[0] as i32).unsigned_abs());
        max_diff_g = max_diff_g.max((j[1] as i32 - r[1] as i32).unsigned_abs());
        max_diff_b = max_diff_b.max((j[2] as i32 - r[2] as i32).unsigned_abs());
    }
    let avg_jxl_r = sum_jxl_r / n_pixels;
    let avg_jxl_g = sum_jxl_g / n_pixels;
    let avg_jxl_b = sum_jxl_b / n_pixels;
    let avg_ref_r = sum_ref_r / n_pixels;
    let avg_ref_g = sum_ref_g / n_pixels;
    let avg_ref_b = sum_ref_b / n_pixels;
    let bias_r = diff_r as f64 / n_pixels as f64;
    let bias_g = diff_g as f64 / n_pixels as f64;
    let bias_b = diff_b as f64 / n_pixels as f64;
    eprintln!(
        "bench_oriented_brg fallback vs ref.png:\n  \
         JXL avg RGB = ({avg_jxl_r}, {avg_jxl_g}, {avg_jxl_b})\n  \
         REF avg RGB = ({avg_ref_r}, {avg_ref_g}, {avg_ref_b})\n  \
         mean signed diff (jxl-ref) = ({bias_r:+.2}, {bias_g:+.2}, {bias_b:+.2})\n  \
         max abs diff = ({max_diff_r}, {max_diff_g}, {max_diff_b})"
    );
    // Tight: the conformance ref is the canonical libjxl decode; if our pipeline drifts more
    // than ~5 code values on average, it's a real bug (and the user sees washing on screen).
    assert!(
        bias_r.abs() < 5.0 && bias_g.abs() < 5.0 && bias_b.abs() < 5.0,
        "SDR fallback drifts from ref.png — bias=({bias_r:+.2}, {bias_g:+.2}, {bias_b:+.2}); \
         check linear/sRGB transfer handling and intensity_target scaling"
    );
}

/// Diagnostic: dump basic info + color encoding + extra channels for `cmyk_layers/input.jxl`
/// so we can see how libjxl describes the source. Symptom: rendered image is missing the
/// "black" word and shifts greens (lime instead of teal) compared to `ref.png`. Hypothesis:
/// the source is CMYK (3 color channels + black extra channel) and we drop the K plane when
/// requesting `JXL_TYPE_FLOAT` RGBA output.
#[cfg(feature = "jpegxl")]
#[test]
fn conformance_cmyk_layers_basic_info_and_channels_when_sample_present() {
    let path = std::path::Path::new(r"F:\HDR\conformance\testcases\cmyk_layers\input.jxl");
    if !path.is_file() {
        return;
    }
    let bytes = std::fs::read(path).expect("read cmyk_layers/input.jxl");

    unsafe {
        let decoder = libjxl_sys::JxlDecoderCreate(std::ptr::null());
        assert!(!decoder.is_null());
        let subscribed = libjxl_sys::JXL_DEC_BASIC_INFO
            | libjxl_sys::JXL_DEC_COLOR_ENCODING
            | libjxl_sys::JXL_DEC_FRAME;
        assert_eq!(
            libjxl_sys::JxlDecoderSubscribeEvents(decoder, subscribed as i32),
            libjxl_sys::JXL_DEC_SUCCESS
        );
        assert_eq!(
            libjxl_sys::JxlDecoderSetInput(decoder, bytes.as_ptr(), bytes.len()),
            libjxl_sys::JXL_DEC_SUCCESS
        );
        libjxl_sys::JxlDecoderCloseInput(decoder);

        let mut info: libjxl_sys::JxlBasicInfo = std::mem::zeroed();
        let mut got_basic = false;
        loop {
            let st = libjxl_sys::JxlDecoderProcessInput(decoder);
            if st == libjxl_sys::JXL_DEC_BASIC_INFO {
                assert_eq!(
                    libjxl_sys::JxlDecoderGetBasicInfo(decoder, &mut info),
                    libjxl_sys::JXL_DEC_SUCCESS
                );
                got_basic = true;
            } else if st == libjxl_sys::JXL_DEC_COLOR_ENCODING {
                let mut color: libjxl_sys::JxlColorEncoding = std::mem::zeroed();
                let cs = libjxl_sys::JxlDecoderGetColorAsEncodedProfile(
                    decoder.cast_const(),
                    libjxl_sys::JXL_COLOR_PROFILE_TARGET_ORIGINAL,
                    &mut color,
                );
                eprintln!(
                    "TARGET_ORIGINAL color: status={cs} color_space={} primaries={} transfer={} rendering_intent={}",
                    color.color_space,
                    color.primaries,
                    color.transfer_function,
                    color.rendering_intent
                );
                let mut color_data: libjxl_sys::JxlColorEncoding = std::mem::zeroed();
                let ds = libjxl_sys::JxlDecoderGetColorAsEncodedProfile(
                    decoder.cast_const(),
                    libjxl_sys::JXL_COLOR_PROFILE_TARGET_DATA,
                    &mut color_data,
                );
                eprintln!(
                    "TARGET_DATA color: status={ds} color_space={} primaries={} transfer={}",
                    color_data.color_space, color_data.primaries, color_data.transfer_function
                );
                break;
            } else if st == libjxl_sys::JXL_DEC_ERROR || st == libjxl_sys::JXL_DEC_NEED_MORE_INPUT {
                panic!("libjxl process error/need-more-input: {st}");
            }
        }
        assert!(got_basic);
        eprintln!(
            "BasicInfo: xsize={} ysize={} bits_per_sample={} num_color_channels={} num_extra_channels={} alpha_bits={} have_animation={} intensity_target={}",
            info.xsize,
            info.ysize,
            info.bits_per_sample,
            info.num_color_channels,
            info.num_extra_channels,
            info.alpha_bits,
            info.have_animation,
            info.intensity_target
        );
        for i in 0..info.num_extra_channels {
            let mut ec: libjxl_sys::JxlExtraChannelInfo = std::mem::zeroed();
            let st = libjxl_sys::JxlDecoderGetExtraChannelInfo(
                decoder.cast_const(),
                i as usize,
                &mut ec,
            );
            if st != libjxl_sys::JXL_DEC_SUCCESS {
                eprintln!("extra channel #{i}: GetExtraChannelInfo status={st}");
                continue;
            }
            let mut name = vec![0u8; (ec.name_length as usize).max(1) + 1];
            let _ = libjxl_sys::JxlDecoderGetExtraChannelName(
                decoder.cast_const(),
                i as usize,
                name.as_mut_ptr().cast(),
                name.len(),
            );
            let name = std::ffi::CStr::from_ptr(name.as_ptr().cast())
                .to_string_lossy()
                .into_owned();
            eprintln!(
                "extra channel #{i}: type={} bits_per_sample={} name=\"{}\"",
                ec.type_, ec.bits_per_sample, name
            );
        }
        libjxl_sys::JxlDecoderDestroy(decoder);
    }
}

/// **Validate the lcms2-based CMYK→sRGB path** end-to-end on `cmyk_layers/input.jxl`.
///
/// Per libjxl PR #237, JPEG-recompressed CMYK files require external color management
/// (4-channel CMYK input → 3-channel sRGB output). libjxl's `JxlDecoderSetOutputColorProfile`
/// is a no-op for non-XYB sources even with a CMS attached.
///
/// Plumbing:
///   1. Decode RGBA float (CMY in RGB slots) + K extra channel (`JXL_CHANNEL_BLACK`).
///   2. Build an interleaved CMYK buffer, **inverting** values: libjxl uses
///      `0 = max ink, 1 = no ink` (per `cms_interface.h`); lcms2 `TYPE_CMYK_FLT` uses the
///      opposite (`0 = no ink, 1 = max ink`).
///   3. Apply the embedded CMYK ICC via `cmsCreateTransform(... LCMS_TYPE_CMYK_FLT, sRGB,
///      LCMS_TYPE_RGBA_FLT, INTENT_PERCEPTUAL, 0)`. Alpha rides as an "extra" channel.
///   4. Quantize to 8-bit and compare against `ref.png` — should match within ~±2 codes
///      per channel.
#[cfg(feature = "jpegxl")]
#[test]
fn conformance_cmyk_layers_cms_srgb_output_matches_ref_png_when_sample_present() {
    let jxl_path = std::path::Path::new(r"F:\HDR\conformance\testcases\cmyk_layers\input.jxl");
    let ref_path = std::path::Path::new(r"F:\HDR\conformance\testcases\cmyk_layers\ref.png");
    if !jxl_path.is_file() || !ref_path.is_file() {
        return;
    }
    let bytes = std::fs::read(jxl_path).expect("read cmyk_layers/input.jxl");

    let mut composed: Vec<u8> = Vec::new();
    let mut width = 0_u32;
    let mut height = 0_u32;
    let mut rgba_f32: Vec<f32> = Vec::new();
    let mut k_f32: Vec<f32> = Vec::new();
    let mut source_icc: Vec<u8> = Vec::new();
    unsafe {
        let decoder = libjxl_sys::JxlDecoderCreate(std::ptr::null());
        assert!(!decoder.is_null());

        let subscribed = libjxl_sys::JXL_DEC_BASIC_INFO
            | libjxl_sys::JXL_DEC_COLOR_ENCODING
            | libjxl_sys::JXL_DEC_FRAME
            | libjxl_sys::JXL_DEC_FULL_IMAGE;
        assert_eq!(
            libjxl_sys::JxlDecoderSubscribeEvents(decoder, subscribed as i32),
            libjxl_sys::JXL_DEC_SUCCESS
        );
        assert_eq!(
            libjxl_sys::JxlDecoderSetInput(decoder, bytes.as_ptr(), bytes.len()),
            libjxl_sys::JXL_DEC_SUCCESS
        );
        libjxl_sys::JxlDecoderCloseInput(decoder);

        let pixel_format = libjxl_sys::JxlPixelFormat {
            num_channels: 4,
            data_type: libjxl_sys::JXL_TYPE_FLOAT,
            endianness: libjxl_sys::JXL_NATIVE_ENDIAN,
            align: 0,
        };
        let k_format = libjxl_sys::JxlPixelFormat {
            num_channels: 1,
            ..pixel_format
        };

        let mut info: libjxl_sys::JxlBasicInfo = std::mem::zeroed();
        let mut k_idx = None::<u32>;
        loop {
            let st = libjxl_sys::JxlDecoderProcessInput(decoder);
            if st == libjxl_sys::JXL_DEC_BASIC_INFO {
                assert_eq!(
                    libjxl_sys::JxlDecoderGetBasicInfo(decoder, &mut info),
                    libjxl_sys::JXL_DEC_SUCCESS
                );
                width = info.xsize;
                height = info.ysize;
                k_idx = super::jxl_find_black_extra_channel_index(decoder, &info);
                assert!(
                    k_idx.is_some(),
                    "expected a JXL_CHANNEL_BLACK extra channel"
                );
            } else if st == libjxl_sys::JXL_DEC_COLOR_ENCODING {
                let mut icc_size = 0_usize;
                assert_eq!(
                    libjxl_sys::JxlDecoderGetICCProfileSize(
                        decoder.cast_const(),
                        libjxl_sys::JXL_COLOR_PROFILE_TARGET_ORIGINAL,
                        &mut icc_size,
                    ),
                    libjxl_sys::JXL_DEC_SUCCESS
                );
                source_icc = vec![0u8; icc_size];
                assert_eq!(
                    libjxl_sys::JxlDecoderGetColorAsICCProfile(
                        decoder.cast_const(),
                        libjxl_sys::JXL_COLOR_PROFILE_TARGET_ORIGINAL,
                        source_icc.as_mut_ptr(),
                        icc_size,
                    ),
                    libjxl_sys::JXL_DEC_SUCCESS
                );
                eprintln!("source CMYK ICC: {} bytes", source_icc.len());
            } else if st == libjxl_sys::JXL_DEC_NEED_IMAGE_OUT_BUFFER {
                let mut size = 0_usize;
                assert_eq!(
                    libjxl_sys::JxlDecoderImageOutBufferSize(
                        decoder.cast_const(),
                        &pixel_format,
                        &mut size
                    ),
                    libjxl_sys::JXL_DEC_SUCCESS
                );
                rgba_f32.resize(size / std::mem::size_of::<f32>(), 0.0);
                assert_eq!(
                    libjxl_sys::JxlDecoderSetImageOutBuffer(
                        decoder,
                        &pixel_format,
                        rgba_f32.as_mut_ptr().cast(),
                        size
                    ),
                    libjxl_sys::JXL_DEC_SUCCESS
                );
                let idx = k_idx.expect("k channel index");
                let mut k_size = 0_usize;
                assert_eq!(
                    libjxl_sys::JxlDecoderExtraChannelBufferSize(
                        decoder.cast_const(),
                        &k_format,
                        &mut k_size,
                        idx,
                    ),
                    libjxl_sys::JXL_DEC_SUCCESS
                );
                k_f32.resize(k_size / std::mem::size_of::<f32>(), 0.0);
                assert_eq!(
                    libjxl_sys::JxlDecoderSetExtraChannelBuffer(
                        decoder,
                        &k_format,
                        k_f32.as_mut_ptr().cast(),
                        k_size,
                        idx,
                    ),
                    libjxl_sys::JXL_DEC_SUCCESS
                );
            } else if st == libjxl_sys::JXL_DEC_FULL_IMAGE {
                break;
            } else if st == libjxl_sys::JXL_DEC_ERROR || st == libjxl_sys::JXL_DEC_NEED_MORE_INPUT {
                panic!("libjxl process error/need-more-input: {st}");
            }
        }
        libjxl_sys::JxlDecoderDestroy(decoder);
    }

    // Build CMYK input following libjxl's `enc_color_management.cc` LCMS path
    // (the "0=white, 100=max ink" comment + `100 - 100 * v` line). lcms2's
    // `TYPE_CMYK_FLT` is encoded in **PostScript percent units** (0..100),
    // and libjxl's RGBA float output uses `0=max ink, 1=white` for CMYK
    // sources. Channel order is (C, M, Y) from RGB slots + K from the
    // BLACK extra channel (matching `CopyToT` in `enc_image_bundle.cc`).
    let n_pixels = (rgba_f32.len() / 4) as u32;
    assert_eq!(n_pixels as usize, k_f32.len());
    let mut cmyk_input = Vec::<f32>::with_capacity(n_pixels as usize * 4);
    let mut alpha = Vec::<f32>::with_capacity(n_pixels as usize);
    for (px, &k) in rgba_f32.chunks_exact(4).zip(k_f32.iter()) {
        cmyk_input.push(100.0 - 100.0 * px[0].clamp(0.0, 1.0));
        cmyk_input.push(100.0 - 100.0 * px[1].clamp(0.0, 1.0));
        cmyk_input.push(100.0 - 100.0 * px[2].clamp(0.0, 1.0));
        cmyk_input.push(100.0 - 100.0 * k.clamp(0.0, 1.0));
        alpha.push(px[3]);
    }

    let mut rgba_out = vec![0.0_f32; n_pixels as usize * 4];
    let in_profile =
        libjxl_sys::CmsProfile::open_from_mem(&source_icc).expect("lcms could not parse CMYK ICC");
    let out_profile =
        libjxl_sys::CmsProfile::new_srgb().expect("lcms could not build sRGB profile");
    // djxl converts CMYK→sRGB with the destination's rendering intent.
    // For its `ColorEncoding::SRGB(false)` target the default intent is
    // perceptual (matches `INTENT_PERCEPTUAL = 0`).
    let transform = libjxl_sys::CmsTransform::new(
        &in_profile,
        libjxl_sys::LCMS_TYPE_CMYK_FLT,
        &out_profile,
        libjxl_sys::LCMS_TYPE_RGBA_FLT,
        libjxl_sys::LCMS_INTENT_PERCEPTUAL,
        0,
    )
    .expect("lcms could not build CMYK→sRGB transform");
    transform.do_transform(
        cmyk_input.as_ptr().cast(),
        rgba_out.as_mut_ptr().cast(),
        n_pixels,
    );

    composed.reserve(n_pixels as usize * 4);
    for (i, px) in rgba_out.chunks_exact(4).enumerate() {
        composed.push(super::srgb_unit_to_u8(px[0]));
        composed.push(super::srgb_unit_to_u8(px[1]));
        composed.push(super::srgb_unit_to_u8(px[2]));
        composed.push(super::srgb_unit_to_u8(alpha[i]));
    }

    let ref_img = image::open(ref_path).expect("decode ref.png").to_rgba8();
    let ref_bytes_for_diag = ref_img.clone().into_raw();
    let pick = |bytes: &[u8], x: u32, y: u32| {
        let i = (y as usize * width as usize + x as usize) * 4;
        (bytes[i], bytes[i + 1], bytes[i + 2])
    };
    eprintln!(
        "lcms diagnostic samples (jxl vs ref.png):\n  black-area(135,14): jxl={:?} ref={:?}\n  bg(256,225):       jxl={:?} ref={:?}\n  bg(220,360):       jxl={:?} ref={:?}",
        pick(&composed, 135, 14),
        pick(&ref_bytes_for_diag, 135, 14),
        pick(&composed, 256, 225),
        pick(&ref_bytes_for_diag, 256, 225),
        pick(&composed, 220, 360),
        pick(&ref_bytes_for_diag, 220, 360),
    );

    assert_eq!((width, height), (ref_img.width(), ref_img.height()));
    let ref_bytes = ref_img.into_raw();
    assert_eq!(composed.len(), ref_bytes.len());
    let n = (composed.len() / 4) as i64;
    let (mut diff_r, mut diff_g, mut diff_b) = (0_i64, 0_i64, 0_i64);
    let (mut max_r, mut max_g, mut max_b) = (0_u32, 0_u32, 0_u32);
    for (j, r) in composed.chunks_exact(4).zip(ref_bytes.chunks_exact(4)) {
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
        "cmyk_layers (lcms2 CMYK→sRGB) vs ref.png:\n  mean signed diff = ({bias_r:+.2}, {bias_g:+.2}, {bias_b:+.2})\n  max abs diff = ({max_r}, {max_g}, {max_b})"
    );
    // ref.png was rendered by djxl with skcms; we use lcms2. Both should
    // produce the same colorimetric transform; small (<5 codes) bias is
    // tolerable due to differences in profile-internal LUT interpolation
    // and intent handling between the two CMSes.
    assert!(
        bias_r.abs() < 5.0 && bias_g.abs() < 5.0 && bias_b.abs() < 5.0,
        "lcms2 CMYK→sRGB drifts too far from ref.png: bias=({bias_r:+.2}, {bias_g:+.2}, {bias_b:+.2})"
    );
}

/// Historical diagnostic: dumps libjxl's CMYK output as raw RGB plus a few
/// hand-rolled compositing models (`R*K`, `R*(1-K)`, `min(R,K)`, etc.) and
/// reports the per-channel pixel diff against the conformance ref.png.
/// All such models are wrong without proper ICC-managed CMYK→sRGB
/// conversion (see PR #237 in libjxl). We retain the test as a debugging
/// aid — it documents how the old "guess the formula" approach misbehaves
/// across ink mixes — but the real fix lives in
/// `apply_cmyk_to_srgb_via_lcms`.
#[cfg(feature = "jpegxl")]
#[test]
fn conformance_cmyk_layers_naive_composition_models_are_all_wrong_when_sample_present() {
    let jxl_path = std::path::Path::new(r"F:\HDR\conformance\testcases\cmyk_layers\input.jxl");
    let ref_path = std::path::Path::new(r"F:\HDR\conformance\testcases\cmyk_layers\ref.png");
    if !jxl_path.is_file() || !ref_path.is_file() {
        return;
    }
    let bytes = std::fs::read(jxl_path).expect("read cmyk_layers/input.jxl");

    unsafe {
        let decoder = libjxl_sys::JxlDecoderCreate(std::ptr::null());
        assert!(!decoder.is_null());
        let subscribed = libjxl_sys::JXL_DEC_BASIC_INFO
            | libjxl_sys::JXL_DEC_COLOR_ENCODING
            | libjxl_sys::JXL_DEC_FRAME
            | libjxl_sys::JXL_DEC_FULL_IMAGE;
        assert_eq!(
            libjxl_sys::JxlDecoderSubscribeEvents(decoder, subscribed as i32),
            libjxl_sys::JXL_DEC_SUCCESS
        );
        assert_eq!(
            libjxl_sys::JxlDecoderSetInput(decoder, bytes.as_ptr(), bytes.len()),
            libjxl_sys::JXL_DEC_SUCCESS
        );
        libjxl_sys::JxlDecoderCloseInput(decoder);

        let pixel_format = libjxl_sys::JxlPixelFormat {
            num_channels: 4,
            data_type: libjxl_sys::JXL_TYPE_FLOAT,
            endianness: libjxl_sys::JXL_NATIVE_ENDIAN,
            align: 0,
        };
        let k_format = libjxl_sys::JxlPixelFormat {
            num_channels: 1,
            ..pixel_format
        };

        let mut info: libjxl_sys::JxlBasicInfo = std::mem::zeroed();
        let mut rgba_f32: Vec<f32> = Vec::new();
        let mut k_f32: Vec<f32> = Vec::new();
        loop {
            let st = libjxl_sys::JxlDecoderProcessInput(decoder);
            if st == libjxl_sys::JXL_DEC_BASIC_INFO {
                assert_eq!(
                    libjxl_sys::JxlDecoderGetBasicInfo(decoder, &mut info),
                    libjxl_sys::JXL_DEC_SUCCESS
                );
            } else if st == libjxl_sys::JXL_DEC_NEED_IMAGE_OUT_BUFFER {
                let mut size = 0_usize;
                assert_eq!(
                    libjxl_sys::JxlDecoderImageOutBufferSize(
                        decoder.cast_const(),
                        &pixel_format,
                        &mut size
                    ),
                    libjxl_sys::JXL_DEC_SUCCESS
                );
                rgba_f32.resize(size / std::mem::size_of::<f32>(), 0.0);
                assert_eq!(
                    libjxl_sys::JxlDecoderSetImageOutBuffer(
                        decoder,
                        &pixel_format,
                        rgba_f32.as_mut_ptr().cast(),
                        size
                    ),
                    libjxl_sys::JXL_DEC_SUCCESS
                );
                // Channel 0 is type=BLACK (per the diagnostic above).
                let mut k_size = 0_usize;
                assert_eq!(
                    libjxl_sys::JxlDecoderExtraChannelBufferSize(
                        decoder.cast_const(),
                        &k_format,
                        &mut k_size,
                        0
                    ),
                    libjxl_sys::JXL_DEC_SUCCESS
                );
                k_f32.resize(k_size / std::mem::size_of::<f32>(), 0.0);
                assert_eq!(
                    libjxl_sys::JxlDecoderSetExtraChannelBuffer(
                        decoder,
                        &k_format,
                        k_f32.as_mut_ptr().cast(),
                        k_size,
                        0
                    ),
                    libjxl_sys::JXL_DEC_SUCCESS
                );
            } else if st == libjxl_sys::JXL_DEC_FULL_IMAGE {
                break;
            } else if st == libjxl_sys::JXL_DEC_ERROR || st == libjxl_sys::JXL_DEC_NEED_MORE_INPUT {
                panic!("libjxl process error/need-more-input: {st}");
            }
        }
        libjxl_sys::JxlDecoderDestroy(decoder);

        let n = (rgba_f32.len() / 4) as u64;
        let denom = n.max(1) as f64;

        // K stats — is it "0=no ink, 1=full ink" or "0=black, 1=white" (visible intensity)?
        let (mut k_min, mut k_max, mut k_sum) = (1.0_f32, 0.0_f32, 0.0_f64);
        for &k in &k_f32 {
            k_min = k_min.min(k);
            k_max = k_max.max(k);
            k_sum += k as f64;
        }
        eprintln!(
            "K channel: min={k_min:.4} max={k_max:.4} mean={:.4}",
            k_sum / denom
        );

        // Sample a few specific pixels at known regions: top of image (the "black" word
        // region) and middle (the "Background" green text region).
        let pick = |x: u32, y: u32| {
            let idx = (y * info.xsize + x) as usize;
            (
                rgba_f32[idx * 4],
                rgba_f32[idx * 4 + 1],
                rgba_f32[idx * 4 + 2],
                rgba_f32[idx * 4 + 3],
                k_f32[idx],
            )
        };
        // Approximate text positions on a 512×512 conformance test card.
        for (label, x, y) in [
            ("top-center  ", 256, 75),
            ("background  ", 256, 200),
            ("layer1      ", 256, 256),
            ("test-name   ", 256, 380),
            ("white-bg    ", 50, 50),
        ] {
            let (r, g, b, a, k) = pick(x, y);
            eprintln!("px ({label}) ({x:3}, {y:3}): R={r:.3} G={g:.3} B={b:.3} A={a:.3} K={k:.3}");
        }

        // Try several compositing models and report mean diff to ref.png.
        let ref_img = image::open(ref_path).expect("decode ref.png").to_rgba8();
        assert_eq!(
            (ref_img.width(), ref_img.height()),
            (info.xsize, info.ysize)
        );
        let ref_bytes = ref_img.into_raw();

        let try_compose = |name: &str, compose: fn(f32, f32, f32, f32) -> [f32; 3]| {
            let mut diff_r = 0_i64;
            let mut diff_g = 0_i64;
            let mut diff_b = 0_i64;
            let (mut max_r, mut max_g, mut max_b) = (0_u32, 0_u32, 0_u32);
            for (i, (px, k)) in rgba_f32.chunks_exact(4).zip(k_f32.iter()).enumerate() {
                let [r, g, b] = compose(px[0], px[1], px[2], *k);
                let r_u = super::srgb_unit_to_u8(r) as i32;
                let g_u = super::srgb_unit_to_u8(g) as i32;
                let b_u = super::srgb_unit_to_u8(b) as i32;
                let ref_r = ref_bytes[i * 4] as i32;
                let ref_g = ref_bytes[i * 4 + 1] as i32;
                let ref_b = ref_bytes[i * 4 + 2] as i32;
                diff_r += (r_u - ref_r) as i64;
                diff_g += (g_u - ref_g) as i64;
                diff_b += (b_u - ref_b) as i64;
                max_r = max_r.max((r_u - ref_r).unsigned_abs());
                max_g = max_g.max((g_u - ref_g).unsigned_abs());
                max_b = max_b.max((b_u - ref_b).unsigned_abs());
            }
            eprintln!(
                "{name}: bias=({:+.2}, {:+.2}, {:+.2}) max=({max_r}, {max_g}, {max_b})",
                diff_r as f64 / n as f64,
                diff_g as f64 / n as f64,
                diff_b as f64 / n as f64
            );
        };

        try_compose("RGB                    ", |r, g, b, _k| [r, g, b]);
        try_compose("RGB * (1 - K)          ", |r, g, b, k| {
            [r * (1.0 - k), g * (1.0 - k), b * (1.0 - k)]
        });
        try_compose("RGB * K                ", |r, g, b, k| {
            [r * k, g * k, b * k]
        });
        try_compose("min(RGB, K)            ", |r, g, b, k| {
            [r.min(k), g.min(k), b.min(k)]
        });
        try_compose("RGB - (1 - K)          ", |r, g, b, k| {
            [
                (r - (1.0 - k)).max(0.0),
                (g - (1.0 - k)).max(0.0),
                (b - (1.0 - k)).max(0.0),
            ]
        });

        // Find the 5 worst-mismatch pixels using the raw RGB output, dump (x, y, JXL, K, ref).
        let mut diffs: Vec<(u32, i64)> = (0..n as u32)
            .map(|i| {
                let j = i as usize;
                let dr = (super::srgb_unit_to_u8(rgba_f32[j * 4]) as i32 - ref_bytes[j * 4] as i32)
                    .abs();
                let dg = (super::srgb_unit_to_u8(rgba_f32[j * 4 + 1]) as i32
                    - ref_bytes[j * 4 + 1] as i32)
                    .abs();
                let db = (super::srgb_unit_to_u8(rgba_f32[j * 4 + 2]) as i32
                    - ref_bytes[j * 4 + 2] as i32)
                    .abs();
                (i, (dr + dg + db) as i64)
            })
            .collect();
        diffs.sort_by_key(|(_, d)| std::cmp::Reverse(*d));
        eprintln!("--- top 8 worst-mismatch pixels (raw RGB vs ref.png) ---");
        for &(i, d) in diffs.iter().take(8) {
            let x = i % info.xsize;
            let y = i / info.xsize;
            let j = i as usize;
            let r = rgba_f32[j * 4];
            let g = rgba_f32[j * 4 + 1];
            let b = rgba_f32[j * 4 + 2];
            let a = rgba_f32[j * 4 + 3];
            let k = k_f32[j];
            let rr = ref_bytes[j * 4];
            let rg = ref_bytes[j * 4 + 1];
            let rb = ref_bytes[j * 4 + 2];
            let ra = ref_bytes[j * 4 + 3];
            eprintln!(
                "({x:3},{y:3}) sum_diff={d:3}: \
                 JXL(R={r:.3} G={g:.3} B={b:.3} A={a:.3} K={k:.3}) \
                 ref(R={rr} G={rg} B={rb} A={ra})"
            );
        }
    }
}
