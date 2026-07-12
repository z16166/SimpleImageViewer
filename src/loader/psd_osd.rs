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

//! PSD-specific OSD metadata (decode stage, compat reveal, layer comp / max-bbox roots).

use rust_i18n::t;

/// PSD decode pipeline stage shown on the OSD.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PsdDecodeStage {
    P1,
    P2,
    P25a,
    P25b,
    P3,
}

impl PsdDecodeStage {
    fn osd_label(self) -> String {
        match self {
            Self::P1 => t!("osd.psd.stage.p1").to_string(),
            Self::P2 => t!("osd.psd.stage.p2").to_string(),
            Self::P25a => t!("osd.psd.stage.p25a").to_string(),
            Self::P25b => t!("osd.psd.stage.p25b").to_string(),
            Self::P3 => t!("osd.psd.stage.p3").to_string(),
        }
    }
}

/// PSD stage-specific detail shown after the stage label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PsdStageDetail {
    Flattened,
    Strict,
    LayerComp,
    MaxBbox,
    MaxBboxForceOpen,
    ShowAllLayers,
    IrThumb,
}

impl PsdStageDetail {
    fn osd_label(self) -> String {
        match self {
            Self::Flattened => t!("osd.psd.detail.flattened").to_string(),
            Self::Strict => t!("osd.psd.detail.strict").to_string(),
            Self::LayerComp => t!("osd.psd.detail.layer_comp").to_string(),
            Self::MaxBbox => t!("osd.psd.detail.max_bbox").to_string(),
            Self::MaxBboxForceOpen => t!("osd.psd.detail.max_bbox_force_open").to_string(),
            Self::ShowAllLayers => t!("osd.psd.detail.show_all_layers").to_string(),
            Self::IrThumb => t!("osd.psd.detail.ir_thumb").to_string(),
        }
    }
}

/// Persistent PSD diagnostics shown on the OSD while browsing a PSD file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PsdOsdInfo {
    pub stage: PsdDecodeStage,
    pub stage_detail: PsdStageDetail,
    pub compat_reveal: bool,
    pub comp_name: Option<String>,
    pub reveal_root: Option<String>,
}

impl PsdOsdInfo {
    pub fn p1_flattened() -> Self {
        Self {
            stage: PsdDecodeStage::P1,
            stage_detail: PsdStageDetail::Flattened,
            compat_reveal: false,
            comp_name: None,
            reveal_root: None,
        }
    }

    pub fn p2_strict() -> Self {
        Self {
            stage: PsdDecodeStage::P2,
            stage_detail: PsdStageDetail::Strict,
            compat_reveal: false,
            comp_name: None,
            reveal_root: None,
        }
    }

    pub fn p25a_layer_comp(name: Option<String>) -> Self {
        Self {
            stage: PsdDecodeStage::P25a,
            stage_detail: PsdStageDetail::LayerComp,
            compat_reveal: true,
            comp_name: name,
            reveal_root: None,
        }
    }

    /// P2.5b max-bbox reveal OSD. `root` is the selected top-level name;
    /// `force_open` forces that subtree open (distinct from [`Self::p25b_show_all`]).
    pub fn p25b_max_bbox(root: Option<String>, force_open: bool) -> Self {
        Self {
            stage: PsdDecodeStage::P25b,
            stage_detail: if force_open {
                PsdStageDetail::MaxBboxForceOpen
            } else {
                PsdStageDetail::MaxBbox
            },
            compat_reveal: true,
            comp_name: None,
            reveal_root: root,
        }
    }

    /// P2.5b force-open-all drawable leaves (not max-bbox with a null root).
    pub fn p25b_show_all() -> Self {
        Self {
            stage: PsdDecodeStage::P25b,
            stage_detail: PsdStageDetail::ShowAllLayers,
            compat_reveal: true,
            comp_name: None,
            reveal_root: None,
        }
    }

    pub fn p3_ir_thumb() -> Self {
        Self {
            stage: PsdDecodeStage::P3,
            stage_detail: PsdStageDetail::IrThumb,
            compat_reveal: false,
            comp_name: None,
            reveal_root: None,
        }
    }

    pub fn compose_osd_line(&self) -> String {
        let mut parts = vec!["PSD".to_string()];
        if self.compat_reveal {
            parts.push(t!("osd.psd.compat_reveal").to_string());
        }
        parts.push(self.stage.osd_label());
        parts.push(self.stage_detail.osd_label());
        if let Some(name) = &self.comp_name {
            parts.push(name.clone());
        }
        if let Some(root) = &self.reveal_root {
            parts.push(root.clone());
        }
        parts.join(" · ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compose_osd_line_marks_compat_reveal_for_p25b() {
        let info = PsdOsdInfo {
            stage: PsdDecodeStage::P25b,
            stage_detail: PsdStageDetail::MaxBboxForceOpen,
            compat_reveal: true,
            comp_name: None,
            reveal_root: Some("封面".into()),
        };
        let line = info.compose_osd_line();
        assert!(line.contains("P2.5b") || line.contains("p2.5b"), "{line}");
        assert!(info.compat_reveal);
    }

    #[test]
    fn compose_osd_line_p1_not_compat() {
        let info = PsdOsdInfo::p1_flattened();
        assert!(!info.compat_reveal);
        assert_eq!(info.stage, PsdDecodeStage::P1);
    }

    #[test]
    fn show_all_layers_uses_distinct_p25b_detail() {
        let info = PsdOsdInfo::p25b_show_all();
        assert_eq!(info.stage, PsdDecodeStage::P25b);
        assert_eq!(info.stage_detail, PsdStageDetail::ShowAllLayers);
        assert!(info.compat_reveal);
        assert_eq!(info.reveal_root, None);
    }
}
