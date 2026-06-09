#!/usr/bin/env python3
from pathlib import Path

base = Path(__file__).resolve().parents[1] / "src/hdr/openexr_core"
chrom = base / "chromaticities.rs"
ct = chrom.read_text(encoding="utf-8")
for fn in [
    "chromaticities_looks_like_aces_ap0",
    "hdr_color_space_from_chromaticities_xy",
    "imf_exr_chromaticities_from_path",
    "openexr_luminance_weights_from_chromaticities_xy",
]:
    ct = ct.replace(f"\nfn {fn}", f"\npub(crate) fn {fn}", 1)
chrom.write_text(ct, encoding="utf-8")
types = base / "types.rs"
tt = types.read_text(encoding="utf-8")
for name in ["OpenExrCoreDecodedChunkKey", "OpenExrCoreDecodedChunk", "OpenExrCoreDecodedChunkCache"]:
    tt = tt.replace(f"struct {name}", f"pub(crate) struct {name}", 1)
types.write_text(tt, encoding="utf-8")
channels = base / "channels.rs"
ct = channels.read_text(encoding="utf-8")
for sym in [
    "assign_channel_roles",
    "copy_channels",
    "decode_pipeline_channels",
    "channel_sample_f32",
    "channel_sample_f32_filtered",
    "sampled_channel_flat_index",
    "OpenExrCoreChannelChunkLayout",
    "ChannelRole",
]:
    ct = ct.replace(f"\nfn {sym}", f"\npub(crate) fn {sym}", 1)
    ct = ct.replace(f"\nenum {sym}", f"\npub(crate) enum {sym}", 1)
    ct = ct.replace(f"\nstruct {sym}", f"\npub(crate) struct {sym}", 1)
channels.write_text(ct, encoding="utf-8")
mmap = base / "mmap.rs"
mt = mmap.read_text(encoding="utf-8")
for sym in ["ExrMmapReadCookie", "ExrMmapCookieGuard", "openexr_memory_map_initializer"]:
    mt = mt.replace(f"\nstruct {sym}", f"\npub(crate) struct {sym}", 1)
    mt = mt.replace(f"\nfn {sym}", f"\npub(crate) fn {sym}", 1)
mmap.write_text(mt, encoding="utf-8")
for sym in ["ChannelRole"]:
    tt = types.read_text(encoding="utf-8")
    tt = tt.replace(f"enum {sym}", f"pub(crate) enum {sym}", 1)
    types.write_text(tt, encoding="utf-8")
for sym in [
    "OpenExrCoreChunkDecodeTiming",
    "OpenExrCoreDecodedChunkFetch",
    "OpenExrCoreTileGrid",
    "OpenExrCoreChannelChunkLayout",
    "exr_attr_string_to_string",
    "exr_result",
]:
    ct = channels.read_text(encoding="utf-8")
    ct = ct.replace(f"\nstruct {sym}", f"\npub(crate) struct {sym}", 1)
    ct = ct.replace(f"\nfn {sym}", f"\npub(crate) fn {sym}", 1)
    channels.write_text(ct, encoding="utf-8")
mod = base / "mod.rs"
mod.write_text(
    mod.read_text(encoding="utf-8")
    .replace("#![allow(dead_code)]", "#[allow(dead_code)]")
    .replace(
        "pub(crate) use types::{OpenExrCoreChannelInfo, OpenExrCorePartInfo, OpenExrCoreRgbaTile};",
        "pub(crate) use chromaticities::{OpenExrCoreChannelInfo, OpenExrCorePartInfo, OpenExrCoreRgbaTile};\n"
        "pub(crate) use types::{OpenExrCoreDecodedChunk, OpenExrCoreDecodedChunkCache, OpenExrCoreDecodedChunkKey};",
    ),
    encoding="utf-8",
)
print("openexr exports ok")
