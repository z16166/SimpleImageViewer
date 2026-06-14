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

use super::*;

pub(crate) fn upload_jpeg_tiled_source_textures(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    deferred: &crate::hdr::types::IsoGainMapGpuSource,
    physical_width: u32,
    physical_height: u32,
    max_texture_dimension_2d: u32,
) -> Result<(CallbackUpload, CallbackUpload), String> {
    let sdr = upload_rgba8_texture(
        device,
        queue,
        "simple-image-viewer-hdr-tile-jpeg-sdr-texture",
        physical_width,
        physical_height,
        deferred.sdr_rgba.as_slice(),
        HDR_APPLE_GAIN_TEXTURE_FORMAT,
        max_texture_dimension_2d,
    )?;
    let gain = upload_rgba8_texture(
        device,
        queue,
        "simple-image-viewer-hdr-tile-jpeg-gain-texture",
        deferred.gain_width,
        deferred.gain_height,
        deferred.gain_rgba.as_slice(),
        HDR_APPLE_GAIN_TEXTURE_FORMAT,
        max_texture_dimension_2d,
    )?;
    Ok((sdr, gain))
}

#[allow(dead_code)]
pub(crate) fn upload_callback_tile(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    tile: &crate::hdr::tiled::HdrTileBuffer,
) -> Result<CallbackUpload, String> {
    let layout = validate_tile_upload_layout(tile, device.limits().max_texture_dimension_2d)?;
    let (upload_bytes, bytes_per_row) = pack_rows_for_texture_copy(
        rgba32f_as_bytes(tile.rgba_f32.as_slice()),
        tile.width,
        tile.height,
        std::mem::size_of::<f32>() as u32 * 4,
    )
    .map_err(|err| format!("HDR tile upload: {err}"))?;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("simple-image-viewer-hdr-tile-plane-callback-texture"),
        size: layout.size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: layout.format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });

    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &upload_bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(bytes_per_row),
            rows_per_image: Some(layout.size.height),
        },
        layout.size,
    );

    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    Ok(CallbackUpload {
        texture,
        view,
        #[cfg(feature = "heif-native")]
        storage_view: None,
    })
}

pub(crate) fn write_rgba32f_to_texture(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    width: u32,
    height: u32,
    rgba_f32: &[f32],
) -> Result<(), String> {
    let (upload_bytes, bytes_per_row) = pack_rows_for_texture_copy(
        rgba32f_as_bytes(rgba_f32),
        width,
        height,
        std::mem::size_of::<f32>() as u32 * 4,
    )
    .map_err(|err| format!("HDR rgba32f texture write: {err}"))?;
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &upload_bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(bytes_per_row),
            rows_per_image: Some(height),
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    Ok(())
}

pub(crate) fn upload_callback_image(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    image: &HdrImageBuffer,
) -> Result<CallbackUpload, String> {
    let layout = validate_upload_layout(image, device.limits().max_texture_dimension_2d)?;
    let (upload_bytes, bytes_per_row) = pack_rows_for_texture_copy(
        rgba32f_as_bytes(image.rgba_f32.as_slice()),
        image.width,
        image.height,
        std::mem::size_of::<f32>() as u32 * 4,
    )
    .map_err(|err| format!("HDR upload: {err}"))?;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("simple-image-viewer-hdr-image-plane-callback-texture"),
        size: layout.size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: layout.format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });

    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &upload_bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(bytes_per_row),
            rows_per_image: Some(layout.size.height),
        },
        layout.size,
    );

    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    Ok(CallbackUpload {
        texture,
        view,
        #[cfg(feature = "heif-native")]
        storage_view: None,
    })
}

pub(crate) fn wgpu_copy_bytes_per_row(unpadded_bytes_per_row: u32) -> u32 {
    wgpu::util::align_to(unpadded_bytes_per_row, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
}

/// Pack tightly laid-out RGBA rows into the pitch required by [`wgpu::Queue::write_texture`].
pub(crate) fn pack_rows_for_texture_copy<'a>(
    tight: &'a [u8],
    width: u32,
    height: u32,
    bytes_per_pixel: u32,
) -> Result<(Cow<'a, [u8]>, u32), String> {
    let unpadded_bytes_per_row = width
        .checked_mul(bytes_per_pixel)
        .ok_or_else(|| format!("row byte count overflows for width {width}"))?;
    let bytes_per_row = wgpu_copy_bytes_per_row(unpadded_bytes_per_row);
    let expected_len = unpadded_bytes_per_row
        .checked_mul(height)
        .map(|len| len as usize)
        .ok_or_else(|| format!("tight buffer length overflows for {width}x{height}"))?;
    if tight.len() != expected_len {
        return Err(format!(
            "Malformed tight buffer: expected {expected_len} bytes for {width}x{height}, got {}",
            tight.len()
        ));
    }
    if bytes_per_row == unpadded_bytes_per_row {
        return Ok((Cow::Borrowed(tight), bytes_per_row));
    }

    let mut padded = vec![0u8; (bytes_per_row * height) as usize];
    for y in 0..height as usize {
        let src_start = y * unpadded_bytes_per_row as usize;
        let dst_start = y * bytes_per_row as usize;
        padded[dst_start..dst_start + unpadded_bytes_per_row as usize]
            .copy_from_slice(&tight[src_start..src_start + unpadded_bytes_per_row as usize]);
    }
    Ok((Cow::Owned(padded), bytes_per_row))
}

pub(crate) fn upload_rgba8_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    label: &str,
    width: u32,
    height: u32,
    rgba: &[u8],
    format: wgpu::TextureFormat,
    max_texture_dimension_2d: u32,
) -> Result<CallbackUpload, String> {
    let layout =
        validate_rgba8_upload_layout(width, height, rgba.len(), max_texture_dimension_2d, label)?;
    let (upload_bytes, bytes_per_row) = pack_rows_for_texture_copy(rgba, width, height, 4)
        .map_err(|err| format!("{label}: {err}"))?;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: layout.size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &upload_bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(bytes_per_row),
            rows_per_image: Some(layout.size.height),
        },
        layout.size,
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    Ok(CallbackUpload {
        texture,
        view,
        storage_view: None,
    })
}

fn convert_and_scale_preview_to_rgba32f(
    preview: &crate::loader::DecodedImage,
    target_w: u32,
    target_h: u32,
) -> Vec<f32> {
    let mut out = vec![0.0f32; (target_w * target_h * 4) as usize];
    let src = preview.rgba();
    let src_w = preview.width as f32;
    let src_h = preview.height as f32;

    for y in 0..target_h {
        let src_y = (((y as f32 + 0.5) / target_h as f32) * src_h).floor() as u32;
        let src_y = src_y.min(preview.height - 1);
        let src_row_idx = (src_y * preview.width * 4) as usize;

        let dest_row_idx = (y * target_w * 4) as usize;

        for x in 0..target_w {
            let src_x = (((x as f32 + 0.5) / target_w as f32) * src_w).floor() as u32;
            let src_x = src_x.min(preview.width - 1);

            let src_pixel_idx = src_row_idx + (src_x * 4) as usize;
            let dest_pixel_idx = dest_row_idx + (x * 4) as usize;

            out[dest_pixel_idx] = src[src_pixel_idx] as f32 / 255.0;
            out[dest_pixel_idx + 1] = src[src_pixel_idx + 1] as f32 / 255.0;
            out[dest_pixel_idx + 2] = src[src_pixel_idx + 2] as f32 / 255.0;
            out[dest_pixel_idx + 3] = src[src_pixel_idx + 3] as f32 / 255.0;
        }
    }
    out
}

pub(crate) fn upload_r16_uint_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    label: &str,
    width: u32,
    height: u32,
    pixels: &[u16],
    max_texture_dimension_2d: u32,
) -> Result<CallbackUpload, String> {
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
    let (upload_bytes, bytes_per_row) = pack_rows_for_texture_copy(tight_bytes, width, height, 2)
        .map_err(|err| format!("{label}: {err}"))?;

    let texture = device.create_texture(&wgpu::TextureDescriptor {
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
    });

    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &upload_bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(bytes_per_row),
            rows_per_image: Some(height),
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );

    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    Ok(CallbackUpload {
        texture,
        view,
        storage_view: None,
    })
}

const GPU_DEMOSAIC_BOOTSTRAP_PREVIEW: bool = true;

pub(crate) fn upload_image_plane(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    image: &HdrImageBuffer,
) -> Result<ImagePlaneUpload, String> {
    if let Some(ref raw_source) = image.metadata.raw_gpu_source {
        #[cfg(feature = "preload-debug")]
        let upload_started = std::time::Instant::now();
        let base = create_empty_rgba32f_texture(device, image.width, image.height)?;
        let raw_pixels = upload_r16_uint_texture(
            device,
            queue,
            "simple-image-viewer-hdr-raw-pixels-texture",
            raw_source.width,
            raw_source.height,
            raw_source.raw_pixels.as_slice(),
            device.limits().max_texture_dimension_2d,
        )?;

        let raw_green_plane = create_empty_r32f_storage_texture(
            device,
            raw_source.width,
            raw_source.height,
            "simple-image-viewer-hdr-raw-green-plane-texture",
        )?;
        let raw_r_at_green_plane = create_empty_r32f_storage_texture(
            device,
            raw_source.width,
            raw_source.height,
            "simple-image-viewer-hdr-raw-r-at-green-texture",
        )?;
        let raw_b_at_green_plane = create_empty_r32f_storage_texture(
            device,
            raw_source.width,
            raw_source.height,
            "simple-image-viewer-hdr-raw-b-at-green-texture",
        )?;

        if GPU_DEMOSAIC_BOOTSTRAP_PREVIEW {
            if let Some(ref preview) = raw_source.bootstrap_preview {
                let scaled_f32 =
                    convert_and_scale_preview_to_rgba32f(preview, image.width, image.height);
                if let Err(err) = write_rgba32f_to_texture(
                    queue,
                    &base.texture,
                    image.width,
                    image.height,
                    &scaled_f32,
                ) {
                    log::warn!("[HDR] GPU Demosaic bootstrap preview upload failed: {err}");
                }
            }
        }

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
            raw_r_at_green_plane: Some(raw_r_at_green_plane),
            raw_b_at_green_plane: Some(raw_b_at_green_plane),
        });
    }

    if let Some(deferred) = iso_deferred_from_metadata(&image.metadata) {
        let base = create_empty_rgba32f_texture(device, image.width, image.height)?;
        let sdr = upload_rgba8_texture(
            device,
            queue,
            "simple-image-viewer-hdr-image-plane-jpeg-sdr-texture",
            image.width,
            image.height,
            deferred.sdr_rgba.as_slice(),
            HDR_APPLE_GAIN_TEXTURE_FORMAT,
            device.limits().max_texture_dimension_2d,
        )?;
        let gain = upload_rgba8_texture(
            device,
            queue,
            "simple-image-viewer-hdr-image-plane-jpeg-gain-texture",
            deferred.gain_width,
            deferred.gain_height,
            deferred.gain_rgba.as_slice(),
            HDR_APPLE_GAIN_TEXTURE_FORMAT,
            device.limits().max_texture_dimension_2d,
        )?;
        return Ok(ImagePlaneUpload {
            base,
            gain: Some(gain),
            sdr_baseline: Some(sdr),
            raw_pixels: None,
            raw_green_plane: None,
            raw_r_at_green_plane: None,
            raw_b_at_green_plane: None,
        });
    }

    #[cfg(feature = "heif-native")]
    if let Some(deferred) = apple_heic_deferred_from_metadata(&image.metadata) {
        let base = create_empty_rgba32f_texture(device, image.width, image.height)?;
        let gain = upload_rgba8_texture(
            device,
            queue,
            "simple-image-viewer-hdr-image-plane-apple-gain-texture",
            deferred.gain_width,
            deferred.gain_height,
            deferred.gain_rgba.as_slice(),
            HDR_APPLE_GAIN_TEXTURE_FORMAT,
            device.limits().max_texture_dimension_2d,
        )?;
        return Ok(ImagePlaneUpload {
            base,
            gain: Some(gain),
            sdr_baseline: None,
            raw_pixels: None,
            raw_green_plane: None,
            raw_r_at_green_plane: None,
            raw_b_at_green_plane: None,
        });
    }

    let base = upload_callback_image(device, queue, image)?;
    Ok(ImagePlaneUpload {
        base,
        gain: None,
        sdr_baseline: None,
        raw_pixels: None,
        raw_green_plane: None,
        raw_r_at_green_plane: None,
        raw_b_at_green_plane: None,
    })
}

pub(crate) fn create_empty_rgba32f_texture(
    device: &wgpu::Device,
    width: u32,
    height: u32,
) -> Result<CallbackUpload, String> {
    let layout = validate_rgba32f_upload_layout(
        width,
        height,
        width as usize * height as usize * 4,
        device.limits().max_texture_dimension_2d,
        "HDR deferred display texture",
    )?;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
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
    });
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
    let texture = device.create_texture(&wgpu::TextureDescriptor {
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
        usage: wgpu::TextureUsages::STORAGE_BINDING,
        view_formats: &[],
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
        view: storage_view.clone(),
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

    let bytes_per_row = wgpu_copy_bytes_per_row(
        width
            .checked_mul(4)
            .and_then(|channels| channels.checked_mul(std::mem::size_of::<f32>() as u32))
            .ok_or_else(|| format!("{label} row byte count overflows for width {width}"))?,
    );

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

    let bytes_per_row = wgpu_copy_bytes_per_row(
        width
            .checked_mul(4)
            .ok_or_else(|| format!("{label} row byte count overflows for width {width}"))?,
    );

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
