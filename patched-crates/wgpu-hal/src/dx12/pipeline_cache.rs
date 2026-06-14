use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use parking_lot::Mutex;

use super::CompiledShader;
use crate::RenderPipelineDescriptor;

const BLOB_MAP_VERSION: u32 = 2;
/// High bit tags DXIL blobs keyed by shader source; PSO blobs use keys with this bit clear.
const SHADER_DISK_KEY_BIT: u64 = 1u64 << 63;

#[derive(Debug, Default)]
pub struct PipelineCache {
    blobs: Mutex<BTreeMap<u64, Arc<Vec<u8>>>>,
}

impl PipelineCache {
    pub fn from_initial_data(data: Option<&[u8]>) -> Self {
        Self {
            blobs: Mutex::new(decode_blob_map(data)),
        }
    }

    pub fn lookup_blob(&self, key: u64) -> Option<Arc<Vec<u8>>> {
        self.blobs.lock().get(&key).cloned()
    }

    pub fn insert_blob(&self, key: u64, blob: Vec<u8>) {
        if blob.is_empty() {
            return;
        }
        self.blobs.lock().insert(key, Arc::new(blob));
    }

    pub fn encode(&self) -> Vec<u8> {
        let blobs = self.blobs.lock();
        let mut out = Vec::new();
        push_u32(&mut out, BLOB_MAP_VERSION);
        push_u32(&mut out, blobs.len() as u32);
        for (key, blob) in blobs.iter() {
            push_u64(&mut out, *key);
            push_u32(&mut out, blob.len() as u32);
            out.extend_from_slice(blob);
        }
        out
    }
}

/// Stable 64-bit FNV-1a. Must not use [`DefaultHasher`]: its seed is randomized per process,
/// which breaks on-disk pipeline cache lookup across restarts.
pub(super) fn hash_bytes(bytes: &[u8], seed: u64) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64 ^ seed;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

pub(super) fn hash_shader(shader: &CompiledShader, seed: u64) -> u64 {
    hash_bytes(shader_bytecode(shader), seed)
}

fn shader_bytecode(shader: &CompiledShader) -> &[u8] {
    match shader {
        CompiledShader::Dxc(blob) => unsafe {
            core::slice::from_raw_parts(blob.GetBufferPointer().cast(), blob.GetBufferSize())
        },
        CompiledShader::Fxc(blob) => unsafe {
            core::slice::from_raw_parts(blob.GetBufferPointer().cast(), blob.GetBufferSize())
        },
        CompiledShader::Precompiled(bytes) => bytes.as_slice(),
    }
}

pub(super) fn compute_pipeline_cache_key(
    layout: &super::PipelineLayout,
    shader: &CompiledShader,
) -> u64 {
    (hash_shader(shader, hash_pipeline_layout(layout))) & !SHADER_DISK_KEY_BIT
}

pub(super) fn render_pipeline_cache_key<L, S, C>(
    desc: &RenderPipelineDescriptor<L, S, C>,
    layout: &super::PipelineLayout,
    vertex_shader: Option<&CompiledShader>,
    pixel_shader: Option<&CompiledShader>,
) -> u64
where
    L: crate::DynPipelineLayout + ?Sized,
    S: crate::DynShaderModule + ?Sized,
    C: crate::DynPipelineCache + ?Sized,
{
    let mut key = hash_pipeline_layout(layout);
    if let Some(vs) = vertex_shader {
        key = hash_shader(vs, key);
    }
    if let Some(ps) = pixel_shader {
        key = hash_shader(ps, key);
    }
    key = hash_u64(key, desc.primitive.topology as u64);
    key = hash_u64(key, desc.multisample.count as u64);
    key = hash_u64(key, desc.multiview_mask.map(|m| m.get()).unwrap_or(0) as u64);
    for target in desc.color_targets {
        if let Some(target) = target.as_ref() {
            key = hash_u64(key, texture_format_key(target.format));
            key = hash_u64(key, target.blend.as_ref().map(|_| 1).unwrap_or(0));
        }
    }
    if let Some(ds) = desc.depth_stencil.as_ref() {
        key = hash_u64(key, texture_format_key(ds.format));
    }
    key & !SHADER_DISK_KEY_BIT
}

fn texture_format_key(format: wgt::TextureFormat) -> u64 {
    hash_bytes(alloc::format!("{format:?}").as_bytes(), 0x5445_5854)
}

fn hash_pipeline_layout(layout: &super::PipelineLayout) -> u64 {
    let mut key = hash_u64(0x504C_4159, layout.shared.total_root_elements as u64);
    for (idx, info) in layout.bind_group_infos.iter().enumerate() {
        if info.is_some() {
            key = hash_u64(key, idx as u64 + 1);
        }
    }
    key
}

fn hash_u64(seed: u64, value: u64) -> u64 {
    hash_bytes(&value.to_le_bytes(), seed)
}

/// Stable on-disk key for DXC inputs (HLSL source + entry point), not DXIL bytecode.
pub(super) fn shader_source_cache_key(
    source: &str,
    entry_point: &str,
    stage: u8,
    shader_model: &str,
) -> u64 {
    let mut key = hash_u64(SHADER_DISK_KEY_BIT | 0x5348_4449, stage as u64);
    key = hash_bytes(shader_model.as_bytes(), key);
    key = hash_bytes(entry_point.as_bytes(), key);
    key = hash_bytes(source.as_bytes(), key);
    key | SHADER_DISK_KEY_BIT
}

pub(super) fn store_shader_bytecode(
    cache: Option<&PipelineCache>,
    key: u64,
    shader: &CompiledShader,
) {
    let Some(cache) = cache else {
        return;
    };
    let Some(bytes) = shader_bytecode_owned(shader) else {
        return;
    };
    cache.insert_blob(key, bytes);
}

fn shader_bytecode_owned(shader: &CompiledShader) -> Option<Vec<u8>> {
    Some(match shader {
        CompiledShader::Dxc(blob) => unsafe {
            core::slice::from_raw_parts(blob.GetBufferPointer().cast(), blob.GetBufferSize())
                .to_vec()
        },
        CompiledShader::Fxc(blob) => unsafe {
            core::slice::from_raw_parts(blob.GetBufferPointer().cast(), blob.GetBufferSize())
                .to_vec()
        },
        CompiledShader::Precompiled(bytes) => bytes.clone(),
    })
}

fn decode_blob_map(data: Option<&[u8]>) -> BTreeMap<u64, Arc<Vec<u8>>> {
    let Some(data) = data else {
        return BTreeMap::new();
    };
    let mut offset = 0usize;
    let version = read_u32(data, &mut offset).unwrap_or(0);
    if version != BLOB_MAP_VERSION {
        return BTreeMap::new();
    }
    let count = read_u32(data, &mut offset).unwrap_or(0) as usize;
    let mut map = BTreeMap::new();
    for _ in 0..count {
        let Some(key) = read_u64(data, &mut offset) else {
            break;
        };
        let Some(len) = read_u32(data, &mut offset) else {
            break;
        };
        let len = len as usize;
        if offset + len > data.len() {
            break;
        }
        map.insert(key, Arc::new(data[offset..offset + len].to_vec()));
        offset += len;
    }
    map
}

fn push_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn read_u32(data: &[u8], offset: &mut usize) -> Option<u32> {
    let bytes = data.get(*offset..*offset + 4)?;
    *offset += 4;
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

fn read_u64(data: &[u8], offset: &mut usize) -> Option<u64> {
    let bytes = data.get(*offset..*offset + 8)?;
    *offset += 8;
    Some(u64::from_le_bytes(bytes.try_into().ok()?))
}

pub(super) fn cached_pipeline_state(
    blob: Option<Arc<Vec<u8>>>,
) -> Direct3D12::D3D12_CACHED_PIPELINE_STATE {
    match blob {
        Some(blob) => Direct3D12::D3D12_CACHED_PIPELINE_STATE {
            pCachedBlob: blob.as_ptr().cast(),
            CachedBlobSizeInBytes: blob.len(),
        },
        None => Direct3D12::D3D12_CACHED_PIPELINE_STATE::default(),
    }
}

pub(super) fn store_cached_blob(
    cache: Option<&PipelineCache>,
    key: u64,
    raw: &Direct3D12::ID3D12PipelineState,
    used_cached_blob: bool,
) {
    if used_cached_blob {
        return;
    }
    let Some(cache) = cache else {
        return;
    };
    let Ok(blob) = (unsafe { raw.GetCachedBlob() }) else {
        return;
    };
    let bytes = unsafe {
        core::slice::from_raw_parts(blob.GetBufferPointer().cast(), blob.GetBufferSize()).to_vec()
    };
    cache.insert_blob(key, bytes);
}

use windows::Win32::Graphics::Direct3D12;

impl crate::DynPipelineCache for PipelineCache {}
