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

use std::collections::HashMap;

use crate::loader::{RawDemosaicBackend, RawOsdInfo, RawRenderPixels};
use crate::ui::osd::{HdrOsdFrame, ImageOsdFrame, ImageOsdMode, OsdEvent};
use crate::ui::osd_param::TrackedParam;

pub(crate) struct ImageViewStatus {
    current_index: TrackedParam<usize, OsdEvent>,
    total_images: TrackedParam<usize, OsdEvent>,
    zoom_pct: TrackedParam<u32, OsdEvent>,
    image_resolution: TrackedParam<(u32, u32), OsdEvent>,
    file_size_bytes: TrackedParam<u64, OsdEvent>,
    image_mode: TrackedParam<ImageOsdMode, OsdEvent>,
    file_name: TrackedParam<String, OsdEvent>,
    hdr_render_path: TrackedParam<Option<crate::hdr::status::HdrRenderPath>, OsdEvent>,
    hdr_color_space: TrackedParam<Option<crate::hdr::types::HdrColorSpace>, OsdEvent>,
    hdr_output_mode: TrackedParam<crate::hdr::types::HdrOutputMode, OsdEvent>,
    hdr_native_presentation_enabled: TrackedParam<bool, OsdEvent>,
    ultra_hdr_decode_capacity: TrackedParam<Option<f32>, OsdEvent>,
    hdr_monitor_label: TrackedParam<Option<String>, OsdEvent>,
    hdr_exposure_ev: TrackedParam<f32, OsdEvent>,
}

impl ImageViewStatus {
    pub(crate) fn new(tx: crossbeam_channel::Sender<OsdEvent>) -> Self {
        Self {
            current_index: TrackedParam::new(0, tx.clone(), OsdEvent::current_index),
            total_images: TrackedParam::new(0, tx.clone(), OsdEvent::total_images),
            zoom_pct: TrackedParam::new(0, tx.clone(), OsdEvent::zoom_pct),
            image_resolution: TrackedParam::new((0, 0), tx.clone(), OsdEvent::image_resolution),
            file_size_bytes: TrackedParam::new(0, tx.clone(), OsdEvent::file_size_bytes),
            image_mode: TrackedParam::new(ImageOsdMode::Static, tx.clone(), OsdEvent::image_mode),
            file_name: TrackedParam::new(String::new(), tx.clone(), OsdEvent::file_name),
            hdr_render_path: TrackedParam::new(None, tx.clone(), OsdEvent::hdr_render_path),
            hdr_color_space: TrackedParam::new(None, tx.clone(), OsdEvent::hdr_color_space),
            hdr_output_mode: TrackedParam::new(
                crate::hdr::types::HdrOutputMode::SdrToneMapped,
                tx.clone(),
                OsdEvent::hdr_output_mode,
            ),
            hdr_native_presentation_enabled: TrackedParam::new(
                false,
                tx.clone(),
                OsdEvent::hdr_native_presentation_enabled,
            ),
            ultra_hdr_decode_capacity: TrackedParam::new(
                None,
                tx.clone(),
                OsdEvent::ultra_hdr_decode_capacity,
            ),
            hdr_monitor_label: TrackedParam::new(None, tx.clone(), OsdEvent::hdr_monitor_label),
            hdr_exposure_ev: TrackedParam::new(0.0, tx, OsdEvent::hdr_exposure_ev),
        }
    }

    pub(crate) fn set_image_frame(&mut self, image: &ImageOsdFrame, file_name: &str) {
        let image = image.cache_key();
        self.current_index.set(image.index);
        self.total_images.set(image.total);
        self.zoom_pct.set(image.zoom_pct);
        self.image_resolution.set(image.res);
        self.file_size_bytes.set(image.file_size_bytes);
        self.image_mode.set(image.mode);
        self.set_file_name(file_name);
    }

    pub(crate) fn set_current_index(&mut self, current_index: usize) {
        self.current_index.set(current_index);
    }

    pub(crate) fn set_image_resolution(&mut self, resolution: Option<(u32, u32)>) {
        self.image_resolution.set(resolution.unwrap_or_default());
    }

    pub(crate) fn set_file_name(&mut self, file_name: &str) {
        if self.file_name.get().as_str() != file_name {
            self.file_name.set(file_name.to_owned());
        }
    }

    pub(crate) fn set_hdr_frame(&mut self, hdr: &HdrOsdFrame<'_>) {
        self.hdr_render_path.set(hdr.render_path);
        self.hdr_color_space.set(hdr.color_space);
        self.hdr_output_mode.set(hdr.output_mode);
        self.hdr_native_presentation_enabled
            .set(hdr.native_presentation_enabled);
        self.ultra_hdr_decode_capacity
            .set(hdr.ultra_hdr_decode_capacity);
        if self.hdr_monitor_label.get().as_deref() != hdr.monitor_label {
            self.hdr_monitor_label
                .set(hdr.monitor_label.map(str::to_owned));
        }
        self.hdr_exposure_ev.set(hdr.exposure_ev);
    }
}

pub(crate) struct RawMetadataStore {
    by_index: HashMap<usize, RawOsdInfo>,
    current_index: usize,
    sensor_size: TrackedParam<(u32, u32), OsdEvent>,
    embedded_preview: TrackedParam<Option<(u32, u32)>, OsdEvent>,
    render_pixels: TrackedParam<RawRenderPixels, OsdEvent>,
    demosaic_backend: TrackedParam<Option<RawDemosaicBackend>, OsdEvent>,
    cpu_demosaic_ms: TrackedParam<Option<u32>, OsdEvent>,
    gpu_extract_ms: TrackedParam<Option<u32>, OsdEvent>,
    gpu_demosaic_ms: TrackedParam<Option<u32>, OsdEvent>,
}

impl RawMetadataStore {
    pub(crate) fn new(tx: crossbeam_channel::Sender<OsdEvent>) -> Self {
        Self {
            by_index: HashMap::new(),
            current_index: 0,
            sensor_size: TrackedParam::new((0, 0), tx.clone(), OsdEvent::raw_sensor_size),
            embedded_preview: TrackedParam::new(None, tx.clone(), OsdEvent::raw_embedded_preview),
            render_pixels: TrackedParam::new(
                RawRenderPixels::Embedded {
                    width: 0,
                    height: 0,
                },
                tx.clone(),
                OsdEvent::raw_render_pixels,
            ),
            demosaic_backend: TrackedParam::new(None, tx.clone(), OsdEvent::raw_demosaic_backend),
            cpu_demosaic_ms: TrackedParam::new(None, tx.clone(), OsdEvent::raw_cpu_demosaic_ms),
            gpu_extract_ms: TrackedParam::new(None, tx.clone(), OsdEvent::raw_gpu_extract_ms),
            gpu_demosaic_ms: TrackedParam::new(None, tx, OsdEvent::raw_gpu_demosaic_ms),
        }
    }

    pub(crate) fn clear(&mut self) {
        self.by_index.clear();
        self.sync_tracked_params_for_current();
    }

    pub(crate) fn set_current_index(&mut self, current_index: usize) {
        if self.current_index == current_index {
            return;
        }
        self.current_index = current_index;
        self.sync_tracked_params_for_current();
    }

    pub(crate) fn insert_or_update(&mut self, index: usize, info: RawOsdInfo) {
        if let Some(existing) = self.by_index.get_mut(&index) {
            existing.merge_loader_fields(&info);
        } else {
            self.by_index.insert(index, info);
        }
        if index == self.current_index {
            self.sync_tracked_params_for_current();
        }
    }

    pub(crate) fn remove(&mut self, index: usize) -> bool {
        let changed = self.by_index.remove(&index).is_some();
        if changed && index == self.current_index {
            self.sync_tracked_params_for_current();
        }
        changed
    }

    pub(crate) fn apply_hq_refine_preview(
        &mut self,
        index: usize,
        width: u32,
        height: u32,
    ) -> bool {
        let Some(info) = self.by_index.get_mut(&index) else {
            return false;
        };
        let before = info.clone();
        info.apply_hq_refine_preview(width, height);
        if before == *info {
            return false;
        }
        if index == self.current_index {
            self.sync_tracked_params_for_current();
        }
        true
    }

    pub(crate) fn promote_gpu_demosaic_complete(&mut self, index: usize) -> bool {
        let Some(info) = self.by_index.get_mut(&index) else {
            return false;
        };
        let before = info.render_pixels;
        info.promote_gpu_demosaic_complete();
        if before == info.render_pixels {
            return false;
        }
        if index == self.current_index {
            self.sync_tracked_params_for_current();
        }
        true
    }

    pub(crate) fn note_gpu_demosaic_pending(
        &mut self,
        index: usize,
        bootstrap: Option<(u32, u32)>,
    ) -> bool {
        let Some(info) = self.by_index.get_mut(&index) else {
            return false;
        };
        let before = info.clone();
        info.note_gpu_demosaic_pending(bootstrap);
        if before == *info {
            return false;
        }
        if index == self.current_index {
            self.sync_tracked_params_for_current();
        }
        true
    }

    pub(crate) fn note_gpu_demosaic_failed(&mut self, index: usize) -> bool {
        let Some(info) = self.by_index.get_mut(&index) else {
            return false;
        };
        let before = info.demosaic_backend;
        info.note_gpu_demosaic_failed();
        if before == info.demosaic_backend {
            return false;
        }
        if index == self.current_index {
            self.sync_tracked_params_for_current();
        }
        true
    }

    pub(crate) fn set_cpu_demosaic_ms(&mut self, index: usize, ms: u32) -> bool {
        let Some(info) = self.by_index.get_mut(&index) else {
            return false;
        };
        if info.cpu_demosaic_ms == Some(ms) {
            return false;
        }
        info.cpu_demosaic_ms = Some(ms);
        if index == self.current_index {
            self.sync_tracked_params_for_current();
        }
        true
    }

    pub(crate) fn set_gpu_demosaic_ms(&mut self, index: usize, ms: u32) -> bool {
        let Some(info) = self.by_index.get_mut(&index) else {
            return false;
        };
        if info.gpu_demosaic_ms == Some(ms) {
            return false;
        }
        info.gpu_demosaic_ms = Some(ms);
        if index == self.current_index {
            self.sync_tracked_params_for_current();
        }
        true
    }

    pub(crate) fn embedded_preview_dims(&self, index: usize) -> Option<(u32, u32)> {
        self.by_index
            .get(&index)
            .and_then(|info| info.embedded_preview)
    }

    pub(crate) fn relocate_index(&mut self, from: usize, to: usize) {
        if let Some(raw) = self.by_index.remove(&from) {
            self.by_index.insert(to, raw);
        }
        if from == self.current_index || to == self.current_index {
            self.sync_tracked_params_for_current();
        }
    }

    pub(crate) fn permute_indices(&mut self, old_to_new: &[usize]) {
        let taken = std::mem::take(&mut self.by_index);
        for (old_idx, info) in taken {
            if old_idx < old_to_new.len() {
                self.by_index.insert(old_to_new[old_idx], info);
            }
        }
        self.sync_tracked_params_for_current();
    }

    pub(crate) fn retain_only_indices(&mut self, keep: impl Fn(usize) -> bool) {
        self.by_index.retain(|&idx, _| keep(idx));
        self.sync_tracked_params_for_current();
    }

    #[cfg(test)]
    pub(crate) fn contains_key(&self, index: usize) -> bool {
        self.by_index.contains_key(&index)
    }

    fn sync_tracked_params_for_current(&mut self) {
        let Some(info) = self.by_index.get(&self.current_index) else {
            self.sensor_size.set((0, 0));
            self.embedded_preview.set(None);
            self.render_pixels.set(RawRenderPixels::Embedded {
                width: 0,
                height: 0,
            });
            self.demosaic_backend.set(None);
            self.cpu_demosaic_ms.set(None);
            self.gpu_extract_ms.set(None);
            self.gpu_demosaic_ms.set(None);
            return;
        };
        self.sensor_size.set(info.sensor_size);
        self.embedded_preview.set(info.embedded_preview);
        self.render_pixels.set(info.render_pixels);
        self.demosaic_backend.set(info.demosaic_backend);
        self.cpu_demosaic_ms.set(info.cpu_demosaic_ms);
        self.gpu_extract_ms.set(info.gpu_extract_ms);
        self.gpu_demosaic_ms.set(info.gpu_demosaic_ms);
    }
}
