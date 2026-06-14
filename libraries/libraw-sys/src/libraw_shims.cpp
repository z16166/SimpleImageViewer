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

#include "libraw/libraw.h"

#include <algorithm>
#include <cmath>
#include <cstring>
#include <limits>
#include <vector>

// Access protected LibRaw color finish for GPU demosaic post-processing.
class LibRawColorShim : public LibRaw {
public:
    /// Match CPU dcraw_process: raw2image_ex(subtract black) then scale_colors.
    static int raw2image_scale_colors(LibRawColorShim *ip) {
        if (!ip) {
            return -1;
        }
        ip->imgdata.params.use_camera_wb = 1;
        ip->imgdata.params.use_camera_matrix = 1;
        ip->imgdata.params.output_color = 1;
        const int rc = ip->raw2image_ex(1);
        if (rc != 0) {
            return rc;
        }
        ip->scale_colors();
        return 0;
    }

    static void apply_color_curve_to_image(LibRawColorShim *ip) {
        if (!ip || !ip->imgdata.image) {
            return;
        }
        const unsigned np =
            static_cast<unsigned>(ip->imgdata.sizes.width) *
            static_cast<unsigned>(ip->imgdata.sizes.height);
        for (unsigned i = 0; i < np; i++) {
            for (int c = 0; c < 3; c++) {
                ip->imgdata.image[i][c] =
                    ip->imgdata.color.curve[ip->imgdata.image[i][c]];
            }
        }
    }

    /// Match dcraw_make_mem_image auto_bright after convert_to_rgb (linear gamma).
    static void apply_auto_bright_after_convert(LibRawColorShim *ip) {
        if (!ip || !ip->libraw_internal_data.output_data.histogram) {
            return;
        }
        libraw_output_params_t &O = ip->imgdata.params;
        libraw_image_sizes_t &S = ip->imgdata.sizes;
        libraw_iparams_t &P1 = ip->imgdata.idata;

        if (O.bright <= 1.0f) {
            O.bright = 8192.0f;
        }

        int perc = 0;
        int val = 0;
        int total = 0;
        int t_white = 0x2000;
        int c = 0;
        perc = static_cast<int>(S.width * S.height * O.auto_bright_thr);
        if (ip->libraw_internal_data.internal_output_params.fuji_width) {
            perc /= 2;
        }
        if (!((O.highlight & ~2) || O.no_auto_bright)) {
            for (t_white = c = 0; c < P1.colors; c++) {
                for (val = 0x2000, total = 0; --val > 32;) {
                    if ((total += ip->libraw_internal_data.output_data.histogram[c][val]) > perc) {
                        break;
                    }
                }
                if (t_white < val) {
                    t_white = val;
                }
            }
        }
        ip->gamma_curve(O.gamm[0], O.gamm[1], 2, (t_white << 3) / O.bright);
        apply_color_curve_to_image(ip);
    }

    static void finish_demosaic_rgb_ex(
        LibRaw *base,
        unsigned short *rgb16,
        unsigned width,
        unsigned height,
        int no_auto_bright
    ) {
        if (!base || !rgb16 || width == 0 || height == 0) {
            return;
        }
        LibRawColorShim *ip = reinterpret_cast<LibRawColorShim *>(base);

        ip->imgdata.params.use_camera_wb = 1;
        ip->imgdata.params.use_camera_matrix = 1;
        ip->imgdata.params.output_color = 1;
        ip->imgdata.params.gamm[0] = 1.0;
        ip->imgdata.params.gamm[1] = 1.0;
        ip->imgdata.params.no_auto_bright = no_auto_bright;
        ip->imgdata.params.highlight = 0;

        const unsigned npixels = width * height;
        if (npixels == 0) {
            return;
        }

        ip->imgdata.sizes.iwidth = static_cast<ushort>(width);
        ip->imgdata.sizes.iheight = static_cast<ushort>(height);
        ip->imgdata.sizes.width = static_cast<ushort>(width);
        ip->imgdata.sizes.height = static_cast<ushort>(height);
        ip->imgdata.idata.colors = 3;
        ip->imgdata.idata.filters = 0;

        int(*temp_histogram)[LIBRAW_HISTOGRAM_SIZE] = nullptr;
        if (!ip->libraw_internal_data.output_data.histogram) {
            temp_histogram = (int(*)[LIBRAW_HISTOGRAM_SIZE])ip->malloc(
                sizeof(*ip->libraw_internal_data.output_data.histogram) * 4);
            if (!temp_histogram) {
                return;
            }
            ip->libraw_internal_data.output_data.histogram = temp_histogram;
        }

        if (ip->imgdata.image) {
            ip->free(ip->imgdata.image);
            ip->imgdata.image = nullptr;
        }
        ip->imgdata.image = (ushort(*)[4])ip->calloc(npixels, sizeof(ushort[4]));
        if (!ip->imgdata.image) {
            if (temp_histogram) {
                ip->free(temp_histogram);
                ip->libraw_internal_data.output_data.histogram = nullptr;
            }
            return;
        }
        for (unsigned i = 0; i < npixels; i++) {
            ip->imgdata.image[i][0] = rgb16[i * 3 + 0];
            ip->imgdata.image[i][1] = rgb16[i * 3 + 1];
            ip->imgdata.image[i][2] = rgb16[i * 3 + 2];
            ip->imgdata.image[i][3] = 0;
        }

        // Input is already LibRaw scale_colors-equivalent CFA scaling + demosaic.
        ip->convert_to_rgb();
        if (!no_auto_bright) {
            apply_auto_bright_after_convert(ip);
        }

        for (unsigned i = 0; i < npixels; i++) {
            rgb16[i * 3 + 0] = ip->imgdata.image[i][0];
            rgb16[i * 3 + 1] = ip->imgdata.image[i][1];
            rgb16[i * 3 + 2] = ip->imgdata.image[i][2];
        }

        if (temp_histogram) {
            ip->free(temp_histogram);
            ip->libraw_internal_data.output_data.histogram = nullptr;
        }
    }

    static void finish_demosaic_rgb(
        LibRaw *base,
        unsigned short *rgb16,
        unsigned width,
        unsigned height
    ) {
        finish_demosaic_rgb_ex(base, rgb16, width, height, 1);
    }

    /// Run LibRaw scale_colors + pre_interpolate + PPG; export camera RGB counts.
    static int ppg_camera_rgb_counts(
        LibRaw *base,
        unsigned short *rgb16,
        unsigned *width_out,
        unsigned *height_out
    ) {
        if (!base || !rgb16) {
            return -1;
        }
        LibRawColorShim *ip = reinterpret_cast<LibRawColorShim *>(base);
        ip->imgdata.params.user_qual = 2;

        const int prep = raw2image_scale_colors(ip);
        if (prep != 0) {
            return prep;
        }
        ip->pre_interpolate();
        ip->ppg_interpolate();

        const unsigned w = ip->imgdata.sizes.width;
        const unsigned h = ip->imgdata.sizes.height;
        if (width_out) {
            *width_out = w;
        }
        if (height_out) {
            *height_out = h;
        }
        if (w == 0 || h == 0 || !ip->imgdata.image) {
            return -2;
        }
        const unsigned npixels = w * h;
        for (unsigned i = 0; i < npixels; i++) {
            rgb16[i * 3 + 0] = ip->imgdata.image[i][0];
            rgb16[i * 3 + 1] = ip->imgdata.image[i][1];
            rgb16[i * 3 + 2] = ip->imgdata.image[i][2];
        }
        return 0;
    }

    /// PPG camera RGB counts when raw2image + scale_colors already ran (e.g. after CFA extract).
    static int ppg_camera_rgb_counts_from_scaled(
        LibRaw *base,
        unsigned short *rgb16,
        unsigned *width_out,
        unsigned *height_out
    ) {
        if (!base || !rgb16) {
            return -1;
        }
        LibRawColorShim *ip = reinterpret_cast<LibRawColorShim *>(base);
        ip->imgdata.params.user_qual = 2;
        ip->pre_interpolate();
        ip->ppg_interpolate();

        const unsigned w = ip->imgdata.sizes.width;
        const unsigned h = ip->imgdata.sizes.height;
        if (width_out) {
            *width_out = w;
        }
        if (height_out) {
            *height_out = h;
        }
        if (w == 0 || h == 0 || !ip->imgdata.image) {
            return -2;
        }
        const unsigned npixels = w * h;
        for (unsigned i = 0; i < npixels; i++) {
            rgb16[i * 3 + 0] = ip->imgdata.image[i][0];
            rgb16[i * 3 + 1] = ip->imgdata.image[i][1];
            rgb16[i * 3 + 2] = ip->imgdata.image[i][2];
        }
        return 0;
    }

    /// LibRaw raw2image + scale_colors; export per-pixel CFA value (FC channel).
    static int extract_scaled_cfa(
        LibRaw *base,
        unsigned short *out,
        unsigned *width_out,
        unsigned *height_out
    ) {
        if (!base || !out) {
            return -1;
        }
        LibRawColorShim *ip = reinterpret_cast<LibRawColorShim *>(base);
        const int prep = raw2image_scale_colors(ip);
        if (prep != 0) {
            return prep;
        }
        const unsigned w = ip->imgdata.sizes.width;
        const unsigned h = ip->imgdata.sizes.height;
        if (width_out) {
            *width_out = w;
        }
        if (height_out) {
            *height_out = h;
        }
        if (w == 0 || h == 0 || !ip->imgdata.image) {
            return -2;
        }
        const unsigned npixels = w * h;
        for (unsigned i = 0; i < npixels; i++) {
            const unsigned row = i / w;
            const unsigned col = i % w;
            const int fc = ip->FC(row + ip->imgdata.sizes.top_margin, col + ip->imgdata.sizes.left_margin);
            out[i] = ip->imgdata.image[i][fc];
        }
        return 0;
    }

    static int ppg_pixel_channels(
        LibRaw *base,
        unsigned row,
        unsigned col,
        unsigned short out4[4]
    ) {
        if (!base || !out4) {
            return -1;
        }
        LibRawColorShim *ip = reinterpret_cast<LibRawColorShim *>(base);
        ip->imgdata.params.user_qual = 2;
        const int prep = raw2image_scale_colors(ip);
        if (prep != 0) {
            return prep;
        }
        ip->pre_interpolate();
        ip->ppg_interpolate();
        const unsigned w = ip->imgdata.sizes.width;
        const unsigned h = ip->imgdata.sizes.height;
        if (row >= h || col >= w || !ip->imgdata.image) {
            return -2;
        }
        const unsigned i = row * w + col;
        for (int c = 0; c < 4; c++) {
            out4[c] = ip->imgdata.image[i][c];
        }
        return 0;
    }

    static int ppg_convert_pixel_channels(
        LibRaw *base,
        unsigned row,
        unsigned col,
        unsigned short out3[3]
    ) {
        if (!base || !out3) {
            return -1;
        }
        LibRawColorShim *ip = reinterpret_cast<LibRawColorShim *>(base);
        ip->imgdata.params.user_qual = 2;
        ip->imgdata.params.gamm[0] = 1.0;
        ip->imgdata.params.gamm[1] = 1.0;
        const int prep = raw2image_scale_colors(ip);
        if (prep != 0) {
            return prep;
        }
        ip->pre_interpolate();
        ip->ppg_interpolate();
        ip->imgdata.idata.filters = 0;
        ip->convert_to_rgb();
        const unsigned w = ip->imgdata.sizes.width;
        const unsigned h = ip->imgdata.sizes.height;
        if (row >= h || col >= w || !ip->imgdata.image) {
            return -2;
        }
        const unsigned i = row * w + col;
        out3[0] = ip->imgdata.image[i][0];
        out3[1] = ip->imgdata.image[i][1];
        out3[2] = ip->imgdata.image[i][2];
        return 0;
    }

    static constexpr unsigned GPU_SCENE_COLOR_CALIB_MAX_SIDE = 512;
    static constexpr unsigned GPU_SCENE_COLOR_CALIB_PATCH = 64;

    static float clip_channel_f(float v) {
        const int iv = static_cast<int>(v);
        if (iv < 0) {
            return 0.0f;
        }
        if (iv > 65535) {
            return 65535.0f;
        }
        return static_cast<float>(iv);
    }

    static void patch_mean_matrix_rgb(
        const unsigned short *counts,
        unsigned width,
        unsigned height,
        const float rgb_cam[12],
        double out_mean[3]
    ) {
        out_mean[0] = out_mean[1] = out_mean[2] = 0.0;
        if (width == 0 || height == 0) {
            return;
        }
        const unsigned cx = width / 2;
        const unsigned cy = height / 2;
        const unsigned patch_half = GPU_SCENE_COLOR_CALIB_PATCH / 2;
        double n = 0.0;
        for (unsigned dy = 0; dy < GPU_SCENE_COLOR_CALIB_PATCH; dy++) {
            for (unsigned dx = 0; dx < GPU_SCENE_COLOR_CALIB_PATCH; dx++) {
                const int x = static_cast<int>(cx + dx) - static_cast<int>(patch_half);
                const int y = static_cast<int>(cy + dy) - static_cast<int>(patch_half);
                if (x < 0 || y < 0 || static_cast<unsigned>(x) >= width
                    || static_cast<unsigned>(y) >= height) {
                    continue;
                }
                const unsigned i =
                    (static_cast<unsigned>(y) * width + static_cast<unsigned>(x)) * 3;
                const float r = static_cast<float>(counts[i + 0]);
                const float g = static_cast<float>(counts[i + 1]);
                const float b = static_cast<float>(counts[i + 2]);
                const float rv = clip_channel_f(
                    rgb_cam[0] * r + rgb_cam[1] * g + rgb_cam[2] * b);
                const float gv = clip_channel_f(
                    rgb_cam[4] * r + rgb_cam[5] * g + rgb_cam[6] * b);
                const float bv = clip_channel_f(
                    rgb_cam[8] * r + rgb_cam[9] * g + rgb_cam[10] * b);
                out_mean[0] += static_cast<double>(rv) / 65535.0;
                out_mean[1] += static_cast<double>(gv) / 65535.0;
                out_mean[2] += static_cast<double>(bv) / 65535.0;
                n += 1.0;
            }
        }
        if (n > 0.0) {
            out_mean[0] /= n;
            out_mean[1] /= n;
            out_mean[2] /= n;
        }
    }

    static void patch_mean_rgb16_norm(
        const unsigned short *rgb16,
        unsigned width,
        unsigned height,
        double out_mean[3]
    ) {
        out_mean[0] = out_mean[1] = out_mean[2] = 0.0;
        if (width == 0 || height == 0) {
            return;
        }
        const unsigned cx = width / 2;
        const unsigned cy = height / 2;
        const unsigned patch_half = GPU_SCENE_COLOR_CALIB_PATCH / 2;
        double n = 0.0;
        for (unsigned dy = 0; dy < GPU_SCENE_COLOR_CALIB_PATCH; dy++) {
            for (unsigned dx = 0; dx < GPU_SCENE_COLOR_CALIB_PATCH; dx++) {
                const int x = static_cast<int>(cx + dx) - static_cast<int>(patch_half);
                const int y = static_cast<int>(cy + dy) - static_cast<int>(patch_half);
                if (x < 0 || y < 0 || static_cast<unsigned>(x) >= width
                    || static_cast<unsigned>(y) >= height) {
                    continue;
                }
                const unsigned i =
                    (static_cast<unsigned>(y) * width + static_cast<unsigned>(x)) * 3;
                out_mean[0] += static_cast<double>(rgb16[i + 0]) / 65535.0;
                out_mean[1] += static_cast<double>(rgb16[i + 1]) / 65535.0;
                out_mean[2] += static_cast<double>(rgb16[i + 2]) / 65535.0;
                n += 1.0;
            }
        }
        if (n > 0.0) {
            out_mean[0] /= n;
            out_mean[1] /= n;
            out_mean[2] /= n;
        }
    }

    static void restore_full_cfa_state(
        LibRawColorShim *ip,
        ushort(*saved_image)[4],
        unsigned saved_w,
        unsigned saved_h,
        unsigned saved_iw,
        unsigned saved_ih,
        unsigned saved_top_margin,
        unsigned saved_left_margin,
        int saved_colors,
        unsigned saved_filters,
        ushort(*decim)[4]
    ) {
        if (decim && ip->imgdata.image == decim) {
            ip->free(decim);
            ip->imgdata.image = nullptr;
        }
        ip->imgdata.image = saved_image;
        ip->imgdata.sizes.width = static_cast<ushort>(saved_w);
        ip->imgdata.sizes.height = static_cast<ushort>(saved_h);
        ip->imgdata.sizes.iwidth = static_cast<ushort>(saved_iw);
        ip->imgdata.sizes.iheight = static_cast<ushort>(saved_ih);
        ip->imgdata.sizes.top_margin = static_cast<ushort>(saved_top_margin);
        ip->imgdata.sizes.left_margin = static_cast<ushort>(saved_left_margin);
        ip->imgdata.idata.colors = saved_colors;
        ip->imgdata.idata.filters = saved_filters;
    }

    /// Center-aligned decimated CFA + PPG (LibRaw FC() needs margin fix after pre_interpolate).
    static int run_center_decimated_ppg(
        LibRawColorShim *ip,
        ushort(*const saved_image)[4],
        unsigned saved_w,
        unsigned saved_h,
        unsigned saved_top_margin,
        unsigned saved_left_margin,
        unsigned &dw,
        unsigned &dh,
        ushort(**decim_out)[4]
    ) {
        unsigned step = 1;
        while (std::max(saved_w / step, saved_h / step) > GPU_SCENE_COLOR_CALIB_MAX_SIDE) {
            step *= 2;
        }
        dw = saved_w / step;
        dh = saved_h / step;
        dw &= ~1u;
        dh &= ~1u;
        if (dw < GPU_SCENE_COLOR_CALIB_PATCH + 8
            || dh < GPU_SCENE_COLOR_CALIB_PATCH + 8) {
            return -4;
        }

        unsigned x0 = 0;
        unsigned y0 = 0;
        if (step > 1 || dw < saved_w) {
            x0 = ((saved_w - (dw - 1) * step) / 2) & ~1u;
            y0 = ((saved_h - (dh - 1) * step) / 2) & ~1u;
        }

        const unsigned decim_pixels = dw * dh;
        ushort(*decim)[4] =
            (ushort(*)[4])ip->calloc(decim_pixels, sizeof(ushort[4]));
        if (!decim) {
            return -5;
        }

        for (unsigned y = 0; y < dh; y++) {
            const unsigned sy = y0 + y * step;
            for (unsigned x = 0; x < dw; x++) {
                const unsigned sx = x0 + x * step;
                const unsigned si = sy * saved_w + sx;
                const unsigned di = y * dw + x;
                decim[di][0] = saved_image[si][0];
                decim[di][1] = saved_image[si][1];
                decim[di][2] = saved_image[si][2];
                decim[di][3] = saved_image[si][3];
            }
        }

        ip->imgdata.image = decim;
        ip->imgdata.sizes.width = static_cast<ushort>(dw);
        ip->imgdata.sizes.height = static_cast<ushort>(dh);
        ip->imgdata.sizes.iwidth = static_cast<ushort>(dw);
        ip->imgdata.sizes.iheight = static_cast<ushort>(dh);
        ip->imgdata.sizes.top_margin = static_cast<ushort>(saved_top_margin + y0);
        ip->imgdata.sizes.left_margin = static_cast<ushort>(saved_left_margin + x0);
        ip->imgdata.params.user_qual = 2;

        ip->pre_interpolate();
        // pre_interpolate() may reset margins; LibRaw FC() needs absolute CFA coordinates.
        ip->imgdata.sizes.top_margin = static_cast<ushort>(saved_top_margin + y0);
        ip->imgdata.sizes.left_margin = static_cast<ushort>(saved_left_margin + x0);
        ip->ppg_interpolate();

        if (!ip->imgdata.image) {
            ip->free(decim);
            return -6;
        }

        *decim_out = decim;
        return 0;
    }

    /// Decimated PPG + rgb_cam matrix; center 64x64 patch mean (no convert_to_rgb).
    static int decimated_ppg_matrix_patch_mean(
        LibRaw *base,
        const float rgb_cam[12],
        float mean_out[3]
    ) {
        if (!base || !rgb_cam || !mean_out) {
            return -1;
        }
        LibRawColorShim *ip = reinterpret_cast<LibRawColorShim *>(base);
        if (!ip->imgdata.image) {
            return -2;
        }

        ushort(*const saved_image)[4] = ip->imgdata.image;
        const unsigned saved_w = ip->imgdata.sizes.width;
        const unsigned saved_h = ip->imgdata.sizes.height;
        const unsigned saved_iw = ip->imgdata.sizes.iwidth;
        const unsigned saved_ih = ip->imgdata.sizes.iheight;
        const int saved_colors = ip->imgdata.idata.colors;
        const unsigned saved_filters = ip->imgdata.idata.filters;
        const unsigned saved_top_margin = ip->imgdata.sizes.top_margin;
        const unsigned saved_left_margin = ip->imgdata.sizes.left_margin;

        if (saved_w < 8 || saved_h < 8) {
            return -3;
        }

        unsigned dw = 0;
        unsigned dh = 0;
        ushort(*decim)[4] = nullptr;
        const int ppg_status = run_center_decimated_ppg(
            ip,
            saved_image,
            saved_w,
            saved_h,
            saved_top_margin,
            saved_left_margin,
            dw,
            dh,
            &decim
        );
        if (ppg_status != 0) {
            restore_full_cfa_state(
                ip,
                saved_image,
                saved_w,
                saved_h,
                saved_iw,
                saved_ih,
                saved_top_margin,
                saved_left_margin,
                saved_colors,
                saved_filters,
                decim
            );
            return ppg_status;
        }

        const unsigned np = dw * dh;
        std::vector<unsigned short> rgb16(np * 3);
        for (unsigned i = 0; i < np; i++) {
            rgb16[i * 3 + 0] = ip->imgdata.image[i][0];
            rgb16[i * 3 + 1] = ip->imgdata.image[i][1];
            rgb16[i * 3 + 2] = ip->imgdata.image[i][2];
        }

        restore_full_cfa_state(
            ip,
            saved_image,
            saved_w,
            saved_h,
            saved_iw,
            saved_ih,
            saved_top_margin,
            saved_left_margin,
            saved_colors,
            saved_filters,
            decim
        );

        double matrix_mean[3];
        patch_mean_matrix_rgb(rgb16.data(), dw, dh, rgb_cam, matrix_mean);

        mean_out[0] = static_cast<float>(matrix_mean[0]);
        mean_out[1] = static_cast<float>(matrix_mean[1]);
        mean_out[2] = static_cast<float>(matrix_mean[2]);
        return 0;
    }

    /// Decimated (max 512px) PPG + rgb_cam matrix vs LibRaw convert_to_rgb/auto_bright scale.
    static int decimated_ppg_scene_color_scale(
        LibRaw *base,
        const float rgb_cam[12],
        float scale_out[3]
    ) {
        if (!base || !rgb_cam || !scale_out) {
            return -1;
        }
        LibRawColorShim *ip = reinterpret_cast<LibRawColorShim *>(base);
        if (!ip->imgdata.image) {
            return -2;
        }

        ushort(*const saved_image)[4] = ip->imgdata.image;
        const unsigned saved_w = ip->imgdata.sizes.width;
        const unsigned saved_h = ip->imgdata.sizes.height;
        const unsigned saved_iw = ip->imgdata.sizes.iwidth;
        const unsigned saved_ih = ip->imgdata.sizes.iheight;
        const int saved_colors = ip->imgdata.idata.colors;
        const unsigned saved_filters = ip->imgdata.idata.filters;
        const unsigned saved_top_margin = ip->imgdata.sizes.top_margin;
        const unsigned saved_left_margin = ip->imgdata.sizes.left_margin;

        if (saved_w < 8 || saved_h < 8) {
            return -3;
        }

        unsigned dw = 0;
        unsigned dh = 0;
        ushort(*decim)[4] = nullptr;
        const int ppg_status = run_center_decimated_ppg(
            ip,
            saved_image,
            saved_w,
            saved_h,
            saved_top_margin,
            saved_left_margin,
            dw,
            dh,
            &decim
        );
        if (ppg_status != 0) {
            restore_full_cfa_state(
                ip,
                saved_image,
                saved_w,
                saved_h,
                saved_iw,
                saved_ih,
                saved_top_margin,
                saved_left_margin,
                saved_colors,
                saved_filters,
                decim
            );
            return ppg_status;
        }

        const unsigned np = dw * dh;
        std::vector<unsigned short> rgb16(np * 3);
        for (unsigned i = 0; i < np; i++) {
            rgb16[i * 3 + 0] = ip->imgdata.image[i][0];
            rgb16[i * 3 + 1] = ip->imgdata.image[i][1];
            rgb16[i * 3 + 2] = ip->imgdata.image[i][2];
        }

        restore_full_cfa_state(
            ip,
            saved_image,
            saved_w,
            saved_h,
            saved_iw,
            saved_ih,
            saved_top_margin,
            saved_left_margin,
            saved_colors,
            saved_filters,
            decim
        );

        double matrix_mean[3];
        patch_mean_matrix_rgb(rgb16.data(), dw, dh, rgb_cam, matrix_mean);

        ip->imgdata.image = nullptr;
        finish_demosaic_rgb(ip, rgb16.data(), dw, dh);
        restore_full_cfa_state(
            ip,
            saved_image,
            saved_w,
            saved_h,
            saved_iw,
            saved_ih,
            saved_top_margin,
            saved_left_margin,
            saved_colors,
            saved_filters,
            nullptr
        );

        double libraw_mean[3];
        patch_mean_rgb16_norm(rgb16.data(), dw, dh, libraw_mean);

        for (int c = 0; c < 3; c++) {
            const double denom = matrix_mean[c] > 1e-9 ? matrix_mean[c] : 1.0;
            scale_out[c] = static_cast<float>(libraw_mean[c] / denom);
            if (!std::isfinite(scale_out[c]) || scale_out[c] <= 0.0f) {
                scale_out[c] = 1.0f;
            } else if (scale_out[c] < 0.25f) {
                scale_out[c] = 0.25f;
            } else if (scale_out[c] > 4.0f) {
                scale_out[c] = 4.0f;
            }
        }
        return 0;
    }

    /// Decimated center PPG + finish_demosaic_rgb; center 64x64 channel-mean sums.
    static int decimated_ppg_scene_center_luma_pair(
        LibRaw *base,
        const float rgb_cam[12],
        double *ab_luma_out,
        double *matrix_luma_out
    ) {
        if (!base || !rgb_cam || !ab_luma_out || !matrix_luma_out) {
            return -1;
        }
        LibRawColorShim *ip = reinterpret_cast<LibRawColorShim *>(base);
        if (!ip->imgdata.image) {
            return -2;
        }

        ushort(*const saved_image)[4] = ip->imgdata.image;
        const unsigned saved_w = ip->imgdata.sizes.width;
        const unsigned saved_h = ip->imgdata.sizes.height;
        const unsigned saved_iw = ip->imgdata.sizes.iwidth;
        const unsigned saved_ih = ip->imgdata.sizes.iheight;
        const int saved_colors = ip->imgdata.idata.colors;
        const unsigned saved_filters = ip->imgdata.idata.filters;
        const unsigned saved_top_margin = ip->imgdata.sizes.top_margin;
        const unsigned saved_left_margin = ip->imgdata.sizes.left_margin;

        if (saved_w < 8 || saved_h < 8) {
            return -3;
        }

        unsigned dw = 0;
        unsigned dh = 0;
        ushort(*decim)[4] = nullptr;
        const int ppg_status = run_center_decimated_ppg(
            ip,
            saved_image,
            saved_w,
            saved_h,
            saved_top_margin,
            saved_left_margin,
            dw,
            dh,
            &decim
        );
        if (ppg_status != 0) {
            restore_full_cfa_state(
                ip,
                saved_image,
                saved_w,
                saved_h,
                saved_iw,
                saved_ih,
                saved_top_margin,
                saved_left_margin,
                saved_colors,
                saved_filters,
                decim
            );
            return ppg_status;
        }

        const unsigned np = dw * dh;
        std::vector<unsigned short> rgb16(np * 3);
        for (unsigned i = 0; i < np; i++) {
            rgb16[i * 3 + 0] = ip->imgdata.image[i][0];
            rgb16[i * 3 + 1] = ip->imgdata.image[i][1];
            rgb16[i * 3 + 2] = ip->imgdata.image[i][2];
        }

        restore_full_cfa_state(
            ip,
            saved_image,
            saved_w,
            saved_h,
            saved_iw,
            saved_ih,
            saved_top_margin,
            saved_left_margin,
            saved_colors,
            saved_filters,
            decim
        );

        double matrix_mean[3];
        patch_mean_matrix_rgb(rgb16.data(), dw, dh, rgb_cam, matrix_mean);
        *matrix_luma_out = matrix_mean[0] + matrix_mean[1] + matrix_mean[2];

        ip->imgdata.image = nullptr;
        finish_demosaic_rgb(ip, rgb16.data(), dw, dh);
        restore_full_cfa_state(
            ip,
            saved_image,
            saved_w,
            saved_h,
            saved_iw,
            saved_ih,
            saved_top_margin,
            saved_left_margin,
            saved_colors,
            saved_filters,
            nullptr
        );

        double libraw_mean[3];
        patch_mean_rgb16_norm(rgb16.data(), dw, dh, libraw_mean);
        *ab_luma_out = libraw_mean[0] + libraw_mean[1] + libraw_mean[2];
        return 0;
    }

    /// Uniform luma ratio (LibRaw auto_bright / matrix rgb_cam) on decimated center PPG.
    static int decimated_ppg_uniform_scene_scale(
        LibRaw *base,
        const float rgb_cam[12],
        float *uniform_out
    ) {
        if (!base || !rgb_cam || !uniform_out) {
            return -1;
        }
        LibRaw *ip = base;
        double ab_luma = 0.0;
        double no_ab_luma = 0.0;
        const int status =
            decimated_ppg_scene_center_luma_pair(ip, rgb_cam, &ab_luma, &no_ab_luma);
        if (status != 0) {
            return status;
        }
        const double denom = no_ab_luma > 1e-9 ? no_ab_luma : 1.0;
        *uniform_out = static_cast<float>(ab_luma / denom);
        if (!std::isfinite(*uniform_out) || *uniform_out <= 0.0f) {
            *uniform_out = 1.0f;
        } else if (*uniform_out < 0.25f) {
            *uniform_out = 0.25f;
        } else if (*uniform_out > 4.0f) {
            *uniform_out = 4.0f;
        }
        return 0;
    }

    static int decimated_ppg_scene_ab_luma_sum(
        LibRaw *base,
        const float rgb_cam[12],
        double *ab_luma_out
    ) {
        if (!base || !rgb_cam || !ab_luma_out) {
            return -1;
        }
        double matrix_luma = 0.0;
        return decimated_ppg_scene_center_luma_pair(
            base, rgb_cam, ab_luma_out, &matrix_luma);
    }
};

// Custom C API shims for Simple Image Viewer
// These are kept in libraries/libraw-sys-msvc to keep 3rdparty/LibRaw clean.
// Using 'siv_' prefix to avoid any symbol collisions with LibRaw's own C API.

extern "C" {
    void siv_libraw_set_use_camera_wb(libraw_data_t *lr, int value) {
        if (!lr) return;
        LibRaw *ip = (LibRaw *)lr->parent_class;
        ip->imgdata.params.use_camera_wb = value;
    }

    unsigned int siv_libraw_get_process_warnings(libraw_data_t *lr) {
        if (!lr) return 0;
        return lr->process_warnings;
    }

    int siv_libraw_get_flip(libraw_data_t *lr) {
        if (!lr) return 0;
        return lr->sizes.flip;
    }

    void siv_libraw_set_user_flip(libraw_data_t *lr, int flip) {
        if (!lr) return;
        LibRaw *ip = (LibRaw *)lr->parent_class;
        ip->imgdata.params.user_flip = flip;
    }

    void siv_libraw_set_use_camera_matrix(libraw_data_t *lr, int value) {
        if (!lr) return;
        LibRaw *ip = (LibRaw *)lr->parent_class;
        ip->imgdata.params.use_camera_matrix = value;
    }

    void siv_libraw_set_auto_bright_thr(libraw_data_t *lr, float value) {
        if (!lr) return;
        LibRaw *ip = (LibRaw *)lr->parent_class;
        ip->imgdata.params.auto_bright_thr = value;
    }

    void siv_libraw_set_output_color(libraw_data_t *lr, int value) {
        if (!lr) return;
        LibRaw *ip = (LibRaw *)lr->parent_class;
        ip->imgdata.params.output_color = value;
    }

    void siv_libraw_set_gamma(libraw_data_t *lr, double power, double slope) {
        if (!lr) return;
        LibRaw *ip = (LibRaw *)lr->parent_class;
        ip->imgdata.params.gamm[0] = power;
        ip->imgdata.params.gamm[1] = slope;
    }

    void siv_libraw_set_user_qual(libraw_data_t *lr, int qual) {
        if (!lr) return;
        LibRaw *ip = (LibRaw *)lr->parent_class;
        ip->imgdata.params.user_qual = qual;
    }

    void siv_libraw_set_highlight(libraw_data_t *lr, int value) {
        if (!lr) return;
        LibRaw *ip = (LibRaw *)lr->parent_class;
        ip->imgdata.params.highlight = value;
    }

    void siv_libraw_set_half_size(libraw_data_t *lr, int value) {
        if (!lr) return;
        LibRaw *ip = (LibRaw *)lr->parent_class;
        ip->imgdata.params.half_size = value;
    }

    float siv_libraw_get_bright(libraw_data_t *lr) {
        if (!lr) return 8192.0f;
        LibRaw *ip = (LibRaw *)lr->parent_class;
        float bright = ip->imgdata.params.bright;
        // LibRaw leaves `bright` at 1.0 until dcraw_process; 8192 is the 16-bit default.
        if (bright <= 1.0f) {
            bright = 8192.0f;
        }
        return bright;
    }

    unsigned short* siv_libraw_get_raw_image(libraw_data_t *lr) {
        if (!lr) return nullptr;
        return lr->rawdata.raw_image;
    }

    int siv_libraw_get_color_at(libraw_data_t *lr, int row, int col) {
        if (!lr) return -1;
        LibRaw *ip = (LibRaw *)lr->parent_class;
        return ip->COLOR(row, col);
    }

    void siv_libraw_get_color_params(libraw_data_t *lr, float *cam_mul, float *cblack, int *black, int *maximum) {
        if (!lr) return;
        for (int i = 0; i < 4; i++) {
            cam_mul[i] = lr->color.cam_mul[i];
            cblack[i] = lr->color.cblack[i];
        }
        *black = lr->color.black;
        *maximum = lr->color.maximum;
    }

    /// Diagnostic: full color metadata for CPU path analysis (cblack layout, pre_mul, raw sample).
    void siv_libraw_get_color_diag(
        libraw_data_t *lr,
        int *black,
        int *maximum,
        int *data_maximum,
        unsigned *cblack0_3,
        unsigned *cblack4,
        unsigned *cblack5,
        float *pre_mul,
        float *cam_mul
    ) {
        if (!lr) return;
        *black = lr->color.black;
        *maximum = lr->color.maximum;
        *data_maximum = lr->color.data_maximum;
        for (int i = 0; i < 4; i++) {
            cblack0_3[i] = lr->color.cblack[i];
            pre_mul[i] = lr->color.pre_mul[i];
            cam_mul[i] = lr->color.cam_mul[i];
        }
        *cblack4 = lr->color.cblack[4];
        *cblack5 = lr->color.cblack[5];
    }

    unsigned short siv_libraw_raw_pixel_at(
        libraw_data_t *lr,
        unsigned row,
        unsigned col
    ) {
        if (!lr || !lr->rawdata.raw_image) return 0;
        const unsigned pitch = lr->sizes.raw_pitch / 2;
        return lr->rawdata.raw_image[row * pitch + col];
    }

    void siv_libraw_get_margins(libraw_data_t *lr, int *left_margin, int *top_margin) {
        if (!lr) return;
        *left_margin = lr->sizes.left_margin;
        *top_margin = lr->sizes.top_margin;
    }

    unsigned int siv_libraw_get_filters(libraw_data_t *lr) {
        if (!lr) return 0;
        return lr->idata.filters;
    }

    int siv_libraw_get_colors(libraw_data_t *lr) {
        if (!lr) return 0;
        return lr->idata.colors;
    }

    /// Nonzero when LibRaw will apply Fuji Super-CCD 45-degree rotation during develop.
    int siv_libraw_is_fuji_rotated(libraw_data_t *lr) {
        if (!lr) return 0;
        LibRaw *ip = (LibRaw *)lr->parent_class;
        if (!ip) return 0;
        return ip->is_fuji_rotated();
    }

    double siv_libraw_get_pixel_aspect(libraw_data_t *lr) {
        if (!lr) return 1.0;
        return lr->sizes.pixel_aspect;
    }

    // Match LibRaw convert_to_rgb + scale_colors for GPU demosaic color processing.
    void siv_libraw_get_gpu_color_params(
        libraw_data_t *lr,
        float *rgb_cam_out,
        float *cblack_rgb,
        float *channel_scale
    ) {
        if (!lr || !rgb_cam_out || !cblack_rgb || !channel_scale) return;

        LibRaw *ip = (LibRaw *)lr->parent_class;
        libraw_output_params_t &output_params = ip->imgdata.params;



        output_params.use_camera_wb = 1;
        output_params.use_camera_matrix = 1;
        output_params.output_color = 1;

        float pre_mul[4];
        std::memcpy(pre_mul, lr->color.pre_mul, sizeof(pre_mul));
        if (pre_mul[0] < 0.00001f) {
            // Fallback to cam_mul normalized so green is 1.0
            std::memcpy(pre_mul, lr->color.cam_mul, sizeof(pre_mul));
            float norm = pre_mul[1] > 0.00001f ? pre_mul[1] : 1.0f;
            for (int c = 0; c < 4; c++) {
                pre_mul[c] /= norm;
            }
        }
        if (pre_mul[0] < 0.00001f) {
            pre_mul[0] = pre_mul[1] = pre_mul[2] = pre_mul[3] = 1.0f;
        }

        float cam_matrix[3][4];
        std::memcpy(cam_matrix, lr->color.rgb_cam, sizeof(cam_matrix));

        float output_matrix[3][4];
        std::memcpy(output_matrix, cam_matrix, sizeof(output_matrix));

        for (int i = 0; i < 3; i++) {
            for (int j = 0; j < 4; j++) {
                rgb_cam_out[i * 4 + j] = output_matrix[i][j];
            }
        }


        int maximum = lr->color.maximum;
        const int black_level = lr->color.black;
        maximum -= black_level;

        float dmin = std::numeric_limits<float>::max();
        float dmax = 0.0f;
        for (int c = 0; c < 4; c++) {
            dmin = std::min(dmin, pre_mul[c]);
            dmax = std::max(dmax, pre_mul[c]);
        }
        if (!output_params.highlight) {
            dmax = dmin;
        }

        float scale_mul[4];
        if (dmax > 0.00001f && maximum > 0) {
            for (int c = 0; c < 4; c++) {
                scale_mul[c] = (pre_mul[c] / dmax) * 65535.0f / static_cast<float>(maximum);
            }
            // Debug prints removed
        } else {
            for (int c = 0; c < 4; c++) {
                scale_mul[c] = 1.0f;
            }
        }

        // Per-CFA black + scale_mul for pre-demosaic scale_colors (LibRaw order).
        for (int c = 0; c < 4; c++) {
            float blk = static_cast<float>(lr->color.black + lr->color.cblack[c]);
            cblack_rgb[c] = blk;
            channel_scale[c] = scale_mul[c];
        }
    }

    // Apply LibRaw scale_colors + convert_to_rgb to a demosaiced RGB buffer (camera counts).
    void siv_libraw_apply_output_color(
        libraw_data_t *lr,
        unsigned short *rgb16,
        unsigned width,
        unsigned height
    ) {
        if (!lr || !rgb16 || width == 0 || height == 0) return;
        LibRaw *ip = (LibRaw *)lr->parent_class;
        LibRawColorShim::finish_demosaic_rgb(ip, rgb16, width, height);
    }

    int siv_libraw_ppg_camera_rgb_counts(
        libraw_data_t *lr,
        unsigned short *rgb16_out,
        unsigned *width_out,
        unsigned *height_out
    ) {
        if (!lr || !rgb16_out) return -1;
        LibRaw *ip = (LibRaw *)lr->parent_class;
        return LibRawColorShim::ppg_camera_rgb_counts(ip, rgb16_out, width_out, height_out);
    }

    int siv_libraw_ppg_camera_rgb_counts_from_scaled(
        libraw_data_t *lr,
        unsigned short *rgb16_out,
        unsigned *width_out,
        unsigned *height_out
    ) {
        if (!lr || !rgb16_out) return -1;
        LibRaw *ip = (LibRaw *)lr->parent_class;
        return LibRawColorShim::ppg_camera_rgb_counts_from_scaled(
            ip, rgb16_out, width_out, height_out);
    }

    int siv_libraw_extract_scaled_cfa(
        libraw_data_t *lr,
        unsigned short *out,
        unsigned *width_out,
        unsigned *height_out
    ) {
        if (!lr || !out) return -1;
        LibRaw *ip = (LibRaw *)lr->parent_class;
        return LibRawColorShim::extract_scaled_cfa(ip, out, width_out, height_out);
    }

    int siv_libraw_decimated_ppg_matrix_patch_mean(
        libraw_data_t *lr,
        const float *rgb_cam,
        float *mean_out
    ) {
        if (!lr || !rgb_cam || !mean_out) return -1;
        LibRaw *ip = (LibRaw *)lr->parent_class;
        return LibRawColorShim::decimated_ppg_matrix_patch_mean(ip, rgb_cam, mean_out);
    }

    int siv_libraw_decimated_ppg_scene_color_scale(
        libraw_data_t *lr,
        const float *rgb_cam,
        float *scale_out
    ) {
        if (!lr || !rgb_cam || !scale_out) return -1;
        LibRaw *ip = (LibRaw *)lr->parent_class;
        return LibRawColorShim::decimated_ppg_scene_color_scale(ip, rgb_cam, scale_out);
    }

    int siv_libraw_decimated_ppg_uniform_scene_scale(
        libraw_data_t *lr,
        const float *rgb_cam,
        float *uniform_out
    ) {
        if (!lr || !rgb_cam || !uniform_out) return -1;
        LibRaw *ip = (LibRaw *)lr->parent_class;
        return LibRawColorShim::decimated_ppg_uniform_scene_scale(ip, rgb_cam, uniform_out);
    }

    int siv_libraw_decimated_ppg_scene_ab_luma_sum(
        libraw_data_t *lr,
        const float *rgb_cam,
        double *ab_luma_out
    ) {
        if (!lr || !rgb_cam || !ab_luma_out) return -1;
        LibRaw *ip = (LibRaw *)lr->parent_class;
        return LibRawColorShim::decimated_ppg_scene_ab_luma_sum(ip, rgb_cam, ab_luma_out);
    }

    int siv_libraw_ppg_pixel_channels(
        libraw_data_t *lr,
        unsigned row,
        unsigned col,
        unsigned short *out4
    ) {
        if (!lr || !out4) return -1;
        LibRaw *ip = (LibRaw *)lr->parent_class;
        return LibRawColorShim::ppg_pixel_channels(ip, row, col, out4);
    }

    int siv_libraw_ppg_convert_pixel(
        libraw_data_t *lr,
        unsigned row,
        unsigned col,
        unsigned short *out3
    ) {
        if (!lr || !out3) return -1;
        LibRaw *ip = (LibRaw *)lr->parent_class;
        return LibRawColorShim::ppg_convert_pixel_channels(ip, row, col, out3);
    }

}

