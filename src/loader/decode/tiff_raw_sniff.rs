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

//! Fast TIFF IFD0 sniffing to avoid LibRaw init/open on every `.tif` in a folder.

use std::path::Path;

const TIFF_TAG_PHOTOMETRIC: u16 = 262;
const TIFF_TAG_MAKE: u16 = 271;
const TIFF_TAG_MODEL: u16 = 272;
const TIFF_TAG_CFA_REPEAT_PATTERN_DIM: u16 = 33421;
const TIFF_TAG_CFA_PATTERN: u16 = 33422;
const PHOTOMETRIC_CFA: u16 = 32803;

const TIFF_TYPE_BYTE: u16 = 1;
const TIFF_TYPE_ASCII: u16 = 2;
const TIFF_TYPE_SHORT: u16 = 3;

/// Camera vendors whose TIFF containers are often RAW (LibRaw handles demosaic).
const CAMERA_MAKE_KEYWORDS: &[&str] = &[
    "kodak",
    "canon",
    "nikon",
    "sony",
    "fujifilm",
    "fuji",
    "olympus",
    "panasonic",
    "pentax",
    "leica",
    "hasselblad",
    "phase one",
    "sigma",
    "minolta",
    "konica",
    "ricoh",
    "casio",
    "gopro",
    "samsung",
    "leaf",
    "mamiya",
    "epson",
    "red digital",
    "sinar",
];

/// Model substrings that strongly suggest TIFF-wrapped camera RAW (e.g. Kodak DCS).
const CAMERA_MODEL_RAW_KEYWORDS: &[&str] = &["dcs", "phase one", "hasselblad"];

/// Returns true when IFD0 looks like camera RAW so we should try LibRaw before libtiff.
pub(crate) fn tiff_may_be_camera_raw(path: &Path) -> bool {
    let Ok(mmap) = crate::mmap_util::map_file(path) else {
        return false;
    };
    tiff_may_be_camera_raw_bytes(mmap.as_ref())
}

/// Same as [`tiff_may_be_camera_raw`] but reuses an already-mapped file buffer.
pub(crate) fn tiff_may_be_camera_raw_bytes(bytes: &[u8]) -> bool {
    sniff_tiff_may_be_camera_raw(bytes)
}

/// IFD0 CFA photometric or CFA tags suggest LibRaw should own the file instead of libtiff RGB preview.
pub(crate) fn tiff_ifd0_suggests_libraw_raw(bytes: &[u8]) -> bool {
    let Some(ifd0) = sniff_ifd0(bytes) else {
        return false;
    };
    ifd0.photometric == Some(PHOTOMETRIC_CFA) || ifd0.has_cfa_tags
}

struct Ifd0Sniff {
    photometric: Option<u16>,
    make: Option<String>,
    model: Option<String>,
    has_cfa_tags: bool,
}

fn sniff_ifd0(bytes: &[u8]) -> Option<Ifd0Sniff> {
    let le = tiff_endianness(bytes)?;

    let ifd_offset = read_u32(bytes, 4, le)? as usize;
    if ifd_offset + 2 > bytes.len() {
        return None;
    }

    let entry_count = read_u16(bytes, ifd_offset, le)? as usize;
    let entries_start = ifd_offset + 2;
    let max_entries = entry_count.min(128);

    let mut photometric: Option<u16> = None;
    let mut make: Option<String> = None;
    let mut model: Option<String> = None;
    let mut has_cfa_tags = false;

    for i in 0..max_entries {
        let entry = entries_start + i * 12;
        if entry + 12 > bytes.len() {
            break;
        }
        let tag = read_u16(bytes, entry, le)?;
        let ty = read_u16(bytes, entry + 2, le)?;
        let count = read_u32(bytes, entry + 4, le)?;
        let value = read_u32(bytes, entry + 8, le)?;

        match tag {
            TIFF_TAG_PHOTOMETRIC => photometric = read_short_value(bytes, ty, count, value, le),
            TIFF_TAG_MAKE => make = read_ascii_value(bytes, ty, count, value, le),
            TIFF_TAG_MODEL => model = read_ascii_value(bytes, ty, count, value, le),
            TIFF_TAG_CFA_REPEAT_PATTERN_DIM | TIFF_TAG_CFA_PATTERN => has_cfa_tags = true,
            _ => {}
        }
    }

    Some(Ifd0Sniff {
        photometric,
        make,
        model,
        has_cfa_tags,
    })
}

fn sniff_tiff_may_be_camera_raw(bytes: &[u8]) -> bool {
    let Some(ifd0) = sniff_ifd0(bytes) else {
        return false;
    };

    if ifd0.photometric == Some(PHOTOMETRIC_CFA) || ifd0.has_cfa_tags {
        return true;
    }

    camera_make_or_model_suggests_raw(ifd0.make.as_deref(), ifd0.model.as_deref())
}

fn tiff_endianness(bytes: &[u8]) -> Option<bool> {
    if bytes.len() < 4 {
        return None;
    }
    match &bytes[0..2] {
        b"II" => {
            if read_u16(bytes, 2, true)? != 42 {
                return None;
            }
            Some(true)
        }
        b"MM" => {
            if read_u16(bytes, 2, false)? != 42 {
                return None;
            }
            Some(false)
        }
        _ => None,
    }
}

fn read_u16(bytes: &[u8], offset: usize, le: bool) -> Option<u16> {
    let chunk = bytes.get(offset..offset + 2)?;
    let arr: [u8; 2] = chunk.try_into().ok()?;
    Some(if le {
        u16::from_le_bytes(arr)
    } else {
        u16::from_be_bytes(arr)
    })
}

fn read_u32(bytes: &[u8], offset: usize, le: bool) -> Option<u32> {
    let chunk = bytes.get(offset..offset + 4)?;
    let arr: [u8; 4] = chunk.try_into().ok()?;
    Some(if le {
        u32::from_le_bytes(arr)
    } else {
        u32::from_be_bytes(arr)
    })
}

fn tiff_type_size(ty: u16) -> Option<usize> {
    Some(match ty {
        TIFF_TYPE_BYTE | TIFF_TYPE_ASCII => 1,
        TIFF_TYPE_SHORT => 2,
        _ => return None,
    })
}

fn read_short_value(bytes: &[u8], ty: u16, count: u32, value_field: u32, le: bool) -> Option<u16> {
    if ty != TIFF_TYPE_SHORT || count == 0 {
        return None;
    }
    if count == 1 {
        let raw = if le {
            (value_field & 0xFFFF) as u16
        } else {
            (value_field >> 16) as u16
        };
        return Some(raw);
    }
    let type_size = tiff_type_size(ty)?;
    let total = type_size.checked_mul(count as usize)?;
    if total > 4 {
        let offset = value_field as usize;
        return read_u16(bytes, offset, le);
    }
    None
}

fn read_ascii_value(
    bytes: &[u8],
    ty: u16,
    count: u32,
    value_field: u32,
    le: bool,
) -> Option<String> {
    if ty != TIFF_TYPE_ASCII || count == 0 {
        return None;
    }
    let len = count.saturating_sub(1) as usize;
    if len == 0 {
        return Some(String::new());
    }

    let raw = if (count as usize) <= 4 {
        let inline = if le {
            value_field.to_le_bytes()
        } else {
            value_field.to_be_bytes()
        };
        inline[..len.min(4)].to_vec()
    } else {
        let offset = value_field as usize;
        bytes.get(offset..offset + len)?.to_vec()
    };

    Some(String::from_utf8_lossy(&raw).trim_matches('\0').to_string())
}

fn camera_make_or_model_suggests_raw(make: Option<&str>, model: Option<&str>) -> bool {
    if let Some(make) = make {
        let lower = make.to_ascii_lowercase();
        if CAMERA_MAKE_KEYWORDS
            .iter()
            .any(|keyword| lower.contains(keyword))
        {
            return true;
        }
    }
    if let Some(model) = model {
        let lower = model.to_ascii_lowercase();
        if CAMERA_MODEL_RAW_KEYWORDS
            .iter()
            .any(|keyword| lower.contains(keyword))
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn sniff_rejects_non_tiff_bytes() {
        assert!(!sniff_tiff_may_be_camera_raw(b"not a tiff"));
        assert!(!sniff_tiff_may_be_camera_raw(&[]));
    }

    #[test]
    fn sniff_accepts_cfa_photometric_ifd0() {
        // Little-endian TIFF: magic 42, IFD at 8, one SHORT tag PhotometricInterpretation=CFA.
        let tiff: &[u8] = &[
            b'I', b'I', 42, 0, // header + version
            8, 0, 0, 0, // IFD offset
            1, 0, // 1 directory entry
            6, 1, // tag 262 (PhotometricInterpretation)
            3, 0, // type SHORT
            1, 0, 0, 0, // count 1
            35, 128, 0, 0, // value 32803 (CFA) inline, little-endian
            0, 0, 0, 0, // next IFD
        ];
        assert!(sniff_tiff_may_be_camera_raw(tiff));
    }

    #[test]
    fn sniff_rejects_rgb_photometric_without_camera_tags() {
        let tiff: &[u8] = &[
            b'I', b'I', 42, 0, 8, 0, 0, 0, 1, 0, 6, 1, 3, 0, 1, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0,
        ];
        assert!(!sniff_tiff_may_be_camera_raw(tiff));
    }

    #[test]
    fn sniff_accepts_kodak_make_at_offset() {
        // Make string lives after IFD; value field holds offset 26.
        let tiff: &[u8] = &[
            b'I', b'I', 42, 0, 8, 0, 0, 0, 1, 0, 15, 1, 2, 0, 6, 0, 0, 0, 26, 0, 0, 0, 0, 0, 0, 0,
            b'K', b'o', b'd', b'a', b'k', 0,
        ];
        assert!(sniff_tiff_may_be_camera_raw(tiff));
    }

    #[test]
    fn ifd0_suggests_libraw_raw_for_cfa_photometric() {
        let tiff: &[u8] = &[
            b'I', b'I', 42, 0, 8, 0, 0, 0, 1, 0, 6, 1, 3, 0, 1, 0, 0, 0, 35, 128, 0, 0, 0, 0, 0, 0,
        ];
        assert!(tiff_ifd0_suggests_libraw_raw(tiff));
    }

    #[test]
    fn ifd0_does_not_suggest_libraw_raw_for_kodak_make_only() {
        let tiff: &[u8] = &[
            b'I', b'I', 42, 0, 8, 0, 0, 0, 1, 0, 15, 1, 2, 0, 6, 0, 0, 0, 26, 0, 0, 0, 0, 0, 0, 0,
            b'K', b'o', b'd', b'a', b'k', 0,
        ];
        assert!(sniff_tiff_may_be_camera_raw(tiff));
        assert!(!tiff_ifd0_suggests_libraw_raw(tiff));
    }

    /// Requires `F:\win7\raws\nikon\RAW_NIKON_D800_L.TIFF`.
    #[test]
    #[ignore]
    fn probe_nikon_d800_tiff_routing() {
        let path = Path::new(r"F:\win7\raws\nikon\RAW_NIKON_D800_L.TIFF");
        if !path.is_file() {
            eprintln!("skip: {}", path.display());
            return;
        }
        let sniff = tiff_may_be_camera_raw(path);
        let probe = crate::raw_processor::probe_libraw_can_open(path);
        eprintln!("sniff={sniff} libraw_probe={probe}");
        let tone = crate::hdr::types::HdrToneMapSettings::default();
        let sdr =
            crate::libtiff_loader::load_via_libtiff(path, 1.0, tone).expect("libtiff sdr load");
        let hdr =
            crate::libtiff_loader::load_via_libtiff(path, 4.0, tone).expect("libtiff hdr load");
        if let crate::loader::ImageData::Static(d) = &sdr {
            eprintln!("libtiff cap=1 Static {}x{}", d.width, d.height);
        }
        match &hdr {
            crate::loader::ImageData::Hdr { hdr, .. } => {
                eprintln!("libtiff cap=4 HDR {}x{}", hdr.width, hdr.height);
            }
            crate::loader::ImageData::Static(d) => {
                eprintln!("libtiff cap=4 Static {}x{} (still SDR)", d.width, d.height);
            }
            _ => eprintln!("libtiff cap=4 other variant"),
        }
        if let Ok(tags) = crate::libtiff_loader::peek_tiff_tags(path) {
            eprintln!("{tags}");
        }
        assert!(
            matches!(hdr, crate::loader::ImageData::Hdr { .. }),
            "Nikon D800 camera TIFF should load as HDR when headroom > 1"
        );
    }

    /// Requires `F:\win7\raws\kodak\RAW_KODAK_DCS460D_FILEVERSION_3.TIF`.
    #[test]
    #[ignore]
    fn sniff_kodak_dcs460d_tif() {
        let path = Path::new(r"F:\win7\raws\kodak\RAW_KODAK_DCS460D_FILEVERSION_3.TIF");
        if !path.is_file() {
            eprintln!("skip: sample not at {}", path.display());
            return;
        }
        assert!(
            tiff_may_be_camera_raw(path),
            "Kodak DCS460D TIFF should match RAW sniff heuristics"
        );
    }
}
