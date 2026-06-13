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

        ip->imgdata.sizes.iwidth = static_cast<ushort>(width);
        ip->imgdata.sizes.iheight = static_cast<ushort>(height);
        ip->imgdata.sizes.width = static_cast<ushort>(width);
        ip->imgdata.sizes.height = static_cast<ushort>(height);
        ip->imgdata.idata.colors = 3;

        const unsigned npixels = width * height;
        if (ip->imgdata.image) {
            ip->free(ip->imgdata.image);
            ip->imgdata.image = nullptr;
        }
        ip->imgdata.image = (ushort(*)[4])ip->malloc(
            static_cast<size_t>(npixels) * sizeof(ushort[4]));
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
        ip->imgdata.idata.filters = 0;
        ip->convert_to_rgb();

        for (unsigned i = 0; i < npixels; i++) {
            rgb16[i * 3 + 0] = ip->imgdata.image[i][0];
            rgb16[i * 3 + 1] = ip->imgdata.image[i][1];
            rgb16[i * 3 + 2] = ip->imgdata.image[i][2];
        }
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

        float cam_matrix[3][4];
        std::memcpy(cam_matrix, lr->color.rgb_cam, sizeof(cam_matrix));

        float output_matrix[3][4];
        std::memcpy(output_matrix, cam_matrix, sizeof(output_matrix));

        for (int i = 0; i < 3; i++) {
            for (int j = 0; j < 4; j++) {
                rgb_cam_out[i * 4 + j] = output_matrix[i][j];
            }
        }

        float pre_mul[4];
        std::memcpy(pre_mul, lr->color.cam_mul, sizeof(pre_mul));
        if (output_params.use_camera_wb && lr->color.cam_mul[0] > 0.00001f
            && lr->color.cam_mul[2] > 0.00001f) {
            std::memcpy(pre_mul, lr->color.cam_mul, sizeof(pre_mul));
        }
        if (pre_mul[1] == 0.0f) {
            pre_mul[1] = 1.0f;
        }
        if (pre_mul[3] == 0.0f) {
            pre_mul[3] = lr->idata.colors < 4 ? pre_mul[1] : 1.0f;
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
        } else {
            for (int c = 0; c < 4; c++) {
                scale_mul[c] = 1.0f;
            }
        }

        // Per-CFA black + scale_mul for pre-demosaic scale_colors (LibRaw order).
        float unified_black = static_cast<float>(lr->color.cblack[0]);
        if (unified_black <= 0.0f) {
            unified_black = static_cast<float>(lr->color.black);
        }
        for (int c = 0; c < 4; c++) {
            float blk = static_cast<float>(lr->color.cblack[c]);
            if (blk <= 0.0f) {
                blk = static_cast<float>(lr->color.black);
            }
            cblack_rgb[c] = blk;
            channel_scale[c] = scale_mul[c];
        }
        (void)unified_black;
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

}

