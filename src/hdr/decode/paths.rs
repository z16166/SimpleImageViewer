// Simple Image Viewer - A high-performance, cross-platform image viewer
use std::path::Path;

pub(crate) fn is_exr_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("exr"))
}

pub(crate) fn is_radiance_hdr_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| matches!(ext.to_ascii_lowercase().as_str(), "hdr" | "pic"))
}
