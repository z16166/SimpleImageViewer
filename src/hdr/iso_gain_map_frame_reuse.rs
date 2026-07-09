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

//! Frame-level ISO gain-map plane reuse for HDR animations (AVIF sequence / JXL jhgm).

use std::sync::Arc;

use crate::hdr::gain_map::GainMapMetadata;

/// Max per-channel abs diff (u8) for [`rgba8_planes_within_threshold`].
pub(crate) const ISO_GAIN_MAP_FRAME_DIFF_MAX_ABS: u8 = 1;

const FNV_OFFSET: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;

/// Cache key: headroom capacities + shaping fingerprint + display target capacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IsoGainMapReuseKey {
    pub(crate) hdr_capacity_min_bits: u32,
    pub(crate) hdr_capacity_max_bits: u32,
    pub(crate) target_hdr_capacity_bits: u32,
    pub(crate) metadata_fingerprint: u64,
}

impl IsoGainMapReuseKey {
    pub(crate) fn from_metadata(metadata: GainMapMetadata, target_hdr_capacity: f32) -> Self {
        Self {
            hdr_capacity_min_bits: metadata.hdr_capacity_min.to_bits(),
            hdr_capacity_max_bits: metadata.hdr_capacity_max.to_bits(),
            target_hdr_capacity_bits: target_hdr_capacity.to_bits(),
            metadata_fingerprint: fingerprint_gain_map_metadata(metadata),
        }
    }
}

/// When a matching key alone may skip decoding a new gain plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IsoGainMapGainDecodePolicy {
    /// JXL: file-level `jhgm` box is fixed; key match skips gain decode.
    KeyMatchSkipsGainDecode,
    /// AVIF: skip gain decode only when key matches and SDR is within threshold.
    KeyAndSdrMatchSkipsGainDecode,
}

/// Previous-frame ISO deferred planes for animation decode reuse.
#[derive(Debug, Clone)]
pub(crate) struct IsoGainMapFrameReuse {
    pub(crate) key: IsoGainMapReuseKey,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) sdr_rgba: Arc<Vec<u8>>,
    pub(crate) gain_rgba: Arc<Vec<u8>>,
    pub(crate) gain_width: u32,
    pub(crate) gain_height: u32,
    pub(crate) metadata: GainMapMetadata,
}

/// Planes selected for attach after reuse / diff decisions.
#[derive(Debug, Clone)]
pub(crate) struct SelectedIsoPlanes {
    pub(crate) sdr_rgba: Arc<Vec<u8>>,
    pub(crate) gain_rgba: Arc<Vec<u8>>,
    pub(crate) gain_width: u32,
    pub(crate) gain_height: u32,
    pub(crate) metadata: GainMapMetadata,
    /// True when the caller must not decode a new gain plane (reuse already applied).
    pub(crate) skipped_gain_decode: bool,
    /// True when gain was requested but not provided and policy forbids skip -- caller must decode.
    pub(crate) needs_gain_decode: bool,
}

/// Stable fingerprint over shaping fields (not headroom -- those are in the key bits).
pub(crate) fn fingerprint_gain_map_metadata(metadata: GainMapMetadata) -> u64 {
    let mut hash = FNV_OFFSET;
    for v in metadata
        .gain_map_min
        .iter()
        .chain(metadata.gain_map_max.iter())
        .chain(metadata.gamma.iter())
        .chain(metadata.offset_sdr.iter())
        .chain(metadata.offset_hdr.iter())
    {
        hash = fnv1a_u32(hash, v.to_bits());
    }
    hash = fnv1a_u8(hash, u8::from(metadata.backward_direction));
    hash
}

#[inline]
fn fnv1a_u32(mut hash: u64, value: u32) -> u64 {
    for byte in value.to_le_bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

#[inline]
fn fnv1a_u8(mut hash: u64, value: u8) -> u64 {
    hash ^= u64::from(value);
    hash.wrapping_mul(FNV_PRIME)
}

/// True when every byte differs by at most `max_abs`.
pub(crate) fn rgba8_planes_within_threshold(a: &[u8], b: &[u8], max_abs: u8) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .all(|(x, y)| x.abs_diff(*y) <= max_abs)
}

/// Decide whether gain decode can be skipped before decoding (probe only).
pub(crate) fn iso_gain_map_may_skip_gain_decode(
    reuse: &Option<IsoGainMapFrameReuse>,
    policy: IsoGainMapGainDecodePolicy,
    width: u32,
    height: u32,
    new_sdr: &[u8],
    metadata: GainMapMetadata,
    target_hdr_capacity: f32,
) -> bool {
    let Some(prev) = reuse.as_ref() else {
        return false;
    };
    if prev.width != width || prev.height != height {
        return false;
    }
    let key = IsoGainMapReuseKey::from_metadata(metadata, target_hdr_capacity);
    if prev.key != key {
        return false;
    }
    match policy {
        IsoGainMapGainDecodePolicy::KeyMatchSkipsGainDecode => true,
        IsoGainMapGainDecodePolicy::KeyAndSdrMatchSkipsGainDecode => {
            rgba8_planes_within_threshold(
                new_sdr,
                prev.sdr_rgba.as_slice(),
                ISO_GAIN_MAP_FRAME_DIFF_MAX_ABS,
            )
        }
    }
}

/// Select SDR/gain Arcs for this frame and update `reuse`.
///
/// When `new_gain` is `None` and policy allows skip, returns previous gain with
/// `skipped_gain_decode = true`. When `new_gain` is `None` and skip is not allowed,
/// returns `needs_gain_decode = true` without updating reuse (caller must decode and retry).
pub(crate) fn select_iso_gain_map_planes(
    reuse: &mut Option<IsoGainMapFrameReuse>,
    policy: IsoGainMapGainDecodePolicy,
    width: u32,
    height: u32,
    new_sdr: Vec<u8>,
    new_gain: Option<(u32, u32, Vec<u8>)>,
    metadata: GainMapMetadata,
    target_hdr_capacity: f32,
) -> SelectedIsoPlanes {
    let key = IsoGainMapReuseKey::from_metadata(metadata, target_hdr_capacity);
    let sdr_matches_prev = reuse.as_ref().is_some_and(|prev| {
        prev.width == width
            && prev.height == height
            && prev.key == key
            && rgba8_planes_within_threshold(
                &new_sdr,
                prev.sdr_rgba.as_slice(),
                ISO_GAIN_MAP_FRAME_DIFF_MAX_ABS,
            )
    });
    let key_matches_prev = reuse.as_ref().is_some_and(|prev| {
        prev.width == width && prev.height == height && prev.key == key
    });

    let may_skip = match policy {
        IsoGainMapGainDecodePolicy::KeyMatchSkipsGainDecode => key_matches_prev,
        IsoGainMapGainDecodePolicy::KeyAndSdrMatchSkipsGainDecode => sdr_matches_prev,
    };

    if new_gain.is_none() {
        if may_skip {
            let prev = reuse.as_ref().expect("may_skip implies reuse present");
            let sdr_rgba = if sdr_matches_prev {
                Arc::clone(&prev.sdr_rgba)
            } else {
                Arc::new(new_sdr)
            };
            let selected = SelectedIsoPlanes {
                sdr_rgba: Arc::clone(&sdr_rgba),
                gain_rgba: Arc::clone(&prev.gain_rgba),
                gain_width: prev.gain_width,
                gain_height: prev.gain_height,
                metadata,
                skipped_gain_decode: true,
                needs_gain_decode: false,
            };
            *reuse = Some(IsoGainMapFrameReuse {
                key,
                width,
                height,
                sdr_rgba,
                gain_rgba: Arc::clone(&selected.gain_rgba),
                gain_width: selected.gain_width,
                gain_height: selected.gain_height,
                metadata,
            });
            return selected;
        }
        return SelectedIsoPlanes {
            sdr_rgba: Arc::new(new_sdr),
            gain_rgba: Arc::new(Vec::new()),
            gain_width: 0,
            gain_height: 0,
            metadata,
            skipped_gain_decode: false,
            needs_gain_decode: true,
        };
    }

    let (gain_width, gain_height, gain_vec) = new_gain.expect("checked is_some");
    let prev = reuse.as_ref();
    let reuse_sdr = prev.is_some_and(|p| {
        p.width == width
            && p.height == height
            && p.key == key
            && rgba8_planes_within_threshold(
                &new_sdr,
                p.sdr_rgba.as_slice(),
                ISO_GAIN_MAP_FRAME_DIFF_MAX_ABS,
            )
    });
    let reuse_gain = prev.is_some_and(|p| {
        p.width == width
            && p.height == height
            && p.key == key
            && p.gain_width == gain_width
            && p.gain_height == gain_height
            && rgba8_planes_within_threshold(
                &gain_vec,
                p.gain_rgba.as_slice(),
                ISO_GAIN_MAP_FRAME_DIFF_MAX_ABS,
            )
    });

    let sdr_rgba = if reuse_sdr {
        Arc::clone(&prev.expect("reuse_sdr").sdr_rgba)
    } else {
        Arc::new(new_sdr)
    };
    let gain_rgba = if reuse_gain {
        Arc::clone(&prev.expect("reuse_gain").gain_rgba)
    } else {
        Arc::new(gain_vec)
    };

    let selected = SelectedIsoPlanes {
        sdr_rgba: Arc::clone(&sdr_rgba),
        gain_rgba: Arc::clone(&gain_rgba),
        gain_width,
        gain_height,
        metadata,
        skipped_gain_decode: false,
        needs_gain_decode: false,
    };
    *reuse = Some(IsoGainMapFrameReuse {
        key,
        width,
        height,
        sdr_rgba,
        gain_rgba,
        gain_width,
        gain_height,
        metadata,
    });
    selected
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_metadata() -> GainMapMetadata {
        GainMapMetadata {
            gain_map_min: [0.0; 3],
            gain_map_max: [1.0; 3],
            gamma: [1.0; 3],
            offset_sdr: [0.0; 3],
            offset_hdr: [0.0; 3],
            hdr_capacity_min: 1.0,
            hdr_capacity_max: 4.0,
            backward_direction: false,
        }
    }

    fn solid_rgba(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..(w * h) {
            out.extend_from_slice(&rgba);
        }
        out
    }

    #[test]
    fn identical_planes_share_arcs() {
        let mut reuse = None;
        let meta = test_metadata();
        let sdr = solid_rgba(2, 2, [10, 20, 30, 255]);
        let gain = solid_rgba(1, 1, [128, 128, 128, 255]);
        let first = select_iso_gain_map_planes(
            &mut reuse,
            IsoGainMapGainDecodePolicy::KeyMatchSkipsGainDecode,
            2,
            2,
            sdr.clone(),
            Some((1, 1, gain.clone())),
            meta,
            4.0,
        );
        let second = select_iso_gain_map_planes(
            &mut reuse,
            IsoGainMapGainDecodePolicy::KeyMatchSkipsGainDecode,
            2,
            2,
            sdr,
            Some((1, 1, gain)),
            meta,
            4.0,
        );
        assert!(Arc::ptr_eq(&first.sdr_rgba, &second.sdr_rgba));
        assert!(Arc::ptr_eq(&first.gain_rgba, &second.gain_rgba));
        assert!(!second.needs_gain_decode);
    }

    #[test]
    fn jxl_policy_skips_gain_decode_on_key_match_even_if_sdr_differs() {
        let mut reuse = None;
        let meta = test_metadata();
        let sdr0 = solid_rgba(2, 2, [10, 20, 30, 255]);
        let sdr1 = solid_rgba(2, 2, [200, 20, 30, 255]);
        let gain = solid_rgba(1, 1, [128, 128, 128, 255]);
        let first = select_iso_gain_map_planes(
            &mut reuse,
            IsoGainMapGainDecodePolicy::KeyMatchSkipsGainDecode,
            2,
            2,
            sdr0,
            Some((1, 1, gain)),
            meta,
            4.0,
        );
        assert!(iso_gain_map_may_skip_gain_decode(
            &reuse,
            IsoGainMapGainDecodePolicy::KeyMatchSkipsGainDecode,
            2,
            2,
            &sdr1,
            meta,
            4.0,
        ));
        let second = select_iso_gain_map_planes(
            &mut reuse,
            IsoGainMapGainDecodePolicy::KeyMatchSkipsGainDecode,
            2,
            2,
            sdr1,
            None,
            meta,
            4.0,
        );
        assert!(second.skipped_gain_decode);
        assert!(!second.needs_gain_decode);
        assert!(Arc::ptr_eq(&first.gain_rgba, &second.gain_rgba));
        assert!(!Arc::ptr_eq(&first.sdr_rgba, &second.sdr_rgba));
    }

    #[test]
    fn avif_policy_requires_sdr_match_to_skip_gain_decode() {
        let mut reuse = None;
        let meta = test_metadata();
        let sdr0 = solid_rgba(2, 2, [10, 20, 30, 255]);
        let sdr1 = solid_rgba(2, 2, [200, 20, 30, 255]);
        let gain = solid_rgba(1, 1, [128, 128, 128, 255]);
        let _ = select_iso_gain_map_planes(
            &mut reuse,
            IsoGainMapGainDecodePolicy::KeyAndSdrMatchSkipsGainDecode,
            2,
            2,
            sdr0,
            Some((1, 1, gain.clone())),
            meta,
            4.0,
        );
        assert!(!iso_gain_map_may_skip_gain_decode(
            &reuse,
            IsoGainMapGainDecodePolicy::KeyAndSdrMatchSkipsGainDecode,
            2,
            2,
            &sdr1,
            meta,
            4.0,
        ));
        let probe = select_iso_gain_map_planes(
            &mut reuse,
            IsoGainMapGainDecodePolicy::KeyAndSdrMatchSkipsGainDecode,
            2,
            2,
            sdr1.clone(),
            None,
            meta,
            4.0,
        );
        assert!(probe.needs_gain_decode);
        assert!(!probe.skipped_gain_decode);

        let after = select_iso_gain_map_planes(
            &mut reuse,
            IsoGainMapGainDecodePolicy::KeyAndSdrMatchSkipsGainDecode,
            2,
            2,
            sdr1,
            Some((1, 1, gain)),
            meta,
            4.0,
        );
        assert!(!after.needs_gain_decode);
        // Same gain content -> Arc reuse after decode+diff.
        assert!(reuse.is_some());
    }

    #[test]
    fn key_change_prevents_reuse() {
        let mut reuse = None;
        let mut meta = test_metadata();
        let sdr = solid_rgba(1, 1, [1, 2, 3, 255]);
        let gain = solid_rgba(1, 1, [4, 5, 6, 255]);
        let first = select_iso_gain_map_planes(
            &mut reuse,
            IsoGainMapGainDecodePolicy::KeyMatchSkipsGainDecode,
            1,
            1,
            sdr.clone(),
            Some((1, 1, gain.clone())),
            meta,
            4.0,
        );
        meta.hdr_capacity_max = 8.0;
        let second = select_iso_gain_map_planes(
            &mut reuse,
            IsoGainMapGainDecodePolicy::KeyMatchSkipsGainDecode,
            1,
            1,
            sdr,
            Some((1, 1, gain)),
            meta,
            4.0,
        );
        assert!(!Arc::ptr_eq(&first.sdr_rgba, &second.sdr_rgba));
        assert!(!Arc::ptr_eq(&first.gain_rgba, &second.gain_rgba));
    }

    #[test]
    fn threshold_boundary_abs_diff() {
        let a = vec![10_u8, 20, 30, 255];
        let b_ok = vec![11_u8, 20, 30, 255];
        let b_bad = vec![12_u8, 20, 30, 255];
        assert!(rgba8_planes_within_threshold(
            &a,
            &b_ok,
            ISO_GAIN_MAP_FRAME_DIFF_MAX_ABS
        ));
        assert!(!rgba8_planes_within_threshold(
            &a,
            &b_bad,
            ISO_GAIN_MAP_FRAME_DIFF_MAX_ABS
        ));
    }

    #[test]
    fn clear_reuse_stops_sharing() {
        let mut reuse = None;
        let meta = test_metadata();
        let sdr = solid_rgba(1, 1, [9, 9, 9, 255]);
        let gain = solid_rgba(1, 1, [1, 1, 1, 255]);
        let first = select_iso_gain_map_planes(
            &mut reuse,
            IsoGainMapGainDecodePolicy::KeyMatchSkipsGainDecode,
            1,
            1,
            sdr.clone(),
            Some((1, 1, gain.clone())),
            meta,
            4.0,
        );
        reuse = None;
        let second = select_iso_gain_map_planes(
            &mut reuse,
            IsoGainMapGainDecodePolicy::KeyMatchSkipsGainDecode,
            1,
            1,
            sdr,
            Some((1, 1, gain)),
            meta,
            4.0,
        );
        assert!(!Arc::ptr_eq(&first.sdr_rgba, &second.sdr_rgba));
        assert!(!Arc::ptr_eq(&first.gain_rgba, &second.gain_rgba));
    }
}
