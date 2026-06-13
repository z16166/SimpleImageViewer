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

// Access protected LibRaw color finish for GPU demosaic post-processing.
class LibRawColorShim : public LibRaw {
public:
    static void finish_demosaic_rgb(
        LibRaw *base,
        unsigned short *rgb16,
        unsigned width,
        unsigned height
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
        ip->imgdata.params.no_auto_bright = 0;
        ip->imgdata.params.auto_bright_thr = 0.01f;
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

        if (ip->imgdata.image) {
            ip->free(ip->imgdata.image);
            ip->imgdata.image = nullptr;
        }
        ip->imgdata.image = (ushort(*)[4])ip->calloc(npixels, sizeof(ushort[4]));
        if (!ip->imgdata.image) {
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

        for (unsigned i = 0; i < npixels; i++) {
            rgb16[i * 3 + 0] = ip->imgdata.image[i][0];
            rgb16[i * 3 + 1] = ip->imgdata.image[i][1];
            rgb16[i * 3 + 2] = ip->imgdata.image[i][2];
        }
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
        ip->imgdata.params.use_camera_wb = 1;
        ip->imgdata.params.use_camera_matrix = 1;
        ip->imgdata.params.user_qual = 2;

        ip->raw2image();
        ip->scale_colors();
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
        ip->imgdata.params.use_camera_wb = 1;
        ip->imgdata.params.use_camera_matrix = 1;
        ip->imgdata.params.output_color = 1;

        ip->raw2image();
        printf("C++ shims: extract_scaled_cfa BEFORE scale_colors: image[0]=[%d, %d, %d, %d], black=%d\n",
            ip->imgdata.image[0][0], ip->imgdata.image[0][1], ip->imgdata.image[0][2], ip->imgdata.image[0][3],
            ip->imgdata.color.black);
        ip->scale_colors();
        printf("C++ shims: extract_scaled_cfa AFTER scale_colors: image[0]=[%d, %d, %d, %d]\n",
            ip->imgdata.image[0][0], ip->imgdata.image[0][1], ip->imgdata.image[0][2], ip->imgdata.image[0][3]);




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
        ip->imgdata.params.use_camera_wb = 1;
        ip->imgdata.params.use_camera_matrix = 1;
        ip->imgdata.params.user_qual = 2;
        ip->raw2image();
        ip->scale_colors();
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
        ip->imgdata.params.use_camera_wb = 1;
        ip->imgdata.params.use_camera_matrix = 1;
        ip->imgdata.params.output_color = 1;
        ip->imgdata.params.user_qual = 2;
        ip->imgdata.params.gamm[0] = 1.0;
        ip->imgdata.params.gamm[1] = 1.0;
        ip->raw2image();
        ip->scale_colors();
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

