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
        self.current_hdr_image
            .as_ref()
            .and_then(|current| current.image_for_index(self.current_index))?;

        let complex_transition_active = self.transition_start.is_some()
            && matches!(
                self.active_transition,
                TransitionStyle::PageFlip | TransitionStyle::Ripple | TransitionStyle::Curtain
            );

        if self.hdr_target_format.is_some() && !complex_transition_active {
            Some(HdrRenderPath::FloatImagePlane)
        } else {
            Some(HdrRenderPath::SdrFallback)
        }
    }

    pub(crate) fn current_hdr_osd_tag(&self) -> Option<String> {
        let render_path = self.current_hdr_render_path()?;
        hdr_osd_tag(true, render_path, &self.hdr_capabilities)
    }
}
