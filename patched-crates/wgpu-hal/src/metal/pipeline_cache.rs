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

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use alloc::string::ToString as _;
use alloc::vec::Vec;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::{NSArray, NSError, NSString, NSURL};
use objc2_metal::{
    MTLBinaryArchive, MTLBinaryArchiveDescriptor, MTLComputePipelineDescriptor, MTLDevice,
    MTLRenderPipelineDescriptor,
};

use super::PipelineCache;

static TEMP_FILE_COUNTER: AtomicU32 = AtomicU32::new(0);

fn get_temp_file_path() -> PathBuf {
    let pid = std::process::id();
    let thread_id = std::thread::current().id();
    let count = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);

    // Stable hash of the ThreadId to avoid cross-platform/toolchain formatting differences
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    thread_id.hash(&mut hasher);
    let thread_hash = hasher.finish();

    let filename = format!(
        "siv_pipeline_cache_{}_{}_{}_{}.bin",
        pid,
        thread_hash,
        count,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    std::env::temp_dir().join(filename)
}

pub struct TempFileGuard {
    path: PathBuf,
}

impl TempFileGuard {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        if self.path.exists() {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

unsafe fn make_custom_error(code: isize) -> Retained<NSError> {
    let ns_domain = NSString::from_str("com.wgpu.metal.pipeline_cache");
    NSError::new(code, &ns_domain)
}

fn path_to_nsurl(path: &std::path::Path) -> Option<Retained<NSURL>> {
    NSURL::from_file_path(path)
}

pub unsafe fn load_binary_archive(
    device: &ProtocolObject<dyn MTLDevice>,
    data: &[u8],
) -> Result<Retained<ProtocolObject<dyn MTLBinaryArchive>>, Retained<NSError>> {
    objc2::rc::autoreleasepool(|_| unsafe {
        let descriptor = MTLBinaryArchiveDescriptor::new();

        let temp_path = get_temp_file_path();
        let _guard = TempFileGuard::new(temp_path.clone());

        if let Err(e) = std::fs::write(&temp_path, data) {
            log::warn!("Failed to write temporary pipeline cache file: {:?}", e);
            return Err(make_custom_error(2));
        }

        let url = path_to_nsurl(&temp_path).ok_or_else(|| make_custom_error(3))?;
        descriptor.setUrl(Some(&url));

        device.newBinaryArchiveWithDescriptor_error(&descriptor)
    })
}

pub unsafe fn write_binary_archive(
    archive: &ProtocolObject<dyn MTLBinaryArchive>,
) -> Option<Vec<u8>> {
    objc2::rc::autoreleasepool(|_| unsafe {
        let temp_path = get_temp_file_path();
        let _guard = TempFileGuard::new(temp_path.clone());

        let url = path_to_nsurl(&temp_path)?;

        match archive.serializeToURL_error(&url) {
            Ok(()) => std::fs::read(&temp_path).ok(),
            Err(err) => {
                let desc = err.localizedDescription().to_string();
                log::debug!(
                    "MTLBinaryArchive serializeToURL failed (might be empty/no pipelines): {}",
                    desc
                );
                None
            }
        }
    })
}

pub unsafe fn create_empty_binary_archive(
    device: &ProtocolObject<dyn MTLDevice>,
) -> Option<Retained<ProtocolObject<dyn MTLBinaryArchive>>> {
    objc2::rc::autoreleasepool(|_| unsafe {
        let descriptor = MTLBinaryArchiveDescriptor::new();
        match device.newBinaryArchiveWithDescriptor_error(&descriptor) {
            Ok(archive) => Some(archive),
            Err(err) => {
                log::warn!("Failed to create new empty MTLBinaryArchive: {:?}", err);
                None
            }
        }
    })
}

pub unsafe fn set_binary_archives_on_compute_descriptor(
    descriptor: &MTLComputePipelineDescriptor,
    cache: &PipelineCache,
) {
    if let Some(ref archive_mutex) = cache.archive {
        let archive = archive_mutex.lock();
        let array = NSArray::from_retained_slice(&[(*archive).clone()]);
        descriptor.setBinaryArchives(Some(&array));
    }
}

pub unsafe fn add_compute_pipeline_to_archive(
    descriptor: &MTLComputePipelineDescriptor,
    cache: &PipelineCache,
) {
    if let Some(ref archive_mutex) = cache.archive {
        let archive = archive_mutex.lock();
        match archive.addComputePipelineFunctionsWithDescriptor_error(descriptor) {
            Ok(()) => {
                cache.dirty.store(true, Ordering::Release);
            }
            Err(err) => {
                let desc = err.localizedDescription().to_string();
                // Match "already exists" specifically to avoid overly broad matching of "exist"
                if desc.contains("already exists") {
                    log::debug!("Compute pipeline already in archive: {}", desc);
                } else {
                    log::warn!("Failed to add compute pipeline to MTLBinaryArchive: {}", desc);
                }
            }
        }
    }
}

pub unsafe fn set_binary_archives_on_render_descriptor(
    descriptor: &MTLRenderPipelineDescriptor,
    cache: &PipelineCache,
) {
    if let Some(ref archive_mutex) = cache.archive {
        let archive = archive_mutex.lock();
        let array = NSArray::from_retained_slice(&[(*archive).clone()]);
        descriptor.setBinaryArchives(Some(&array));
    }
}

pub unsafe fn add_render_pipeline_to_archive(
    descriptor: &MTLRenderPipelineDescriptor,
    cache: &PipelineCache,
) {
    if let Some(ref archive_mutex) = cache.archive {
        let archive = archive_mutex.lock();
        match archive.addRenderPipelineFunctionsWithDescriptor_error(descriptor) {
            Ok(()) => {
                cache.dirty.store(true, Ordering::Release);
            }
            Err(err) => {
                let desc = err.localizedDescription().to_string();
                // Match "already exists" specifically to avoid overly broad matching of "exist"
                if desc.contains("already exists") {
                    log::debug!("Render pipeline already in archive: {}", desc);
                } else {
                    log::warn!("Failed to add render pipeline to MTLBinaryArchive: {}", desc);
                }
            }
        }
    }
}
