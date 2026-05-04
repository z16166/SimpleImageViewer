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

use std::collections::HashSet;
use std::sync::{Arc, OnceLock, RwLock};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum FormatGroup {
    Standard,
    Pro,
    WicSystem,
    WicRaw,
    Others,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ImageFormat {
    pub extension: String,
    pub group: FormatGroup,
    pub description: String,
    // CLSID is Windows specific, but we keep it as raw bytes for cross-platform data safety
    pub wic_clsid: Option<[u8; 16]>,
}

pub struct FormatRegistry {
    #[allow(dead_code)]
    pub formats: Vec<ImageFormat>,
    pub extensions: HashSet<String>,
    #[allow(dead_code)]
    pub discovery_finished: bool,
}

impl FormatRegistry {
    pub fn new() -> Self {
        let mut formats = Vec::new();
        let mut extensions = HashSet::new();

        let builtin_standard = [
            ("png", "PNG Image"),
            ("apng", "Animated PNG Image"),
            ("jpg", "JPEG Image"),
            ("jpeg", "JPEG Image"),
            ("gif", "GIF Image"),
            ("bmp", "Bitmap Image"),
            ("webp", "WebP Image"),
            ("ico", "Icon Image"),
            ("avif", "AVIF Image"),
            ("avifs", "AVIF Image Sequence"),
        ];

        let builtin_pro = [
            ("tiff", "TIFF Image"),
            ("tif", "TIFF Image"),
            ("tga", "TGA Image"),
            ("psd", "Photoshop Image"),
            ("psb", "Photoshop Large Image"),
            ("exr", "OpenEXR Image"),
            ("hdr", "High Dynamic Range Image"),
            ("qoi", "QOI Image"),
            ("ppm", "PPM Image"),
            ("pbm", "PBM Image"),
            ("pgm", "PGM Image"),
            ("pnm", "PNM Image"),
            ("heif", "HEIF Image"),
            ("heic", "HEIC Image"),
            ("jxl", "JPEG XL Image"),
        ];

        for (ext, desc) in builtin_standard {
            formats.push(ImageFormat {
                extension: ext.to_string(),
                group: FormatGroup::Standard,
                description: desc.to_string(),
                wic_clsid: None,
            });
            extensions.insert(ext.to_string());
        }

        for (ext, desc) in builtin_pro {
            formats.push(ImageFormat {
                extension: ext.to_string(),
                group: FormatGroup::Pro,
                description: desc.to_string(),
                wic_clsid: None,
            });
            extensions.insert(ext.to_string());
        }

        // Dynamically register all RAW formats supported by LibRaw.
        // We use our custom-exported libraw_get_supported_extensions() API.
        let libraw_exts = crate::raw_processor::get_supported_extensions();
        let libraw_count = libraw_exts.len();
        for ext in libraw_exts {
            if !extensions.contains(&ext) {
                formats.push(ImageFormat {
                    extension: ext.clone(),
                    group: FormatGroup::WicRaw,
                    description: format!("{} RAW Image", ext.to_uppercase()),
                    wic_clsid: None,
                });
                extensions.insert(ext);
            }
        }
        log::info!(
            "LibRaw dynamic discovery: successfully registered {} camera formats.",
            libraw_count
        );

        #[allow(unused_mut)]
        let mut registry = Self {
            formats,
            extensions,
            discovery_finished: cfg!(not(target_os = "windows")), // Non-windows is always "finished" discovering
        };

        #[cfg(target_os = "macos")]
        {
            let macos_exts = crate::macos_image_io::discover_imageio_codecs();
            let discovered_count = macos_exts.len();
            for ext in macos_exts {
                registry.add_format(ImageFormat {
                    extension: ext.clone(),
                    group: FormatGroup::Others,
                    description: format!("macOS {} Image", ext.to_uppercase()),
                    wic_clsid: None,
                });
            }
            log::info!(
                "macOS ImageIO discovery: found {} system formats. Total registry size: {} extensions.",
                discovered_count,
                registry.extensions.len()
            );
        }

        registry
    }

    #[allow(dead_code)]
    pub fn add_format(&mut self, format: ImageFormat) {
        if !self.extensions.contains(&format.extension) {
            self.extensions.insert(format.extension.clone());
            self.formats.push(format);
        }
    }
}

pub fn get_registry() -> &'static Arc<RwLock<FormatRegistry>> {
    static REGISTRY: OnceLock<Arc<RwLock<FormatRegistry>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Arc::new(RwLock::new(FormatRegistry::new())))
}

/// Helper to check if a file extension is supported on the current platform.
pub fn is_supported_extension(ext: &std::ffi::OsStr) -> bool {
    if let Some(e) = ext.to_str() {
        let e_lower = e.to_lowercase();
        if let Ok(reg) = get_registry().read() {
            return reg.extensions.contains(&e_lower);
        }
    }
    false
}
