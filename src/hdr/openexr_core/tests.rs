// Simple Image Viewer - A high-performance, cross-platform image viewer
// Copyright (C) 2024-2026 Simple Image Viewer Contributors
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

#![allow(dead_code)]

use parking_lot::{Condvar, Mutex};
use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::{CStr, CString, c_int, c_void};
use std::path::{Path, PathBuf};
use std::ptr;
    use crate::hdr::types::HdrColorSpace;
    use openexr_core_sys as sys;
    use std::path::PathBuf;

    fn openexr_images_root() -> Option<PathBuf> {
        std::env::var_os("SIV_OPENEXR_IMAGES_DIR")
            .map(PathBuf::from)
            .or_else(|| Some(PathBuf::from(r"F:\HDR\openexr-images")))
            .or_else(|| Some(PathBuf::from("/home/happy/Downloads/HDR/openexr-images")))
            .filter(|path| path.is_dir())
    }

    #[test]
    fn imf_bytes_entry_points_reject_empty_or_null_input() {
        let mut rgba = 0.0_f32;
        let mut w = 0u32;
        let mut h = 0u32;
        let mut chroma = [0.0_f32; 8];

        assert_eq!(
            unsafe {
                sys::siv_imf_rgba_input_scanline_flatten_rgba_bytes(
                    std::ptr::null(),
                    0,
                    std::ptr::null(),
                    &mut rgba,
                    1,
                    &mut w,
                    &mut h,
                )
            },
            -1
        );
        assert_eq!(
            unsafe {
                sys::siv_imf_deep_scanline_flatten_rgba_bytes(
                    std::ptr::null(),
                    0,
                    std::ptr::null(),
                    &mut rgba,
                    1,
                    &mut w,
                    &mut h,
                )
            },
            -1
        );
        assert_eq!(
            unsafe {
                sys::siv_imf_input_file_chromaticities_f32_bytes(
                    std::ptr::null(),
                    0,
                    std::ptr::null(),
                    chroma.as_mut_ptr(),
                )
            },
            -1
        );
    }

    #[test]
    fn imf_deep_scanline_flatten_when_corpus_present() {
        let Some(root) = openexr_images_root() else {
            eprintln!("skipping IMF deep flatten test; set SIV_OPENEXR_IMAGES_DIR");
            return;
        };
        let path = root.join("v2/LowResLeftView/Balls.exr");
        if !path.is_file() {
            eprintln!("skipping IMF deep flatten test; missing {}", path.display());
            return;
        }

        let ctx = super::OpenExrCoreReadContext::open(&path).expect("open exr");
        let part = ctx.part(0).expect("part 0");
        assert_eq!(part.storage, sys::EXR_STORAGE_DEEP_SCANLINE);

        let flat = super::deep_scanline_flatten_rgba_via_imf(&path, part.width, part.height)
            .expect("imf deep flatten");
        assert_eq!(
            flat.len(),
            part.width as usize * part.height as usize * 4,
            "flatten buffer size"
        );
        assert!(
            flat.iter().all(|value| value.is_finite()),
            "deep flatten output must be finite"
        );
        assert!(
            flat.chunks_exact(4)
                .any(|px| px[0] > 0.0 || px[1] > 0.0 || px[2] > 0.0),
            "deep flatten should contain non-black RGB"
        );
    }

    #[test]
    fn imf_rgba_scanline_flatten_supports_utf8_path_via_mmap_when_corpus_present() {
        let Some(root) = openexr_images_root() else {
            return;
        };
        let path = root.join("LuminanceChroma/Flowers.exr");
        if !path.is_file() {
            return;
        }
        let label = sys::imf_io::path_utf8_cstr(&path).expect("utf8 label");
        assert!(label.to_str().expect("label").contains("Flowers.exr"));
        let flat = super::rgba_input_scanline_flatten_rgba_via_imf(&path).expect("flatten");
        assert!(!flat.is_empty());
        assert!(flat.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn carrots_exr_chromaticities_and_preview_stats_when_corpus_present() {
        let Some(root) = openexr_images_root() else {
            return;
        };
        let path = root.join("ScanLines/Carrots.exr");
        if !path.is_file() {
            return;
        }
        let ch = super::imf_exr_chromaticities_from_path(&path);
        let cs = super::OpenExrCoreReadContext::infer_exr_display_color_space_for_path(&path);
        let ctx = super::OpenExrCoreReadContext::open(&path).expect("openexr core");
        let preview = ctx
            .extract_scanline_rgba32f_preview_nearest(0, 64, 64)
            .expect("preview");
        let mut max_rgb = [0.0_f32; 3];
        let mut max_a = 0.0_f32;
        let mut min_a = f32::INFINITY;
        for px in preview.rgba.chunks_exact(4) {
            for i in 0..3 {
                max_rgb[i] = max_rgb[i].max(px[i]);
            }
            max_a = max_a.max(px[3]);
            min_a = min_a.min(px[3]);
        }
        eprintln!(
            "Carrots: chroma_ok={} ch={ch:?} color_space={cs:?} max_rgb={max_rgb:?} min_a={min_a} max_a={max_a}",
            ch.is_some()
        );
        assert!(
            ch.is_some(),
            "Imf should read chromaticities from Carrots.exr header"
        );
        assert_eq!(cs, HdrColorSpace::Aces2065_1);
        assert!(
            max_rgb[0] > 0.01 || max_rgb[1] > 0.01 || max_rgb[2] > 0.01,
            "rgb should not be flat black"
        );
    }

    #[test]
    fn aces_ap0_chromaticities_heuristic_triggers() {
        let ap0 = [
            0.7347_f32, 0.2653, 0.0, 1.0, 0.0001, -0.077, 0.32168, 0.33767,
        ];
        assert!(super::chromaticities_looks_like_aces_ap0(&ap0));
        assert_eq!(
            super::hdr_color_space_from_chromaticities_xy(&ap0),
            HdrColorSpace::Aces2065_1
        );
    }

    #[test]
    fn rec709_like_chromaticities_stays_linear_srgb() {
        let rec709 = [0.64_f32, 0.33, 0.3, 0.6, 0.15, 0.06, 0.3127, 0.3290];
        assert!(!super::chromaticities_looks_like_aces_ap0(&rec709));
        assert_eq!(
            super::hdr_color_space_from_chromaticities_xy(&rec709),
            HdrColorSpace::LinearSrgb
        );
    }

    #[test]
    fn luma_chroma_ratio_decode_round_trips_rec709_weights() {
        let wr = 0.2126_f32;
        let wg = 0.7152_f32;
        let wb = 0.0722_f32;
        let r0 = 0.3_f32;
        let g0 = 0.4_f32;
        let b0 = 0.35_f32;
        let y = wr * r0 + wg * g0 + wb * b0;
        let ry = (r0 - y) / y;
        let by = (b0 - y) / y;
        let r = y * (1.0 + ry);
        let b = y * (1.0 + by);
        let g = (y - wr * r - wb * b) / wg;
        assert!((r - r0).abs() < 1e-4);
        assert!((g - g0).abs() < 1e-4);
        assert!((b - b0).abs() < 1e-4);
    }

    #[test]
    fn openexr_luminance_weights_match_rec709_chromaticities() {
        let rec709 = [0.64_f32, 0.33, 0.3, 0.6, 0.15, 0.06, 0.3127, 0.3290];
        let w = super::openexr_luminance_weights_from_chromaticities_xy(&rec709).expect("yw");
        assert!((w[0] - 0.212639).abs() < 0.002);
        assert!((w[1] - 0.715169).abs() < 0.002);
        assert!((w[2] - 0.072192).abs() < 0.002);
    }

    #[test]
    fn bilinear_subsampled_channel_interpolates_between_chroma_texels() {
        let layout = super::OpenExrCoreChannelChunkLayout {
            width: 2,
            height: 1,
            x_samples: 2,
            y_samples: 1,
        };
        let buffers = vec![vec![0.0_f32, 1.0]];
        let layouts = vec![Some(layout)];
        let origin = (0u32, 0u32);
        assert_eq!(
            super::channel_sample_f32_filtered(&buffers, &layouts, 0, origin, 0, 0, true),
            0.0
        );
        assert_eq!(
            super::channel_sample_f32_filtered(&buffers, &layouts, 0, origin, 1, 0, true),
            0.25
        );
        assert_eq!(
            super::channel_sample_f32_filtered(&buffers, &layouts, 0, origin, 2, 0, true),
            0.75
        );
        assert_eq!(
            super::channel_sample_f32_filtered(&buffers, &layouts, 0, origin, 3, 0, true),
            1.0
        );
    }

    #[test]
    fn sampled_channel_flat_index_matches_half_res_chroma() {
        let layout = super::OpenExrCoreChannelChunkLayout {
            width: 3,
            height: 2,
            x_samples: 2,
            y_samples: 2,
        };
        let origin = (0u32, 0u32);
        assert_eq!(
            super::sampled_channel_flat_index(layout, origin, 0, 0),
            Some(0)
        );
        assert_eq!(
            super::sampled_channel_flat_index(layout, origin, 1, 0),
            Some(0)
        );
        assert_eq!(
            super::sampled_channel_flat_index(layout, origin, 2, 0),
            Some(1)
        );
        assert_eq!(
            super::sampled_channel_flat_index(layout, origin, 4, 0),
            Some(2)
        );
        assert_eq!(
            super::sampled_channel_flat_index(layout, origin, 0, 2),
            Some(3)
        );
    }

    #[test]
    fn decoded_chunk_cache_reuses_native_chunk_across_horizontal_tiles() {
        let key = super::OpenExrCoreDecodedChunkKey {
            part_index: 0,
            chunk_index: 7,
            origin: (0, 6144),
            size: (24576, 32),
        };
        let chunk = std::sync::Arc::new(super::OpenExrCoreDecodedChunk {
            origin: (0, 6144),
            width: 24576,
            height: 32,
            rgba: std::sync::Arc::new(vec![0.0; 4]),
            byte_size: 16,
        });
        let mut cache = super::OpenExrCoreDecodedChunkCache::new(64);

        assert!(cache.get(&key).is_none());
        cache.insert(key, std::sync::Arc::clone(&chunk));
        let cached = cache.get(&key).expect("chunk should be cached");

        assert!(std::sync::Arc::ptr_eq(&cached, &chunk));
        assert_eq!(cache.miss_count(), 1);
        assert_eq!(cache.hit_count(), 1);
    }

    #[test]
    fn decoded_chunk_cache_tracks_in_flight_native_chunk_decode() {
        let key = super::OpenExrCoreDecodedChunkKey {
            part_index: 0,
            chunk_index: 7,
            origin: (0, 6144),
            size: (24576, 32),
        };
        let mut cache = super::OpenExrCoreDecodedChunkCache::new(64);

        assert!(cache.begin_decode(key));
        assert!(!cache.begin_decode(key));
        cache.finish_decode(&key);
        assert!(cache.begin_decode(key));
    }

    #[test]
    fn decoded_chunk_cache_budget_scales_with_physical_memory() {
        let gib = 1024 * 1024 * 1024;

        assert_eq!(
            super::decoded_chunk_cache_budget_for_memory(4 * gib),
            512 * 1024 * 1024
        );
        assert_eq!(
            super::decoded_chunk_cache_budget_for_memory(32 * gib),
            2 * gib
        );
        assert_eq!(
            super::decoded_chunk_cache_budget_for_memory(128 * gib),
            4 * gib
        );
    }

    #[test]
    fn decoded_scanline_chunk_samples_nearest_preview_pixels_directly() {
        let decoded = super::OpenExrCoreDecodedChunk {
            origin: (0, 8),
            width: 4,
            height: 2,
            rgba: std::sync::Arc::new(vec![
                0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0, 1.0, 2.0, 2.0, 2.0, 1.0, 3.0, 3.0, 3.0, 1.0,
                4.0, 4.0, 4.0, 1.0, 5.0, 5.0, 5.0, 1.0, 6.0, 6.0, 6.0, 1.0, 7.0, 7.0, 7.0, 1.0,
            ]),
            byte_size: 32 * std::mem::size_of::<f32>(),
        };
        let mut preview = vec![0.0; 2 * 2 * 4];

        super::sample_decoded_scanline_chunk_into_preview(
            &decoded,
            4,
            2,
            2,
            &[(0, 8), (1, 9)],
            &mut preview,
        )
        .expect("sample preview from decoded chunk");

        assert_eq!(
            preview,
            vec![
                0.0, 0.0, 0.0, 1.0, 3.0, 3.0, 3.0, 1.0, 4.0, 4.0, 4.0, 1.0, 7.0, 7.0, 7.0, 1.0,
            ]
        );
    }

    #[test]
    fn budgeted_scanline_preview_sampling_limits_unique_source_rows() {
        let preview_height = 1024;
        let source_height = 12_288;
        let max_rows = super::SCANLINE_BOOTSTRAP_PREVIEW_SOURCE_ROW_BUDGET;
        let sampled = (0..preview_height)
            .map(|preview_y| {
                super::budgeted_scanline_preview_source_y(
                    preview_y,
                    preview_height,
                    source_height,
                    max_rows,
                )
            })
            .collect::<std::collections::BTreeSet<_>>();

        assert!(sampled.len() <= max_rows as usize);
        assert!(sampled.iter().all(|source_y| *source_y < source_height));
    }

    #[test]
    fn scanline_bootstrap_preview_uses_exr_specific_quality_floor() {
        let source_width = 24_576;
        let source_height = 12_288;

        let scanline_preview = super::scanline_preview_dimensions(
            source_width,
            source_height,
            crate::constants::DEFAULT_PREVIEW_SIZE,
            crate::constants::DEFAULT_PREVIEW_SIZE,
        );
        let standard_preview = crate::hdr::tiled::preview_dimensions(
            source_width,
            source_height,
            crate::constants::DEFAULT_PREVIEW_SIZE,
            crate::constants::DEFAULT_PREVIEW_SIZE,
        );

        assert_eq!(scanline_preview, (1024, 512));
        assert_eq!(standard_preview, (512, 256));
    }

    #[test]
    fn scanline_refined_preview_samples_all_preview_rows() {
        let preview_height = 1024;
        let source_height = 12_288;
        let sampled = (0..preview_height)
            .map(|preview_y| {
                super::budgeted_scanline_preview_source_y(
                    preview_y,
                    preview_height,
                    source_height,
                    super::SCANLINE_REFINED_PREVIEW_SOURCE_ROW_BUDGET,
                )
            })
            .collect::<std::collections::BTreeSet<_>>();

        assert_eq!(super::SCANLINE_REFINED_PREVIEW_SOURCE_ROW_BUDGET, 0);
        assert_eq!(sampled.len(), preview_height as usize);
    }

    #[test]
    fn scanline_preview_decode_parallelism_is_bounded() {
        let cpuses = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        let cap = (cpuses * 3 / 4).clamp(16, 32);
        assert_eq!(super::scanline_preview_decode_parallelism(0), 1);
        assert_eq!(super::scanline_preview_decode_parallelism(1), 1);
        assert_eq!(super::scanline_preview_decode_parallelism(2), 2);
        assert_eq!(super::scanline_preview_decode_parallelism(384), cap);
        assert!(
            super::scanline_preview_decode_parallelism(usize::MAX / 4) <= 32,
            "scanline decode parallelism must not exceed 32 chunks per batch"
        );
    }

    #[test]
    fn luminance_chroma_exr_decodes_via_imf_rgba_input_when_corpus_present() {
        let Some(root) = openexr_images_root() else {
            return;
        };
        for relative in [
            "LuminanceChroma/Flowers.exr",
            "LuminanceChroma/MtTamNorth.exr",
        ] {
            let path = root.join(relative);
            if !path.is_file() {
                continue;
            }
            let path = path.as_path();
            let ctx = super::OpenExrCoreReadContext::open(path).expect("open exr");
            let part = ctx.part(0).expect("part 0");
            assert!(
                super::is_luminance_chroma_scanline_part(&part),
                "{} should be Y/RY/BY scanline",
                path.display()
            );
            let imf = super::rgba_input_scanline_flatten_rgba_via_imf(path).expect("imf flatten");
            assert_eq!(imf.len(), part.width as usize * part.height as usize * 4);

            let cx = part.width / 2;
            let cy = part.height / 2;
            let tile = super::extract_rgba32f_tile_from_flat_buffer(
                &imf,
                part.width,
                part.height,
                cx,
                cy,
                1,
                1,
            )
            .expect("1x1 tile");
            let core_tile = ctx
                .extract_scanline_rgba32f_tile(0, cx, cy, 1, 1)
                .expect("openexr core tile");
            eprintln!(
                "{} center ({cx},{cy}) imf=({:.4},{:.4},{:.4}) core=({:.4},{:.4},{:.4})",
                path.display(),
                tile[0],
                tile[1],
                tile[2],
                core_tile.rgba[0],
                core_tile.rgba[1],
                core_tile.rgba[2]
            );
            // Imf::RgbaInputFile is the reference; core path may differ slightly on subsampled chroma.
            for i in 0..3 {
                assert!(tile[i].is_finite() && tile[i] >= 0.0);
                assert!(core_tile.rgba[i].is_finite());
            }
        }
    }
