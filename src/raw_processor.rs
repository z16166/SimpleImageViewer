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

use image::DynamicImage;
use libraw_sys as ffi;
#[cfg(not(target_os = "windows"))]
use std::ffi::CString;
use std::path::Path;

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawDisplayMode {
    SdrDeveloped,
    SceneLinearHdr,
}

#[allow(dead_code)]
pub(crate) fn unpack_libraw_rgb16_rows_to_rgba_f32(
    rgb16_bytes: &[u8],
    width: u32,
    height: u32,
    row_stride: usize,
    bytes_per_pixel: usize,
) -> Result<Vec<f32>, String> {
    if bytes_per_pixel != 6 {
        return Err(rust_i18n::t!("error.buffer_size_mismatch").to_string());
    }
    let w = width as usize;
    let h = height as usize;
    let tight_row_bytes = w * bytes_per_pixel;
    let inv_scale = 1.0 / 65535.0;
    let mut rgba_f32 = Vec::with_capacity(w * h * crate::constants::RGBA_CHANNELS);
    for y in 0..h {
        let row_off = y * row_stride;
        let row_end = row_off + tight_row_bytes;
        let row = rgb16_bytes
            .get(row_off..row_end)
            .ok_or_else(|| rust_i18n::t!("error.buffer_size_mismatch").to_string())?;
        let dst_start = y * w * 4;
        rgba_f32.resize(dst_start + w * 4, 0.0);
        simple_image_viewer::simd_pixel_convert::normalize_uint16_rgb_scanline_to_rgba32f(
            row,
            &mut rgba_f32[dst_start..dst_start + w * 4],
            w,
            3,
            0.0,
            inv_scale,
        );
    }
    Ok(rgba_f32)
}

pub fn raw_scene_linear_metadata() -> crate::hdr::types::HdrImageMetadata {
    crate::hdr::types::HdrImageMetadata {
        transfer_function: crate::hdr::types::HdrTransferFunction::Linear,
        reference: crate::hdr::types::HdrReference::SceneLinear,
        color_profile: crate::hdr::types::HdrColorProfile::from_color_space(
            crate::hdr::types::HdrColorSpace::LinearSrgb,
        ),
        luminance: crate::hdr::types::HdrLuminanceMetadata::default(),
        gain_map: None,
        raw_gpu_source: None,
    }
}

#[cfg(test)]
type RawColorDiag = (
    i32,
    i32,
    i32,
    [u32; 4],
    u32,
    u32,
    [f32; 4],
    [f32; 4],
    [f32; 4],
    [f32; 4],
);

/// Keeps mmap-backed bytes alive for `libraw_open_buffer` (LibRaw does not copy).
enum RawOpenBacking {
    Mmap(memmap2::Mmap),
}

impl RawOpenBacking {
    fn as_slice(&self) -> &[u8] {
        match self {
            Self::Mmap(mmap) => mmap.as_ref(),
        }
    }
}

pub struct RawProcessor {
    data: *mut ffi::libraw_data_t,
    is_unpacked: bool,
    /// Owned file bytes when opened via [`Self::open_buffer`] / [`Self::open_buffer_mmap`].
    open_backing: Option<RawOpenBacking>,
}

/// RAII wrapper for memory allocated by LibRaw (e.g., via `libraw_dcraw_make_mem_image`).
/// Delegates memory management to [`ffi::LibRawProcessedImageGuard`].
struct LibRawMemory {
    guard: Option<ffi::LibRawProcessedImageGuard>,
}

impl LibRawMemory {
    fn new(ptr: *mut ffi::libraw_processed_image_t) -> Option<Self> {
        if ptr.is_null() {
            None
        } else {
            Some(Self {
                guard: Some(unsafe { ffi::LibRawProcessedImageGuard::from_ptr(ptr) }),
            })
        }
    }

    fn as_ref(&self) -> &ffi::libraw_processed_image_t {
        self.guard
            .as_ref()
            .expect("LibRawMemory used after drop")
            .as_ref()
    }
}

impl Drop for LibRawMemory {
    fn drop(&mut self) {
        self.guard.take();
    }
}

unsafe impl Send for RawProcessor {}

/// LibRaw `green_matching()` on cropped Bayer u16 (G2 index 3 vs G1 index 1).
#[allow(dead_code)] // reserved for future CFA green-plane calibration experiments
fn equilibrate_dual_green_raw(
    pixels: &mut [u16],
    width: u32,
    height: u32,
    color_at: impl Fn(i32, i32) -> u32,
    maximum: u16,
) {
    if width < 8 || height < 8 {
        return;
    }
    let w = width as i32;
    let h = height as i32;
    let margin = 3;
    let thr = maximum as f32 * 0.01;
    let sat = maximum as f32 * 0.95;

    let mut oj = 2i32;
    let mut oi = 2i32;
    if color_at(oj, oi) != 3 {
        oj += 1;
    }
    if color_at(oj, oi) != 3 {
        oi += 1;
    }
    if color_at(oj, oi) != 3 {
        oj -= 1;
    }

    let neighbor_var = |vals: [u16; 4]| -> f32 {
        let a = vals[0] as f32;
        let b = vals[1] as f32;
        let c = vals[2] as f32;
        let d = vals[3] as f32;
        ((a - b).abs()
            + (a - c).abs()
            + (a - d).abs()
            + (b - c).abs()
            + (c - d).abs()
            + (b - d).abs())
            / 6.0
    };

    for j in (oj..h - margin).step_by(2) {
        for i in (oi..w - margin).step_by(2) {
            if color_at(j, i) != 3 {
                continue;
            }
            let o1 = [
                pixels[((j - 1) * w + i - 1) as usize],
                pixels[((j - 1) * w + i + 1) as usize],
                pixels[((j + 1) * w + i - 1) as usize],
                pixels[((j + 1) * w + i + 1) as usize],
            ];
            let o2 = [
                pixels[((j - 2) * w + i) as usize],
                pixels[((j + 2) * w + i) as usize],
                pixels[(j * w + i - 2) as usize],
                pixels[(j * w + i + 2) as usize],
            ];
            let m1 = (o1[0] as f64 + o1[1] as f64 + o1[2] as f64 + o1[3] as f64) / 4.0;
            let m2 = (o2[0] as f64 + o2[1] as f64 + o2[2] as f64 + o2[3] as f64) / 4.0;
            if m2 <= 0.0 {
                continue;
            }
            let c1 = neighbor_var(o1);
            let c2 = neighbor_var(o2);
            let idx = (j * w + i) as usize;
            let v = pixels[idx] as f32;
            if v < sat && c1 < thr && c2 < thr {
                pixels[idx] = (v * (m1 / m2) as f32).clamp(0.0, 65535.0) as u16;
            }
        }
    }
}

/// True when LibRaw `idata.filters` encodes a standard 2x2 Bayer CFA usable by the GPU demosaic shader.
fn libraw_filters_is_standard_bayer(filters: u32, colors: i32) -> bool {
    if colors != 3 {
        return false;
    }
    // 0 = not CFA (Foveon, linear RGB, etc.)
    if filters == 0 {
        return false;
    }
    // Values below 1000 are reserved (Leaf Catchlight=1, legacy X-Trans=2, X-Trans=9, ...).
    filters >= 1000
}

#[cfg(test)]
mod libraw_bayer_filter_tests {
    use super::libraw_filters_is_standard_bayer;

    #[test]
    fn rejects_xtrans_and_other_reserved_filters() {
        assert!(!libraw_filters_is_standard_bayer(0, 3));
        assert!(!libraw_filters_is_standard_bayer(1, 3));
        assert!(!libraw_filters_is_standard_bayer(2, 3));
        assert!(!libraw_filters_is_standard_bayer(9, 3));
    }

    #[test]
    fn accepts_typical_bayer_bitmask() {
        assert!(libraw_filters_is_standard_bayer(0x9494_9494, 3));
    }
}

impl RawProcessor {
    pub fn new() -> Option<Self> {
        unsafe {
            let data = ffi::libraw_init(0);
            if data.is_null() {
                log::error!("{}", rust_i18n::t!("error.libraw_init"));
                None
            } else {
                Some(Self {
                    data,
                    is_unpacked: false,
                    open_backing: None,
                })
            }
        }
    }

    pub fn open_buffer(&mut self, buffer: &[u8]) -> Result<(), String> {
        if buffer.is_empty() {
            return Err("empty buffer".to_string());
        }
        unsafe {
            let ret = ffi::libraw_open_buffer(
                self.data,
                buffer.as_ptr() as *const std::os::raw::c_void,
                buffer.len(),
            );
            if ret != 0 {
                return Err(rust_i18n::t!("error.libraw_open", code = ret).to_string());
            }
        }
        Ok(())
    }

    /// Open from an owned mmap. LibRaw keeps a pointer into the buffer; the mmap must outlive
    /// this processor (stored in [`Self::open_backing`]).
    pub fn open_buffer_mmap(&mut self, mmap: memmap2::Mmap) -> Result<(), String> {
        if mmap.is_empty() {
            return Err("empty buffer".to_string());
        }
        // Capture ptr/len before storing; mapped pages stay valid while owned by `open_backing`.
        let backing = RawOpenBacking::Mmap(mmap);
        let buffer = backing.as_slice();
        let buffer_ptr = buffer.as_ptr();
        let buffer_len = buffer.len();
        self.open_backing = Some(backing);
        unsafe {
            let ret = ffi::libraw_open_buffer(
                self.data,
                buffer_ptr as *const std::os::raw::c_void,
                buffer_len,
            );
            if ret != 0 {
                self.open_backing = None;
                return Err(rust_i18n::t!("error.libraw_open", code = ret).to_string());
            }
        }
        Ok(())
    }

    pub fn open<P: AsRef<Path>>(&mut self, path: P) -> Result<(), String> {
        self.open_backing = None;
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::ffi::OsStrExt;
            let mut wide_path: Vec<u16> = path.as_ref().as_os_str().encode_wide().collect();
            wide_path.push(0);
            unsafe {
                let ret = ffi::libraw_open_wfile(self.data, wide_path.as_ptr());
                if ret != 0 {
                    return Err(rust_i18n::t!("error.libraw_open", code = ret).to_string());
                }
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            let path_str = path.as_ref().to_string_lossy();
            let c_path = CString::new(path_str.as_ref()).map_err(|_| "Invalid path")?;
            unsafe {
                let ret = ffi::libraw_open_file(self.data, c_path.as_ptr());
                if ret != 0 {
                    return Err(rust_i18n::t!("error.libraw_open", code = ret).to_string());
                }
            }
        }
        Ok(())
    }

    pub fn width(&self) -> u32 {
        unsafe { ffi::libraw_get_iwidth(self.data) as u32 }
    }

    pub fn height(&self) -> u32 {
        unsafe { ffi::libraw_get_iheight(self.data) as u32 }
    }

    pub fn flip(&self) -> i32 {
        unsafe { ffi::siv_libraw_get_flip(self.data) }
    }

    pub fn set_user_flip(&mut self, flip: i32) {
        unsafe { ffi::siv_libraw_set_user_flip(self.data, flip) }
    }

    /// CFA / sensor width from LibRaw (`raw_width`). May exceed [`Self::width`] when margins exist.
    pub fn raw_width(&self) -> u32 {
        unsafe { ffi::libraw_get_raw_width(self.data) as u32 }
    }

    /// CFA / sensor height from LibRaw (`raw_height`). May exceed [`Self::height`] when margins exist.
    pub fn raw_height(&self) -> u32 {
        unsafe { ffi::libraw_get_raw_height(self.data) as u32 }
    }

    pub fn is_supported_bayer(&self) -> bool {
        let filters = unsafe { ffi::siv_libraw_get_filters(self.data) };
        let colors = unsafe { ffi::siv_libraw_get_colors(self.data) };
        libraw_filters_is_standard_bayer(filters, colors)
    }

    /// True when the GPU Bayer demosaic path can produce the same geometry as LibRaw develop.
    pub fn is_gpu_demosaic_compatible(&self) -> bool {
        if !self.is_supported_bayer() {
            return false;
        }
        // Fuji Super-CCD HR/SR sensors report a Bayer-like filters bitmask but require
        // LibRaw's 45-degree rotation; rectangular GPU demosaic mis-orients the image.
        if unsafe { ffi::siv_libraw_is_fuji_rotated(self.data) } != 0 {
            return false;
        }
        // Non-square sensor pixels (e.g. Nikon D1X) need LibRaw stretch during develop.
        const ASPECT_EPS: f64 = 0.001;
        let aspect = unsafe { ffi::siv_libraw_get_pixel_aspect(self.data) };
        (aspect - 1.0).abs() <= ASPECT_EPS
    }

    #[cfg(test)]
    pub fn pixel_aspect(&self) -> f64 {
        unsafe { ffi::siv_libraw_get_pixel_aspect(self.data) }
    }

    /// LibRaw `sizes.height` from identify (visible raw area height).
    pub fn sizes_height(&self) -> u32 {
        unsafe { ffi::siv_libraw_get_sizes_height(self.data) as u32 }
    }

    /// LibRaw `sizes.width` from identify (visible raw area width).
    pub fn sizes_width(&self) -> u32 {
        unsafe { ffi::siv_libraw_get_sizes_width(self.data) as u32 }
    }

    /// Developed output grid for tiling and HQ size checks.
    ///
    /// After [`Self::unpack`], uses LibRaw `iwidth`/`iheight` (post-unpack output grid).
    /// Before unpack, uses `sizes.width`/`sizes.height` from identify (not stale `iwidth`).
    pub fn developed_output_dimensions(&self) -> (u32, u32) {
        developed_output_dimensions_from_libraw(
            self.is_unpacked,
            self.width(),
            self.height(),
            self.sizes_width(),
            self.sizes_height(),
        )
    }

    pub fn margins(&self) -> (i32, i32) {
        let mut left = 0;
        let mut top = 0;
        unsafe {
            ffi::siv_libraw_get_margins(self.data, &mut left, &mut top);
        }
        (left, top)
    }

    pub fn unpack(&mut self) -> Result<(), String> {
        unsafe {
            let ret = ffi::libraw_unpack(self.data);
            if ret != 0 {
                return Err(rust_i18n::t!("error.libraw_unpack", code = ret).to_string());
            }
            self.is_unpacked = true;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn is_sensor_data_unpacked(&self) -> bool {
        self.is_unpacked
    }

    #[cfg(test)]
    pub(crate) fn test_color_diag_after_unpack(&self) -> RawColorDiag {
        let mut black = 0i32;
        let mut maximum = 0i32;
        let mut data_maximum = 0i32;
        let mut cblack = [0u32; 4];
        let mut cblack4 = 0u32;
        let mut cblack5 = 0u32;
        let mut pre_mul = [0f32; 4];
        let mut cam_mul = [0f32; 4];
        let mut gpu_cblack = [0f32; 4];
        let mut gpu_scale = [0f32; 4];
        let mut rgb_cam = [0f32; 12];
        unsafe {
            ffi::siv_libraw_get_color_diag(
                self.data,
                &mut black,
                &mut maximum,
                &mut data_maximum,
                cblack.as_mut_ptr(),
                &mut cblack4,
                &mut cblack5,
                pre_mul.as_mut_ptr(),
                cam_mul.as_mut_ptr(),
            );
            ffi::siv_libraw_get_gpu_color_params(
                self.data,
                rgb_cam.as_mut_ptr(),
                gpu_cblack.as_mut_ptr(),
                gpu_scale.as_mut_ptr(),
            );
        }
        (
            black,
            maximum,
            data_maximum,
            cblack,
            cblack4,
            cblack5,
            pre_mul,
            cam_mul,
            gpu_cblack,
            gpu_scale,
        )
    }

    #[cfg(test)]
    pub(crate) fn test_raw_pixel_at(&self, row: u32, col: u32) -> u16 {
        unsafe { ffi::siv_libraw_raw_pixel_at(self.data, row, col) }
    }

    pub fn extract_raw_gpu_source(
        &mut self,
        demosaic_method: crate::settings::RawDemosaicMethod,
    ) -> Result<crate::hdr::types::RawGpuSource, String> {
        if !self.is_unpacked {
            self.unpack()?;
        }

        // Match CPU develop path WB metadata before reading cam_mul / black.
        unsafe {
            ffi::siv_libraw_set_use_camera_wb(self.data, 1);
            ffi::siv_libraw_set_use_camera_matrix(self.data, 1);
            ffi::siv_libraw_set_output_color(self.data, 1);
            ffi::siv_libraw_set_highlight(self.data, 0);
        }

        let w = self.width();
        let h = self.height();
        let total_pixels = w as usize * h as usize;
        let mut scaled_pixels = vec![0u16; total_pixels];
        let mut scaled_w = 0u32;
        let mut scaled_h = 0u32;
        let extract_ret = unsafe {
            ffi::siv_libraw_extract_scaled_cfa(
                self.data,
                scaled_pixels.as_mut_ptr(),
                &mut scaled_w,
                &mut scaled_h,
            )
        };
        if extract_ret != 0 {
            return Err(format!(
                "LibRaw scaled CFA extraction failed (code {extract_ret})"
            ));
        }
        if scaled_w != w || scaled_h != h {
            return Err(format!(
                "LibRaw scaled CFA size mismatch: expected {w}x{h}, got {scaled_w}x{scaled_h}"
            ));
        }

        let (left_margin, top_margin) = self.margins();
        let color_at = |row: i32, col: i32| -> u32 {
            unsafe {
                ffi::siv_libraw_get_color_at(self.data, row + top_margin, col + left_margin) as u32
            }
        };

        // Query colors, filters, and params
        let p00 = color_at(0, 0);
        let p01 = color_at(0, 1);
        let p10 = color_at(1, 0);
        let p11 = color_at(1, 1);
        let bayer_pattern = [p00, p01, p10, p11];

        let mut rgb_cam = [0.0f32; 12];
        let mut _cblack = [0.0f32; 4];
        let mut _cfa_scale = [0.0f32; 4];
        let mut black = 0;
        let mut maximum = 0;
        let mut _cam_mul = [0.0f32; 4];
        let mut _cblack_tmp = [0.0f32; 4];
        unsafe {
            ffi::siv_libraw_get_gpu_color_params(
                self.data,
                rgb_cam.as_mut_ptr(),
                _cblack.as_mut_ptr(),
                _cfa_scale.as_mut_ptr(),
            );
            ffi::siv_libraw_get_color_params(
                self.data,
                _cam_mul.as_mut_ptr(),
                _cblack_tmp.as_mut_ptr(),
                &mut black,
                &mut maximum,
            );
        }

        // CFA buffer is post raw2image_ex + scale_colors (same as CPU dcraw_process).
        // Do not subtract black or re-apply cfa_scale on GPU.
        let black_level = [0.0f32; 4];
        let cfa_scale = [1.0f32; 4];
        let _ = (_cblack, _cfa_scale, black);

        log::debug!(
            "[Loader] RAW GPU source extraction parameters: maximum={}, black_level={:?}, cfa_scale={:?}, rgb_cam={:?}, bayer_pattern={:?}",
            maximum,
            black_level,
            cfa_scale,
            rgb_cam,
            bayer_pattern
        );

        Ok(crate::hdr::types::RawGpuSource {
            raw_width: self.raw_width(),
            raw_height: self.raw_height(),
            width: w,
            height: h,
            raw_pixels: std::sync::Arc::new(scaled_pixels),
            black_level,
            cfa_scale,
            rgb_cam,
            maximum: maximum as f32,
            bayer_pattern,
            demosaic_method,
            scene_color_scale: [1.0, 1.0, 1.0],
            bootstrap_preview: None,
        })
    }

    #[allow(dead_code)]
    pub fn set_half_size(&mut self, value: bool) {
        unsafe { ffi::siv_libraw_set_half_size(self.data, if value { 1 } else { 0 }) }
    }

    fn clamp_scene_color_scale(scale: [f32; 3]) -> [f32; 3] {
        scale.map(|v| {
            if !v.is_finite() || v <= 0.0 {
                1.0
            } else {
                v.clamp(0.25, 4.0)
            }
        })
    }

    /// Match LibRaw develop center brightness to full-res GPU PPG on a contiguous 64x64 patch.
    ///
    /// Uses decimated PPG + auto_bright (small image, no full develop) for the CPU reference
    /// and a ~128x128 center CFA crop for the GPU PPG reference.
    /// Patch-match calib for integration tests; production uses linear baseline (identity scale).
    #[allow(dead_code)]
    pub fn estimate_gpu_scene_color_scale_from_patch_match(
        &mut self,
        source: &crate::hdr::types::RawGpuSource,
        rgb_cam: &[f32; 12],
    ) -> [f32; 3] {
        let mut ab_luma = 0.0f64;
        let status = unsafe {
            ffi::siv_libraw_decimated_ppg_scene_ab_luma_sum(
                self.data,
                rgb_cam.as_ptr(),
                &mut ab_luma,
            )
        };
        let gpu_sum = crate::hdr::raw_demosaic_gpu::scene_linear_center_luma_from_source(source);
        if status == 0 && gpu_sum > 1e-9 && ab_luma > 0.0 {
            const PATCH_PIXELS: f64 = 4096.0;
            let cpu_sum = PATCH_PIXELS * ab_luma;
            let uniform = Self::clamp_scene_color_scale([(cpu_sum / gpu_sum) as f32, 0.0, 0.0])[0];
            log::debug!(
                "[RawProcessor] GPU scene_color_scale={uniform} (patch ab/gpu center luma {cpu_sum:.4}/{gpu_sum:.4})"
            );
            return [uniform, uniform, uniform];
        }
        log::debug!(
            "[RawProcessor] patch/GPU center calib unavailable ({status}); falling back to decimated LibRaw ratio"
        );
        Self::estimate_gpu_scene_color_scale_from_processor(self, rgb_cam)
    }

    /// Decimated center PPG + LibRaw auto_bright vs rgb_cam matrix; uniform luma scale.
    ///
    /// Per-channel [`siv_libraw_decimated_ppg_scene_color_scale`] can diverge badly on some
    /// bodies (e.g. Canon G11) and skew R/G/B when applied full-frame on the GPU.
    pub fn estimate_gpu_scene_color_scale_from_processor(
        &mut self,
        rgb_cam: &[f32; 12],
    ) -> [f32; 3] {
        let mut uniform = 1.0f32;
        let status = unsafe {
            ffi::siv_libraw_decimated_ppg_uniform_scene_scale(
                self.data,
                rgb_cam.as_ptr(),
                &mut uniform,
            )
        };
        if status != 0 {
            log::debug!(
                "[RawProcessor] GPU scene_color_scale decimated calib failed ({status}); using identity"
            );
            return [1.0, 1.0, 1.0];
        }
        let uniform = Self::clamp_scene_color_scale([uniform, uniform, uniform])[0];
        log::debug!(
            "[RawProcessor] GPU scene_color_scale={uniform} (decimated PPG auto_bright/matrix)"
        );
        [uniform, uniform, uniform]
    }

    /// Opens the file and runs decimated calibration (integration / offline tooling).
    #[cfg(test)]
    pub fn estimate_gpu_scene_color_scale(path: &Path) -> [f32; 3] {
        let mut processor = match Self::new() {
            Some(p) => p,
            None => return [1.0, 1.0, 1.0],
        };
        if processor.open(path).is_err() {
            return [1.0, 1.0, 1.0];
        }
        if processor
            .extract_raw_gpu_source(crate::settings::RawDemosaicMethod::Ppg)
            .is_err()
        {
            return [1.0, 1.0, 1.0];
        }
        let rgb_cam = {
            let mut rgb_cam = [0.0f32; 12];
            unsafe {
                ffi::siv_libraw_get_gpu_color_params(
                    processor.data,
                    rgb_cam.as_mut_ptr(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                );
            }
            rgb_cam
        };
        Self::estimate_gpu_scene_color_scale_from_processor(&mut processor, &rgb_cam)
    }

    #[cfg(test)]
    fn libraw_clip_channel(v: f32) -> f32 {
        (v as i32).clamp(0, 65535) as f32
    }

    #[cfg(test)]
    fn ppg_matrix_patch_mean(
        counts: &[u16],
        width: usize,
        height: usize,
        rgb_cam: &[f32; 12],
    ) -> Result<[f64; 3], String> {
        if width == 0 || height == 0 {
            return Err("Invalid dimensions".to_string());
        }
        let cx = width / 2;
        let cy = height / 2;
        let m = rgb_cam;
        let mut mr = 0.0f64;
        let mut mg = 0.0f64;
        let mut mb = 0.0f64;
        for dy in 0..64 {
            for dx in 0..64 {
                let x = cx + dx - 32;
                let y = cy + dy - 32;
                if x >= width || y >= height {
                    continue;
                }
                let i = (y * width + x) * 3;
                let r = counts[i] as f32;
                let g = counts[i + 1] as f32;
                let b = counts[i + 2] as f32;
                let r_val = Self::libraw_clip_channel(m[0] * r + m[1] * g + m[2] * b);
                let g_val = Self::libraw_clip_channel(m[4] * r + m[5] * g + m[6] * b);
                let b_val = Self::libraw_clip_channel(m[8] * r + m[9] * g + m[10] * b);
                mr += r_val as f64 / 65535.0;
                mg += g_val as f64 / 65535.0;
                mb += b_val as f64 / 65535.0;
            }
        }
        let n = 64.0 * 64.0;
        Ok([mr / n, mg / n, mb / n])
    }

    #[cfg(test)]
    fn ppg_rgb16_patch_mean(
        rgb16: &[u16],
        width: usize,
        height: usize,
    ) -> Result<[f64; 3], String> {
        if width == 0 || height == 0 {
            return Err("Invalid dimensions".to_string());
        }
        let cx = width / 2;
        let cy = height / 2;
        let mut dr = 0.0f64;
        let mut dg = 0.0f64;
        let mut db = 0.0f64;
        for dy in 0..64 {
            for dx in 0..64 {
                let x = cx + dx - 32;
                let y = cy + dy - 32;
                if x >= width || y >= height {
                    continue;
                }
                let i = (y * width + x) * 3;
                dr += rgb16[i] as f64 / 65535.0;
                dg += rgb16[i + 1] as f64 / 65535.0;
                db += rgb16[i + 2] as f64 / 65535.0;
            }
        }
        let n = 64.0 * 64.0;
        Ok([dr / n, dg / n, db / n])
    }

    #[cfg(test)]
    #[allow(dead_code)]
    fn scene_color_scale_from_ppg_counts(
        counts: &mut [u16],
        width: u32,
        height: u32,
        rgb_cam: &[f32; 12],
        processor: &mut Self,
    ) -> Result<[f32; 3], String> {
        let matrix_mean =
            Self::ppg_matrix_patch_mean(counts, width as usize, height as usize, rgb_cam)?;
        processor.apply_libraw_output_color(counts, width, height)?;
        let libraw_mean = Self::ppg_rgb16_patch_mean(counts, width as usize, height as usize)?;
        Ok([
            (libraw_mean[0] / matrix_mean[0].max(1e-9)) as f32,
            (libraw_mean[1] / matrix_mean[1].max(1e-9)) as f32,
            (libraw_mean[2] / matrix_mean[2].max(1e-9)) as f32,
        ])
    }

    /// Test-only: full-image PPG + LibRaw `convert_to_rgb` calibration (not on GPU load path).
    #[cfg(test)]
    #[allow(dead_code)]
    pub fn compute_ppg_scene_color_scale_from_processor(
        processor: &mut Self,
        source: &crate::hdr::types::RawGpuSource,
    ) -> Result<[f32; 3], String> {
        let w = source.width;
        let h = source.height;
        if w == 0 || h == 0 {
            return Err("Invalid dimensions".to_string());
        }
        let mut counts = processor.libraw_ppg_camera_rgb_counts_from_scaled()?;
        Self::scene_color_scale_from_ppg_counts(&mut counts, w, h, &source.rgb_cam, processor)
    }

    /// Re-opens file and runs full-image PPG for color calibration experiments.
    #[cfg(test)]
    #[allow(dead_code)]
    pub fn compute_ppg_scene_color_scale(
        path: &std::path::Path,
        source: &crate::hdr::types::RawGpuSource,
    ) -> Result<[f32; 3], String> {
        let mut processor = RawProcessor::new().ok_or("libraw init failed")?;
        processor.open(path)?;
        let mut counts = processor.libraw_ppg_camera_rgb_counts()?;
        Self::scene_color_scale_from_ppg_counts(
            &mut counts,
            source.width,
            source.height,
            &source.rgb_cam,
            &mut processor,
        )
    }

    #[allow(dead_code)]
    pub fn set_use_camera_matrix(&mut self, value: i32) {
        unsafe { ffi::siv_libraw_set_use_camera_matrix(self.data, value) }
    }

    #[allow(dead_code)]
    pub fn set_auto_bright_thr(&mut self, value: f32) {
        unsafe { ffi::siv_libraw_set_auto_bright_thr(self.data, value) }
    }

    pub fn develop(&mut self) -> Result<DynamicImage, String> {
        if !self.is_unpacked {
            self.unpack()?;
        }

        unsafe {
            ffi::libraw_set_output_bps(self.data, 8);
            ffi::siv_libraw_set_use_camera_wb(self.data, 1);
            ffi::siv_libraw_set_use_camera_matrix(self.data, 1);
            ffi::libraw_set_no_auto_bright(self.data, 0);
            ffi::siv_libraw_set_auto_bright_thr(self.data, crate::constants::RAW_AUTO_BRIGHT_THR);

            let ret = ffi::libraw_dcraw_process(self.data);
            if ret != 0 {
                return Err(rust_i18n::t!("error.libraw_process", code = ret).to_string());
            }

            let mut err = 0;
            let processed =
                LibRawMemory::new(ffi::libraw_dcraw_make_mem_image(self.data, &mut err))
                    .ok_or_else(|| {
                        rust_i18n::t!("error.libraw_mem_image", code = err).to_string()
                    })?;

            let img = processed.as_ref();
            if img.image_type != ffi::LibRaw_image_formats::LIBRAW_IMAGE_BITMAP as u32 {
                return Err(rust_i18n::t!(
                    "error.unsupported_raw_type",
                    img_type = img.image_type,
                    expected = ffi::LibRaw_image_formats::LIBRAW_IMAGE_BITMAP as u32
                )
                .to_string());
            }

            if img.colors != crate::constants::RGB_CHANNELS as u16
                || img.bits != crate::constants::BIT_DEPTH_8 as u16
            {
                return Err(rust_i18n::t!(
                    "error.unsupported_raw_format",
                    colors = img.colors,
                    bits = img.bits
                )
                .to_string());
            }

            let width = img.width as u32;
            let height = img.height as u32;
            let data_ptr = img.data.as_ptr();
            let data_len = img.data_size as usize;

            if data_ptr.is_null() || data_len == 0 {
                return Err(rust_i18n::t!("error.libraw_mem_image", code = -1).to_string());
            }

            let expected_min = width as usize * height as usize * crate::constants::RGB_CHANNELS;
            if data_len < expected_min {
                return Err(rust_i18n::t!("error.buffer_size_mismatch").to_string());
            }

            // SINGLE-PASS PACKING OPTIMIZATION:
            let mut rgba = vec![
                crate::constants::MAX_CHANNEL_VALUE;
                width as usize * height as usize * crate::constants::RGBA_CHANNELS
            ];
            let slice = std::slice::from_raw_parts(data_ptr, expected_min);

            simple_image_viewer::simd_swizzle::interleave_rgb_packed_to_rgba_packed(
                slice, &mut rgba,
            );

            let rgba_img = image::RgbaImage::from_raw(width, height, rgba)
                .ok_or_else(|| rust_i18n::t!("error.rgb_image_create_failed").to_string())?;

            Ok(DynamicImage::ImageRgba8(rgba_img))
        }
    }

    /// Test-only: apply LibRaw `convert_to_rgb` to demosaiced camera-RGB counts.
    #[cfg(test)]
    #[allow(dead_code)]
    pub fn apply_libraw_output_color(
        &mut self,
        rgb16: &mut [u16],
        width: u32,
        height: u32,
    ) -> Result<(), String> {
        if width == 0 || height == 0 {
            return Err("Invalid dimensions".to_string());
        }
        let expected = width as usize * height as usize * 3;
        if rgb16.len() < expected {
            return Err("RGB16 buffer too small".to_string());
        }
        if !self.is_unpacked {
            self.unpack()?;
        }
        unsafe {
            ffi::siv_libraw_set_use_camera_wb(self.data, 1);
            ffi::siv_libraw_set_use_camera_matrix(self.data, 1);
            ffi::siv_libraw_set_output_color(self.data, 1);
            ffi::libraw_set_no_auto_bright(self.data, 0);
            ffi::siv_libraw_set_auto_bright_thr(self.data, crate::constants::RAW_AUTO_BRIGHT_THR);
            ffi::siv_libraw_set_gamma(self.data, 1.0, 1.0);
            ffi::siv_libraw_apply_output_color(self.data, rgb16.as_mut_ptr(), width, height);
        }
        Ok(())
    }

    /// Test-only: PPG counts after [`Self::extract_raw_gpu_source`] (same LibRaw session).
    #[cfg(test)]
    #[allow(dead_code)]
    pub fn libraw_ppg_camera_rgb_counts_from_scaled(&mut self) -> Result<Vec<u16>, String> {
        if !self.is_unpacked {
            self.unpack()?;
        }
        let w = self.width();
        let h = self.height();
        let mut out = vec![0u16; w as usize * h as usize * 3];
        let mut out_w = 0u32;
        let mut out_h = 0u32;
        let status = unsafe {
            ffi::siv_libraw_ppg_camera_rgb_counts_from_scaled(
                self.data,
                out.as_mut_ptr(),
                &mut out_w,
                &mut out_h,
            )
        };
        if status != 0 {
            return Err(format!(
                "siv_libraw_ppg_camera_rgb_counts_from_scaled failed: {status}"
            ));
        }
        if out_w != w || out_h != h {
            return Err(format!(
                "LibRaw PPG from_scaled size mismatch: expected {w}x{h}, got {out_w}x{out_h}"
            ));
        }
        Ok(out)
    }

    /// Test-only: LibRaw scale_colors + pre_interpolate + PPG camera RGB counts.
    #[cfg(test)]
    pub fn libraw_ppg_camera_rgb_counts(&mut self) -> Result<Vec<u16>, String> {
        if !self.is_unpacked {
            self.unpack()?;
        }
        let w = self.width();
        let h = self.height();
        let mut out = vec![0u16; w as usize * h as usize * 3];
        let mut out_w = 0u32;
        let mut out_h = 0u32;
        let status = unsafe {
            ffi::siv_libraw_ppg_camera_rgb_counts(
                self.data,
                out.as_mut_ptr(),
                &mut out_w,
                &mut out_h,
            )
        };
        if status != 0 {
            return Err(format!("siv_libraw_ppg_camera_rgb_counts failed: {status}"));
        }
        if out_w != w || out_h != h {
            return Err(format!(
                "LibRaw PPG size mismatch: extract {w}x{h} vs libraw {out_w}x{out_h}"
            ));
        }
        Ok(out)
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub fn ppg_pixel_channels_at(&mut self, row: u32, col: u32) -> Result<[u16; 4], String> {
        if !self.is_unpacked {
            self.unpack()?;
        }
        let mut out = [0u16; 4];
        let status =
            unsafe { ffi::siv_libraw_ppg_pixel_channels(self.data, row, col, out.as_mut_ptr()) };
        if status != 0 {
            return Err(format!("siv_libraw_ppg_pixel_channels failed: {status}"));
        }
        Ok(out)
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub fn ppg_convert_pixel_at(&mut self, row: u32, col: u32) -> Result<[u16; 3], String> {
        if !self.is_unpacked {
            self.unpack()?;
        }
        let mut out = [0u16; 3];
        let status =
            unsafe { ffi::siv_libraw_ppg_convert_pixel(self.data, row, col, out.as_mut_ptr()) };
        if status != 0 {
            return Err(format!("siv_libraw_ppg_convert_pixel failed: {status}"));
        }
        Ok(out)
    }

    pub fn develop_scene_linear_hdr(
        &mut self,
    ) -> Result<crate::hdr::types::HdrImageBuffer, String> {
        // PPG (user_qual=2) matches the GPU demosaic shader; AHD diverges in foliage/high-frequency areas.
        // No auto_bright: GPU path is linear PPG + rgb_cam only; EV/tone-map controls brightness in HDR.
        self.develop_scene_linear_hdr_with_qual(true, 2)
    }

    #[allow(dead_code)]
    pub fn develop_scene_linear_hdr_no_auto_bright(
        &mut self,
    ) -> Result<crate::hdr::types::HdrImageBuffer, String> {
        self.develop_scene_linear_hdr_with_qual(true, 2)
    }

    /// LibRaw `user_qual`: 2 = PPG, 3 = AHD.
    pub fn develop_scene_linear_hdr_with_qual(
        &mut self,
        no_auto_bright: bool,
        user_qual: i32,
    ) -> Result<crate::hdr::types::HdrImageBuffer, String> {
        if !self.is_unpacked {
            self.unpack()?;
        }

        unsafe {
            ffi::libraw_set_output_bps(self.data, 16);
            ffi::siv_libraw_set_use_camera_wb(self.data, 1);
            ffi::siv_libraw_set_use_camera_matrix(self.data, 1);
            ffi::siv_libraw_set_output_color(self.data, 1);
            ffi::libraw_set_no_auto_bright(self.data, if no_auto_bright { 1 } else { 0 });
            ffi::siv_libraw_set_auto_bright_thr(self.data, crate::constants::RAW_AUTO_BRIGHT_THR);
            ffi::siv_libraw_set_gamma(self.data, 1.0, 1.0);
            ffi::siv_libraw_set_user_qual(self.data, user_qual);
            // Match finish_demosaic_rgb_ex used in scene_color_scale calibration.
            ffi::siv_libraw_set_highlight(self.data, 0);

            let ret = ffi::libraw_dcraw_process(self.data);
            if ret != 0 {
                return Err(rust_i18n::t!("error.libraw_process", code = ret).to_string());
            }

            let mut err = 0;
            let processed =
                LibRawMemory::new(ffi::libraw_dcraw_make_mem_image(self.data, &mut err))
                    .ok_or_else(|| {
                        rust_i18n::t!("error.libraw_mem_image", code = err).to_string()
                    })?;

            let img = processed.as_ref();
            if img.image_type != ffi::LibRaw_image_formats::LIBRAW_IMAGE_BITMAP as u32 {
                return Err(rust_i18n::t!(
                    "error.unsupported_raw_type",
                    img_type = img.image_type,
                    expected = ffi::LibRaw_image_formats::LIBRAW_IMAGE_BITMAP as u32
                )
                .to_string());
            }
            if img.colors != crate::constants::RGB_CHANNELS as u16 || img.bits != 16 {
                return Err(rust_i18n::t!(
                    "error.unsupported_raw_format",
                    colors = img.colors,
                    bits = img.bits
                )
                .to_string());
            }

            let width = img.width as u32;
            let height = img.height as u32;
            let data_ptr = img.data.as_ptr();
            let data_len = img.data_size as usize;
            let colors = img.colors as usize;
            let bytes_per_sample = (img.bits as usize) / 8;
            let bytes_per_pixel = colors * bytes_per_sample;
            let tight_row_bytes = width as usize * bytes_per_pixel;
            let tight_size = tight_row_bytes * height as usize;
            if data_ptr.is_null() || data_len < tight_size || bytes_per_pixel == 0 {
                return Err(rust_i18n::t!("error.buffer_size_mismatch").to_string());
            }

            let row_stride = if height > 0 {
                data_len / height as usize
            } else {
                tight_row_bytes
            };
            if row_stride < tight_row_bytes {
                return Err(rust_i18n::t!("error.buffer_size_mismatch").to_string());
            }

            let rgb16_bytes = std::slice::from_raw_parts(data_ptr, data_len);
            let rgba_f32 = unpack_libraw_rgb16_rows_to_rgba_f32(
                rgb16_bytes,
                width,
                height,
                row_stride,
                bytes_per_pixel,
            )?;

            let metadata = raw_scene_linear_metadata();
            Ok(crate::hdr::types::HdrImageBuffer {
                width,
                height,
                format: crate::hdr::types::HdrPixelFormat::Rgba32Float,
                color_space: metadata.color_space_hint(),
                metadata,
                rgba_f32: std::sync::Arc::new(rgba_f32),
            })
        }
    }

    pub fn unpack_thumb(&mut self) -> Result<crate::loader::DecodedImage, String> {
        let mut err = 0;
        unsafe {
            let res = ffi::libraw_unpack_thumb(self.data);
            if res != 0 {
                return Err(rust_i18n::t!("error.libraw_unpack", code = res).to_string());
            }

            let processed =
                LibRawMemory::new(ffi::libraw_dcraw_make_mem_thumb(self.data, &mut err))
                    .ok_or_else(|| {
                        rust_i18n::t!("error.libraw_mem_image", code = err).to_string()
                    })?;

            let img = processed.as_ref();
            let data_ptr = img.data.as_ptr();
            let data_size = img.data_size as usize;

            if data_ptr.is_null() || data_size == 0 {
                return Err(rust_i18n::t!("error.libraw_mem_image", code = -2).to_string());
            }

            let slice = std::slice::from_raw_parts(data_ptr, data_size);

            if img.image_type == ffi::LibRaw_image_formats::LIBRAW_IMAGE_JPEG as u32 {
                // JPEG thumbnail
                match image::load_from_memory(slice) {
                    Ok(decoded) => {
                        let rgba = decoded.into_rgba8();
                        Ok(crate::loader::DecodedImage::new(
                            rgba.width(),
                            rgba.height(),
                            rgba.into_raw(),
                        ))
                    }
                    Err(e) => Err(rust_i18n::t!("error.decode_thumb_failed", err = e).to_string()),
                }
            } else if img.image_type == ffi::LibRaw_image_formats::LIBRAW_IMAGE_BITMAP as u32 {
                // Bitmap thumbnail (RGB)
                if img.colors == crate::constants::RGB_CHANNELS as u16
                    && img.bits == crate::constants::BIT_DEPTH_8 as u16
                {
                    let count = img.width as usize * img.height as usize;
                    let required_rgb = count
                        .checked_mul(crate::constants::RGB_CHANNELS)
                        .filter(|&len| len <= slice.len());
                    if required_rgb.is_none() {
                        return Err(rust_i18n::t!(
                            "error.decode_thumb_failed",
                            err = "bitmap size mismatch"
                        )
                        .to_string());
                    }
                    let rgb_len = required_rgb.unwrap_or(0);
                    let mut rgba = vec![
                        crate::constants::MAX_CHANNEL_VALUE;
                        count * crate::constants::RGBA_CHANNELS
                    ];

                    if let Some(rgb) = image::RgbImage::from_raw(
                        img.width as u32,
                        img.height as u32,
                        slice[..rgb_len].to_vec(),
                    ) {
                        let rgba_img = image::DynamicImage::ImageRgb8(rgb).into_rgba8();
                        Ok(crate::loader::DecodedImage::new(
                            img.width as u32,
                            img.height as u32,
                            rgba_img.into_raw(),
                        ))
                    } else {
                        // Fallback to manual if RgbImage::from_raw fails (shouldn't happen)
                        for i in 0..count {
                            let src = i * crate::constants::RGB_CHANNELS;
                            if src + 2 >= slice.len() {
                                break;
                            }
                            rgba[i * crate::constants::RGBA_CHANNELS] = slice[src];
                            rgba[i * crate::constants::RGBA_CHANNELS + 1] = slice[src + 1];
                            rgba[i * crate::constants::RGBA_CHANNELS + 2] = slice[src + 2];
                        }
                        Ok(crate::loader::DecodedImage::new(
                            img.width as u32,
                            img.height as u32,
                            rgba,
                        ))
                    }
                } else {
                    // Heuristic fallback: Some cameras (like Fuji) might report a thumbnail as
                    // a bitmap type but actually embed a JPEG, or report bits/colors as 0.
                    if slice.len() > crate::constants::RGB_CHANNELS
                        && slice[0] == 0xFF
                        && slice[1] == 0xD8
                        && slice[2] == 0xFF
                    {
                        match image::load_from_memory(slice) {
                            Ok(decoded) => {
                                let rgba = decoded.into_rgba8();
                                Ok(crate::loader::DecodedImage::new(
                                    rgba.width(),
                                    rgba.height(),
                                    rgba.into_raw(),
                                ))
                            }
                            Err(e) => {
                                Err(rust_i18n::t!("error.heuristic_jpeg_failed", err = e)
                                    .to_string())
                            }
                        }
                    } else {
                        Err(rust_i18n::t!(
                            "error.unsupported_thumb_format",
                            colors = img.colors,
                            bits = img.bits,
                            img_type = img.image_type
                        )
                        .to_string())
                    }
                }
            } else {
                Err(
                    rust_i18n::t!("error.unknown_thumb_type", img_type = img.image_type)
                        .to_string(),
                )
            }
        }
    }

    pub fn process_warnings(&self) -> u32 {
        unsafe { ffi::siv_libraw_get_process_warnings(self.data) }
    }
}

impl Drop for RawProcessor {
    fn drop(&mut self) {
        unsafe {
            ffi::libraw_close(self.data);
        }
    }
}

pub fn version() -> String {
    ffi::version()
}

pub const RAW_EXTENSIONS: &[&str] = &[
    "crw", "cr2", "cr3", // Canon
    "nef", "nrw", "nrv", // Nikon
    "arw", "srf", "sr2", "sr1", "sr",  // Sony
    "raf", // Fujifilm
    "orf", "ori", "obm", // Olympus
    "rw2", "raw", // Panasonic
    "pef", "ptx", "pkx", // Pentax
    "3fr", "fff", // Hasselblad
    "iiq", "cap", "eip", // Phase One
    "dcr", "dcs", "drf", "k25", "kdc", "kqc", "kc2", // Kodak
    "rwl", "dng", // Leica (dng is shared, listed generically below too)
    "srw", // Samsung
    "x3f", // Sigma
    "mos", "mef", "mfw", // Leaf / Mamiya
    "erf", // Epson
    "gpr", // GoPro
    "rw1", "j6i", // Ricoh
    "bay", "cam", // Casio
    "ari", // ARRI
    "r3d", // RED
    "stx", "sti", // Sinar
    "pxn", // Logitech
    "mrw", "mdc", // Minolta
    "dng", "rwz", "cxi", "fpix", "rdc", "qtk", // Generic / Other (rawzor, foveon, etc)
];

pub fn is_raw_extension(ext: &str) -> bool {
    RAW_EXTENSIONS
        .iter()
        .any(|raw_ext| raw_ext.eq_ignore_ascii_case(ext))
}

/// LibRaw identifies camera RAW by file content, not extension. Some vendors (e.g. Kodak DCS)
/// store RAW in `.tif` containers; probe before the generic TIFF decoder so we demosaic IFD0
/// instead of showing a tiny embedded RGB preview IFD.
pub fn probe_libraw_can_open_bytes(bytes: &[u8]) -> bool {
    thread_local! {
        static RAW_PROBE: std::cell::RefCell<Option<RawProcessor>> = const { std::cell::RefCell::new(None) };
    }
    RAW_PROBE.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            *slot = RawProcessor::new();
        }
        let Some(processor) = slot.as_mut() else {
            return false;
        };
        if processor.open_buffer(bytes).is_err() {
            return false;
        }
        let w = processor.width();
        let h = processor.height();
        w > 0 && h > 0
    })
}

pub fn probe_libraw_can_open(path: &Path) -> bool {
    let Ok(mmap) = crate::mmap_util::map_file(path) else {
        return false;
    };
    probe_libraw_can_open_bytes(mmap.as_ref())
}

#[cfg(test)]
mod tests {
    use super::{
        RawDisplayMode, RawProcessor, is_raw_extension, probe_libraw_can_open,
        probe_libraw_can_open_bytes, raw_scene_linear_metadata,
        unpack_libraw_rgb16_rows_to_rgba_f32,
    };
    use crate::hdr::types::{HdrReference, HdrTransferFunction};
    use std::path::Path;

    #[test]
    fn unpack_libraw_rgb16_respects_row_stride_padding() {
        // Two RGB pixels per row; LibRaw pads each row to 16 bytes (12 + 4).
        let mut data = vec![0_u8; 32];
        for row in 0..2 {
            let base = row * 16;
            data[base] = 0xFF;
            data[base + 1] = 0xFF; // R
            data[base + 8] = 0xFF;
            data[base + 9] = 0xFF; // G of pixel 2
        }
        let rgba = unpack_libraw_rgb16_rows_to_rgba_f32(&data, 2, 2, 16, 6).expect("unpack");
        assert_eq!(rgba.len(), 2 * 2 * 4);
        assert!((rgba[0] - 1.0).abs() < 0.01); // row0 px0 R
        assert!((rgba[5] - 1.0).abs() < 0.01); // row0 px1 G
        assert!((rgba[8] - 1.0).abs() < 0.01); // row1 px0 R (after stride skip)
        assert!((rgba[13] - 1.0).abs() < 0.01); // row1 px1 G
    }

    #[test]
    fn raw_scene_linear_metadata_enters_hdr_pipeline_as_linear_scene_data() {
        let metadata = raw_scene_linear_metadata();

        assert_eq!(metadata.transfer_function, HdrTransferFunction::Linear);
        assert_eq!(metadata.reference, HdrReference::SceneLinear);
    }

    #[test]
    fn raw_display_mode_defaults_to_existing_sdr_developed_behavior() {
        let mode = RawDisplayMode::SdrDeveloped;

        assert_eq!(mode, RawDisplayMode::SdrDeveloped);
    }

    #[test]
    fn open_buffer_mmap_keeps_backing_alive_for_unpack_thumb() {
        let path = Path::new(r"F:\win7\raws\canon\40d\RAW_CANON_40D_RAW_V103.CR2");
        if !path.is_file() {
            eprintln!("skip: {}", path.display());
            return;
        }

        let mmap = crate::mmap_util::map_file(path).expect("mmap");
        let mut processor = RawProcessor::new().expect("libraw init");
        processor
            .open_buffer_mmap(mmap)
            .expect("open_buffer_mmap should succeed");
        // Before the fix, dropping the local mmap here made unpack_thumb read freed memory.
        let thumb = processor
            .unpack_thumb()
            .expect("unpack_thumb after mmap-backed open");
        assert!(thumb.width > 0 && thumb.height > 0);
    }

    #[test]
    fn tif_extension_is_not_treated_as_raw_by_extension_alone() {
        assert!(!is_raw_extension("tif"));
        assert!(!is_raw_extension("tiff"));
    }

    #[test]
    #[ignore = "manual diagnostic: cargo test analyze_canon_40d_cpu_libraw_path -- --ignored --nocapture"]
    fn analyze_canon_40d_cpu_libraw_path() {
        let path = Path::new(r"F:\win7\raws\canon\40d\RAW_CANON_40D_RAW_V103.CR2");
        if !path.is_file() {
            eprintln!("skip: {}", path.display());
            return;
        }
        let mut p = RawProcessor::new().expect("libraw init");
        p.open(path).expect("open");
        p.unpack().expect("unpack");

        let (
            black,
            maximum,
            data_maximum,
            cblack,
            cblack4,
            cblack5,
            pre_mul,
            cam_mul,
            gpu_cblack,
            gpu_scale,
        ) = p.test_color_diag_after_unpack();
        let (lm, tm) = p.margins();
        let w = p.width();
        let h = p.height();
        let cx = w / 2;
        let cy = h / 2;
        let raw_row = (cy as i32 + tm) as u32;
        let raw_col = (cx as i32 + lm) as u32;
        let raw_px = p.test_raw_pixel_at(raw_row, raw_col);

        let scaled_cfa = {
            let mut ex = RawProcessor::new().expect("libraw init");
            ex.open(path).expect("open");
            let src = ex
                .extract_raw_gpu_source(crate::settings::RawDemosaicMethod::Ppg)
                .expect("extract");
            src.raw_pixels[(cy as usize * w as usize) + cx as usize]
        };

        let ppg_counts_center = {
            let mut pc = RawProcessor::new().expect("libraw init");
            pc.open(path).expect("open");
            let counts = pc.libraw_ppg_camera_rgb_counts().expect("ppg counts");
            let i = (cy as usize * w as usize + cx as usize) * 3;
            (counts[i], counts[i + 1], counts[i + 2])
        };
        let ppg_no_ab = {
            let mut d = RawProcessor::new().expect("libraw init");
            d.open(path).expect("open");
            d.develop_scene_linear_hdr_with_qual(true, 2)
                .expect("develop no ab")
        };
        let ppg_ab = {
            let mut d = RawProcessor::new().expect("libraw init");
            d.open(path).expect("open");
            d.develop_scene_linear_hdr_with_qual(false, 2)
                .expect("develop ab")
        };
        let ahd_ab = {
            let mut d = RawProcessor::new().expect("libraw init");
            d.open(path).expect("open");
            d.develop_scene_linear_hdr_with_qual(false, 3)
                .expect("develop ahd")
        };
        let idx = ((cy as usize * w as usize) + cx as usize) * 4;
        eprintln!("=== Canon 40D CPU LibRaw path diagnostic ===");
        eprintln!("output size: {w}x{h}, margins left={lm} top={tm}");
        eprintln!("color.black={black} color.maximum={maximum} data_maximum={data_maximum}");
        eprintln!("cblack[0..3]={cblack:?} cblack[4]={cblack4} cblack[5]={cblack5}");
        eprintln!("cam_mul={cam_mul:?} pre_mul={pre_mul:?}");
        eprintln!("gpu: cblack_rgb(black+cblack)={gpu_cblack:?} scale_mul={gpu_scale:?}");
        eprintln!("center ({cx},{cy}): raw[{raw_row},{raw_col}]={raw_px} scaled_cfa={scaled_cfa}");
        eprintln!(
            "ppg camera counts at center rgb=({}, {}, {})",
            ppg_counts_center.0, ppg_counts_center.1, ppg_counts_center.2
        );
        let f = |buf: &[f32], i: usize| (buf[i], buf[i + 1], buf[i + 2]);
        let (nr, ng, nb) = f(ppg_no_ab.rgba_f32.as_slice(), idx);
        let (ar, ag, ab) = f(ppg_ab.rgba_f32.as_slice(), idx);
        let (hr, hg, hb) = f(ahd_ab.rgba_f32.as_slice(), idx);
        eprintln!(
            "develop PPG no_auto_bright center=({nr:.6}, {ng:.6}, {nb:.6}) R/B={:.4}",
            nr / nb.max(1e-9)
        );
        eprintln!(
            "develop PPG auto_bright     center=({ar:.6}, {ag:.6}, {ab:.6}) R/B={:.4}",
            ar / ab.max(1e-9)
        );
        eprintln!(
            "develop AHD auto_bright     center=({hr:.6}, {hg:.6}, {hb:.6}) R/B={:.4}",
            hr / hb.max(1e-9)
        );
        eprintln!(
            "auto_bright gain vs no_ab: R={:.3}x G={:.3}x B={:.3}x",
            ar / nr.max(1e-9),
            ag / ng.max(1e-9),
            ab / nb.max(1e-9)
        );
    }

    #[test]
    fn probe_libraw_can_open_bytes_false_for_empty_buffer() {
        assert!(!probe_libraw_can_open_bytes(&[]));
    }

    #[test]
    fn probe_libraw_can_open_false_for_missing_file() {
        assert!(!probe_libraw_can_open(Path::new(
            "definitely_missing_kodak_dcs460d.tif"
        )));
    }

    fn luminance_stats_rgba8(pixels: &[u8]) -> (f64, f64, f64, u8) {
        let mut r_sum = 0u64;
        let mut g_sum = 0u64;
        let mut b_sum = 0u64;
        let mut max = 0u8;
        let mut n = 0u64;
        for chunk in pixels.chunks_exact(4) {
            r_sum += chunk[0] as u64;
            g_sum += chunk[1] as u64;
            b_sum += chunk[2] as u64;
            max = max.max(chunk[0]).max(chunk[1]).max(chunk[2]);
            n += 1;
        }
        if n == 0 {
            return (0.0, 0.0, 0.0, 0);
        }
        (
            r_sum as f64 / n as f64,
            g_sum as f64 / n as f64,
            b_sum as f64 / n as f64,
            max,
        )
    }

    #[test]
    #[ignore]
    fn probe_legacy_raw_hdr_paths() {
        let samples = [
            ("aptus75", Path::new(r"F:\win7\raws\leaf\RAW_APTUS_75.MOS")),
            (
                "aptus22",
                Path::new(r"F:\win7\raws\leaf\aptus22\RAW_LEAF_APTUS_22.MOS"),
            ),
            (
                "mamiya_zd",
                Path::new(r"F:\win7\raws\mamiya\zd\RAW_MAMIYA_ZD.MEF"),
            ),
            (
                "nikon1_v1",
                Path::new(r"F:\win7\raws\nikon\RAW_NIKON1_V1.NEF"),
            ),
        ];
        for (label, path) in samples {
            if !path.is_file() {
                eprintln!("skip {label}: {}", path.display());
                continue;
            }
            let mut processor = RawProcessor::new().expect("libraw init");
            processor.open(path).expect("libraw open");
            let w = processor.width();
            let h = processor.height();
            eprintln!(
                "{label}: libraw {w}x{h} ({:.1} MP)",
                (w as f64 * h as f64) / 1e6
            );

            let mut thumb_processor = RawProcessor::new().expect("libraw init");
            thumb_processor.open(path).expect("libraw open");
            if let Ok(thumb) = thumb_processor.unpack_thumb() {
                let (r, g, b, max) = luminance_stats_rgba8(thumb.rgba());
                eprintln!(
                    "{label}: unpack_thumb {}x{} avg=({r:.1},{g:.1},{b:.1}) max={max}",
                    thumb.width, thumb.height
                );
            } else {
                eprintln!("{label}: unpack_thumb failed");
            }

            let sdr = processor.develop().expect("develop");
            let rgba = sdr.to_rgba8();
            let (r, g, b, max) = luminance_stats_rgba8(rgba.as_raw());
            eprintln!(
                "{label}: develop avg=({r:.1},{g:.1},{b:.1}) max={max} size={}x{}",
                rgba.width(),
                rgba.height()
            );
            assert!(max > 0, "{label}: develop produced all-black image");

            let mut hdr_processor = RawProcessor::new().expect("libraw init");
            hdr_processor.open(path).expect("libraw open");
            let hdr = hdr_processor
                .develop_scene_linear_hdr()
                .expect("develop_scene_linear_hdr");
            let mut max_l = 0.0f32;
            for px in hdr.rgba_f32.chunks_exact(4) {
                let l = 0.2126 * px[0] + 0.7152 * px[1] + 0.0722 * px[2];
                max_l = max_l.max(l);
            }
            eprintln!("{label}: scene_linear max_l={max_l:.6}");

            for cap in [1.0_f32, 4.0_f32] {
                let mut tone_processor = RawProcessor::new().expect("libraw init");
                tone_processor.open(path).expect("libraw open");
                let hdr = tone_processor
                    .develop_scene_linear_hdr()
                    .expect("develop_scene_linear_hdr");
                let fallback = crate::loader::hdr_sdr_fallback_rgba8_or_placeholder(&hdr)
                    .expect("sdr fallback");
                let (r, g, b, max) = luminance_stats_rgba8(fallback.pixels.as_ref());
                eprintln!("{label}: sdr_fallback cap={cap} avg=({r:.1},{g:.1},{b:.1}) max={max}");
                assert!(max > 0, "{label}: sdr_fallback cap={cap} must not be black");
            }
            assert!(max_l > 0.0, "{label}: scene linear HDR is all zero");
        }
    }

    /// Requires `F:\win7\raws\canon\5dm2\RAW_CANON_5DMARK2_PREPROD.CR2` on the test machine.
    #[test]
    fn log_canon_5d2_gpu_extract_metadata() {
        let path = Path::new(r"F:\win7\raws\canon\5dm2\RAW_CANON_5DMARK2_PREPROD.CR2");
        if !path.is_file() {
            eprintln!("skip: Canon 5D2 sample not present at {}", path.display());
            return;
        }
        let mut processor = RawProcessor::new().expect("libraw init");
        processor.open(path).expect("libraw open");
        let source = processor
            .extract_raw_gpu_source(crate::settings::RawDemosaicMethod::Ppg)
            .expect("extract gpu source");
        eprintln!(
            "canon 5d2 gpu meta: maximum={} black_level={:?} cfa_scale={:?} rgb_cam={:?} bayer={:?} size={}x{}",
            source.maximum,
            source.black_level,
            source.cfa_scale,
            source.rgb_cam,
            source.bayer_pattern,
            source.width,
            source.height
        );
        let pixels = source.raw_pixels.as_slice();
        let mut min_v = u16::MAX;
        let mut max_v = 0u16;
        for &v in pixels.iter().step_by(9973) {
            min_v = min_v.min(v);
            max_v = max_v.max(v);
        }
        eprintln!("canon 5d2 raw sample min={min_v} max={max_v}");
    }

    /// Requires `F:\win7\raws\canon\5dm2\RAW_CANON_5DMARK2_PREPROD.CR2` on the test machine.
    #[test]
    fn compare_canon_5d2_cpu_scene_linear_stats() {
        let path = Path::new(r"F:\win7\raws\canon\5dm2\RAW_CANON_5DMARK2_PREPROD.CR2");
        if !path.is_file() {
            eprintln!("skip: Canon 5D2 sample not present at {}", path.display());
            return;
        }
        let mut processor = RawProcessor::new().expect("libraw init");
        processor.open(path).expect("libraw open");
        let hdr = processor
            .develop_scene_linear_hdr()
            .expect("develop_scene_linear_hdr");
        let w = hdr.width as usize;
        let h = hdr.height as usize;
        let cx = w / 2;
        let cy = h / 2;
        let mut r_sum = 0.0f64;
        let mut g_sum = 0.0f64;
        let mut b_sum = 0.0f64;
        let mut count = 0u64;
        for dy in 0..64 {
            for dx in 0..64 {
                let x = cx + dx - 32;
                let y = cy + dy - 32;
                if x >= w || y >= h {
                    continue;
                }
                let i = (y * w + x) * 4;
                r_sum += hdr.rgba_f32[i] as f64;
                g_sum += hdr.rgba_f32[i + 1] as f64;
                b_sum += hdr.rgba_f32[i + 2] as f64;
                count += 1;
            }
        }
        let n = count as f64;
        eprintln!(
            "canon 5d2 cpu center avg rgb=({:.4}, {:.4}, {:.4})",
            r_sum / n,
            g_sum / n,
            b_sum / n
        );
        assert!(
            g_sum > r_sum,
            "expected scene to be G/B dominant (blue night), not red"
        );
    }

    /// Requires `F:\win7\raws\kodak\RAW_KODAK_DCS460D_FILEVERSION_3.TIF` on the test machine.
    #[test]
    #[ignore]
    fn probe_libraw_can_open_kodak_dcs460d_tif() {
        let path = Path::new(r"F:\win7\raws\kodak\RAW_KODAK_DCS460D_FILEVERSION_3.TIF");
        if !path.is_file() {
            eprintln!(
                "skip: Kodak DCS460D sample not present at {}",
                path.display()
            );
            return;
        }
        assert!(
            probe_libraw_can_open(path),
            "LibRaw should recognize Kodak DCS460D TIFF container as camera RAW"
        );
        let mut processor = RawProcessor::new().expect("libraw init");
        processor.open(path).expect("libraw open");
        assert!(
            processor.width() > 256 && processor.height() > 256,
            "expected full sensor dimensions, got {}x{}",
            processor.width(),
            processor.height()
        );
    }
}

/// LibRaw develop output grid: post-unpack `iwidth`/`iheight`, pre-unpack `sizes.width`/`height`.
pub(crate) fn developed_output_dimensions_from_libraw(
    is_unpacked: bool,
    iwidth: u32,
    iheight: u32,
    sizes_width: u32,
    sizes_height: u32,
) -> (u32, u32) {
    if is_unpacked {
        if iwidth > 0 && iheight > 0 {
            (iwidth, iheight)
        } else if sizes_width > 0 && sizes_height > 0 {
            (sizes_width, sizes_height)
        } else {
            (0, 0)
        }
    } else if sizes_width > 0 && sizes_height > 0 {
        (sizes_width, sizes_height)
    } else if iwidth > 0 && iheight > 0 {
        (iwidth, iheight)
    } else {
        (0, 0)
    }
}

#[cfg(test)]
mod developed_output_dimensions_tests {
    use super::developed_output_dimensions_from_libraw;

    #[test]
    fn pre_unpack_uses_sizes_not_stale_iwidth() {
        assert_eq!(
            developed_output_dimensions_from_libraw(false, 640, 424, 3040, 2024),
            (3040, 2024)
        );
    }

    #[test]
    fn post_unpack_uses_iwidth() {
        assert_eq!(
            developed_output_dimensions_from_libraw(true, 6240, 4680, 11662, 8746),
            (6240, 4680)
        );
    }

    #[test]
    fn post_unpack_iwidth_wins_over_sizes() {
        assert_eq!(
            developed_output_dimensions_from_libraw(true, 4000, 3000, 11662, 8746),
            (4000, 3000)
        );
    }
}

pub fn get_supported_extensions() -> Vec<String> {
    // According to LibRaw's design, identification is based on Magic Numbers,
    // not file extensions. For UI filtering purposes, we use this comprehensive
    // list of common professional RAW formats.
    RAW_EXTENSIONS.iter().map(|s| s.to_string()).collect()
}
