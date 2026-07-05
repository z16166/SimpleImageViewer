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

use std::borrow::Cow;
use std::collections::VecDeque;
use std::sync::Arc;

/// Stage for HDR GPU texture writes. Higher stages are flushed first and evicted last.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum HdrGpuUploadStage {
    /// Full image plane RGBA32F / R16 uploads (display-critical).
    PlaneCreate,
    /// Per-tile RGBA32F plane uploads.
    TileCreate,
    /// CPU compose results written into existing display textures.
    ComposeWrite,
    /// Auxiliary RGBA8 sidecars (SDR/gain sources).
    AuxRgba8,
}

impl HdrGpuUploadStage {
    const FLUSH_ORDER: [Self; 4] = [
        Self::PlaneCreate,
        Self::TileCreate,
        Self::ComposeWrite,
        Self::AuxRgba8,
    ];

    const EVICT_ORDER: [Self; 4] = [
        Self::AuxRgba8,
        Self::ComposeWrite,
        Self::TileCreate,
        Self::PlaneCreate,
    ];
}

/// Max queued GPU writes before O(1) front eviction drops the lowest-priority stage.
pub(crate) const MAX_HDR_PENDING_GPU_WRITES: usize = 256;

/// Max `write_texture` calls drained per logic tick (checklist #3).
pub(crate) const MAX_HDR_GPU_WRITES_PER_LOGIC: usize = 8;

pub(crate) struct PendingGpuWrite {
    pub texture: Arc<wgpu::Texture>,
    pub bytes: Vec<u8>,
    pub bytes_per_row: u32,
    pub rows_per_image: u32,
    pub extent: wgpu::Extent3d,
}

#[derive(Default)]
pub(crate) struct HdrPendingGpuWriteQueues {
    plane_create: VecDeque<PendingGpuWrite>,
    tile_create: VecDeque<PendingGpuWrite>,
    compose_write: VecDeque<PendingGpuWrite>,
    aux_rgba8: VecDeque<PendingGpuWrite>,
}

impl HdrPendingGpuWriteQueues {
    pub(crate) fn pending_len(&self) -> usize {
        self.plane_create.len()
            + self.tile_create.len()
            + self.compose_write.len()
            + self.aux_rgba8.len()
    }

    fn queue_for_mut(&mut self, stage: HdrGpuUploadStage) -> &mut VecDeque<PendingGpuWrite> {
        match stage {
            HdrGpuUploadStage::PlaneCreate => &mut self.plane_create,
            HdrGpuUploadStage::TileCreate => &mut self.tile_create,
            HdrGpuUploadStage::ComposeWrite => &mut self.compose_write,
            HdrGpuUploadStage::AuxRgba8 => &mut self.aux_rgba8,
        }
    }

    pub(crate) fn enqueue(
        &mut self,
        stage: HdrGpuUploadStage,
        texture: Arc<wgpu::Texture>,
        bytes: Vec<u8>,
        bytes_per_row: u32,
        rows_per_image: u32,
        extent: wgpu::Extent3d,
    ) {
        let write = PendingGpuWrite {
            texture,
            bytes,
            bytes_per_row,
            rows_per_image,
            extent,
        };
        if self.pending_len() >= MAX_HDR_PENDING_GPU_WRITES {
            let need = self
                .pending_len()
                .saturating_sub(MAX_HDR_PENDING_GPU_WRITES - 1);
            let evicted = self.evict(need);
            if !evicted.is_empty() {
                log::warn!(
                    "[HDR] Pending GPU write queue full; re-queuing {} evicted write(s)",
                    evicted.len()
                );
            }
            for (evict_stage, item) in evicted {
                self.queue_for_mut(evict_stage).push_back(item);
            }
        }
        self.queue_for_mut(stage).push_back(write);
    }

    fn evict(&mut self, need: usize) -> Vec<(HdrGpuUploadStage, PendingGpuWrite)> {
        if need == 0 {
            return Vec::new();
        }
        let mut evicted = Vec::new();
        for stage in HdrGpuUploadStage::EVICT_ORDER {
            while evicted.len() < need {
                let Some(item) = self.queue_for_mut(stage).pop_front() else {
                    break;
                };
                evicted.push((stage, item));
            }
            if evicted.len() >= need {
                break;
            }
        }
        evicted
    }

    pub(crate) fn flush(&mut self, queue: &wgpu::Queue, quota: usize) -> usize {
        if quota == 0 {
            return 0;
        }
        let mut remaining = quota;
        let mut flushed = 0usize;
        for stage in HdrGpuUploadStage::FLUSH_ORDER {
            while remaining > 0 {
                let Some(item) = self.queue_for_mut(stage).pop_front() else {
                    break;
                };
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &item.texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    &item.bytes,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(item.bytes_per_row),
                        rows_per_image: Some(item.rows_per_image),
                    },
                    item.extent,
                );
                flushed += 1;
                remaining -= 1;
            }
        }
        flushed
    }
}

#[derive(Clone, Copy)]
pub(crate) enum GpuUploadSink<'a> {
    Immediate(&'a wgpu::Queue),
    Pending {
        queues: &'a super::pending_work::HdrPendingWorkQueues,
        stage: HdrGpuUploadStage,
    },
}

pub(crate) fn submit_texture_write<'a>(
    sink: GpuUploadSink<'_>,
    texture: Arc<wgpu::Texture>,
    upload_bytes: Cow<'a, [u8]>,
    bytes_per_row: u32,
    rows_per_image: u32,
    extent: wgpu::Extent3d,
) -> Result<(), String> {
    match sink {
        GpuUploadSink::Immediate(queue) => {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                upload_bytes.as_ref(),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bytes_per_row),
                    rows_per_image: Some(rows_per_image),
                },
                extent,
            );
            Ok(())
        }
        GpuUploadSink::Pending { queues, stage } => {
            queues.gpu_writes.lock().enqueue(
                stage,
                texture,
                upload_bytes.into_owned(),
                bytes_per_row,
                rows_per_image,
                extent,
            );
            queues.bump_active_work(1);
            Ok(())
        }
    }
}
