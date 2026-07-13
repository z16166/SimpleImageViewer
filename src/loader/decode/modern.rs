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

//! AVIF, JPEG XL, HEIF/HIF loaders.

use crate::hdr::types::HdrToneMapSettings;
use crate::loader::{
    DecodedImage, HdrAnimationFrame, ImageData, LoadResult, apply_exif_orientation_to_image_data,
    hdr_gain_map_decode_capacity, hdr_sdr_fallback_rgba8_or_placeholder, source_key_for_path,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[allow(dead_code)]
pub(crate) fn is_avif_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("avif") || ext.eq_ignore_ascii_case("avifs"))
}

pub(crate) fn is_heif_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            ext.eq_ignore_ascii_case("heic")
                || ext.eq_ignore_ascii_case("heif")
                || ext.eq_ignore_ascii_case("hif")
        })
}

#[allow(dead_code)]
pub(crate) fn is_jxl_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("jxl"))
}

pub(crate) fn is_hdr_capable_modern_format_path(path: &Path) -> bool {
    is_avif_path(path) || is_heif_path(path) || is_jxl_path(path)
}

/// Heuristic: modern HDR-capable containers that often embed an SDR preview (EXIF JPEG or
/// libheif thumbnail). Today aliases [`is_hdr_capable_modern_format_path`]; may narrow to
/// verified gain-map containers later.
pub(crate) fn path_may_have_gain_map_embedded_sdr_preview(path: &Path) -> bool {
    is_hdr_capable_modern_format_path(path)
}

pub(crate) struct AvifLoadOutcome {
    pub image: ImageData,
    pub sequence_remainder: Option<AvifSequenceRemainderJob>,
}

pub(crate) struct AvifSequenceRemainderJob {
    pub mmap: Arc<memmap2::Mmap>,
    pub path: PathBuf,
    pub hdr_target_capacity: f32,
    pub hdr_tone_map: HdrToneMapSettings,
    pub cancel: crate::loader::DecodeCancelFlag,
}

#[allow(dead_code)]
pub(crate) fn load_avif_with_target_capacity(
    path: &Path,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
    prefer_embedded_sdr_master: bool,
    cancel: crate::loader::DecodeCancelFlag,
) -> Result<ImageData, String> {
    let mmap = Arc::new(
        crate::mmap_util::map_file(path)
            .map_err(|e| format!("Failed to read AVIF: {e}"))?
            .0,
    );
    load_avif_with_target_capacity_from_mmap(
        path,
        &mmap,
        hdr_target_capacity,
        hdr_tone_map,
        prefer_embedded_sdr_master,
        cancel,
    )
}

pub(crate) fn load_avif_with_target_capacity_from_mmap(
    path: &Path,
    mmap: &Arc<memmap2::Mmap>,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
    prefer_embedded_sdr_master: bool,
    cancel: crate::loader::DecodeCancelFlag,
) -> Result<ImageData, String> {
    load_avif_with_target_capacity_outcome_from_mmap(
        path,
        Arc::clone(mmap),
        hdr_target_capacity,
        hdr_tone_map,
        prefer_embedded_sdr_master,
        false,
        cancel,
    )
    .map(|outcome| outcome.image)
}

#[allow(dead_code)]
pub(crate) fn load_avif_with_target_capacity_outcome(
    path: &Path,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
    prefer_embedded_sdr_master: bool,
    bootstrap_animation: bool,
    cancel: crate::loader::DecodeCancelFlag,
) -> Result<AvifLoadOutcome, String> {
    let mmap = Arc::new(
        crate::mmap_util::map_file(path)
            .map_err(|e| format!("Failed to read AVIF: {e}"))?
            .0,
    );
    load_avif_with_target_capacity_outcome_from_mmap(
        path,
        mmap,
        hdr_target_capacity,
        hdr_tone_map,
        prefer_embedded_sdr_master,
        bootstrap_animation,
        cancel,
    )
}

pub(crate) fn load_avif_with_target_capacity_outcome_from_mmap(
    path: &Path,
    mmap: Arc<memmap2::Mmap>,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
    prefer_embedded_sdr_master: bool,
    bootstrap_animation: bool,
    cancel: crate::loader::DecodeCancelFlag,
) -> Result<AvifLoadOutcome, String> {
    load_avif_with_target_capacity_outcome_impl(
        path,
        mmap,
        hdr_target_capacity,
        hdr_tone_map,
        prefer_embedded_sdr_master,
        bootstrap_animation,
        cancel,
    )
}

#[cfg(feature = "avif-native")]
pub(crate) fn spawn_avif_sequence_remainder_decode(
    job: AvifSequenceRemainderJob,
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
        #[cfg(target_os = "windows")]
        let _com = crate::wic::ComGuard::new();

        let decode_capacity =
            hdr_gain_map_decode_capacity(job.hdr_target_capacity, &job.hdr_tone_map);
        let decode = match crate::hdr::avif::try_decode_avif_image_sequence_hdr_limited(
            job.mmap.as_ref(),
            decode_capacity,
            None,
            Some(job.cancel.as_atomic()),
        ) {
            Ok(Some(decode)) => decode,
            Ok(None) => return,
            Err(err) => {
                log::warn!(
                    "[Loader] AVIF sequence remainder decode failed for {}: {err}",
                    job.path.display()
                );
                return;
            }
        };
        if job.cancel.is_cancelled() {
            return;
        }
        let frames: Result<Vec<HdrAnimationFrame>, String> = decode
            .frames
            .into_iter()
            .map(|(delay, hdr)| {
                let fallback = DecodedImage::from_hdr_sdr_fallback(
                    hdr.width,
                    hdr.height,
                    hdr_sdr_fallback_rgba8_or_placeholder(&hdr)?,
                );
                Ok(HdrAnimationFrame::new(hdr, fallback, delay))
            })
            .collect();
        let Ok(frames) = frames else {
            return;
        };
        let image = apply_exif_orientation_to_image_data(
            &job.path,
            ImageData::HdrAnimated(frames),
            Some(job.mmap.as_ref()),
        );
        if job.cancel.is_cancelled() {
            return;
        }
        log::info!(
            "[Loader] AVIF image sequence remainder: {} frames -- {}",
            decode.total_frame_count,
            job.path.display()
        );
        let load_result = LoadResult {
            index,
            decode_profile: decode_profile.clone(),
            source_key: source_key_for_path(&job.path),
            ultra_hdr_capacity_sensitive: true,
            result: Ok(image),
            preview_bundle: PreviewBundle::initial(),
            sdr_fallback_is_placeholder: false,
            target_hdr_capacity: job.hdr_target_capacity,
            raw_osd: None,
            psd_osd: None,
            uploaded_planes: None,
            device_id: None,
            staged_gpu_plane_upload: false,
        };
        let _ = tx.send(LoaderOutput::Image(Box::new(load_result)));
    });
}

#[cfg(not(feature = "avif-native"))]
pub(crate) fn spawn_avif_sequence_remainder_decode(
    _job: AvifSequenceRemainderJob,
    _tx: crate::loader::orchestrator::LoaderOutputSender,
    _index: usize,
    _decode_profile: crate::loader::DecodeProfile,
) {
}

fn hdr_animated_from_sequence_decode(
    path: &Path,
    bytes: &[u8],
    decode: crate::hdr::avif::AvifSequenceDecode,
) -> Result<ImageData, String> {
    let frames: Vec<HdrAnimationFrame> = decode
        .frames
        .into_iter()
        .map(|(delay, hdr)| {
            let fallback = DecodedImage::from_hdr_sdr_fallback(
                hdr.width,
                hdr.height,
                hdr_sdr_fallback_rgba8_or_placeholder(&hdr)?,
            );
            Ok(HdrAnimationFrame::new(hdr, fallback, delay))
        })
        .collect::<Result<Vec<_>, String>>()?;
    log::info!(
        "[Loader] AVIF image sequence: {} frames (HdrAnimated) — {}",
        decode.total_frame_count,
        path.display()
    );
    Ok(apply_exif_orientation_to_image_data(
        path,
        ImageData::HdrAnimated(frames),
        Some(bytes),
    ))
}

fn load_avif_with_target_capacity_outcome_impl(
    path: &Path,
    mmap: Arc<memmap2::Mmap>,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
    prefer_embedded_sdr_master: bool,
    bootstrap_animation: bool,
    cancel: crate::loader::DecodeCancelFlag,
) -> Result<AvifLoadOutcome, String> {
    let bytes = mmap.as_ref();
    #[cfg(feature = "avif-native")]
    {
        crate::loader::check_decode_cancel_str(Some(cancel.as_atomic()))?;
        let gain_map_probe = crate::hdr::avif::avif_probe_gain_map_strip_kind(bytes);
        let skip_embedded_sdr = crate::hdr::avif::path_is_avif_image_sequence(path)
            || matches!(
                gain_map_probe,
                Some(crate::hdr::avif::AvifGainMapStripProbe::PrecomposedHdr)
            );

        let decode_capacity = hdr_gain_map_decode_capacity(hdr_target_capacity, &hdr_tone_map);
        log::debug!(
            "[HDR][AVIF] load path={} hdr_cap={:.3} decode_capacity={:.3} tone_target={:.3} bootstrap={}",
            path.display(),
            hdr_target_capacity,
            decode_capacity,
            hdr_tone_map.target_hdr_capacity(),
            bootstrap_animation
        );
        let max_frames = bootstrap_animation.then_some(1);
        match crate::hdr::avif::try_decode_avif_image_sequence_hdr_limited(
            bytes,
            decode_capacity,
            max_frames,
            Some(cancel.as_atomic()),
        ) {
            Ok(Some(decode)) if decode.total_frame_count > 1 => {
                let remainder =
                    if bootstrap_animation && decode.frames.len() < decode.total_frame_count {
                        log::info!(
                            "[Loader] AVIF sequence bootstrap: {} / {} frames -- {}",
                            decode.frames.len(),
                            decode.total_frame_count,
                            path.display()
                        );
                        Some(AvifSequenceRemainderJob {
                            mmap: Arc::clone(&mmap),
                            path: path.to_path_buf(),
                            hdr_target_capacity,
                            hdr_tone_map,
                            cancel: cancel.clone(),
                        })
                    } else {
                        None
                    };
                let image = hdr_animated_from_sequence_decode(path, bytes, decode)?;
                return Ok(AvifLoadOutcome {
                    image,
                    sequence_remainder: remainder,
                });
            }
            Ok(_) => {}
            Err(e) => {
                log::debug!(
                    "[Loader] AVIF sequence decode failed for {} ({e}); trying static HDR path",
                    path.display()
                );
            }
        }

        crate::loader::check_decode_cancel_str(Some(cancel.as_atomic()))?;
        let try_embedded = !skip_embedded_sdr
            && crate::loader::should_use_embedded_sdr_master_load(
                prefer_embedded_sdr_master,
                hdr_target_capacity,
            );
        match crate::hdr::avif::decode_avif_static_with_optional_embedded_sdr(
            bytes,
            path,
            decode_capacity,
            try_embedded,
        ) {
            Ok(image) => Ok(AvifLoadOutcome {
                image,
                sequence_remainder: None,
            }),
            Err(err) => {
                log::warn!(
                    "[Loader] libavif decode failed for {}: {err}",
                    path.display()
                );
                #[cfg(all(feature = "avif-native", feature = "heif-native"))]
                {
                    let lower = err.to_ascii_lowercase();
                    if lower.contains("invalid ftyp")
                        || lower.contains("ftyp")
                        || lower.contains("file type box")
                    {
                        log::info!(
                            "[Loader] libavif rejected container/brands -- trying libheif for {}",
                            path.display()
                        );
                        return load_heif_hdr_aware_from_bytes(
                            path,
                            bytes,
                            hdr_target_capacity,
                            hdr_tone_map,
                            crate::hdr::heif::HeifHdrDecodeDiag::default(),
                            false,
                            Some(cancel.as_atomic()),
                        )
                        .map(|image| AvifLoadOutcome {
                            image,
                            sequence_remainder: None,
                        })
                        .map_err(|heif_err| {
                            format!(
                                "[Loader] libavif failed ({err}); HEIF fallback also failed ({heif_err})"
                            )
                        });
                    }
                }
                Err(err)
            }
        }
    }

    #[cfg(not(feature = "avif-native"))]
    {
        let _ = (
            path,
            bytes,
            hdr_target_capacity,
            hdr_tone_map,
            prefer_embedded_sdr_master,
            bootstrap_animation,
            cancel,
        );
        Err("AVIF decoding requires the avif-native feature (e.g. hdr-modern-formats).".to_string())
    }
}

#[allow(dead_code)]
pub(crate) fn load_jxl_with_target_capacity(
    path: &Path,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
    prefer_embedded_sdr_master: bool,
    cancel: crate::loader::DecodeCancelFlag,
) -> Result<ImageData, String> {
    load_jxl_with_target_capacity_outcome(
        path,
        hdr_target_capacity,
        hdr_tone_map,
        prefer_embedded_sdr_master,
        false,
        cancel,
    )
    .map(|outcome| outcome.image)
}

pub(crate) fn load_jxl_with_target_capacity_from_mmap(
    path: &Path,
    mmap: &Arc<memmap2::Mmap>,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
    prefer_embedded_sdr_master: bool,
    cancel: crate::loader::DecodeCancelFlag,
) -> Result<ImageData, String> {
    load_jxl_with_target_capacity_outcome_from_mmap(
        path,
        Arc::clone(mmap),
        hdr_target_capacity,
        hdr_tone_map,
        prefer_embedded_sdr_master,
        false,
        cancel,
    )
    .map(|outcome| outcome.image)
}

pub(crate) struct JxlAnimationRemainderJob {
    pub mmap: Arc<memmap2::Mmap>,
    pub path: PathBuf,
    pub hdr_target_capacity: f32,
    pub hdr_tone_map: HdrToneMapSettings,
    pub prefer_embedded_sdr_master: bool,
    pub cancel: crate::loader::DecodeCancelFlag,
}

pub(crate) struct JxlLoadOutcome {
    pub image: ImageData,
    pub remainder_job: Option<JxlAnimationRemainderJob>,
}

#[allow(dead_code)]
pub(crate) fn load_jxl_with_target_capacity_outcome(
    path: &Path,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
    prefer_embedded_sdr_master: bool,
    bootstrap_animation: bool,
    cancel: crate::loader::DecodeCancelFlag,
) -> Result<JxlLoadOutcome, String> {
    let mmap = Arc::new(
        crate::mmap_util::map_file(path)
            .map_err(|err| format!("Failed to read JPEG XL: {err}"))?
            .0,
    );
    load_jxl_with_target_capacity_outcome_from_mmap(
        path,
        mmap,
        hdr_target_capacity,
        hdr_tone_map,
        prefer_embedded_sdr_master,
        bootstrap_animation,
        cancel,
    )
}

pub(crate) fn load_jxl_with_target_capacity_outcome_from_mmap(
    path: &Path,
    mmap: Arc<memmap2::Mmap>,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
    prefer_embedded_sdr_master: bool,
    bootstrap_animation: bool,
    cancel: crate::loader::DecodeCancelFlag,
) -> Result<JxlLoadOutcome, String> {
    #[cfg(feature = "jpegxl")]
    {
        let try_embedded_sdr_master = crate::loader::should_use_embedded_sdr_master_load(
            prefer_embedded_sdr_master,
            hdr_target_capacity,
        );
        let decode_capacity = hdr_gain_map_decode_capacity(hdr_target_capacity, &hdr_tone_map);
        let output = crate::hdr::jpegxl::load_jxl_hdr_with_target_capacity_from_bytes(
            crate::hdr::jpegxl::JxlHdrLoadFromBytesInput {
                path,
                bytes: &mmap[..],
                decode_target_hdr_capacity: decode_capacity,
                display_hdr_target_capacity: hdr_target_capacity,
                tone_map: hdr_tone_map,
                bootstrap_animation,
                try_embedded_sdr_master,
                cancel: Some(cancel.as_atomic()),
            },
        )?;
        let remainder_job = if output.animation_remainder {
            Some(JxlAnimationRemainderJob {
                mmap: Arc::clone(&mmap),
                path: path.to_path_buf(),
                hdr_target_capacity,
                hdr_tone_map,
                prefer_embedded_sdr_master,
                cancel: cancel.clone(),
            })
        } else {
            None
        };
        Ok(JxlLoadOutcome {
            image: apply_exif_orientation_to_image_data(path, output.image, Some(&mmap[..])),
            remainder_job,
        })
    }

    #[cfg(not(feature = "jpegxl"))]
    {
        let _ = (
            path,
            mmap,
            hdr_target_capacity,
            hdr_tone_map,
            prefer_embedded_sdr_master,
            bootstrap_animation,
            cancel,
        );
        Err("JPEG XL support requires the jpegxl feature".to_string())
    }
}

pub(crate) fn spawn_jxl_animation_remainder_decode(
    job: JxlAnimationRemainderJob,
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
        let Ok(image) = load_jxl_with_target_capacity_from_mmap(
            &job.path,
            &job.mmap,
            job.hdr_target_capacity,
            job.hdr_tone_map,
            job.prefer_embedded_sdr_master,
            job.cancel.clone(),
        ) else {
            return;
        };
        if job.cancel.is_cancelled() {
            return;
        }
        log::info!(
            "[Loader] JPEG XL animation remainder: {}",
            job.path.display()
        );
        let load_result = LoadResult {
            index,
            decode_profile: decode_profile.clone(),
            source_key: source_key_for_path(&job.path),
            ultra_hdr_capacity_sensitive: true,
            result: Ok(image),
            preview_bundle: PreviewBundle::initial(),
            sdr_fallback_is_placeholder: false,
            target_hdr_capacity: job.hdr_target_capacity,
            raw_osd: None,
            psd_osd: None,
            uploaded_planes: None,
            device_id: None,
            staged_gpu_plane_upload: false,
        };
        let _ = tx.send(LoaderOutput::Image(Box::new(load_result)));
    });
}

#[allow(dead_code)]
pub(crate) fn load_heif_hdr_aware(
    path: &Path,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
    diag: crate::hdr::heif::HeifHdrDecodeDiag<'_>,
    prefer_embedded_sdr_master: bool,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<ImageData, String> {
    #[cfg(feature = "heif-native")]
    {
        let (mmap, _) = crate::mmap_util::map_file(path)
            .map_err(|err| format!("Failed to read HEIF: {err}"))?;
        load_heif_hdr_aware_from_mmap(
            path,
            &mmap,
            hdr_target_capacity,
            hdr_tone_map,
            diag,
            prefer_embedded_sdr_master,
            cancel,
        )
    }

    #[cfg(not(feature = "heif-native"))]
    {
        let _ = (
            path,
            hdr_target_capacity,
            hdr_tone_map,
            diag,
            prefer_embedded_sdr_master,
            cancel,
        );
        Err(
            "HEIF/HEIC decoding requires the heif-native feature (e.g. hdr-modern-formats)."
                .to_string(),
        )
    }
}

pub(crate) fn load_heif_hdr_aware_from_mmap(
    path: &Path,
    mmap: &memmap2::Mmap,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
    diag: crate::hdr::heif::HeifHdrDecodeDiag<'_>,
    prefer_embedded_sdr_master: bool,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<ImageData, String> {
    load_heif_hdr_aware_from_bytes(
        path,
        &mmap[..],
        hdr_target_capacity,
        hdr_tone_map,
        diag,
        prefer_embedded_sdr_master,
        cancel,
    )
}

pub(crate) fn load_heif_hdr_aware_from_bytes(
    path: &Path,
    bytes: &[u8],
    hdr_target_capacity: f32,
    _hdr_tone_map: HdrToneMapSettings,
    diag: crate::hdr::heif::HeifHdrDecodeDiag<'_>,
    prefer_embedded_sdr_master: bool,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<ImageData, String> {
    #[cfg(feature = "heif-native")]
    {
        crate::loader::check_decode_cancel_str(cancel)?;
        let try_embedded = crate::hdr::heif::heif_should_use_embedded_sdr_primary_load(
            prefer_embedded_sdr_master,
            hdr_target_capacity,
        );
        match crate::hdr::heif::load_heif_with_optional_embedded_sdr_from_bytes(
            bytes,
            path,
            hdr_target_capacity,
            diag,
            try_embedded,
            cancel,
        ) {
            Ok(image) => Ok(apply_exif_orientation_to_image_data(
                path,
                image,
                Some(bytes),
            )),
            Err(err) => {
                log::warn!(
                    "[Loader] libheif decode failed for {}: {err}",
                    path.display()
                );
                Err(err)
            }
        }
    }

    #[cfg(not(feature = "heif-native"))]
    {
        let _ = (
            path,
            bytes,
            hdr_target_capacity,
            hdr_tone_map,
            diag,
            prefer_embedded_sdr_master,
            cancel,
        );
        Err(
            "HEIF/HEIC decoding requires the heif-native feature (e.g. hdr-modern-formats)."
                .to_string(),
        )
    }
}
