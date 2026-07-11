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

//! Optional routing tests against `scripts/gen_psd_hdr_routing_fixtures.py` output.
//!
//! When `tests/data/psd_hdr_routing/manifest.json` is missing, tests no-op so CI
//! stays green without generated fixtures.

#[cfg(test)]
mod tests {
    use crate::hdr::types::HdrToneMapSettings;
    use crate::psb_hdr_main::{decode_psd_hdr_main_from_bytes_with_cancel, psd_should_try_hdr};
    use crate::psb_sdr_main::decode_psd_sdr_main_from_bytes_with_cancel;
    use crate::psb_section_index::PsdSectionIndex;
    use std::path::PathBuf;

    fn fixture_dir() -> Option<PathBuf> {
        let dir = PathBuf::from("tests/data/psd_hdr_routing");
        if dir.join("manifest.json").is_file() {
            Some(dir)
        } else {
            None
        }
    }

    /// Minimal manifest parser: pairs of "file" / "expected_branch" string fields.
    fn parse_manifest(text: &str) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let mut file: Option<String> = None;
        for raw in text.lines() {
            let line = raw.trim();
            if let Some(rest) = line.strip_prefix("\"file\":") {
                let v = rest.trim().trim_matches(',').trim().trim_matches('"');
                file = Some(v.to_string());
            } else if let Some(rest) = line.strip_prefix("\"expected_branch\":") {
                let v = rest.trim().trim_matches(',').trim().trim_matches('"');
                if let Some(f) = file.take() {
                    out.push((f, v.to_string()));
                }
            }
        }
        out
    }

    #[test]
    fn hdr_routing_fixtures_match_manifest_branches() {
        let Some(dir) = fixture_dir() else {
            eprintln!("skipping hdr_routing_fixtures; run scripts/gen_psd_hdr_routing_fixtures.py");
            return;
        };
        let text = std::fs::read_to_string(dir.join("manifest.json")).expect("read manifest");
        let entries = parse_manifest(&text);
        assert!(!entries.is_empty(), "manifest produced no entries");
        let tone = HdrToneMapSettings::default();

        for (file, expected) in entries {
            let path = dir.join(&file);
            if !path.is_file() {
                eprintln!("skip missing fixture {file}");
                continue;
            }
            let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {file}: {e}"));
            let index = PsdSectionIndex::parse(&bytes).unwrap_or_else(|e| panic!("{file}: {e}"));
            let icc = crate::psb_reader::extract_icc_profile_from_ir(
                &bytes,
                index.ir_start,
                index.ir_end,
            );

            match expected.as_str() {
                "hdr_p1" | "hdr_p2" => {
                    assert!(
                        psd_should_try_hdr(index.depth, icc.as_deref(), 2.0),
                        "{file}: content+env should want HDR"
                    );
                    assert!(
                        !psd_should_try_hdr(index.depth, icc.as_deref(), 1.0),
                        "{file}: SDR capacity must not select HDR"
                    );
                    let crate::psb_hdr_main::PsdHdrMainDecode { hdr, .. } =
                        decode_psd_hdr_main_from_bytes_with_cancel(&bytes, None, &tone, false)
                            .unwrap_or_else(|e| panic!("{file}: HDR decode failed: {e}"));
                    assert!(hdr.width > 0 && hdr.height > 0, "{file}: empty HDR dims");
                    assert!(!hdr.rgba_f32.is_empty(), "{file}: empty HDR pixels");
                }
                "sdr_p1" | "sdr_p2" | "sdr_p3" => {
                    assert!(
                        !psd_should_try_hdr(index.depth, icc.as_deref(), 2.0),
                        "{file}: SDR fixture must not trip HDR content gate"
                    );
                    let sdr = decode_psd_sdr_main_from_bytes_with_cancel(&bytes, None, None)
                        .unwrap_or_else(|e| panic!("{file}: SDR decode failed: {e}"));
                    assert!(
                        sdr.composite.width > 0 && sdr.composite.height > 0,
                        "{file}: empty SDR dims"
                    );
                }
                other => panic!("{file}: unknown expected_branch {other}"),
            }
        }
    }
}
