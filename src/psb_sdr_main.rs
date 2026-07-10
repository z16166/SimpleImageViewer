// Simple Image Viewer - A high-performance, cross-platform image viewer
// Copyright (C) 2024-2026 Simple Image Viewer Contributors
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! PSD/PSB SDR main-image decode state machine.
//!
//! Drives the flattened-composite -> layer-composite -> IR-thumbnail fallback
//! (see `decode_psd_sdr_main_from_bytes_with_cancel`) from a single
//! `PsdSectionIndex` structural walk, shared by P1/P2/P3, instead of each
//! stage re-parsing the header/color-mode/image-resources/layer-mask sections
//! on its own.
//!
//! A structural index-parse failure skips P1 and P2 (there is no verified
//! `image_data_pos`/`lm_start`/`lm_end` to use) and falls back to P3 only via
//! the self-contained (re-walking) thumbnail extractor. Image Data truncation
//! is handled by P1 after the shared index has been built, so P2 can still try
//! the verified layer/mask section.

use crate::psb_section_index::PsdSectionIndex;

/// SDR main-image state machine: flattened composite -> strict layer composite
/// -> IR thumbnail -> explicit failure. Hidden layers are never opened.
///
/// P1 accepts a structurally valid flattened buffer only when it is not an
/// absolute blank (all-alpha-0 or all-RGB-0). P2 accepts a strict-visibility
/// composite only when it is not zero-information (all-alpha-0 or solid RGB
/// with variance 0). P3 accepts an IR thumbnail under the same zero-information
/// barrier as P2. All barriers are full-buffer SIMD scans.
pub fn decode_psd_sdr_main_from_bytes_with_cancel(
    bytes: &[u8],
    cancel: Option<&std::sync::atomic::AtomicBool>,
    gpu: Option<&crate::psb_layer_blend_gpu::PsdGpuContext>,
) -> Result<crate::psb_reader::PsbComposite, crate::loader::DecodeError> {
    decode_psd_sdr_main_inner(bytes, cancel, gpu, false)
}

/// Same as [`decode_psd_sdr_main_from_bytes_with_cancel`], but skips P1 flattened
/// Image Data. Used when an oversized PSB disk-tiled probe already rejected a
/// blank (or unreadable) flat and must not re-decode the full canvas.
pub fn decode_psd_sdr_main_skip_flattened_with_cancel(
    bytes: &[u8],
    cancel: Option<&std::sync::atomic::AtomicBool>,
    gpu: Option<&crate::psb_layer_blend_gpu::PsdGpuContext>,
) -> Result<crate::psb_reader::PsbComposite, crate::loader::DecodeError> {
    decode_psd_sdr_main_inner(bytes, cancel, gpu, true)
}

fn decode_psd_sdr_main_inner(
    bytes: &[u8],
    cancel: Option<&std::sync::atomic::AtomicBool>,
    gpu: Option<&crate::psb_layer_blend_gpu::PsdGpuContext>,
    skip_flattened: bool,
) -> Result<crate::psb_reader::PsbComposite, crate::loader::DecodeError> {
    // Single structural walk feeds P1 (image_data_pos), P2 (lm_start/lm_end),
    // and P3 (ir_start/ir_end); every stage below reuses this same index.
    let index = match PsdSectionIndex::parse(bytes) {
        Ok(index) => index,
        Err(e) if PsdSectionIndex::is_structural_error(&e) => {
            crate::preload_debug!("[PreloadDebug][PsdSdrMain] stage=P1_fail err={e}");
            log::debug!("PSD SDR main P1 flattened decode failed: {e}");
            // Header/structural failures cannot be recovered by P2; go straight to P3.
            crate::preload_debug!("[PreloadDebug][PsdSdrMain] stage=P1_structural_fail -> skip_P2");
            log::debug!("PSD SDR main: skipping P2 after structural header failure");
            return decode_psd_sdr_main_p3_only(bytes, cancel);
        }
        // Unexpected non-structural index failures still leave no valid shared
        // index to drive P1/P2/P3 from. Fail closed instead of re-walking the
        // file through the legacy self-contained paths.
        Err(e) => return Err(e.into()),
    };

    // P1: structurally valid flattened Image Data, then absolute blank barrier.
    if skip_flattened {
        crate::preload_debug!("[PreloadDebug][PsdSdrMain] stage=P1_skipped -> degrade_P2");
        log::debug!(
            "PSD SDR main: skipping P1 flattened (caller already rejected blank/unreadable flat)"
        );
    } else {
        match crate::psb_reader::read_composite_from_index(&index, bytes, cancel) {
            Ok(composite) => {
                let absolutely_blank = crate::psb_reader::rgba8_is_absolutely_blank_with_cancel(
                    &composite.pixels,
                    cancel,
                )?;
                if absolutely_blank {
                    crate::preload_debug!(
                        "[PreloadDebug][PsdSdrMain] stage=P1_absolute_blank {}x{} \
                         pixels={} -> degrade_P2",
                        composite.width,
                        composite.height,
                        composite.pixels.len()
                    );
                    log::debug!(
                        "PSD SDR main: P1 flattened {}x{} is absolute blank \
                         (all-transparent or all-RGB-0); degrading to P2",
                        composite.width,
                        composite.height
                    );
                } else {
                    crate::preload_debug!(
                        "[PreloadDebug][PsdSdrMain] stage=P1_flattened {}x{} pixels={}",
                        composite.width,
                        composite.height,
                        composite.pixels.len()
                    );
                    log::debug!(
                        "PSD SDR main: P1 flattened composite {}x{}",
                        composite.width,
                        composite.height
                    );
                    return Ok(composite);
                }
            }
            Err(e) if e.is_cancelled() => return Err(e),
            Err(e) => {
                crate::preload_debug!("[PreloadDebug][PsdSdrMain] stage=P1_fail err={e}");
                log::debug!("PSD SDR main P1 flattened decode failed: {e}");
            }
        }
    }

    // P1 -> P2: poll cancel after absolute-blank degrade (or P1 fail) before P2 work.
    crate::psb_reader::check_decode_cancel(cancel)?;

    // P2: strict visibility layer composite, then zero-information barrier.
    // Only reachable with a structurally valid `index` -- never falls back to
    // the legacy self-contained `composite_layers_from_bytes_with_cancel`.
    let mut p2_no_drawable_visible = false;
    match crate::psb_layer_composite::composite_layers_from_index(&index, bytes, cancel, gpu) {
        Ok(composite) => {
            let zero_info = crate::psb_reader::rgba8_is_zero_information_with_cancel(
                &composite.pixels,
                cancel,
            )?;
            if zero_info {
                crate::preload_debug!(
                    "[PreloadDebug][PsdSdrMain] stage=P2_zero_information {}x{} \
                     pixels={} -> degrade_P3",
                    composite.width,
                    composite.height,
                    composite.pixels.len()
                );
                log::debug!(
                    "PSD SDR main: P2 strict composite {}x{} is zero-information \
                     (all-transparent or solid RGB); degrading to P3",
                    composite.width,
                    composite.height
                );
            } else {
                crate::preload_debug!(
                    "[PreloadDebug][PsdSdrMain] stage=P2_strict_layers {}x{} pixels={}",
                    composite.width,
                    composite.height,
                    composite.pixels.len()
                );
                log::debug!(
                    "PSD SDR main: P2 strict layer composite {}x{}",
                    composite.width,
                    composite.height
                );
                return Ok(composite);
            }
        }
        Err(e) if e.is_cancelled() => return Err(e),
        Err(e) => {
            p2_no_drawable_visible = e.is_no_drawable_visible_layers();
            crate::preload_debug!("[PreloadDebug][PsdSdrMain] stage=P2_fail err={e}");
            log::debug!("PSD SDR main P2 layer composite unavailable: {e}");
        }
    }

    // P3: embedded Photoshop IR thumbnail (via the already-parsed index's
    // ir_start/ir_end), then zero-information barrier.
    crate::psb_reader::check_decode_cancel(cancel)?;
    match crate::psb_reader::extract_photoshop_thumbnail_from_ir(
        bytes,
        index.ir_start,
        index.ir_end,
    ) {
        Some(thumb) => {
            crate::psb_reader::check_decode_cancel(cancel)?;
            let zero_info =
                crate::psb_reader::rgba8_is_zero_information_with_cancel(&thumb.pixels, cancel)?;
            if zero_info {
                crate::preload_debug!(
                    "[PreloadDebug][PsdSdrMain] stage=P3_zero_information {}x{} \
                     pixels={} -> fail",
                    thumb.width,
                    thumb.height,
                    thumb.pixels.len()
                );
                log::debug!(
                    "PSD SDR main: P3 IR thumbnail {}x{} is zero-information \
                     (all-transparent or solid RGB); no displayable image",
                    thumb.width,
                    thumb.height
                );
            } else {
                crate::preload_debug!(
                    "[PreloadDebug][PsdSdrMain] stage=P3_ir_thumbnail {}x{} pixels={}",
                    thumb.width,
                    thumb.height,
                    thumb.pixels.len()
                );
                log::debug!(
                    "PSD SDR main: P3 IR thumbnail {}x{}",
                    thumb.width,
                    thumb.height
                );
                return Ok(thumb);
            }
        }
        None => {
            crate::preload_debug!("[PreloadDebug][PsdSdrMain] stage=P3_fail no_ir_thumbnail");
            log::debug!("PSD SDR main P3: no embedded IR thumbnail");
        }
    }

    crate::preload_debug!("[PreloadDebug][PsdSdrMain] stage=fail no_p1_p2_p3");
    if p2_no_drawable_visible {
        return Err(rust_i18n::t!("error.psd_all_layers_hidden")
            .to_string()
            .into());
    }
    Err(rust_i18n::t!("error.psd_no_displayable_image")
        .to_string()
        .into())
}

/// P3-only path used when [`PsdSectionIndex::parse`] fails structurally: there
/// is no valid index to resolve P1's Image Data or P2's layer/mask sections,
/// so both are skipped entirely. P3 falls back to the self-contained
/// (re-walking) thumbnail extractor since there is no `ir_start`/`ir_end` to
/// reuse from an index that never parsed.
fn decode_psd_sdr_main_p3_only(
    bytes: &[u8],
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<crate::psb_reader::PsbComposite, crate::loader::DecodeError> {
    crate::psb_reader::check_decode_cancel(cancel)?;
    match crate::psb_reader::try_extract_photoshop_thumbnail(bytes) {
        Some(thumb) => {
            crate::psb_reader::check_decode_cancel(cancel)?;
            let zero_info =
                crate::psb_reader::rgba8_is_zero_information_with_cancel(&thumb.pixels, cancel)?;
            if zero_info {
                crate::preload_debug!(
                    "[PreloadDebug][PsdSdrMain] stage=P3_zero_information {}x{} \
                     pixels={} -> fail",
                    thumb.width,
                    thumb.height,
                    thumb.pixels.len()
                );
                log::debug!(
                    "PSD SDR main: P3 IR thumbnail {}x{} is zero-information \
                     (all-transparent or solid RGB); no displayable image",
                    thumb.width,
                    thumb.height
                );
            } else {
                crate::preload_debug!(
                    "[PreloadDebug][PsdSdrMain] stage=P3_ir_thumbnail {}x{} pixels={}",
                    thumb.width,
                    thumb.height,
                    thumb.pixels.len()
                );
                log::debug!(
                    "PSD SDR main: P3 IR thumbnail {}x{}",
                    thumb.width,
                    thumb.height
                );
                return Ok(thumb);
            }
        }
        None => {
            crate::preload_debug!("[PreloadDebug][PsdSdrMain] stage=P3_fail no_ir_thumbnail");
            log::debug!("PSD SDR main P3: no embedded IR thumbnail");
        }
    }

    crate::preload_debug!("[PreloadDebug][PsdSdrMain] stage=fail no_p1_p2_p3");
    Err(rust_i18n::t!("error.psd_no_displayable_image")
        .to_string()
        .into())
}

#[cfg(test)]
mod tests {
    use super::{PsdSectionIndex, decode_psd_sdr_main_from_bytes_with_cancel};
    use std::path::Path;

    fn tiny_raw_rgb_psd(width: u32, height: u32, rgb_planar: &[u8]) -> Vec<u8> {
        assert_eq!(rgb_planar.len(), (width * height * 3) as usize);
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"8BPS");
        bytes.extend_from_slice(&1u16.to_be_bytes()); // version = PSD
        bytes.extend_from_slice(&[0u8; 6]); // reserved
        bytes.extend_from_slice(&3u16.to_be_bytes()); // channels
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&8u16.to_be_bytes()); // depth
        bytes.extend_from_slice(&3u16.to_be_bytes()); // color mode (RGB)
        bytes.extend_from_slice(&0u32.to_be_bytes()); // color mode data length
        bytes.extend_from_slice(&0u32.to_be_bytes()); // image resources length
        bytes.extend_from_slice(&0u32.to_be_bytes()); // layer and mask info length
        bytes.extend_from_slice(&0u16.to_be_bytes()); // Image Data compression = Raw
        bytes.extend_from_slice(rgb_planar);
        bytes
    }

    #[test]
    fn decode_psd_sdr_main_decodes_tiny_raw_rgb_flattened() {
        let bytes = tiny_raw_rgb_psd(1, 1, &[0x10, 0x20, 0x30]);

        let main = decode_psd_sdr_main_from_bytes_with_cancel(&bytes, None, None)
            .expect("tiny raw RGB PSD should decode through P1");

        assert_eq!((main.width, main.height), (1, 1));
        assert_eq!(main.pixels, vec![0x10, 0x20, 0x30, 0xFF]);
    }

    #[test]
    fn decode_psd_sdr_main_degrades_to_p2_when_image_data_compression_missing() {
        // Header + color-mode + image-resources + layer-mask sections are all
        // present (zero-length), but the file ends right before the 2-byte
        // Image Data compression field. The shared index is still valid for P2
        // because layer/mask offsets are known; only P1 should fail here.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"8BPS");
        bytes.extend_from_slice(&1u16.to_be_bytes()); // version = PSD
        bytes.extend_from_slice(&[0u8; 6]); // reserved
        bytes.extend_from_slice(&3u16.to_be_bytes()); // channels
        bytes.extend_from_slice(&1u32.to_be_bytes()); // height
        bytes.extend_from_slice(&1u32.to_be_bytes()); // width
        bytes.extend_from_slice(&8u16.to_be_bytes()); // depth
        bytes.extend_from_slice(&3u16.to_be_bytes()); // color mode (RGB)
        bytes.extend_from_slice(&0u32.to_be_bytes()); // color mode data length
        bytes.extend_from_slice(&0u32.to_be_bytes()); // image resources length
        bytes.extend_from_slice(&0u32.to_be_bytes()); // layer and mask info length
        // No trailing Image Data compression u16 -- file ends here.

        let index = PsdSectionIndex::parse(&bytes)
            .expect("missing Image Data compression must not reject the shared index");
        assert_eq!(index.image_data_pos, bytes.len() as u64);
        let p1_err = index.image_data_compression(&bytes).unwrap_err();
        assert_eq!(p1_err, "PSD/PSB Image Data compression truncated");

        let err = decode_psd_sdr_main_from_bytes_with_cancel(&bytes, None, None)
            .expect_err("P1 failure should degrade to P2/P3 fallback");
        let expected = rust_i18n::t!("error.psd_all_layers_hidden").to_string();
        assert_eq!(err.as_str(), expected);
        assert_ne!(err.as_str(), p1_err);
    }

    #[test]
    fn decode_01_02_psd_sdr_main_returns_structurally_valid_image() {
        // Flattened Image Data may be a solid-ish placeholder; under the SDR
        // state machine that is still a valid P1 result (no pixel heuristics).
        let path = Path::new(r"F:\BaiduNetdiskDownload\素材库\45套 psd企业画册模板\12\01-02.psd");
        if !path.is_file() {
            eprintln!("skipping decode_01_02_psd_sdr_main...; sample missing");
            return;
        }
        let bytes = std::fs::read(path).unwrap();
        let main = decode_psd_sdr_main_from_bytes_with_cancel(&bytes, None, None).expect("main");
        assert_eq!((main.width, main.height), (5031, 3437));
        assert_eq!(main.pixels.len(), 5031 * 3437 * 4);
    }

    #[test]
    fn decode_psd_sdr_main_all_hidden_reports_photoshop_hint() {
        let path = Path::new(r"F:\BaiduNetdiskDownload\素材库\45套 psd企业画册模板\18\18\1-2.psd");
        if !path.is_file() {
            eprintln!("skipping decode_psd_sdr_main_all_hidden...; sample missing");
            return;
        }
        let bytes = std::fs::read(path).expect("read");
        let err = decode_psd_sdr_main_from_bytes_with_cancel(&bytes, None, None)
            .expect_err("expected fail when all layers hidden and P3 is blank");
        let expected = rust_i18n::t!("error.psd_all_layers_hidden").to_string();
        assert_eq!(err.as_str(), expected);
        assert!(
            err.as_str().contains("designer")
                || err.as_str().contains("设计师")
                || err.as_str().contains("設計師"),
            "error should attribute hidden layers to the designer: {err}"
        );
        assert!(
            err.as_str().contains("Photoshop"),
            "error should point users to Photoshop: {err}"
        );
    }

    #[test]
    fn decode_psd_sdr_main_prefers_structurally_valid_flattened() {
        // 10.psd has a usable flattened composite -- P1 must win even if layers exist.
        let path = Path::new(r"F:\BaiduNetdiskDownload\素材库\45套 psd企业画册模板\10\10.psd");
        if !path.is_file() {
            eprintln!(
                "skipping decode_psd_sdr_main_prefers_structurally_valid_flattened; sample missing"
            );
            return;
        }
        let bytes = std::fs::read(path).expect("read");
        let flat = crate::psb_reader::read_composite_from_bytes(&bytes).expect("flat");
        let main = decode_psd_sdr_main_from_bytes_with_cancel(&bytes, None, None).expect("main");
        assert_eq!((main.width, main.height), (flat.width, flat.height));
        assert_eq!(main.pixels, flat.pixels);
    }
}
