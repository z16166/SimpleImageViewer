use std::path::{Path, PathBuf};
use crossbeam_channel::Sender;

pub const SUPPORTED_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "bmp", "tiff", "tif",
    "webp", "ico", "tga", "hdr", "ppm", "pbm", "pgm", "pnm",
    "avif", "qoi", "exr",
];

pub fn is_supported_image(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| SUPPORTED_EXTENSIONS.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

#[cfg(target_os = "windows")]
pub fn is_offline(path: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    // Attributes indicating the file data is not fully present locally:
    // FILE_ATTRIBUTE_OFFLINE (0x1000)
    // FILE_ATTRIBUTE_RECALL_ON_OPEN (0x40000)
    // FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS (0x400000)
    const OFFLINE_MASK: u32 = 0x1000 | 0x40000 | 0x400000;

    if let Ok(metadata) = std::fs::metadata(path) {
        let attr = metadata.file_attributes();
        (attr & OFFLINE_MASK) != 0
    } else {
        false
    }
}

#[cfg(not(target_os = "windows"))]
pub fn is_offline(_path: &Path) -> bool {
    false
}

pub fn scan_directory(dir: PathBuf, recursive: bool, tx: Sender<Vec<PathBuf>>) {
    std::thread::spawn(move || {
        let mut files = Vec::new();

        if recursive {
            for entry in walkdir::WalkDir::new(&dir)
                .follow_links(false)
                .into_iter()
                .flatten()
            {
                if entry.file_type().is_file() 
                    && is_supported_image(entry.path()) 
                    && !is_offline(entry.path()) 
                {
                    files.push(entry.path().to_owned());
                }
            }
            files.sort();
        } else if let Ok(entries) = std::fs::read_dir(&dir) {
            let mut paths: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_file() && is_supported_image(p) && !is_offline(p))
                .collect();
            paths.sort();
            files = paths;
        }

        let _ = tx.send(files);
    });
}
