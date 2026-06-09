mod tests {
    use super::*;

    fn assert_radiance_logical_roundtrip(line: &str) {
        let r = parse_radiance_dimensions_line(line).unwrap_or_else(|_| panic!("parse {line}"));
        for oa in 0..r.outer_len {
            for ib in 0..r.inner_len {
                let (lx, ly) = r.logical_xy(oa, ib);
                assert_eq!(
                    r.file_indices_for_logical_xy(lx, ly),
                    (oa, ib),
                    "line={line} oa={oa} ib={ib} logical=({lx},{ly})",
                );
            }
        }
    }

    #[test]
    fn radiance_dimensions_line_accepts_x_then_y_token_order() {
        let r = parse_radiance_dimensions_line("+x 200 -Y 100").unwrap();
        assert_eq!((r.width, r.height), (200, 100));
        assert_eq!((r.outer_len, r.inner_len), (200, 100));
    }

    #[test]
    fn radiance_dimensions_minus_y_plus_x_flags_row_major_native() {
        let r = parse_radiance_dimensions_line("-Y 4 +X 7").unwrap();
        assert_eq!((r.width, r.height), (7, 4));
        assert!(r.is_row_major_top_left());
    }

    #[test]
    fn radiance_logical_xy_file_indices_inverse_for_all_sign_variants() {
        for line in [
            "-Y 2 +X 3",
            "+X 3 -Y 2",
            "+Y 2 +X 3",
            "-Y 2 -X 3",
            "+X 3 +Y 2",
            "-X 3 -Y 2",
            "+Y 2 -X 3",
            "-X 3 +Y 2",
        ] {
            assert_radiance_logical_roundtrip(line);
        }
    }

    #[test]
    fn extract_tile_applies_radiance_exposure_and_colorcorr() {
        let path = std::env::temp_dir().join(format!(
            "simple_image_viewer_radiance_tile_params_{}.hdr",
            std::process::id()
        ));
        let bytes = b"#?RADIANCE\nFORMAT=32-bit_rle_rgbe\nEXPOSURE=2\nCOLORCORR=2 4 8\n\n-Y 1 +X 1\n\x80\x80\x80\x81";
        std::fs::write(&path, bytes).expect("write test HDR");

        let source = RadianceHdrTiledImageSource::open(&path).expect("open Radiance HDR source");
        let tile = source
            .extract_tile_rgba32f_arc(0, 0, 1, 1)
            .expect("extract Radiance HDR tile");
        let _ = std::fs::remove_file(&path);

        assert!((tile.rgba_f32[0] - 0.25).abs() < 0.01);
        assert!((tile.rgba_f32[1] - 0.125).abs() < 0.01);
        assert!((tile.rgba_f32[2] - 0.0625).abs() < 0.01);
        assert_eq!(tile.rgba_f32[3], 1.0);
    }

    #[test]
    fn static_and_tiled_radiance_decode_apply_same_header_params() {
        let path = std::env::temp_dir().join(format!(
            "simple_image_viewer_radiance_static_tile_consistency_{}.hdr",
            std::process::id()
        ));
        let bytes = b"#?RADIANCE\nFORMAT=32-bit_rle_rgbe\nEXPOSURE=2\nCOLORCORR=2 4 8\n\n-Y 1 +X 1\n\x80\x80\x80\x81";
        std::fs::write(&path, bytes).expect("write test HDR");

        let static_hdr = crate::hdr::decode::decode_hdr_image(&path).expect("decode static HDR");
        let source = RadianceHdrTiledImageSource::open(&path).expect("open tiled HDR");
        let tile = source
            .extract_tile_rgba32f_arc(0, 0, 1, 1)
            .expect("extract tiled HDR");
        let _ = std::fs::remove_file(&path);

        assert_eq!(static_hdr.color_space, tile.color_space);
        assert_eq!(static_hdr.rgba_f32.len(), tile.rgba_f32.len());
        for (static_value, tile_value) in static_hdr.rgba_f32.iter().zip(tile.rgba_f32.iter()) {
            assert!(
                (static_value - tile_value).abs() < 0.01,
                "static={static_value}, tile={tile_value}"
            );
        }
    }

    #[test]
    fn extract_tile_does_not_require_full_image_decode_budget() {
        let path = std::env::temp_dir().join(format!(
            "simple_image_viewer_radiance_tile_window_{}.hdr",
            std::process::id()
        ));
        let width = 8193_u32;
        let height = 8193_u32;
        let mut bytes =
            format!("#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n-Y {height} +X {width}\n").into_bytes();
        for _ in 0..height {
            append_constant_new_rle_scanline(&mut bytes, width, [128, 128, 128], 129);
        }
        std::fs::write(&path, bytes).expect("write oversized tiled HDR");

        let source = RadianceHdrTiledImageSource::open(&path).expect("open Radiance HDR source");
        let tile = source
            .extract_tile_rgba32f_arc(0, 0, 1, 1)
            .expect("extract first pixel without full-image decode");
        let _ = std::fs::remove_file(&path);

        assert_eq!((tile.width, tile.height), (1, 1));
        assert!((tile.rgba_f32[0] - 1.0).abs() < 0.01);
        assert!((tile.rgba_f32[1] - 1.0).abs() < 0.01);
        assert!((tile.rgba_f32[2] - 1.0).abs() < 0.01);
        assert_eq!(tile.rgba_f32[3], 1.0);
    }

    #[test]
    fn generate_preview_does_not_require_full_image_decode_budget() {
        let path = std::env::temp_dir().join(format!(
            "simple_image_viewer_radiance_preview_window_{}.hdr",
            std::process::id()
        ));
        let width = 8193_u32;
        let height = 8193_u32;
        let mut bytes =
            format!("#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n-Y {height} +X {width}\n").into_bytes();
        for _ in 0..height {
            append_constant_new_rle_scanline(&mut bytes, width, [128, 128, 128], 129);
        }
        std::fs::write(&path, bytes).expect("write oversized preview HDR");

        let source = RadianceHdrTiledImageSource::open(&path).expect("open Radiance HDR source");
        let (preview_width, preview_height, pixels) = source
            .generate_sdr_preview(1, 1)
            .expect("generate sampled preview without full-image decode");
        let _ = std::fs::remove_file(&path);

        assert_eq!((preview_width, preview_height), (1, 1));
        assert_eq!(pixels.len(), 4);
        assert!(pixels[0] > 0);
        assert!(pixels[1] > 0);
        assert!(pixels[2] > 0);
        assert_eq!(pixels[3], 255);
    }

    #[test]
    fn open_indexes_radiance_scanline_offsets_for_direct_tile_decode() {
        let path = std::env::temp_dir().join(format!(
            "simple_image_viewer_radiance_scanline_index_{}.hdr",
            std::process::id()
        ));
        let width = 4_u32;
        let height = 4_u32;
        let mut bytes =
            format!("#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n-Y {height} +X {width}\n").into_bytes();
        for row in 0..height {
            append_constant_new_rle_scanline(
                &mut bytes,
                width,
                [32 + row as u8, 64 + row as u8, 96 + row as u8],
                129,
            );
        }
        std::fs::write(&path, bytes).expect("write indexed HDR");

        let source = RadianceHdrTiledImageSource::open(&path).expect("open Radiance HDR source");
        assert_eq!(source.scanline_offsets.len(), height as usize);

        let tile = source
            .extract_tile_rgba32f_arc(1, 3, 2, 1)
            .expect("extract deep tile via scanline offset index");
        let _ = std::fs::remove_file(&path);

        let expected = f32::from(32_u8 + 3) * 2.0_f32.powi(129 - 128 - 8);
        assert!((tile.rgba_f32[0] - expected).abs() < 0.001);
        assert!((tile.rgba_f32[4] - expected).abs() < 0.001);
    }

    fn append_constant_new_rle_scanline(
        bytes: &mut Vec<u8>,
        width: u32,
        rgb: [u8; 3],
        exponent: u8,
    ) {
        bytes.extend_from_slice(&[2, 2, (width >> 8) as u8, (width & 0xff) as u8]);
        for value in [rgb[0], rgb[1], rgb[2], exponent] {
            let mut remaining = width;
            while remaining > 0 {
                let run = remaining.min(127);
                bytes.push(128 + run as u8);
                bytes.push(value);
                remaining -= run;
            }
        }
    }
}
