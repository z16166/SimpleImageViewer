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
}
