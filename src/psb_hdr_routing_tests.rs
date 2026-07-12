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
//! When `tests/data/psd_hdr_routing/manifest.json` is missing, tests no-op unless
//! `SIV_REQUIRE_PSD_HDR_FIXTURES=1` requires generated fixtures. In that strict
//! mode every manifest entry must exist on disk and at least one fixture must run.

#[cfg(test)]
mod tests {
    use crate::hdr::types::HdrToneMapSettings;
    use crate::psb_hdr_main::{decode_psd_hdr_main_from_bytes_with_cancel, psd_should_try_hdr};
    use crate::psb_sdr_main::decode_psd_sdr_main_from_bytes_with_cancel;
    use crate::psb_section_index::PsdSectionIndex;
    use serde::Deserialize;
    use std::path::PathBuf;

    #[derive(Debug, Deserialize)]
    struct Manifest {
        fixtures: Vec<ManifestEntry>,
    }

    #[derive(Debug, Deserialize)]
    struct ManifestEntry {
        file: String,
        expected_branch: String,
    }

    fn fixture_dir() -> Option<PathBuf> {
        let dir = PathBuf::from("tests/data/psd_hdr_routing");
        if dir.join("manifest.json").is_file() {
            Some(dir)
        } else {
            None
        }
    }

    fn parse_manifest(text: &str) -> Result<Vec<(String, String)>, String> {
        // Accept either `{ "fixtures": [ ... ] }` or a bare JSON array.
        if let Ok(manifest) = serde_json::from_str::<Manifest>(text) {
            return Ok(manifest
                .fixtures
                .into_iter()
                .map(|e| (e.file, e.expected_branch))
                .collect());
        }
        let entries: Vec<ManifestEntry> = serde_json::from_str(text)
            .map_err(|e| format!("PSD HDR routing manifest JSON parse failed: {e}"))?;
        Ok(entries
            .into_iter()
            .map(|e| (e.file, e.expected_branch))
            .collect())
    }

    #[test]
    fn hdr_routing_fixtures_match_manifest_branches() {
        let require = std::env::var("SIV_REQUIRE_PSD_HDR_FIXTURES").as_deref() == Ok("1");
        let Some(dir) = fixture_dir() else {
            if require {
                panic!(
                    "PSD HDR routing fixtures are required; run scripts/gen_psd_hdr_routing_fixtures.py"
                );
            }
            eprintln!("skipping hdr_routing_fixtures; run scripts/gen_psd_hdr_routing_fixtures.py");
            return;
        };
        let text = std::fs::read_to_string(dir.join("manifest.json")).expect("read manifest");
        let entries = match parse_manifest(&text) {
            Ok(entries) => entries,
            Err(e) => {
                if require {
                    panic!("{e}");
                }
                eprintln!("skipping hdr_routing_fixtures: {e}");
                return;
            }
        };
        if entries.is_empty() {
            if require {
                panic!("manifest produced no entries");
            }
            eprintln!("skipping hdr_routing_fixtures; empty manifest");
            return;
        }
        let tone = HdrToneMapSettings::default();

        let mut executed = 0usize;
        for (file, expected) in entries {
            let path = dir.join(&file);
            if !path.is_file() {
                if require {
                    panic!(
                        "missing PSD HDR routing fixture {file}; run scripts/gen_psd_hdr_routing_fixtures.py"
                    );
                }
                eprintln!("skip missing fixture {file}");
                continue;
            }
            executed += 1;
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
                        decode_psd_hdr_main_from_bytes_with_cancel(
                            &bytes,
                            None,
                            &tone,
                            false,
                            crate::settings::PsdHiddenLayerStrategy::Heuristic,
                        )
                        .unwrap_or_else(|e| panic!("{file}: HDR decode failed: {e}"));
                    assert!(hdr.width > 0 && hdr.height > 0, "{file}: empty HDR dims");
                    assert!(!hdr.rgba_f32.is_empty(), "{file}: empty HDR pixels");
                }
                "sdr_p1" | "sdr_p2" | "sdr_p3" => {
                    assert!(
                        !psd_should_try_hdr(index.depth, icc.as_deref(), 2.0),
                        "{file}: SDR fixture must not trip HDR content gate"
                    );
                    let sdr = decode_psd_sdr_main_from_bytes_with_cancel(
                        &bytes,
                        None,
                        None,
                        crate::settings::PsdHiddenLayerStrategy::Heuristic,
                    )
                    .unwrap_or_else(|e| panic!("{file}: SDR decode failed: {e}"));
                    assert!(
                        sdr.composite.width > 0 && sdr.composite.height > 0,
                        "{file}: empty SDR dims"
                    );
                }
                // Content may want HDR (e.g. 32-bit) or CMYK flats may not
                // trip the absolute-blank barrier; still require SDR main P2
                // (skip-flattened when needed) to produce viewable pixels when
                // the display environment is SDR-only.
                "sdr_env_p2" => {
                    assert!(
                        !psd_should_try_hdr(index.depth, icc.as_deref(), 1.0),
                        "{file}: SDR capacity must not select HDR"
                    );
                    let sdr = if index.color_mode == 4 {
                        crate::psb_sdr_main::decode_psd_sdr_main_skip_flattened_with_cancel(
                            &bytes,
                            None,
                            None,
                            crate::settings::PsdHiddenLayerStrategy::Heuristic,
                        )
                    } else {
                        decode_psd_sdr_main_from_bytes_with_cancel(
                            &bytes,
                            None,
                            None,
                            crate::settings::PsdHiddenLayerStrategy::Heuristic,
                        )
                    }
                    .unwrap_or_else(|e| panic!("{file}: SDR env P2 decode failed: {e}"));
                    assert!(
                        sdr.composite.width > 0 && sdr.composite.height > 0,
                        "{file}: empty SDR dims"
                    );
                    assert!(!sdr.composite.pixels.is_empty(), "{file}: empty SDR pixels");
                    assert_eq!(
                        sdr.osd,
                        crate::loader::PsdOsdInfo::p2_strict(),
                        "{file}: expected P2 strict OSD"
                    );
                }
                other => {
                    if require {
                        panic!("{file}: unknown expected_branch {other}");
                    }
                    eprintln!("skip unknown expected_branch {other} for {file}");
                }
            }
        }
        if require {
            assert!(
                executed > 0,
                "SIV_REQUIRE_PSD_HDR_FIXTURES=1 but no fixture entries were executed"
            );
        }
    }
}
