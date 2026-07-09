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

use super::pending_gpu_writes::{
    GpuUploadSink, HdrGpuUploadStage, TextureUploadBytes, submit_texture_write,
};
use super::{SharedGpuTexturePool, *};
use std::sync::Arc;

fn create_poolable_texture(
    device: &wgpu::Device,
    pool: Option<&SharedGpuTexturePool>,
    desc: &wgpu::TextureDescriptor<'_>,
) -> Arc<wgpu::Texture> {
    if let Some(pool) = pool {
        super::texture_pool::acquire_from_shared_pool(pool, device, desc)
    } else {
        Arc::new(device.create_texture(desc))
    }
}

pub(crate) fn upload_jpeg_tiled_source_textures(
    device: &wgpu::Device,
    sink: GpuUploadSink<'_>,
    deferred: &crate::hdr::types::IsoGainMapGpuSource,
    physical_width: u32,
    physical_height: u32,
    max_texture_dimension_2d: u32,
    texture_pool: Option<&SharedGpuTexturePool>,
) -> Result<(CallbackUpload, CallbackUpload), String> {
    let sdr = upload_rgba8_texture(
        device,
        sink,
        HdrGpuUploadStage::AuxRgba8,
        Rgba8TextureUpload {
            label: "simple-image-viewer-hdr-tile-jpeg-sdr-texture",
            width: physical_width,
            height: physical_height,
            rgba: deferred.sdr_rgba.as_slice(),
            format: HDR_APPLE_GAIN_TEXTURE_FORMAT,
            max_texture_dimension_2d,
        },
        texture_pool,
    )?;
    let gain = upload_rgba8_texture(
        device,
        sink,
        HdrGpuUploadStage::AuxRgba8,
        Rgba8TextureUpload {
            label: "simple-image-viewer-hdr-tile-jpeg-gain-texture",
            width: deferred.gain_width,
            height: deferred.gain_height,
            rgba: deferred.gain_rgba.as_slice(),
            format: HDR_APPLE_GAIN_TEXTURE_FORMAT,
            max_texture_dimension_2d,
        },
        texture_pool,
    )?;
    Ok((sdr, gain))
}

#[allow(dead_code)]
pub(crate) fn upload_callback_tile(
    device: &wgpu::Device,
    sink: GpuUploadSink<'_>,
    tile: &crate::hdr::tiled::HdrTileBuffer,
    texture_pool: Option<&SharedGpuTexturePool>,
) -> Result<CallbackUpload, String> {
    let layout = validate_tile_upload_layout(tile, device.limits().max_texture_dimension_2d)?;
    let upload_bytes = pack_rgba32f_for_texture_upload(
        &tile.rgba_f32,
        tile.width,
        tile.height,
        layout.bytes_per_row,
    )
    .map_err(|err| format!("HDR tile upload: {err}"))?;
    let texture = create_poolable_texture(
        device,
        texture_pool,
        &wgpu::TextureDescriptor {
            label: Some("simple-image-viewer-hdr-tile-plane-callback-texture"),
            size: layout.size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: layout.format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        },
    );

    submit_texture_write(
        match sink {
            GpuUploadSink::Immediate(queue) => GpuUploadSink::Immediate(queue),
            GpuUploadSink::Pending { queues, .. } => GpuUploadSink::Pending {
                queues,
                stage: HdrGpuUploadStage::TileCreate,
            },
        },
        Arc::clone(&texture),
        upload_bytes,
        layout.bytes_per_row,
        layout.size.height,
        layout.size,
    )?;

    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    Ok(CallbackUpload {
        texture,
        view,
        #[cfg(feature = "heif-native")]
        storage_view: None,
    })
}

pub(crate) fn write_rgba32f_to_texture(
    sink: GpuUploadSink<'_>,
    texture: Arc<wgpu::Texture>,
    width: u32,
    height: u32,
    rgba_f32: Arc<Vec<f32>>,
) -> Result<(), String> {
    let bytes_per_row = texture_write_bytes_per_row(width, std::mem::size_of::<f32>() as u32 * 4)
        .map_err(|err| format!("HDR rgba32f texture write: {err}"))?;
    let upload_bytes = pack_rgba32f_for_texture_upload(&rgba_f32, width, height, bytes_per_row)
        .map_err(|err| format!("HDR rgba32f texture write: {err}"))?;
    submit_texture_write(
        sink,
        texture,
        upload_bytes,
        bytes_per_row,
        height,
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    )
}

pub(crate) fn upload_callback_image(
    device: &wgpu::Device,
    sink: GpuUploadSink<'_>,
    image: &HdrImageBuffer,
    texture_pool: Option<&SharedGpuTexturePool>,
) -> Result<CallbackUpload, String> {
    let layout = validate_upload_layout(image, device.limits().max_texture_dimension_2d)?;
    let upload_bytes = pack_rgba32f_for_texture_upload(
        &image.rgba_f32,
        image.width,
        image.height,
        layout.bytes_per_row,
    )
    .map_err(|err| format!("HDR upload: {err}"))?;
    let texture = create_poolable_texture(
        device,
        texture_pool,
        &wgpu::TextureDescriptor {
            label: Some("simple-image-viewer-hdr-image-plane-callback-texture"),
            size: layout.size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: layout.format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        },
    );

    submit_texture_write(
        match sink {
            GpuUploadSink::Immediate(queue) => GpuUploadSink::Immediate(queue),
            GpuUploadSink::Pending { queues, .. } => GpuUploadSink::Pending {
                queues,
                stage: HdrGpuUploadStage::PlaneCreate,
            },
        },
        Arc::clone(&texture),
        upload_bytes,
        layout.bytes_per_row,
        layout.size.height,
        layout.size,
    )?;

    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    Ok(CallbackUpload {
        texture,
        view,
        #[cfg(feature = "heif-native")]
        storage_view: None,
    })
}

/// Row pitch required by `Queue::write_texture` for tightly packed source rows.
///
/// `wgpu::COPY_BYTES_PER_ROW_ALIGNMENT` is a buffer-texture copy requirement. It does not apply
/// to `Queue::write_texture`; wgpu validates that path with row-pitch alignment disabled and
/// stages the data internally. Keeping the upload stride tight avoids allocating and filling a
/// full-size padded temporary buffer for large HDR images or tiles.
fn texture_write_bytes_per_row(width: u32, bytes_per_pixel: u32) -> Result<u32, String> {
    width
        .checked_mul(bytes_per_pixel)
        .ok_or_else(|| format!("row byte count overflows for width {width}"))
}

/// Row pitch required by `copy_buffer_to_texture` / `copy_texture_to_buffer` operations.
pub(crate) fn wgpu_copy_bytes_per_row(unpadded_bytes_per_row: u32) -> u32 {
    wgpu::util::align_to(unpadded_bytes_per_row, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
}

/// Prepare RGBA32F rows for `Queue::write_texture`.
///
/// This intentionally keeps the original tight `Arc<Vec<f32>>` even when the row byte count is not
/// a multiple of `COPY_BYTES_PER_ROW_ALIGNMENT`. That alignment only applies to explicit
/// buffer-texture copies, not to `Queue::write_texture`, so padding here would only add a large CPU
/// allocation and row copy before wgpu performs its own staging upload.
pub(crate) fn pack_rgba32f_for_texture_upload(
    rgba_f32: &Arc<Vec<f32>>,
    width: u32,
    height: u32,
    bytes_per_row: u32,
) -> Result<TextureUploadBytes<'_>, String> {
    validate_tight_texture_write_rows(
        rgba32f_as_bytes(rgba_f32.as_slice()),
        width,
        height,
        bytes_per_row,
    )?;

    Ok(TextureUploadBytes::Rgba32f(Arc::clone(rgba_f32)))
}

/// Validate tightly laid-out rows for `Queue::write_texture` without adding row padding.
///
/// Unlike `CommandEncoder::copy_buffer_to_texture`, `Queue::write_texture` accepts unaligned
/// `bytes_per_row`. Returning the original slice here keeps the upload path zero-copy at the
/// application layer while preserving the checked length validation that protects wgpu calls from
/// malformed image buffers.
pub(crate) fn rows_for_texture_write(
    tight: &[u8],
    width: u32,
    height: u32,
    bytes_per_pixel: u32,
) -> Result<(&[u8], u32), String> {
    let bytes_per_row = texture_write_bytes_per_row(width, bytes_per_pixel)?;
    Ok((
        validate_tight_texture_write_rows(tight, width, height, bytes_per_row)?,
        bytes_per_row,
    ))
}

fn validate_tight_texture_write_rows(
    tight: &[u8],
    width: u32,
    height: u32,
    bytes_per_row: u32,
) -> Result<&[u8], String> {
    let expected_len = bytes_per_row
        .checked_mul(height)
        .map(|len| len as usize)
        .ok_or_else(|| format!("tight buffer length overflows for {width}x{height}"))?;
    if tight.len() != expected_len {
        return Err(format!(
            "Malformed tight buffer: expected {expected_len} bytes for {width}x{height}, got {}",
            tight.len()
        ));
    }

    Ok(tight)
}

pub(crate) struct Rgba8TextureUpload<'a> {
    pub(crate) label: &'a str,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) rgba: &'a [u8],
    pub(crate) format: wgpu::TextureFormat,
    pub(crate) max_texture_dimension_2d: u32,
}

pub(crate) struct R16UintTextureUpload<'a> {
    pub(crate) label: &'a str,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) pixels: &'a [u16],
    pub(crate) max_texture_dimension_2d: u32,
}

pub(crate) fn upload_rgba8_texture(
    device: &wgpu::Device,
    sink: GpuUploadSink<'_>,
    stage: HdrGpuUploadStage,
    upload: Rgba8TextureUpload<'_>,
    texture_pool: Option<&SharedGpuTexturePool>,
) -> Result<CallbackUpload, String> {
    let Rgba8TextureUpload {
        label,
        width,
        height,
        rgba,
        format,
        max_texture_dimension_2d,
    } = upload;
    let layout =
        validate_rgba8_upload_layout(width, height, rgba.len(), max_texture_dimension_2d, label)?;
    let upload_bytes = validate_tight_texture_write_rows(rgba, width, height, layout.bytes_per_row)
        .map_err(|err| format!("{label}: {err}"))?;
    let texture = create_poolable_texture(
        device,
        texture_pool,
        &wgpu::TextureDescriptor {
            label: Some(label),
            size: layout.size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        },
    );
    let sink = match sink {
        GpuUploadSink::Immediate(queue) => GpuUploadSink::Immediate(queue),
        GpuUploadSink::Pending { queues, .. } => GpuUploadSink::Pending { queues, stage },
    };
    submit_texture_write(
        sink,
        Arc::clone(&texture),
        TextureUploadBytes::Borrowed(upload_bytes),
        layout.bytes_per_row,
        layout.size.height,
        layout.size,
    )?;
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    Ok(CallbackUpload {
        texture,
        view,
        storage_view: None,
    })
}

pub(crate) fn upload_r16_uint_texture(
    device: &wgpu::Device,
    sink: GpuUploadSink<'_>,
    upload: R16UintTextureUpload<'_>,
    texture_pool: Option<&SharedGpuTexturePool>,
) -> Result<CallbackUpload, String> {
    let R16UintTextureUpload {
        label,
        width,
        height,
        pixels,
        max_texture_dimension_2d,
    } = upload;
    if width == 0 || height == 0 {
        return Err(format!(
            "{label} requires non-zero dimensions, got {width}x{height}"
        ));
    }
    if width > max_texture_dimension_2d || height > max_texture_dimension_2d {
        return Err(format!(
            "{label} dimensions {width}x{height} exceed device max_texture_dimension_2d {max_texture_dimension_2d}",
        ));
    }
    let tight_bytes = bytemuck::cast_slice(pixels);
    let (upload_bytes, bytes_per_row) = rows_for_texture_write(tight_bytes, width, height, 2)
        .map_err(|err| format!("{label}: {err}"))?;

    let texture = create_poolable_texture(
        device,
        texture_pool,
        &wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R16Uint,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        },
    );

    submit_texture_write(
        match sink {
            GpuUploadSink::Immediate(queue) => GpuUploadSink::Immediate(queue),
            GpuUploadSink::Pending { queues, .. } => GpuUploadSink::Pending {
                queues,
                stage: HdrGpuUploadStage::PlaneCreate,
            },
        },
        Arc::clone(&texture),
        TextureUploadBytes::Borrowed(upload_bytes),
        bytes_per_row,
        height,
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    )?;

    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    Ok(CallbackUpload {
        texture,
        view,
        storage_view: None,
    })
}

#[cfg(test)]
pub(crate) fn test_upload_image_plane(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    image: &HdrImageBuffer,
) -> Result<ImagePlaneUpload, String> {
    let pending = super::pending_work::HdrPendingWorkQueues::new_shared();
    let uploaded = upload_image_plane_with_sink(
        device,
        GpuUploadSink::Pending {
            queues: pending.as_ref(),
            stage: HdrGpuUploadStage::PlaneCreate,
        },
        image,
        None,
    )?;
    pending.flush_staged_writes_for_registration(queue);
    Ok(uploaded)
}

/// Loader worker path: enqueue plane GPU writes and respect in-flight concurrency cap.
pub(crate) fn loader_background_upload_image_plane(
    device: &wgpu::Device,
    pending_work: &super::pending_work::HdrPendingWorkQueues,
    image: &HdrImageBuffer,
) -> Result<Option<ImagePlaneUpload>, String> {
    if !pending_work.try_begin_loader_plane_upload() {
        return Ok(None);
    }
    let result = upload_image_plane_with_sink(
        device,
        GpuUploadSink::Pending {
            queues: pending_work,
            stage: HdrGpuUploadStage::PlaneCreate,
        },
        image,
        None,
    )
    .map(Some);
    pending_work.finish_loader_plane_upload();
    result
}

pub(crate) fn upload_image_plane_with_sink(
    device: &wgpu::Device,
    sink: GpuUploadSink<'_>,
    image: &HdrImageBuffer,
    texture_pool: Option<&SharedGpuTexturePool>,
) -> Result<ImagePlaneUpload, String> {
    if let Some(ref raw_source) = image.metadata.raw_gpu_source {
        #[cfg(feature = "preload-debug")]
        let upload_started = std::time::Instant::now();
        let base = create_empty_rgba32f_texture(device, image.width, image.height, texture_pool)?;
        let raw_pixels = upload_r16_uint_texture(
            device,
            sink,
            R16UintTextureUpload {
                label: "simple-image-viewer-hdr-raw-pixels-texture",
                width: raw_source.width,
                height: raw_source.height,
                pixels: raw_source.raw_pixels.as_slice(),
                max_texture_dimension_2d: device.limits().max_texture_dimension_2d,
            },
            texture_pool,
        )?;

        let raw_green_plane = create_empty_r32f_storage_texture(
            device,
            raw_source.width,
            raw_source.height,
            "simple-image-viewer-hdr-raw-green-plane-texture",
        )?;

        #[cfg(feature = "preload-debug")]
        {
            crate::preload_debug!(
                "[PreloadDebug][RAW-GPU] upload plane {}x{} cfa={}x{} bootstrap={} {:.0}ms",
                image.width,
                image.height,
                raw_source.width,
                raw_source.height,
                raw_source.bootstrap_preview.is_some(),
                upload_started.elapsed().as_secs_f64() * 1000.0
            );
        }

        return Ok(ImagePlaneUpload {
            base,
            gain: None,
            sdr_baseline: None,
            raw_pixels: Some(raw_pixels),
            raw_green_plane: Some(raw_green_plane),
        });
    }

    if let Some(deferred) = iso_deferred_from_metadata(&image.metadata) {
        let base = create_empty_rgba32f_texture(device, image.width, image.height, texture_pool)?;
        let sdr = upload_rgba8_texture(
            device,
            sink,
            HdrGpuUploadStage::AuxRgba8,
            Rgba8TextureUpload {
                label: "simple-image-viewer-hdr-image-plane-jpeg-sdr-texture",
                width: image.width,
                height: image.height,
                rgba: deferred.sdr_rgba.as_slice(),
                format: HDR_APPLE_GAIN_TEXTURE_FORMAT,
                max_texture_dimension_2d: device.limits().max_texture_dimension_2d,
            },
            texture_pool,
        )?;
        let gain = upload_rgba8_texture(
            device,
            sink,
            HdrGpuUploadStage::AuxRgba8,
            Rgba8TextureUpload {
                label: "simple-image-viewer-hdr-image-plane-jpeg-gain-texture",
                width: deferred.gain_width,
                height: deferred.gain_height,
                rgba: deferred.gain_rgba.as_slice(),
                format: HDR_APPLE_GAIN_TEXTURE_FORMAT,
                max_texture_dimension_2d: device.limits().max_texture_dimension_2d,
            },
            texture_pool,
        )?;
        return Ok(ImagePlaneUpload {
            base,
            gain: Some(gain),
            sdr_baseline: Some(sdr),
            raw_pixels: None,
            raw_green_plane: None,
        });
    }

    #[cfg(feature = "heif-native")]
    if let Some(deferred) = apple_heic_deferred_from_metadata(&image.metadata) {
        let base = create_empty_rgba32f_texture(device, image.width, image.height, texture_pool)?;
        let gain = upload_rgba8_texture(
            device,
            sink,
            HdrGpuUploadStage::AuxRgba8,
            Rgba8TextureUpload {
                label: "simple-image-viewer-hdr-image-plane-apple-gain-texture",
                width: deferred.gain_width,
                height: deferred.gain_height,
                rgba: deferred.gain_rgba.as_slice(),
                format: HDR_APPLE_GAIN_TEXTURE_FORMAT,
                max_texture_dimension_2d: device.limits().max_texture_dimension_2d,
            },
            texture_pool,
        )?;
        return Ok(ImagePlaneUpload {
            base,
            gain: Some(gain),
            sdr_baseline: None,
            raw_pixels: None,
            raw_green_plane: None,
        });
    }

    let base = upload_callback_image(device, sink, image, texture_pool)?;
    Ok(ImagePlaneUpload {
        base,
        gain: None,
        sdr_baseline: None,
        raw_pixels: None,
        raw_green_plane: None,
    })
}

pub(crate) fn create_empty_rgba32f_texture(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    texture_pool: Option<&SharedGpuTexturePool>,
) -> Result<CallbackUpload, String> {
    let layout = validate_rgba32f_upload_layout(
        width,
        height,
        width as usize * height as usize * 4,
        device.limits().max_texture_dimension_2d,
        "HDR deferred display texture",
    )?;
    let texture = create_poolable_texture(
        device,
        texture_pool,
        &wgpu::TextureDescriptor {
            label: Some("simple-image-viewer-hdr-image-plane-callback-texture"),
            size: layout.size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: layout.format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        },
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let storage_view = texture.create_view(&wgpu::TextureViewDescriptor {
        label: Some("simple-image-viewer-hdr-deferred-display-storage-view"),
        format: Some(wgpu::TextureFormat::Rgba32Float),
        dimension: Some(wgpu::TextureViewDimension::D2),
        aspect: wgpu::TextureAspect::All,
        usage: Some(wgpu::TextureUsages::STORAGE_BINDING),
        ..Default::default()
    });
    Ok(CallbackUpload {
        texture,
        view,
        storage_view: Some(storage_view),
    })
}

pub(crate) fn create_empty_r32f_storage_texture(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    label: &str,
) -> Result<CallbackUpload, String> {
    if width == 0 || height == 0 {
        return Err(format!("{label}: invalid dimensions {width}x{height}"));
    }
    if width > device.limits().max_texture_dimension_2d
        || height > device.limits().max_texture_dimension_2d
    {
        return Err(format!(
            "{label}: dimensions {width}x{height} exceed device limit {}",
            device.limits().max_texture_dimension_2d
        ));
    }
    let texture = Arc::new(device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R32Float,
        usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    }));
    let view = texture.create_view(&wgpu::TextureViewDescriptor {
        label: Some(&format!("{label}-read-view")),
        format: Some(wgpu::TextureFormat::R32Float),
        dimension: Some(wgpu::TextureViewDimension::D2),
        aspect: wgpu::TextureAspect::All,
        usage: Some(wgpu::TextureUsages::TEXTURE_BINDING),
        ..Default::default()
    });
    let storage_view = texture.create_view(&wgpu::TextureViewDescriptor {
        label: Some(&format!("{label}-storage-view")),
        format: Some(wgpu::TextureFormat::R32Float),
        dimension: Some(wgpu::TextureViewDimension::D2),
        aspect: wgpu::TextureAspect::All,
        usage: Some(wgpu::TextureUsages::STORAGE_BINDING),
        ..Default::default()
    });
    Ok(CallbackUpload {
        texture,
        view,
        storage_view: Some(storage_view),
    })
}

pub(crate) fn create_hdr_image_plane_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    hdr_view: &wgpu::TextureView,
    gain_view: &wgpu::TextureView,
    tone_map_buffer: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("simple-image-viewer-hdr-image-plane-bind-group"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(hdr_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(gain_view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: tone_map_buffer.as_entire_binding(),
            },
        ],
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HdrUploadLayout {
    pub(super) size: wgpu::Extent3d,
    pub(super) bytes_per_row: u32,
    pub(super) format: wgpu::TextureFormat,
}

pub(crate) fn validate_upload_layout(
    image: &HdrImageBuffer,
    max_texture_dimension_2d: u32,
) -> Result<HdrUploadLayout, String> {
    if image.format != HdrPixelFormat::Rgba32Float {
        return Err(format!(
            "HDR upload currently supports only Rgba32Float buffers, got {:?}",
            image.format
        ));
    }

    if image
        .metadata
        .gain_map
        .as_ref()
        .is_some_and(|gain_map| gain_map.gpu_compose_pending())
    {
        return Err(
            "HDR upload rejected: gain-map GPU compose is pending; rgba_f32 is not display-ready"
                .to_string(),
        );
    }

    validate_rgba32f_upload_layout(
        image.width,
        image.height,
        image.rgba_f32.len(),
        max_texture_dimension_2d,
        "HDR upload",
    )
}

#[allow(dead_code)]
pub(crate) fn validate_tile_upload_layout(
    tile: &crate::hdr::tiled::HdrTileBuffer,
    max_texture_dimension_2d: u32,
) -> Result<HdrUploadLayout, String> {
    validate_rgba32f_upload_layout(
        tile.width,
        tile.height,
        tile.rgba_f32.len(),
        max_texture_dimension_2d,
        "HDR tile upload",
    )
}

pub(crate) fn validate_rgba32f_upload_layout(
    width: u32,
    height: u32,
    actual_len: usize,
    max_texture_dimension_2d: u32,
    label: &str,
) -> Result<HdrUploadLayout, String> {
    if width == 0 || height == 0 {
        return Err(format!(
            "{label} requires non-zero dimensions, got {width}x{height}"
        ));
    }

    if width > max_texture_dimension_2d || height > max_texture_dimension_2d {
        return Err(format!(
            "{label} dimensions {width}x{height} exceed device max_texture_dimension_2d {max_texture_dimension_2d}",
        ));
    }

    let expected_len = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .map(|len| len as usize)
        .ok_or_else(|| format!("{label} dimensions overflow: {width}x{height}"))?;

    if actual_len != expected_len {
        return Err(format!(
            "Malformed {label} buffer: expected {expected_len} floats for {width}x{height} RGBA, got {actual_len}",
        ));
    }

    let bytes_per_row =
        texture_write_bytes_per_row(width, std::mem::size_of::<f32>() as u32 * 4)
            .map_err(|_| format!("{label} row byte count overflows for width {width}"))?;

    Ok(HdrUploadLayout {
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        bytes_per_row,
        format: HDR_IMAGE_PLANE_TEXTURE_FORMAT,
    })
}

pub(crate) fn validate_rgba8_upload_layout(
    width: u32,
    height: u32,
    actual_len: usize,
    max_texture_dimension_2d: u32,
    label: &str,
) -> Result<HdrUploadLayout, String> {
    if width == 0 || height == 0 {
        return Err(format!(
            "{label} requires non-zero dimensions, got {width}x{height}"
        ));
    }

    if width > max_texture_dimension_2d || height > max_texture_dimension_2d {
        return Err(format!(
            "{label} dimensions {width}x{height} exceed device max_texture_dimension_2d {max_texture_dimension_2d}",
        ));
    }

    let expected_len = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .map(|len| len as usize)
        .ok_or_else(|| format!("{label} dimensions overflow: {width}x{height}"))?;

    if actual_len != expected_len {
        return Err(format!(
            "Malformed {label} buffer: expected {expected_len} bytes for {width}x{height} RGBA, got {actual_len}",
        ));
    }

    let bytes_per_row = texture_write_bytes_per_row(width, 4)
        .map_err(|_| format!("{label} row byte count overflows for width {width}"))?;

    Ok(HdrUploadLayout {
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        bytes_per_row,
        format: wgpu::TextureFormat::Rgba8Unorm,
    })
}

pub(crate) fn rgba32f_as_bytes(values: &[f32]) -> &[u8] {
    bytemuck::cast_slice(values)
}
