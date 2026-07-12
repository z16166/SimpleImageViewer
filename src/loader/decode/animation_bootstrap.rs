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

//! First-frame animation bootstrap for raster formats (GIF / APNG / WebP).

use crate::constants::{DEFAULT_ANIMATION_DELAY_MS, MIN_ANIMATION_DELAY_THRESHOLD_MS};
use crate::hdr::types::HdrToneMapSettings;
use crate::loader::{AnimationFrame, ImageData, apply_exif_orientation_to_image_data};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use super::raster::{
    image_frame_to_static_image_data, load_gif_from_mmap, load_png_from_mmap, load_webp_from_mmap,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RasterAnimationFormat {
    Gif,
    Apng,
    Webp,
}

pub(crate) struct RasterAnimationRemainderJob {
    pub path: PathBuf,
    pub mmap: Arc<memmap2::Mmap>,
    pub format: RasterAnimationFormat,
    pub hdr_target_capacity: f32,
    pub hdr_tone_map: HdrToneMapSettings,
    pub cancel: crate::loader::DecodeCancelFlag,
}

pub(crate) struct RasterAnimationBootstrapOutcome {
    pub image: ImageData,
    pub remainder: Option<RasterAnimationRemainderJob>,
}

fn image_frame_delay(frame: &image::Frame) -> Duration {
    let (numer, denom) = frame.delay().numer_denom_ms();
    let delay_ms = numer
        .checked_div(denom)
        .unwrap_or(DEFAULT_ANIMATION_DELAY_MS);
    let delay_ms = if delay_ms <= MIN_ANIMATION_DELAY_THRESHOLD_MS {
        DEFAULT_ANIMATION_DELAY_MS
    } else {
        delay_ms
    };
    Duration::from_millis(delay_ms as u64)
}

fn image_frame_to_animation_frame(frame: image::Frame) -> AnimationFrame {
    let delay = image_frame_delay(&frame);
    let buffer = frame.into_buffer();
    let (width, height) = buffer.dimensions();
    AnimationFrame::new(width, height, buffer.into_raw(), delay)
}

fn raster_animation_remainder_job(
    path: &Path,
    mmap: Arc<memmap2::Mmap>,
    format: RasterAnimationFormat,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
    cancel: crate::loader::DecodeCancelFlag,
) -> RasterAnimationRemainderJob {
    RasterAnimationRemainderJob {
        path: path.to_path_buf(),
        mmap,
        format,
        hdr_target_capacity,
        hdr_tone_map,
        cancel,
    }
}

fn load_raster_animation_bootstrap_from_mmap(
    path: &Path,
    mmap: Arc<memmap2::Mmap>,
    format: RasterAnimationFormat,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
    bootstrap_animation: bool,
    cancel: crate::loader::DecodeCancelFlag,
) -> Result<RasterAnimationBootstrapOutcome, String> {
    use image::AnimationDecoder;

    crate::loader::check_decode_cancel_str(Some(cancel.as_atomic()))?;
    let reader = Cursor::new(mmap.as_ref().as_ref());

    match format {
        RasterAnimationFormat::Gif => {
            use image::codecs::gif::GifDecoder;
            let decoder = GifDecoder::new(reader).map_err(|e| e.to_string())?;
            if !bootstrap_animation {
                let image = load_gif_from_mmap(
                    path,
                    mmap.as_ref(),
                    hdr_target_capacity,
                    hdr_tone_map,
                    Some(cancel.as_atomic()),
                )?;
                return Ok(RasterAnimationBootstrapOutcome {
                    image,
                    remainder: None,
                });
            }
            let mut frames = decoder.into_frames();
            let Some(first) = frames.next().transpose().map_err(|e| e.to_string())? else {
                return Err("GIF animation decoder produced no frames".to_string());
            };
            if frames
                .next()
                .transpose()
                .map_err(|e| e.to_string())?
                .is_none()
            {
                // Single-frame GIF: reuse the already-decoded first frame.
                let image = image_frame_to_static_image_data(first, path, Some(mmap.as_ref()));
                return Ok(RasterAnimationBootstrapOutcome {
                    image,
                    remainder: None,
                });
            }
            let first_anim = image_frame_to_animation_frame(first);
            log::info!(
                "[Loader] raster animation bootstrap: 1 frame ({:?}) -- {}",
                format,
                path.display()
            );
            let image = apply_exif_orientation_to_image_data(
                path,
                ImageData::Animated(vec![first_anim]),
                Some(mmap.as_ref()),
            );
            Ok(RasterAnimationBootstrapOutcome {
                image,
                remainder: Some(raster_animation_remainder_job(
                    path,
                    Arc::clone(&mmap),
                    format,
                    hdr_target_capacity,
                    hdr_tone_map,
                    cancel.clone(),
                )),
            })
        }
        RasterAnimationFormat::Apng => {
            use image::codecs::png::PngDecoder;
            let decoder = PngDecoder::new(reader).map_err(|e| e.to_string())?;
            if !decoder.is_apng().map_err(|e| e.to_string())? {
                let image = load_png_from_mmap(
                    path,
                    mmap.as_ref(),
                    hdr_target_capacity,
                    hdr_tone_map,
                    Some(cancel.as_atomic()),
                )?;
                return Ok(RasterAnimationBootstrapOutcome {
                    image,
                    remainder: None,
                });
            }
            if !bootstrap_animation {
                let image = load_png_from_mmap(
                    path,
                    mmap.as_ref(),
                    hdr_target_capacity,
                    hdr_tone_map,
                    Some(cancel.as_atomic()),
                )?;
                return Ok(RasterAnimationBootstrapOutcome {
                    image,
                    remainder: None,
                });
            }
            let mut frames = decoder.apng().map_err(|e| e.to_string())?.into_frames();
            let Some(first) = frames.next().transpose().map_err(|e| e.to_string())? else {
                return Err("APNG decoder produced no frames".to_string());
            };
            if frames
                .next()
                .transpose()
                .map_err(|e| e.to_string())?
                .is_none()
            {
                // Single-frame APNG: reuse the already-decoded first frame.
                let image = image_frame_to_static_image_data(first, path, Some(mmap.as_ref()));
                return Ok(RasterAnimationBootstrapOutcome {
                    image,
                    remainder: None,
                });
            }
            let first_anim = image_frame_to_animation_frame(first);
            log::info!(
                "[Loader] raster animation bootstrap: 1 frame ({:?}) -- {}",
                format,
                path.display()
            );
            let image = apply_exif_orientation_to_image_data(
                path,
                ImageData::Animated(vec![first_anim]),
                Some(mmap.as_ref()),
            );
            Ok(RasterAnimationBootstrapOutcome {
                image,
                remainder: Some(raster_animation_remainder_job(
                    path,
                    Arc::clone(&mmap),
                    format,
                    hdr_target_capacity,
                    hdr_tone_map,
                    cancel.clone(),
                )),
            })
        }
        RasterAnimationFormat::Webp => {
            use image::codecs::webp::WebPDecoder;
            let decoder = WebPDecoder::new(reader).map_err(|e| e.to_string())?;
            if !bootstrap_animation {
                let image = load_webp_from_mmap(
                    path,
                    mmap.as_ref(),
                    hdr_target_capacity,
                    hdr_tone_map,
                    Some(cancel.as_atomic()),
                )?;
                return Ok(RasterAnimationBootstrapOutcome {
                    image,
                    remainder: None,
                });
            }
            let mut frames = decoder.into_frames();
            let Some(first) = frames.next().transpose().map_err(|e| e.to_string())? else {
                return Err("animated WebP decoder produced no frames".to_string());
            };
            if frames
                .next()
                .transpose()
                .map_err(|e| e.to_string())?
                .is_none()
            {
                // Single-frame WebP: reuse the already-decoded first frame.
                let image = image_frame_to_static_image_data(first, path, Some(mmap.as_ref()));
                return Ok(RasterAnimationBootstrapOutcome {
                    image,
                    remainder: None,
                });
            }
            let first_anim = image_frame_to_animation_frame(first);
            log::info!(
                "[Loader] raster animation bootstrap: 1 frame ({:?}) -- {}",
                format,
                path.display()
            );
            let image = apply_exif_orientation_to_image_data(
                path,
                ImageData::Animated(vec![first_anim]),
                Some(mmap.as_ref()),
            );
            Ok(RasterAnimationBootstrapOutcome {
                image,
                remainder: Some(raster_animation_remainder_job(
                    path,
                    Arc::clone(&mmap),
                    format,
                    hdr_target_capacity,
                    hdr_tone_map,
                    cancel.clone(),
                )),
            })
        }
    }
}

pub(crate) fn load_gif_with_bootstrap_from_mmap(
    path: &Path,
    mmap: Arc<memmap2::Mmap>,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
    bootstrap_animation: bool,
    cancel: crate::loader::DecodeCancelFlag,
) -> Result<RasterAnimationBootstrapOutcome, String> {
    load_raster_animation_bootstrap_from_mmap(
        path,
        mmap,
        RasterAnimationFormat::Gif,
        hdr_target_capacity,
        hdr_tone_map,
        bootstrap_animation,
        cancel,
    )
}

pub(crate) fn load_png_with_bootstrap_from_mmap(
    path: &Path,
    mmap: Arc<memmap2::Mmap>,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
    bootstrap_animation: bool,
    cancel: crate::loader::DecodeCancelFlag,
) -> Result<RasterAnimationBootstrapOutcome, String> {
    load_raster_animation_bootstrap_from_mmap(
        path,
        mmap,
        RasterAnimationFormat::Apng,
        hdr_target_capacity,
        hdr_tone_map,
        bootstrap_animation,
        cancel,
    )
}

pub(crate) fn load_webp_with_bootstrap_from_mmap(
    path: &Path,
    mmap: Arc<memmap2::Mmap>,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
    bootstrap_animation: bool,
    cancel: crate::loader::DecodeCancelFlag,
) -> Result<RasterAnimationBootstrapOutcome, String> {
    load_raster_animation_bootstrap_from_mmap(
        path,
        mmap,
        RasterAnimationFormat::Webp,
        hdr_target_capacity,
        hdr_tone_map,
        bootstrap_animation,
        cancel,
    )
}

pub(crate) fn spawn_raster_animation_remainder_decode(
    job: RasterAnimationRemainderJob,
    tx: crate::loader::orchestrator::LoaderOutputSender,
    index: usize,
    decode_profile: crate::loader::DecodeProfile,
) {
    use crate::loader::preview_caps::REFINEMENT_POOL;
    use crate::loader::{LoaderOutput, PreviewBundle};

    REFINEMENT_POOL.spawn(move || {
        if job.cancel.is_cancelled() {
            return;
        }
        let RasterAnimationRemainderJob {
            path,
            mmap,
            format,
            hdr_target_capacity,
            hdr_tone_map,
            cancel,
        } = job;
        let image = match format {
            RasterAnimationFormat::Gif => load_gif_from_mmap(
                &path,
                mmap.as_ref(),
                hdr_target_capacity,
                hdr_tone_map,
                Some(cancel.as_atomic()),
            ),
            RasterAnimationFormat::Apng => load_png_from_mmap(
                &path,
                mmap.as_ref(),
                hdr_target_capacity,
                hdr_tone_map,
                Some(cancel.as_atomic()),
            ),
            RasterAnimationFormat::Webp => load_webp_from_mmap(
                &path,
                mmap.as_ref(),
                hdr_target_capacity,
                hdr_tone_map,
                Some(cancel.as_atomic()),
            ),
        };
        drop(mmap);
        if cancel.is_cancelled() {
            return;
        }
        let Ok(image) = image else {
            return;
        };
        log::info!(
            "[Loader] raster animation remainder: {:?} -- {}",
            format,
            path.display()
        );
        let load_result = crate::loader::LoadResult {
            index,
            decode_profile: decode_profile.clone(),
            source_key: crate::loader::source_key_for_path(&path),
            ultra_hdr_capacity_sensitive: false,
            result: Ok(image),
            preview_bundle: PreviewBundle::initial(),
            sdr_fallback_is_placeholder: false,
            target_hdr_capacity: hdr_target_capacity,
            raw_osd: None,
            psd_osd: None,
            uploaded_planes: None,
            device_id: None,
            staged_gpu_plane_upload: false,
        };
        let _ = tx.send(LoaderOutput::Image(Box::new(load_result)));
    });
}
