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

use crate::app::{ImageViewerApp, TransitionStyle};
use crate::hdr::status::{HdrRenderPath, hdr_osd_tag};

impl ImageViewerApp {
    pub(crate) fn current_hdr_render_path(&self) -> Option<HdrRenderPath> {
        let has_hdr_tiled_source = self
            .current_hdr_tiled_image
            .as_ref()
            .is_some_and(|current| current.source_for_index(self.current_index).is_some());
        let has_sdr_fallback = self.hdr_sdr_fallback_indices.contains(&self.current_index);

        let has_hdr_image = self
            .current_hdr_image
            .as_ref()
            .is_some_and(|current| current.image_for_index(self.current_index).is_some());

        let complex_transition_active = self.transition_start.is_some()
            && matches!(
                self.active_transition,
                TransitionStyle::PageFlip | TransitionStyle::Ripple | TransitionStyle::Curtain
            );

        hdr_render_path_for_state(
            has_hdr_tiled_source,
            has_hdr_image,
            has_sdr_fallback,
            self.hdr_target_format.is_some(),
            complex_transition_active,
        )
    }

    pub(crate) fn current_hdr_osd_tag(&self) -> Option<String> {
        let render_path = self.current_hdr_render_path()?;
        hdr_osd_tag(true, render_path, &self.hdr_capabilities)
    }
}

fn hdr_render_path_for_state(
    has_hdr_tiled_source: bool,
    has_hdr_image: bool,
    has_sdr_fallback: bool,
    has_hdr_target_format: bool,
    complex_transition_active: bool,
) -> Option<HdrRenderPath> {
    if has_hdr_tiled_source && has_hdr_target_format {
        return Some(HdrRenderPath::FloatTilePlane);
    }

    if has_hdr_image && has_hdr_target_format && !complex_transition_active {
        return Some(HdrRenderPath::FloatImagePlane);
    }

    if has_hdr_image || has_hdr_tiled_source || has_sdr_fallback {
        Some(HdrRenderPath::SdrFallback)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::hdr_render_path_for_state;
    use crate::hdr::status::HdrRenderPath;

    #[test]
    fn hdr_tiled_source_reports_tile_plane_before_sdr_fallback() {
        assert_eq!(
            hdr_render_path_for_state(true, false, true, true, false),
            Some(HdrRenderPath::FloatTilePlane)
        );
    }

    #[test]
    fn hdr_tiled_source_reports_sdr_fallback_without_hdr_target() {
        assert_eq!(
            hdr_render_path_for_state(true, false, true, false, false),
            Some(HdrRenderPath::SdrFallback)
        );
    }

    #[test]
    fn complex_transition_keeps_full_image_hdr_on_sdr_fallback() {
        assert_eq!(
            hdr_render_path_for_state(false, true, false, true, true),
            Some(HdrRenderPath::SdrFallback)
        );
    }
}
