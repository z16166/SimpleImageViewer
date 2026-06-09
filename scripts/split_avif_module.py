#!/usr/bin/env python3
"""Split src/hdr/avif.rs into src/hdr/avif/ submodule (one-shot helper)."""

from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SRC = ROOT / "_split_avif_src.rs"
OUT = ROOT / "src" / "hdr" / "avif"

HEADER = """// Simple Image Viewer - A high-performance, cross-platform image viewer
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

"""


def slice_lines(lines: list[str], start: int, end: int) -> str:
    return "".join(lines[start - 1 : end])


def main() -> None:
    text = SRC.read_text(encoding="utf-8")
    lines = text.splitlines(keepends=True)

    OUT.mkdir(parents=True, exist_ok=True)

    (OUT / "brand.rs").write_text(
        HEADER
        + slice_lines(lines, 33, 36)
        + "\n",
        encoding="utf-8",
    )

    (OUT / "metadata.rs").write_text(
        HEADER
        + "use crate::hdr::types::HdrImageMetadata;\n\n"
        + slice_lines(lines, 274, 288)
        + "\n"
        + slice_lines(lines, 1053, 1068)
        + "\n"
        + slice_lines(lines, 654, 688).replace(
            "fn avif_yuv_to_rgb_output_metadata", "pub(crate) fn avif_yuv_to_rgb_output_metadata", 1
        ).replace("trait AvifMetadataExt", "pub(crate) trait AvifMetadataExt", 1)
        + "\n",
        encoding="utf-8",
    )

    (OUT / "orientation.rs").write_text(
        HEADER
        + "#![cfg(feature = \"avif-native\")]\n\n"
        + slice_lines(lines, 180, 182)
        + "\n"
        + slice_lines(lines, 190, 272)
        + "\n",
        encoding="utf-8",
    )

    (OUT / "gain_map.rs").write_text(
        HEADER
        + "#![cfg(feature = \"avif-native\")]\n\n"
        + "use crate::hdr::gain_map::{GainMapMetadata, IsoGainMapFraction};\n\n"
        + "use super::decode::{decode_avif_image_rgba_u16, rgb_channel_max_f};\n\n"
        + slice_lines(lines, 988, 1050).replace(
            "fn decode_avif_gain_map", "pub(crate) fn decode_avif_gain_map", 1
        )
        + "\n#[cfg(test)]\n"
        + "pub(crate) fn test_signed_fraction(n: i32, d: u32) -> libavif_sys::avifSignedFraction {\n"
        + "    libavif_sys::avifSignedFraction { n, d }\n"
        + "}\n\n"
        + "#[cfg(test)]\n"
        + "pub(crate) fn test_unsigned_fraction(n: u32, d: u32) -> libavif_sys::avifUnsignedFraction {\n"
        + "    libavif_sys::avifUnsignedFraction { n, d }\n"
        + "}\n",
        encoding="utf-8",
    )

    decode_body = (
        "#![cfg(feature = \"avif-native\")]\n\n"
        + "use std::ffi::CStr;\n\n"
        + "use crate::hdr::types::HdrImageBuffer;\n\n"
        + slice_lines(lines, 39, 47)  # avif_ftyp_major_brand
        + "\n"
        + slice_lines(lines, 50, 58)  # libavif_result_to_string
        + "\n"
        + slice_lines(lines, 292, 389)  # decode_avif_hdr* through read
        + slice_lines(lines, 536, 650)  # icc + alpha helpers
        + slice_lines(lines, 609, 614)  # rgb_channel_max_f (duplicate order fix below)
    )
    # rebuild decode without duplicate rgb_channel_max_f
    decode_body = (
        "#![cfg(feature = \"avif-native\")]\n\n"
        + "use std::ffi::CStr;\n\n"
        + "use crate::hdr::types::HdrImageBuffer;\n\n"
        + slice_lines(lines, 39, 47)
        + "\n"
        + slice_lines(lines, 50, 58)
        + "\n"
        + slice_lines(lines, 292, 389)
        + slice_lines(lines, 536, 650)
        + slice_lines(lines, 696, 985)  # yuv/rgb path; make decode_avif_image_rgba_u16 pub(crate)
    )
    decode_body = decode_body.replace(
        "fn decode_avif_image_rgba_u16",
        "pub(crate) fn decode_avif_image_rgba_u16",
    )
    decode_body = decode_body.replace(
        "fn rgb_channel_max_f",
        "pub(crate) fn rgb_channel_max_f",
    )
    decode_body = decode_body.replace(
        "fn avif_ftyp_major_brand",
        "pub(crate) fn avif_ftyp_major_brand",
    )
    decode_body = decode_body.replace(
        "fn libavif_result_to_string",
        "pub(crate) fn libavif_result_to_string",
    )
    decode_body = decode_body.replace(
        "fn avif_image_icc_bytes",
        "pub(crate) fn avif_image_icc_bytes",
    )
    decode_body = decode_body.replace(
        "fn apply_icc_to_srgb_via_lcms",
        "pub(crate) fn apply_icc_to_srgb_via_lcms",
    )
    decode_body = decode_body.replace(
        "fn avif_image_has_alpha_plane",
        "pub(crate) fn avif_image_has_alpha_plane",
    )
    decode_body = decode_body.replace(
        "fn avif_fill_opaque_alpha_u16_if_no_alpha_plane",
        "pub(crate) fn avif_fill_opaque_alpha_u16_if_no_alpha_plane",
    )
    decode_body = decode_body.replace(
        "fn avif_fill_opaque_alpha_f32_if_no_alpha_plane",
        "pub(crate) fn avif_fill_opaque_alpha_f32_if_no_alpha_plane",
    )
    decode_body = decode_body.replace(
        "    avif_image_to_hdr_buffer(image.as_ptr(), target_hdr_capacity)",
        "    super::avif_image_to_hdr_buffer(image.as_ptr(), target_hdr_capacity)",
    )
    (OUT / "decode.rs").write_text(HEADER + decode_body, encoding="utf-8")

    sequence_body = (
        "#![cfg(feature = \"avif-native\")]\n\n"
        + "use crate::hdr::types::HdrImageBuffer;\n\n"
        + "use super::decode::{avif_ftyp_major_brand, libavif_result_to_string};\n\n"
        + slice_lines(lines, 63, 123)  # avif_open_image_sequence_decoder
        + "\n"
        + slice_lines(lines, 132, 177)  # try_decode_avif_image_sequence_hdr
    )
    sequence_body = sequence_body.replace(
        "        let hdr = avif_image_to_hdr_buffer(img_ptr, target_hdr_capacity)?;",
        "        let hdr = super::avif_image_to_hdr_buffer(img_ptr, target_hdr_capacity)?;",
    )
    (OUT / "sequence.rs").write_text(HEADER + sequence_body, encoding="utf-8")

    mod_body = (
        "mod brand;\nmod metadata;\n\n"
        + "#[cfg(feature = \"avif-native\")]\nmod decode;\n"
        + "#[cfg(feature = \"avif-native\")]\nmod gain_map;\n"
        + "#[cfg(feature = \"avif-native\")]\nmod orientation;\n"
        + "#[cfg(feature = \"avif-native\")]\nmod sequence;\n\n"
        + "#[cfg(test)]\nmod tests;\n\n"
        + "pub(crate) use brand::is_avif_brand;\n"
        + "pub(crate) use metadata::avif_cicp_to_metadata;\n\n"
        + "#[cfg(feature = \"avif-native\")]\n"
        + "pub(crate) use decode::{\n"
        + "    decode_avif_hdr, decode_avif_hdr_bytes, decode_avif_hdr_bytes_with_target_capacity,\n"
        + "    decode_avif_hdr_with_target_capacity,\n"
        + "};\n"
        + "#[cfg(feature = \"avif-native\")]\n"
        + "pub(crate) use gain_map::avif_gain_map_to_metadata;\n"
        + "#[cfg(feature = \"avif-native\")]\n"
        + "pub(crate) use orientation::{\n"
        + "    avif_irot_imir_to_exif_orientation, avif_transforms_to_exif_orientation,\n"
        + "    libavif_probe_exif_orientation_from_bytes, libavif_probe_exif_orientation_from_path,\n"
        + "};\n"
        + "#[cfg(feature = \"avif-native\")]\n"
        + "pub(crate) use sequence::try_decode_avif_image_sequence_hdr;\n\n"
        + "#[cfg(feature = \"avif-native\")]\n"
        + "use std::sync::Arc;\n\n"
        + "#[cfg(feature = \"avif-native\")]\n"
        + "use crate::hdr::avif_gain_map_deferred::{\n"
        + "    attach_avif_gain_map_gpu_deferred, avif_build_iso_sdr_baseline_rgba8,\n"
        + "};\n"
        + "#[cfg(feature = \"avif-native\")]\n"
        + "use crate::hdr::gain_map::iso_gain_map_skips_forward_compose;\n"
        + "#[cfg(feature = \"avif-native\")]\n"
        + "use crate::hdr::types::{\n"
        + "    HdrColorProfile, HdrImageBuffer, HdrImageMetadata, HdrPixelFormat, HdrReference,\n"
        + "    HdrTransferFunction,\n"
        + "};\n\n"
        + "#[cfg(feature = \"avif-native\")]\n"
        + "use metadata::{avif_yuv_to_rgb_output_metadata, AvifMetadataExt};\n\n"
        + "#[cfg(feature = \"avif-native\")]\n"
        + "use decode::{\n"
        + "    apply_icc_to_srgb_via_lcms, avif_fill_opaque_alpha_f32_if_no_alpha_plane,\n"
        + "    avif_fill_opaque_alpha_u16_if_no_alpha_plane, avif_image_icc_bytes,\n"
        + "    decode_avif_image_rgba_u16, libavif_result_to_string, rgb_channel_max_f,\n"
        + "};\n\n"
        + "#[cfg(feature = \"avif-native\")]\n"
        + "use gain_map::decode_avif_gain_map;\n\n"
        + "#[cfg(feature = \"avif-native\")]\n"
        + slice_lines(lines, 391, 531)  # avif_image_to_hdr_buffer
    )
    mod_body = mod_body.replace(
        "fn avif_image_to_hdr_buffer",
        "pub(crate) fn avif_image_to_hdr_buffer",
    )
    (OUT / "mod.rs").write_text(HEADER + mod_body, encoding="utf-8")

    tests_src = slice_lines(lines, 1071, 1498)
    tests_src = tests_src.replace("mod tests {", "", 1)
    tests_src = tests_src.rstrip()
    if tests_src.endswith("}"):
        tests_src = tests_src[:-1]
    tests_src = tests_src.replace("use super::{", "use crate::hdr::avif::orientation::{")
    tests_src = tests_src.replace(
        "super::avif_yuv_to_rgb_output_metadata",
        "crate::hdr::avif::metadata::avif_yuv_to_rgb_output_metadata",
    )
    tests_src = tests_src.replace(
        "super::decode_avif_hdr_bytes_with_target_capacity",
        "crate::hdr::avif::decode_avif_hdr_bytes_with_target_capacity",
    )
    tests_src = tests_src.replace(
        "super::decode_avif_hdr_bytes(",
        "crate::hdr::avif::decode_avif_hdr_bytes(",
    )
    tests_src = tests_src.replace(
        "super::try_decode_avif_image_sequence_hdr",
        "crate::hdr::avif::try_decode_avif_image_sequence_hdr",
    )
    # orientation test imports
    tests_src = tests_src.replace(
        "        use super::{AVIF_TRANSFORM_IMIR_FLAG as IMIR, AVIF_TRANSFORM_IROT_FLAG as IROT};\n\n        assert_eq!(super::avif_irot_imir_to_exif_orientation",
        "        use crate::hdr::avif::orientation::{\n            avif_irot_imir_to_exif_orientation, AVIF_TRANSFORM_IMIR_FLAG as IMIR,\n            AVIF_TRANSFORM_IROT_FLAG as IROT,\n        };\n\n        assert_eq!(avif_irot_imir_to_exif_orientation",
    )
    tests_src = tests_src.replace("super::avif_irot_imir_to_exif_orientation", "avif_irot_imir_to_exif_orientation")
    # gain map test helpers
    tests_src = tests_src.replace(
        "    fn signed(n: i32, d: u32) -> libavif_sys::avifSignedFraction {\n        libavif_sys::avifSignedFraction { n, d }\n    }\n\n    #[cfg(feature = \"avif-native\")]\n    fn unsigned(n: u32, d: u32) -> libavif_sys::avifUnsignedFraction {\n        libavif_sys::avifUnsignedFraction { n, d }\n    }\n\n",
        "",
    )
    tests_src = tests_src.replace(
        "gainMapMin: [signed(", "gainMapMin: [crate::hdr::avif::gain_map::test_signed_fraction("
    )
    tests_src = tests_src.replace(
        "gainMapMax: [signed(", "gainMapMax: [crate::hdr::avif::gain_map::test_signed_fraction("
    )
    tests_src = tests_src.replace(
        "gainMapGamma: [unsigned(", "gainMapGamma: [crate::hdr::avif::gain_map::test_unsigned_fraction("
    )
    tests_src = tests_src.replace(
        "baseOffset: [signed(", "baseOffset: [crate::hdr::avif::gain_map::test_signed_fraction("
    )
    tests_src = tests_src.replace(
        "alternateOffset: [signed(", "alternateOffset: [crate::hdr::avif::gain_map::test_signed_fraction("
    )
    tests_src = tests_src.replace(
        "baseHdrHeadroom: unsigned(", "baseHdrHeadroom: crate::hdr::avif::gain_map::test_unsigned_fraction("
    )
    tests_src = tests_src.replace(
        "alternateHdrHeadroom: unsigned(",
        "alternateHdrHeadroom: crate::hdr::avif::gain_map::test_unsigned_fraction(",
    )
    (OUT / "tests.rs").write_text(tests_src, encoding="utf-8")

    print("Wrote avif module files to", OUT)


if __name__ == "__main__":
    main()
