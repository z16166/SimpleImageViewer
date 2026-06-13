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

}

