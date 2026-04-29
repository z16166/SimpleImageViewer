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

use super::types::{HdrImageBuffer, HdrPixelFormat, HdrToneMapSettings};

pub const HDR_IMAGE_PLANE_TEXTURE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba32Float;

#[allow(dead_code)]
pub const HDR_IMAGE_PLANE_SHADER: &str = r#"
// Largest finite half-float value; caps extreme HDR values before tone mapping.
const MAX_FINITE_HDR_VALUE: f32 = 65504.0;
// Current SDR fallback approximates standard display gamma encoding.
const INVERSE_DISPLAY_GAMMA: f32 = 1.0 / 2.2;
// Keeps generated UVs inside the texture for the fullscreen triangle edge.
const MAX_UV_CLAMP: f32 = 0.999999;

struct ToneMapSettings {
    exposure_ev: f32,
    sdr_white_nits: f32,
    max_display_nits: f32,
    _pad: f32,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@group(0) @binding(0) var hdr_texture: texture_2d<f32>;
@group(0) @binding(1) var<uniform> tone_map: ToneMapSettings;

fn reinhard_tone_map(rgb: vec3<f32>) -> vec3<f32> {
    return rgb / (vec3<f32>(1.0) + rgb);
}

fn sanitize_hdr_rgb(rgb: vec3<f32>) -> vec3<f32> {
    let positive = select(vec3<f32>(0.0), rgb, rgb > vec3<f32>(0.0));
    return min(positive, vec3<f32>(MAX_FINITE_HDR_VALUE));
}

fn encode_sdr(rgb: vec3<f32>, settings: ToneMapSettings) -> vec3<f32> {
    let exposure_scale = exp2(settings.exposure_ev);
    let display_scale = settings.sdr_white_nits / max(settings.max_display_nits, settings.sdr_white_nits);
    let exposed = sanitize_hdr_rgb(rgb * exposure_scale * display_scale);
    let mapped = reinhard_tone_map(exposed);
    return pow(clamp(mapped, vec3<f32>(0.0), vec3<f32>(1.0)), vec3<f32>(INVERSE_DISPLAY_GAMMA));
}

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    let positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    let uvs = array<vec2<f32>, 3>(
        vec2<f32>(0.0, 1.0),
        vec2<f32>(2.0, 1.0),
        vec2<f32>(0.0, -1.0),
    );

    var output: VertexOutput;
    output.position = vec4<f32>(positions[vertex_index], 0.0, 1.0);
    output.uv = uvs[vertex_index];
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let texture_size = vec2<f32>(textureDimensions(hdr_texture));
    let clamped_uv = clamp(input.uv, vec2<f32>(0.0), vec2<f32>(MAX_UV_CLAMP));
    let texel = vec2<i32>(clamped_uv * texture_size);
    let hdr = textureLoad(hdr_texture, texel, 0);
    return vec4<f32>(encode_sdr(hdr.rgb, tone_map), clamp(hdr.a, 0.0, 1.0));
}
"#;

#[allow(dead_code)]
pub struct UploadedHdrImage {
    pub size: wgpu::Extent3d,
    pub format: wgpu::TextureFormat,
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub sampler: wgpu::Sampler,
}

pub struct HdrImageRenderer {
    pub tone_map: HdrToneMapSettings,
    uploaded_image: Option<UploadedHdrImage>,
}

impl HdrImageRenderer {
    pub fn new() -> Self {
        Self {
            tone_map: HdrToneMapSettings::default(),
            uploaded_image: None,
        }
    }

    #[allow(dead_code)]
    pub fn uploaded_image(&self) -> Option<&UploadedHdrImage> {
        self.uploaded_image.as_ref()
    }

    #[allow(dead_code)]
    pub fn clear_uploaded_image(&mut self) {
        self.uploaded_image = None;
    }

    #[allow(dead_code)]
    pub fn upload_image(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        image: &HdrImageBuffer,
    ) -> Result<(), String> {
        let layout = validate_upload_layout(image, device.limits().max_texture_dimension_2d)?;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("simple-image-viewer-hdr-image-plane"),
            size: layout.size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: layout.format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let bytes = rgba32f_as_bytes(image.rgba_f32.as_slice());

        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(layout.bytes_per_row),
                rows_per_image: Some(layout.size.height),
            },
            layout.size,
        );

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("simple-image-viewer-hdr-image-plane-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        self.uploaded_image = Some(UploadedHdrImage {
            size: layout.size,
            format: layout.format,
            texture,
            view,
            sampler,
        });

        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HdrUploadLayout {
    size: wgpu::Extent3d,
    bytes_per_row: u32,
    format: wgpu::TextureFormat,
}

fn validate_upload_layout(
    image: &HdrImageBuffer,
    max_texture_dimension_2d: u32,
) -> Result<HdrUploadLayout, String> {
    if image.width == 0 || image.height == 0 {
        return Err(format!(
            "HDR upload requires non-zero dimensions, got {}x{}",
            image.width, image.height
        ));
    }

    if image.format != HdrPixelFormat::Rgba32Float {
        return Err(format!(
            "HDR upload currently supports only Rgba32Float buffers, got {:?}",
            image.format
        ));
    }

    if image.width > max_texture_dimension_2d || image.height > max_texture_dimension_2d {
        return Err(format!(
            "HDR upload dimensions {}x{} exceed device max_texture_dimension_2d {}",
            image.width, image.height, max_texture_dimension_2d
        ));
    }

    let expected_len = image
        .width
        .checked_mul(image.height)
        .and_then(|pixels| pixels.checked_mul(4))
        .map(|len| len as usize)
        .ok_or_else(|| {
            format!(
                "HDR upload dimensions overflow: {}x{}",
                image.width, image.height
            )
        })?;

    if image.rgba_f32.len() != expected_len {
        return Err(format!(
            "Malformed HDR upload buffer: expected {} floats for {}x{} RGBA, got {}",
            expected_len,
            image.width,
            image.height,
            image.rgba_f32.len()
        ));
    }

    let bytes_per_row = image
        .width
        .checked_mul(4)
        .and_then(|channels| channels.checked_mul(std::mem::size_of::<f32>() as u32))
        .ok_or_else(|| {
            format!(
                "HDR upload row byte count overflows for width {}",
                image.width
            )
        })?;

    Ok(HdrUploadLayout {
        size: wgpu::Extent3d {
            width: image.width,
            height: image.height,
            depth_or_array_layers: 1,
        },
        bytes_per_row,
        format: HDR_IMAGE_PLANE_TEXTURE_FORMAT,
    })
}

fn rgba32f_as_bytes(values: &[f32]) -> &[u8] {
    bytemuck::cast_slice(values)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hdr::types::{HdrColorSpace, HdrImageBuffer, HdrPixelFormat};
    use std::sync::Arc;

    #[test]
    fn renderer_starts_without_uploaded_image_state() {
        let renderer = HdrImageRenderer::new();

        assert!(renderer.uploaded_image().is_none());
        assert_eq!(
            HDR_IMAGE_PLANE_TEXTURE_FORMAT,
            wgpu::TextureFormat::Rgba32Float
        );
    }

    #[test]
    fn upload_layout_matches_rgba32f_rows() {
        let image = hdr_image(3, 2, HdrPixelFormat::Rgba32Float, vec![0.0; 3 * 2 * 4]);

        let layout = validate_upload_layout(&image, 4096).expect("valid upload layout");

        assert_eq!(layout.size.width, 3);
        assert_eq!(layout.size.height, 2);
        assert_eq!(
            layout.bytes_per_row,
            3 * 4 * std::mem::size_of::<f32>() as u32
        );
        assert_eq!(layout.format, wgpu::TextureFormat::Rgba32Float);
    }

    #[test]
    fn upload_layout_rejects_zero_dimensions() {
        let image = hdr_image(0, 1, HdrPixelFormat::Rgba32Float, Vec::new());

        let err = validate_upload_layout(&image, 4096).expect_err("reject zero-width upload");

        assert!(err.contains("non-zero"));
    }

    #[test]
    fn upload_layout_rejects_malformed_buffer_length() {
        let image = hdr_image(2, 1, HdrPixelFormat::Rgba32Float, vec![0.0; 7]);

        let err = validate_upload_layout(&image, 4096).expect_err("reject malformed upload");

        assert!(err.contains("expected 8 floats"));
        assert!(err.contains("got 7"));
    }

    #[test]
    fn upload_layout_rejects_unsupported_cpu_format() {
        let image = hdr_image(1, 1, HdrPixelFormat::Rgba16Float, vec![0.0; 4]);

        let err = validate_upload_layout(&image, 4096).expect_err("reject unsupported format");

        assert!(err.contains("Rgba32Float"));
    }

    #[test]
    fn upload_layout_rejects_device_texture_limit_overflow() {
        let image = hdr_image(
            2049,
            1024,
            HdrPixelFormat::Rgba32Float,
            vec![0.0; 2049 * 1024 * 4],
        );

        let err = validate_upload_layout(&image, 2048).expect_err("reject texture limit overflow");

        assert!(err.contains("exceed"));
        assert!(err.contains("2048"));
    }

    #[test]
    fn shader_sanitizes_non_finite_hdr_rgb_before_tone_mapping() {
        assert!(HDR_IMAGE_PLANE_SHADER.contains("fn sanitize_hdr_rgb"));
        assert!(HDR_IMAGE_PLANE_SHADER.contains("rgb > vec3<f32>(0.0)"));
        assert!(HDR_IMAGE_PLANE_SHADER.contains("const MAX_FINITE_HDR_VALUE: f32"));
        assert!(HDR_IMAGE_PLANE_SHADER.contains("min(positive, vec3<f32>(MAX_FINITE_HDR_VALUE))"));
    }

    #[test]
    fn shader_names_tone_map_numeric_constants() {
        assert!(HDR_IMAGE_PLANE_SHADER.contains("const INVERSE_DISPLAY_GAMMA: f32"));
        assert!(HDR_IMAGE_PLANE_SHADER.contains("const MAX_UV_CLAMP: f32"));
        assert!(HDR_IMAGE_PLANE_SHADER.contains("vec3<f32>(INVERSE_DISPLAY_GAMMA)"));
        assert!(HDR_IMAGE_PLANE_SHADER.contains("vec2<f32>(MAX_UV_CLAMP)"));
    }

    #[test]
    fn rgba32f_byte_view_does_not_allocate_or_copy() {
        let values = [1.0, -2.5, 0.25, f32::INFINITY];

        let bytes = rgba32f_as_bytes(&values);

        assert_eq!(bytes.len(), values.len() * std::mem::size_of::<f32>());
        assert_eq!(bytes.as_ptr(), values.as_ptr().cast::<u8>());
        assert_eq!(&bytes[0..4], &1.0_f32.to_ne_bytes());
    }

    fn hdr_image(
        width: u32,
        height: u32,
        format: HdrPixelFormat,
        rgba_f32: Vec<f32>,
    ) -> HdrImageBuffer {
        HdrImageBuffer {
            width,
            height,
            format,
            color_space: HdrColorSpace::LinearSrgb,
            rgba_f32: Arc::new(rgba_f32),
        }
    }
}
