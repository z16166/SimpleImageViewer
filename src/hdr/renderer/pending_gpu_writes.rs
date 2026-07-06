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

/// Pixel payload for [`submit_texture_write`]. Immediate uploads may borrow; pending uploads
/// stage shared ownership without an extra `Vec` clone when rows are already WGSL-aligned.
pub(crate) enum TextureUploadBytes<'a> {
    Cow(Cow<'a, [u8]>),
    #[allow(dead_code)]
    Arc(Arc<[u8]>),
    /// Row-aligned RGBA32F plane backed by `Arc<Vec<f32>>` (zero-copy pending queue).
    Rgba32f(Arc<Vec<f32>>),
}

impl<'a> TextureUploadBytes<'a> {
    pub(crate) fn as_slice(&self) -> &[u8] {
        match self {
            Self::Cow(cow) => cow.as_ref(),
            Self::Arc(arc) => arc.as_ref(),
            Self::Rgba32f(rgba) => rgba32f_as_bytes(rgba.as_slice()),
        }
    }

    fn stage_for_pending(self) -> StagedTextureBytes {
        match self {
            Self::Cow(Cow::Borrowed(slice)) => StagedTextureBytes::Bytes(Arc::from(slice)),
            Self::Cow(Cow::Owned(vec)) => StagedTextureBytes::Bytes(Arc::from(vec)),
            Self::Arc(arc) => StagedTextureBytes::Bytes(arc),
            Self::Rgba32f(rgba) => StagedTextureBytes::Rgba32f(rgba),
        }
    }
}

#[derive(Clone)]
pub(crate) enum StagedTextureBytes {
    Bytes(Arc<[u8]>),
    Rgba32f(Arc<Vec<f32>>),
}

impl StagedTextureBytes {
    fn as_slice(&self) -> &[u8] {
        match self {
            Self::Bytes(bytes) => bytes.as_ref(),
            Self::Rgba32f(rgba) => rgba32f_as_bytes(rgba.as_slice()),
        }
    }
}

#[inline]
fn rgba32f_as_bytes(values: &[f32]) -> &[u8] {
    bytemuck::cast_slice(values)
}

/// Stage for HDR GPU texture writes. Higher stages are flushed first.
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
}

/// Error text returned when the staged GPU write backlog is at capacity.
pub(crate) const HDR_PENDING_GPU_WRITE_QUEUE_FULL: &str =
    "HDR pending GPU write queue full; retry later";

pub(crate) fn pending_gpu_write_queue_full_err(err: &str) -> bool {
    err == HDR_PENDING_GPU_WRITE_QUEUE_FULL
}

/// Max queued GPU writes before new staged writes are refused until flush drains backlog.
pub(crate) const MAX_HDR_PENDING_GPU_WRITES: usize = 256;

/// Max `write_texture` calls drained per logic tick (checklist #3).
pub(crate) const MAX_HDR_GPU_WRITES_PER_LOGIC: usize = 8;

pub(crate) struct PendingGpuWrite {
    pub texture: Arc<wgpu::Texture>,
    pub bytes: StagedTextureBytes,
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

    fn try_enqueue(&mut self, stage: HdrGpuUploadStage, write: PendingGpuWrite) -> Result<(), ()> {
        if self.pending_len() >= MAX_HDR_PENDING_GPU_WRITES {
            return Err(());
        }
        self.queue_for_mut(stage).push_back(write);
        Ok(())
    }

    pub(crate) fn enqueue(
        &mut self,
        stage: HdrGpuUploadStage,
        texture: Arc<wgpu::Texture>,
        bytes: StagedTextureBytes,
        bytes_per_row: u32,
        rows_per_image: u32,
        extent: wgpu::Extent3d,
    ) -> Result<(), ()> {
        self.try_enqueue(
            stage,
            PendingGpuWrite {
                texture,
                bytes,
                bytes_per_row,
                rows_per_image,
                extent,
            },
        )
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
                    item.bytes.as_slice(),
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
    upload_bytes: TextureUploadBytes<'a>,
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
                upload_bytes.as_slice(),
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
            queues
                .gpu_writes
                .lock()
                .enqueue(
                    stage,
                    texture,
                    upload_bytes.stage_for_pending(),
                    bytes_per_row,
                    rows_per_image,
                    extent,
                )
                .map_err(|()| HDR_PENDING_GPU_WRITE_QUEUE_FULL.to_string())?;
            queues.bump_active_work(1);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staged_rgba32f_reuses_arc_allocation_for_pending_queue() {
        let rgba = Arc::new(vec![0.25_f32, 0.5, 0.75, 1.0]);
        let staged = TextureUploadBytes::Rgba32f(Arc::clone(&rgba)).stage_for_pending();
        match staged {
            StagedTextureBytes::Rgba32f(staged_rgba) => {
                assert!(Arc::ptr_eq(&rgba, &staged_rgba));
            }
            StagedTextureBytes::Bytes(_) => panic!("expected Rgba32f staging"),
        }
    }

    #[test]
    fn staged_cow_owned_moves_vec_into_arc_without_extra_clone() {
        let vec = vec![1_u8, 2, 3, 4];
        let staged = TextureUploadBytes::Cow(Cow::Owned(vec)).stage_for_pending();
        match staged {
            StagedTextureBytes::Bytes(arc) => assert_eq!(arc.as_ref(), &[1, 2, 3, 4]),
            StagedTextureBytes::Rgba32f(_) => panic!("expected Bytes staging"),
        }
    }
}
