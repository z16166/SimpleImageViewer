use super::*;

fn diagnose_conformance_blendmodes_and_patches_when_sample_present() {
    // Kept as a hand-runnable diagnostic: prints per-channel bias / max-abs
    // / histogram for a handful of conformance pairs. Drop by name into
    // `cargo test -- --nocapture diagnose_conformance` when investigating
    // a new SDR-fallback regression.
    for case in [
        "bench_oriented_brg",
        "blendmodes",
        "patches",
        "cmyk_layers",
        "bike",
    ] {
        let jxl = std::path::Path::new(r"F:\HDR\conformance\testcases")
            .join(case)
            .join("input.jxl");
        let png = std::path::Path::new(r"F:\HDR\conformance\testcases")
            .join(case)
            .join("ref.png");
        diagnose_conformance_pair(case, &jxl, &png);
    }
}

/// For each conformance file, sample a few specific pixels that are NOT
/// pure black/white in `ref.png` and report:
///   - libjxl's raw float values out of `JxlDecoderProcessInput`
///   - what `srgb_unit_to_u8(v*255)` produces (direct quantize)
///   - what `linear_to_srgb_u8(v)` produces (apply sRGB OETF first)
///   - what `ref.png` actually has at that location
/// The encoding that matches ref.png tells us how libjxl emitted floats
/// for that bitstream — Modular-mode files with TF=Linear preserve linear
/// values, while sRGB-tagged Modular-mode files preserve sRGB-encoded
/// values, etc.
/// Count `JXL_DEC_FRAME` events fired by libjxl for `blendmodes/input.jxl`
/// — if it's >1 with `have_animation=0` we know the file ships multiple
/// blend-mode layers that libjxl coalesces; if our pipeline is somehow
/// giving back the un-coalesced last layer that explains why our SDR
/// fallback differs from `ref.png` on partially-transparent pixels.
#[cfg(feature = "jpegxl")]
#[test]
fn diagnose_blendmodes_frame_count_when_sample_present() {
    let path = std::path::Path::new(r"F:\HDR\conformance\testcases\blendmodes\input.jxl");
    if !path.is_file() {
        return;
    }
    let bytes = std::fs::read(path).expect("read");
    let mut frame_count = 0_u32;
    let mut full_image_count = 0_u32;
    unsafe {
        let decoder = libjxl_sys::JxlDecoderCreate(std::ptr::null());
        let subscribed = libjxl_sys::JXL_DEC_BASIC_INFO
            | libjxl_sys::JXL_DEC_COLOR_ENCODING
            | libjxl_sys::JXL_DEC_FRAME
            | libjxl_sys::JXL_DEC_FULL_IMAGE;
        libjxl_sys::JxlDecoderSubscribeEvents(decoder, subscribed as i32);
        libjxl_sys::JxlDecoderSetInput(decoder, bytes.as_ptr(), bytes.len());
        libjxl_sys::JxlDecoderCloseInput(decoder);
        let pf = libjxl_sys::JxlPixelFormat {
            num_channels: 4,
            data_type: libjxl_sys::JXL_TYPE_FLOAT,
            endianness: libjxl_sys::JXL_NATIVE_ENDIAN,
            align: 0,
        };
        let mut buf: Vec<f32> = Vec::new();
        loop {
            let st = libjxl_sys::JxlDecoderProcessInput(decoder);
            if st == libjxl_sys::JXL_DEC_FRAME {
                frame_count += 1;
            } else if st == libjxl_sys::JXL_DEC_FULL_IMAGE {
                full_image_count += 1;
            } else if st == libjxl_sys::JXL_DEC_NEED_IMAGE_OUT_BUFFER {
                let mut size = 0_usize;
                libjxl_sys::JxlDecoderImageOutBufferSize(decoder.cast_const(), &pf, &mut size);
                buf.resize(size / 4, 0.0);
                libjxl_sys::JxlDecoderSetImageOutBuffer(
                    decoder,
                    &pf,
                    buf.as_mut_ptr().cast(),
                    size,
                );
            } else if st == libjxl_sys::JXL_DEC_SUCCESS || st == libjxl_sys::JXL_DEC_ERROR {
                break;
            } else if st == libjxl_sys::JXL_DEC_NEED_MORE_INPUT {
                break;
            }
        }
        libjxl_sys::JxlDecoderDestroy(decoder);
    }
    eprintln!(
        "[blendmodes] JXL_DEC_FRAME fired {frame_count}× JXL_DEC_FULL_IMAGE fired {full_image_count}×"
    );
}

/// Hunt for pixels with the largest channel diff between our SDR fallback
/// bytes and `ref.png` for `blendmodes/input.jxl` and dump float + alpha
/// + neighbours so we can identify whether the discrepancy is a clamp,
/// alpha-compositing, or layer blend bug.
#[cfg(feature = "jpegxl")]
#[test]
fn diagnose_blendmodes_worst_pixels_when_sample_present() {
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
        .expect("decode"),
    );
    let crate::loader::ImageData::Hdr { fallback, hdr, .. } = img else {
        return;
    };
    let our = fallback.rgba().to_vec();
    let r = image::open(ref_path).expect("ref").to_rgba8().into_raw();
    let w = hdr.width as usize;
    let mut worst = Vec::<(i32, usize, usize)>::new();
    for y in (0..hdr.height as usize).step_by(8) {
        for x in (0..w).step_by(8) {
            let i = (y * w + x) * 4;
            let dr = (our[i] as i32 - r[i] as i32).abs();
            let dg = (our[i + 1] as i32 - r[i + 1] as i32).abs();
            let db = (our[i + 2] as i32 - r[i + 2] as i32).abs();
            let m = dr.max(dg).max(db);
            if m >= 30 {
                worst.push((m, x, y));
            }
        }
    }
    worst.sort_by(|a, b| b.0.cmp(&a.0));
    worst.truncate(10);
    for &(diff, x, y) in &worst {
        let i = (y * w + x) * 4;
        let f_i = (y * hdr.width as usize + x) * 4;
        let f = &hdr.rgba_f32[f_i..f_i + 4];
        eprintln!(
            "[worst] ({x:4},{y:4}) diff={diff} ours=({},{},{},a:{}) ref=({},{},{},a:{}) f32=({:.3},{:.3},{:.3},a:{:.3})",
            our[i],
            our[i + 1],
            our[i + 2],
            our[i + 3],
            r[i],
            r[i + 1],
            r[i + 2],
            r[i + 3],
            f[0],
            f[1],
            f[2],
            f[3]
        );
    }
}

#[cfg(feature = "jpegxl")]
#[test]
fn diagnose_jxl_float_buffer_encoding_when_samples_present() {
    for case in ["blendmodes", "patches", "bench_oriented_brg", "bike"] {
        let jxl_path = std::path::Path::new(r"F:\HDR\conformance\testcases")
            .join(case)
            .join("input.jxl");
        let ref_path = std::path::Path::new(r"F:\HDR\conformance\testcases")
            .join(case)
            .join("ref.png");
        if !jxl_path.is_file() || !ref_path.is_file() {
            continue;
        }
        let bytes = std::fs::read(&jxl_path).expect("read jxl");
        let ref_img = image::open(&ref_path).expect("decode ref.png").to_rgba8();
        let (rw, rh) = (ref_img.width(), ref_img.height());
        let ref_bytes = ref_img.into_raw();

        let mut rgba_f32: Vec<f32> = Vec::new();
        let mut width: u32 = 0;
        unsafe {
            let decoder = libjxl_sys::JxlDecoderCreate(std::ptr::null());
            let subscribed = libjxl_sys::JXL_DEC_BASIC_INFO
                | libjxl_sys::JXL_DEC_COLOR_ENCODING
                | libjxl_sys::JXL_DEC_FRAME
                | libjxl_sys::JXL_DEC_FULL_IMAGE;
            libjxl_sys::JxlDecoderSubscribeEvents(decoder, subscribed as i32);
            libjxl_sys::JxlDecoderSetInput(decoder, bytes.as_ptr(), bytes.len());
            libjxl_sys::JxlDecoderCloseInput(decoder);
            let pixel_format = libjxl_sys::JxlPixelFormat {
                num_channels: 4,
                data_type: libjxl_sys::JXL_TYPE_FLOAT,
                endianness: libjxl_sys::JXL_NATIVE_ENDIAN,
                align: 0,
            };
            let mut info: libjxl_sys::JxlBasicInfo = std::mem::zeroed();
            loop {
                let st = libjxl_sys::JxlDecoderProcessInput(decoder);
                if st == libjxl_sys::JXL_DEC_BASIC_INFO {
                    libjxl_sys::JxlDecoderGetBasicInfo(decoder, &mut info);
                    width = info.xsize;
                } else if st == libjxl_sys::JXL_DEC_NEED_IMAGE_OUT_BUFFER {
                    let mut size = 0_usize;
                    libjxl_sys::JxlDecoderImageOutBufferSize(
                        decoder.cast_const(),
                        &pixel_format,
                        &mut size,
                    );
                    rgba_f32.resize(size / std::mem::size_of::<f32>(), 0.0);
                    libjxl_sys::JxlDecoderSetImageOutBuffer(
                        decoder,
                        &pixel_format,
                        rgba_f32.as_mut_ptr().cast(),
                        size,
                    );
                } else if st == libjxl_sys::JXL_DEC_FULL_IMAGE {
                    break;
                } else if st == libjxl_sys::JXL_DEC_ERROR
                    || st == libjxl_sys::JXL_DEC_NEED_MORE_INPUT
                {
                    break;
                }
            }
            libjxl_sys::JxlDecoderDestroy(decoder);
        }
        if width == 0 || rgba_f32.is_empty() {
            continue;
        }
        // Pick 6 sample pixels evenly spaced
        let samples: [(u32, u32); 6] = [
            (rw / 8, rh / 8),
            (rw / 4, rh / 4),
            (rw / 2, rh / 4),
            (rw / 2, rh / 2),
            (rw * 3 / 4, rh / 2),
            (rw * 3 / 4, rh * 3 / 4),
        ];
        eprintln!("\n--- {case} ({rw}x{rh}) — float vs ref.png ---");
        for (x, y) in samples {
            let i = (y as usize * width as usize + x as usize) * 4;
            if i + 2 >= rgba_f32.len() {
                continue;
            }
            let r = rgba_f32[i];
            let g = rgba_f32[i + 1];
            let b = rgba_f32[i + 2];
            let direct = (
                super::srgb_unit_to_u8(r),
                super::srgb_unit_to_u8(g),
                super::srgb_unit_to_u8(b),
            );
            let linear_to_srgb = (
                super::linear_to_srgb_u8(r),
                super::linear_to_srgb_u8(g),
                super::linear_to_srgb_u8(b),
            );
            let ref_pix = (ref_bytes[i], ref_bytes[i + 1], ref_bytes[i + 2]);
            eprintln!(
                "  ({x:4},{y:4}) f32=({r:.3},{g:.3},{b:.3}) direct={direct:?} linear->srgb={linear_to_srgb:?} ref={ref_pix:?}"
            );
        }
    }
