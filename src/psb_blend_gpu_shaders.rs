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

//! GPU blend-mode constants, WGSL shader string, and admission helpers.
//!
//! Split from `psb_layer_blend_gpu.rs` to keep that file under the 2000-line
//! review limit while allowing all separable PSD/PSB blend modes to execute
//! on GPU when the canvas is large enough.

// These `pub(crate)` items are consumed by the binary crate's
// `psb_layer_blend_gpu.rs`, not by `lib.rs`.  Suppress unused warnings
// when checking the lib crate alone.
#![allow(dead_code)]

/// WGSL `mode` uniform values (must match shader entry points).
pub(crate) const BLEND_MODE_NORMAL: u32 = 0;
pub(crate) const BLEND_MODE_SCREEN: u32 = 1;
pub(crate) const BLEND_MODE_LINEAR_DODGE: u32 = 2;
pub(crate) const BLEND_MODE_MULTIPLY: u32 = 3;
pub(crate) const BLEND_MODE_OVERLAY: u32 = 4;
pub(crate) const BLEND_MODE_SOFT_LIGHT: u32 = 5;
pub(crate) const BLEND_MODE_HARD_LIGHT: u32 = 6;
pub(crate) const BLEND_MODE_DARKEN: u32 = 7;
pub(crate) const BLEND_MODE_COLOR_BURN: u32 = 8;
pub(crate) const BLEND_MODE_LINEAR_BURN: u32 = 9;
pub(crate) const BLEND_MODE_LIGHTEN: u32 = 10;
pub(crate) const BLEND_MODE_COLOR_DODGE: u32 = 11;
pub(crate) const BLEND_MODE_VIVID_LIGHT: u32 = 12;
pub(crate) const BLEND_MODE_LINEAR_LIGHT: u32 = 13;
pub(crate) const BLEND_MODE_PIN_LIGHT: u32 = 14;
pub(crate) const BLEND_MODE_HARD_MIX: u32 = 15;
pub(crate) const BLEND_MODE_DIFFERENCE: u32 = 16;
pub(crate) const BLEND_MODE_EXCLUSION: u32 = 17;
pub(crate) const BLEND_MODE_SUBTRACT: u32 = 18;
pub(crate) const BLEND_MODE_DIVIDE: u32 = 19;

/// Map a PSD 4-byte blend key to the WGSL `mode` uniform value.
///
/// Returns [`BLEND_MODE_NORMAL`] for non-separable keys; those layers
/// are gated by [`is_gpu_separable_blend`] and never reach the shader.
#[inline]
pub(crate) fn separable_blend_mode_u32(blend: &[u8; 4]) -> u32 {
    match blend {
        b"scrn" => BLEND_MODE_SCREEN,
        b"lddg" => BLEND_MODE_LINEAR_DODGE,
        b"mul " => BLEND_MODE_MULTIPLY,
        b"dark" => BLEND_MODE_DARKEN,
        b"idiv" => BLEND_MODE_COLOR_BURN,
        b"lbrn" => BLEND_MODE_LINEAR_BURN,
        b"lite" => BLEND_MODE_LIGHTEN,
        b"div " => BLEND_MODE_COLOR_DODGE,
        b"vLit" => BLEND_MODE_VIVID_LIGHT,
        b"lLit" => BLEND_MODE_LINEAR_LIGHT,
        b"pLit" => BLEND_MODE_PIN_LIGHT,
        b"hMix" => BLEND_MODE_HARD_MIX,
        b"over" => BLEND_MODE_OVERLAY,
        b"sLit" => BLEND_MODE_SOFT_LIGHT,
        b"hLit" => BLEND_MODE_HARD_LIGHT,
        b"diff" => BLEND_MODE_DIFFERENCE,
        b"excl" => BLEND_MODE_EXCLUSION,
        b"subt" => BLEND_MODE_SUBTRACT,
        b"fdiv" => BLEND_MODE_DIVIDE,
        // Non-separable (hue / sat / colr / lum / dkCl / lgCl / diss / pass)
        // and truly unknown keys → Normal.  The GPU admission gate prevents
        // non-separable keys from reaching the shader.
        _ => BLEND_MODE_NORMAL,
    }
}

/// True when `blend` has a GPU compute-shader entry point (separable modes).
///
/// Returns `false` for modes that require cross-channel computation and therefore
/// stay on CPU:
///   - **Non-separable:** Hue, Saturation, Color, Luminosity — need
///     `SetLum`/`ClipColor` across R/G/B together, not per-channel.
///   - **Per-pixel luminance compare:** Darker Color, Lighter Color — compare
///     whole-pixel luminance, cannot split per channel.
///   - **Stochastic / pass-through:** Dissolve (per-pixel random dither),
///     Pass Through (group-level operator, equivalent to Normal).
///
/// When any layer in a batch returns false here, the entire composite falls
/// back to CPU (`blend_layers_with_clipping`).
#[inline]
pub(crate) fn is_gpu_separable_blend(blend: &[u8; 4]) -> bool {
    matches!(
        blend,
        b"norm"
            | b"scrn"
            | b"lddg"
            | b"mul "
            | b"dark"
            | b"idiv"
            | b"lbrn"
            | b"lite"
            | b"div "
            | b"over"
            | b"sLit"
            | b"hLit"
            | b"vLit"
            | b"lLit"
            | b"pLit"
            | b"hMix"
            | b"diff"
            | b"excl"
            | b"subt"
            | b"fdiv"
    )
}

// ── WGSL shader ───────────────────────────────────────────────────────────

/// Complete WGSL compute-shader source for all GPU-accelerated separable
/// blend modes plus clipping-group helpers.
pub(crate) const PSD_SEPARABLE_BLEND_SHADER: &str = r#"
struct BlendParams {
    canvas_w: u32,
    canvas_h: u32,
    layer_w: u32,
    layer_h: u32,
    layer_left: i32,
    layer_top: i32,
    mode: u32,
    _pad0: u32,
};

@group(0) @binding(0) var target: texture_storage_2d<rgba8unorm, read_write>;
@group(0) @binding(1) var layer_tex: texture_2d<f32>;
@group(0) @binding(2) var<uniform> params: BlendParams;

// ── Blend helper (shared by all separable entry points) ───────────────────

fn blend_store(dst_coord: vec2<i32>, src: vec4<f32>, dst: vec4<f32>,
               blended: vec3<f32>) {
    let sa = src.a;
    let da = dst.a;
    let out_a = sa + da * (1.0 - sa);
    if (out_a <= 0.0) {
        textureStore(target, dst_coord, vec4<f32>(0.0));
        return;
    }
    let co = sa * (1.0 - da) * src.rgb + sa * da * blended + da * (1.0 - sa) * dst.rgb;
    let out_rgb = co / max(out_a, 1e-20);
    // Explicit SDR clamp: the current `rgba8unorm` texture format implicitly
    // clamps to [0, 1] on `textureStore`, but writing the clamp here defends
    // against any future switch to float-format textures (where negatives
    // would write through unmodified and corrupt downstream blending / tone
    // mapping).  Blended values above 1.0 *are* meaningful in HDR, but the
    // current SDR compositor never preserves headroom, so clamping to 1.0 is
    // correct.  A future HDR GPU path should differentiate its clamping
    // strategy per mode.
    let out_rgb = clamp(out_rgb, vec3<f32>(0.0), vec3<f32>(1.0));
    textureStore(target, dst_coord, vec4<f32>(out_rgb, out_a));
}

// ── Separable blend entry points ──────────────────────────────────────────

@compute @workgroup_size(16, 16, 1)
fn cs_blend_normal(@builtin(global_invocation_id) gid: vec3<u32>) {
    let sx = i32(gid.x);
    let sy = i32(gid.y);
    if (sx >= i32(params.layer_w) || sy >= i32(params.layer_h)) { return; }
    let dx = params.layer_left + sx;
    let dy = params.layer_top + sy;
    if (dx < 0 || dy < 0 || dx >= i32(params.canvas_w) || dy >= i32(params.canvas_h)) { return; }
    let src = textureLoad(layer_tex, vec2<i32>(sx, sy), 0);
    let sa = src.a;
    if (sa <= 0.0) { return; }
    let dst_coord = vec2<i32>(dx, dy);
    if (sa >= 1.0) {
        // Opaque source: PDF/ISO 32000-1 porter-duff "over" with B(Cb,Cs) = Cs
        // simplifies to co = Cs, out_a = 1.0.  This is exactly what blend_store
        // would compute, but the direct store avoids the extra texture read of
        // `dst` (the destination is fully overwritten).
        textureStore(target, dst_coord, vec4<f32>(src.rgb, 1.0));
        return;
    }
    let dst = textureLoad(target, dst_coord);
    let blended = src.rgb;
    blend_store(dst_coord, src, dst, blended);
}

@compute @workgroup_size(16, 16, 1)
fn cs_blend_screen(@builtin(global_invocation_id) gid: vec3<u32>) {
    let sx = i32(gid.x);
    let sy = i32(gid.y);
    if (sx >= i32(params.layer_w) || sy >= i32(params.layer_h)) { return; }
    let dx = params.layer_left + sx;
    let dy = params.layer_top + sy;
    if (dx < 0 || dy < 0 || dx >= i32(params.canvas_w) || dy >= i32(params.canvas_h)) { return; }
    let src = textureLoad(layer_tex, vec2<i32>(sx, sy), 0);
    let sa = src.a;
    if (sa <= 0.0) { return; }
    let dst_coord = vec2<i32>(dx, dy);
    let dst = textureLoad(target, dst_coord);

    let blended = dst.rgb + src.rgb - dst.rgb * src.rgb;
    blend_store(dst_coord, src, dst, blended);
}

@compute @workgroup_size(16, 16, 1)
fn cs_blend_linear_dodge(@builtin(global_invocation_id) gid: vec3<u32>) {
    let sx = i32(gid.x);
    let sy = i32(gid.y);
    if (sx >= i32(params.layer_w) || sy >= i32(params.layer_h)) { return; }
    let dx = params.layer_left + sx;
    let dy = params.layer_top + sy;
    if (dx < 0 || dy < 0 || dx >= i32(params.canvas_w) || dy >= i32(params.canvas_h)) { return; }
    let src = textureLoad(layer_tex, vec2<i32>(sx, sy), 0);
    let sa = src.a;
    if (sa <= 0.0) { return; }
    let dst_coord = vec2<i32>(dx, dy);
    let dst = textureLoad(target, dst_coord);
    let blended = min(dst.rgb + src.rgb, vec3<f32>(1.0));
    blend_store(dst_coord, src, dst, blended);
}

@compute @workgroup_size(16, 16, 1)
fn cs_blend_multiply(@builtin(global_invocation_id) gid: vec3<u32>) {
    let sx = i32(gid.x);
    let sy = i32(gid.y);
    if (sx >= i32(params.layer_w) || sy >= i32(params.layer_h)) { return; }
    let dx = params.layer_left + sx;
    let dy = params.layer_top + sy;
    if (dx < 0 || dy < 0 || dx >= i32(params.canvas_w) || dy >= i32(params.canvas_h)) { return; }
    let src = textureLoad(layer_tex, vec2<i32>(sx, sy), 0);
    let sa = src.a;
    if (sa <= 0.0) { return; }
    let dst_coord = vec2<i32>(dx, dy);
    let dst = textureLoad(target, dst_coord);
    let blended = dst.rgb * src.rgb;
    blend_store(dst_coord, src, dst, blended);
}

// ── Overlay / Soft Light / Hard Light (GPU, scalar path on CPU) ───────────

@compute @workgroup_size(16, 16, 1)
fn cs_blend_overlay(@builtin(global_invocation_id) gid: vec3<u32>) {
    let sx = i32(gid.x);
    let sy = i32(gid.y);
    if (sx >= i32(params.layer_w) || sy >= i32(params.layer_h)) { return; }
    let dx = params.layer_left + sx;
    let dy = params.layer_top + sy;
    if (dx < 0 || dy < 0 || dx >= i32(params.canvas_w) || dy >= i32(params.canvas_h)) { return; }
    let src = textureLoad(layer_tex, vec2<i32>(sx, sy), 0);
    let sa = src.a;
    if (sa <= 0.0) { return; }
    let dst_coord = vec2<i32>(dx, dy);
    let dst = textureLoad(target, dst_coord);
    // overlay(cb, cs) = 2*cb*cs when cb <= 0.5, else 1 - 2*(1-cb)*(1-cs)
    let blended = select(
        1.0 - 2.0 * (1.0 - dst.rgb) * (1.0 - src.rgb),
        2.0 * dst.rgb * src.rgb,
        dst.rgb <= vec3<f32>(0.5)
    );
    blend_store(dst_coord, src, dst, blended);
}

@compute @workgroup_size(16, 16, 1)
fn cs_blend_soft_light(@builtin(global_invocation_id) gid: vec3<u32>) {
    let sx = i32(gid.x);
    let sy = i32(gid.y);
    if (sx >= i32(params.layer_w) || sy >= i32(params.layer_h)) { return; }
    let dx = params.layer_left + sx;
    let dy = params.layer_top + sy;
    if (dx < 0 || dy < 0 || dx >= i32(params.canvas_w) || dy >= i32(params.canvas_h)) { return; }
    let src = textureLoad(layer_tex, vec2<i32>(sx, sy), 0);
    let sa = src.a;
    if (sa <= 0.0) { return; }
    let dst_coord = vec2<i32>(dx, dy);
    let dst = textureLoad(target, dst_coord);
    // PDF soft-light: cs <= 0.5 => cb - (1-2*cs)*cb*(1-cb)
    //                cs >  0.5 => cb + (2*cs-1)*(D(cb)-cb) where D(cb)=((16*cb-12)*cb+4)*cb or sqrt(cb)
    let cb = dst.rgb;
    let cs = src.rgb;
    let d = select(sqrt(cb), ((16.0 * cb - 12.0) * cb + 4.0) * cb, cb <= vec3<f32>(0.25));
    let lo = cb - (1.0 - 2.0 * cs) * cb * (1.0 - cb);
    let hi = cb + (2.0 * cs - 1.0) * (d - cb);
    let blended = select(hi, lo, cs <= vec3<f32>(0.5));
    blend_store(dst_coord, src, dst, blended);
}

@compute @workgroup_size(16, 16, 1)
fn cs_blend_hard_light(@builtin(global_invocation_id) gid: vec3<u32>) {
    let sx = i32(gid.x);
    let sy = i32(gid.y);
    if (sx >= i32(params.layer_w) || sy >= i32(params.layer_h)) { return; }
    let dx = params.layer_left + sx;
    let dy = params.layer_top + sy;
    if (dx < 0 || dy < 0 || dx >= i32(params.canvas_w) || dy >= i32(params.canvas_h)) { return; }
    let src = textureLoad(layer_tex, vec2<i32>(sx, sy), 0);
    let sa = src.a;
    if (sa <= 0.0) { return; }
    let dst_coord = vec2<i32>(dx, dy);
    let dst = textureLoad(target, dst_coord);
    // hard-light(cb, cs) = 2*cb*cs when cs <= 0.5, else 1 - 2*(1-cb)*(1-cs)
    let blended = select(
        1.0 - 2.0 * (1.0 - dst.rgb) * (1.0 - src.rgb),
        2.0 * dst.rgb * src.rgb,
        src.rgb <= vec3<f32>(0.5)
    );
    blend_store(dst_coord, src, dst, blended);
}

// ── Darken group ──────────────────────────────────────────────────────────

@compute @workgroup_size(16, 16, 1)
fn cs_blend_darken(@builtin(global_invocation_id) gid: vec3<u32>) {
    let sx = i32(gid.x);
    let sy = i32(gid.y);
    if (sx >= i32(params.layer_w) || sy >= i32(params.layer_h)) { return; }
    let dx = params.layer_left + sx;
    let dy = params.layer_top + sy;
    if (dx < 0 || dy < 0 || dx >= i32(params.canvas_w) || dy >= i32(params.canvas_h)) { return; }
    let src = textureLoad(layer_tex, vec2<i32>(sx, sy), 0);
    let sa = src.a;
    if (sa <= 0.0) { return; }
    let dst_coord = vec2<i32>(dx, dy);
    let dst = textureLoad(target, dst_coord);
    let blended = min(dst.rgb, src.rgb);
    blend_store(dst_coord, src, dst, blended);
}

@compute @workgroup_size(16, 16, 1)
fn cs_blend_color_burn(@builtin(global_invocation_id) gid: vec3<u32>) {
    let sx = i32(gid.x);
    let sy = i32(gid.y);
    if (sx >= i32(params.layer_w) || sy >= i32(params.layer_h)) { return; }
    let dx = params.layer_left + sx;
    let dy = params.layer_top + sy;
    if (dx < 0 || dy < 0 || dx >= i32(params.canvas_w) || dy >= i32(params.canvas_h)) { return; }
    let src = textureLoad(layer_tex, vec2<i32>(sx, sy), 0);
    let sa = src.a;
    if (sa <= 0.0) { return; }
    let dst_coord = vec2<i32>(dx, dy);
    let dst = textureLoad(target, dst_coord);
    // 1 - min(1, (1-cb)/cs) when cs > 0, else 0
    let cb = dst.rgb;
    let cs = src.rgb;
    let blended = 1.0 - min(vec3<f32>(1.0), (1.0 - cb) / max(cs, vec3<f32>(1e-20)));
    blend_store(dst_coord, src, dst, blended);
}

@compute @workgroup_size(16, 16, 1)
fn cs_blend_linear_burn(@builtin(global_invocation_id) gid: vec3<u32>) {
    let sx = i32(gid.x);
    let sy = i32(gid.y);
    if (sx >= i32(params.layer_w) || sy >= i32(params.layer_h)) { return; }
    let dx = params.layer_left + sx;
    let dy = params.layer_top + sy;
    if (dx < 0 || dy < 0 || dx >= i32(params.canvas_w) || dy >= i32(params.canvas_h)) { return; }
    let src = textureLoad(layer_tex, vec2<i32>(sx, sy), 0);
    let sa = src.a;
    if (sa <= 0.0) { return; }
    let dst_coord = vec2<i32>(dx, dy);
    let dst = textureLoad(target, dst_coord);
    let blended = dst.rgb + src.rgb - 1.0;
    blend_store(dst_coord, src, dst, blended);
}

// ── Lighten group ─────────────────────────────────────────────────────────

@compute @workgroup_size(16, 16, 1)
fn cs_blend_lighten(@builtin(global_invocation_id) gid: vec3<u32>) {
    let sx = i32(gid.x);
    let sy = i32(gid.y);
    if (sx >= i32(params.layer_w) || sy >= i32(params.layer_h)) { return; }
    let dx = params.layer_left + sx;
    let dy = params.layer_top + sy;
    if (dx < 0 || dy < 0 || dx >= i32(params.canvas_w) || dy >= i32(params.canvas_h)) { return; }
    let src = textureLoad(layer_tex, vec2<i32>(sx, sy), 0);
    let sa = src.a;
    if (sa <= 0.0) { return; }
    let dst_coord = vec2<i32>(dx, dy);
    let dst = textureLoad(target, dst_coord);
    let blended = max(dst.rgb, src.rgb);
    blend_store(dst_coord, src, dst, blended);
}

@compute @workgroup_size(16, 16, 1)
fn cs_blend_color_dodge(@builtin(global_invocation_id) gid: vec3<u32>) {
    let sx = i32(gid.x);
    let sy = i32(gid.y);
    if (sx >= i32(params.layer_w) || sy >= i32(params.layer_h)) { return; }
    let dx = params.layer_left + sx;
    let dy = params.layer_top + sy;
    if (dx < 0 || dy < 0 || dx >= i32(params.canvas_w) || dy >= i32(params.canvas_h)) { return; }
    let src = textureLoad(layer_tex, vec2<i32>(sx, sy), 0);
    let sa = src.a;
    if (sa <= 0.0) { return; }
    let dst_coord = vec2<i32>(dx, dy);
    let dst = textureLoad(target, dst_coord);
    // min(1, cb/(1-cs)) when cs < 1, else 1
    let cb = dst.rgb;
    let cs = src.rgb;
    let blended = min(vec3<f32>(1.0), cb / max(1.0 - cs, vec3<f32>(1e-20)));
    blend_store(dst_coord, src, dst, blended);
}

// ── Contrast group ────────────────────────────────────────────────────────

@compute @workgroup_size(16, 16, 1)
fn cs_blend_vivid_light(@builtin(global_invocation_id) gid: vec3<u32>) {
    let sx = i32(gid.x);
    let sy = i32(gid.y);
    if (sx >= i32(params.layer_w) || sy >= i32(params.layer_h)) { return; }
    let dx = params.layer_left + sx;
    let dy = params.layer_top + sy;
    if (dx < 0 || dy < 0 || dx >= i32(params.canvas_w) || dy >= i32(params.canvas_h)) { return; }
    let src = textureLoad(layer_tex, vec2<i32>(sx, sy), 0);
    let sa = src.a;
    if (sa <= 0.0) { return; }
    let dst_coord = vec2<i32>(dx, dy);
    let dst = textureLoad(target, dst_coord);
    let cb = dst.rgb;
    let cs = src.rgb;
    // cs <= 0.5: 1 - (1-cb)/(2*cs); cs > 0.5: cb/(2*(1-cs))
    let burn = 1.0 - min((1.0 - cb) / max(2.0 * cs, vec3<f32>(1e-20)), vec3<f32>(1.0));
    let dodge = min(cb / max(2.0 * (1.0 - cs), vec3<f32>(1e-20)), vec3<f32>(1.0));
    let blended = select(dodge, burn, cs <= vec3<f32>(0.5));
    blend_store(dst_coord, src, dst, blended);
}

@compute @workgroup_size(16, 16, 1)
fn cs_blend_linear_light(@builtin(global_invocation_id) gid: vec3<u32>) {
    let sx = i32(gid.x);
    let sy = i32(gid.y);
    if (sx >= i32(params.layer_w) || sy >= i32(params.layer_h)) { return; }
    let dx = params.layer_left + sx;
    let dy = params.layer_top + sy;
    if (dx < 0 || dy < 0 || dx >= i32(params.canvas_w) || dy >= i32(params.canvas_h)) { return; }
    let src = textureLoad(layer_tex, vec2<i32>(sx, sy), 0);
    let sa = src.a;
    if (sa <= 0.0) { return; }
    let dst_coord = vec2<i32>(dx, dy);
    let dst = textureLoad(target, dst_coord);
    let blended = dst.rgb + 2.0 * src.rgb - 1.0;
    blend_store(dst_coord, src, dst, blended);
}

@compute @workgroup_size(16, 16, 1)
fn cs_blend_pin_light(@builtin(global_invocation_id) gid: vec3<u32>) {
    let sx = i32(gid.x);
    let sy = i32(gid.y);
    if (sx >= i32(params.layer_w) || sy >= i32(params.layer_h)) { return; }
    let dx = params.layer_left + sx;
    let dy = params.layer_top + sy;
    if (dx < 0 || dy < 0 || dx >= i32(params.canvas_w) || dy >= i32(params.canvas_h)) { return; }
    let src = textureLoad(layer_tex, vec2<i32>(sx, sy), 0);
    let sa = src.a;
    if (sa <= 0.0) { return; }
    let dst_coord = vec2<i32>(dx, dy);
    let dst = textureLoad(target, dst_coord);
    let cb = dst.rgb;
    let cs = src.rgb;
    // cs <= 0.5: min(cb, 2*cs); else max(cb, 2*cs-1)
    let lo = min(cb, 2.0 * cs);
    let hi = max(cb, 2.0 * cs - 1.0);
    let blended = select(hi, lo, cs <= vec3<f32>(0.5));
    blend_store(dst_coord, src, dst, blended);
}

@compute @workgroup_size(16, 16, 1)
fn cs_blend_hard_mix(@builtin(global_invocation_id) gid: vec3<u32>) {
    let sx = i32(gid.x);
    let sy = i32(gid.y);
    if (sx >= i32(params.layer_w) || sy >= i32(params.layer_h)) { return; }
    let dx = params.layer_left + sx;
    let dy = params.layer_top + sy;
    if (dx < 0 || dy < 0 || dx >= i32(params.canvas_w) || dy >= i32(params.canvas_h)) { return; }
    let src = textureLoad(layer_tex, vec2<i32>(sx, sy), 0);
    let sa = src.a;
    if (sa <= 0.0) { return; }
    let dst_coord = vec2<i32>(dx, dy);
    let dst = textureLoad(target, dst_coord);
    // Per-channel: cb + cs >= 1.0 ? 1.0 : 0.0
    let blended = select(vec3<f32>(0.0), vec3<f32>(1.0), dst.rgb + src.rgb >= vec3<f32>(1.0));
    blend_store(dst_coord, src, dst, blended);
}

// ── Comparative group ─────────────────────────────────────────────────────

@compute @workgroup_size(16, 16, 1)
fn cs_blend_difference(@builtin(global_invocation_id) gid: vec3<u32>) {
    let sx = i32(gid.x);
    let sy = i32(gid.y);
    if (sx >= i32(params.layer_w) || sy >= i32(params.layer_h)) { return; }
    let dx = params.layer_left + sx;
    let dy = params.layer_top + sy;
    if (dx < 0 || dy < 0 || dx >= i32(params.canvas_w) || dy >= i32(params.canvas_h)) { return; }
    let src = textureLoad(layer_tex, vec2<i32>(sx, sy), 0);
    let sa = src.a;
    if (sa <= 0.0) { return; }
    let dst_coord = vec2<i32>(dx, dy);
    let dst = textureLoad(target, dst_coord);
    let blended = abs(dst.rgb - src.rgb);
    blend_store(dst_coord, src, dst, blended);
}

@compute @workgroup_size(16, 16, 1)
fn cs_blend_exclusion(@builtin(global_invocation_id) gid: vec3<u32>) {
    let sx = i32(gid.x);
    let sy = i32(gid.y);
    if (sx >= i32(params.layer_w) || sy >= i32(params.layer_h)) { return; }
    let dx = params.layer_left + sx;
    let dy = params.layer_top + sy;
    if (dx < 0 || dy < 0 || dx >= i32(params.canvas_w) || dy >= i32(params.canvas_h)) { return; }
    let src = textureLoad(layer_tex, vec2<i32>(sx, sy), 0);
    let sa = src.a;
    if (sa <= 0.0) { return; }
    let dst_coord = vec2<i32>(dx, dy);
    let dst = textureLoad(target, dst_coord);
    let blended = dst.rgb + src.rgb - 2.0 * dst.rgb * src.rgb;
    blend_store(dst_coord, src, dst, blended);
}

@compute @workgroup_size(16, 16, 1)
fn cs_blend_subtract(@builtin(global_invocation_id) gid: vec3<u32>) {
    let sx = i32(gid.x);
    let sy = i32(gid.y);
    if (sx >= i32(params.layer_w) || sy >= i32(params.layer_h)) { return; }
    let dx = params.layer_left + sx;
    let dy = params.layer_top + sy;
    if (dx < 0 || dy < 0 || dx >= i32(params.canvas_w) || dy >= i32(params.canvas_h)) { return; }
    let src = textureLoad(layer_tex, vec2<i32>(sx, sy), 0);
    let sa = src.a;
    if (sa <= 0.0) { return; }
    let dst_coord = vec2<i32>(dx, dy);
    let dst = textureLoad(target, dst_coord);
    let blended = dst.rgb - src.rgb;
    blend_store(dst_coord, src, dst, blended);
}

@compute @workgroup_size(16, 16, 1)
fn cs_blend_divide(@builtin(global_invocation_id) gid: vec3<u32>) {
    let sx = i32(gid.x);
    let sy = i32(gid.y);
    if (sx >= i32(params.layer_w) || sy >= i32(params.layer_h)) { return; }
    let dx = params.layer_left + sx;
    let dy = params.layer_top + sy;
    if (dx < 0 || dy < 0 || dx >= i32(params.canvas_w) || dy >= i32(params.canvas_h)) { return; }
    let src = textureLoad(layer_tex, vec2<i32>(sx, sy), 0);
    let sa = src.a;
    if (sa <= 0.0) { return; }
    let dst_coord = vec2<i32>(dx, dy);
    let dst = textureLoad(target, dst_coord);
    // cs > 0 ? min(1, cb / cs) : 1
    let blended = min(vec3<f32>(1.0), dst.rgb / max(src.rgb, vec3<f32>(1e-20)));
    blend_store(dst_coord, src, dst, blended);
}

// ── Clip-group helpers (unchanged) ────────────────────────────────────────

@compute @workgroup_size(16, 16, 1)
fn cs_capture_base_alpha(@builtin(global_invocation_id) gid: vec3<u32>) {
    let sx = i32(gid.x);
    let sy = i32(gid.y);
    if (sx >= i32(params.layer_w) || sy >= i32(params.layer_h)) { return; }
    let dx = params.layer_left + sx;
    let dy = params.layer_top + sy;
    if (dx < 0 || dy < 0 || dx >= i32(params.canvas_w) || dy >= i32(params.canvas_h)) { return; }
    let base = textureLoad(layer_tex, vec2<i32>(sx, sy), 0);
    textureStore(target, vec2<i32>(dx, dy), vec4<f32>(base.a, 0.0, 0.0, 0.0));
}

@compute @workgroup_size(16, 16, 1)
fn cs_apply_base_alpha_mask(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.canvas_w || gid.y >= params.canvas_h) { return; }
    let coord = vec2<i32>(i32(gid.x), i32(gid.y));
    let mask = textureLoad(layer_tex, coord, 0).r;
    if (mask <= 0.0) { textureStore(target, coord, vec4<f32>(0.0)); return; }
    if (mask >= 1.0) { return; }
    let group = textureLoad(target, coord);
    let a_u = u32(floor(group.a * 255.0 + 0.5));
    let m_u = u32(floor(mask * 255.0 + 0.5));
    let out_a_u = (a_u * m_u) / 255u;
    if (out_a_u == 0u) { textureStore(target, coord, vec4<f32>(0.0)); return; }
    textureStore(target, coord, vec4<f32>(group.rgb, f32(out_a_u) / 255.0));
}

@compute @workgroup_size(16, 16, 1)
fn cs_clear_storage(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.canvas_w || gid.y >= params.canvas_h) { return; }
    textureStore(target, vec2<i32>(i32(gid.x), i32(gid.y)), vec4<f32>(0.0));
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_constants_covers_all_gpu_separable() {
        assert_eq!(separable_blend_mode_u32(b"scrn"), BLEND_MODE_SCREEN);
        assert_eq!(separable_blend_mode_u32(b"lddg"), BLEND_MODE_LINEAR_DODGE);
        assert_eq!(separable_blend_mode_u32(b"mul "), BLEND_MODE_MULTIPLY);
        assert_eq!(separable_blend_mode_u32(b"dark"), BLEND_MODE_DARKEN);
        assert_eq!(separable_blend_mode_u32(b"idiv"), BLEND_MODE_COLOR_BURN);
        assert_eq!(separable_blend_mode_u32(b"lbrn"), BLEND_MODE_LINEAR_BURN);
        assert_eq!(separable_blend_mode_u32(b"lite"), BLEND_MODE_LIGHTEN);
        assert_eq!(separable_blend_mode_u32(b"div "), BLEND_MODE_COLOR_DODGE);
        assert_eq!(separable_blend_mode_u32(b"vLit"), BLEND_MODE_VIVID_LIGHT);
        assert_eq!(separable_blend_mode_u32(b"lLit"), BLEND_MODE_LINEAR_LIGHT);
        assert_eq!(separable_blend_mode_u32(b"pLit"), BLEND_MODE_PIN_LIGHT);
        assert_eq!(separable_blend_mode_u32(b"hMix"), BLEND_MODE_HARD_MIX);
        assert_eq!(separable_blend_mode_u32(b"diff"), BLEND_MODE_DIFFERENCE);
        assert_eq!(separable_blend_mode_u32(b"excl"), BLEND_MODE_EXCLUSION);
        assert_eq!(separable_blend_mode_u32(b"subt"), BLEND_MODE_SUBTRACT);
        assert_eq!(separable_blend_mode_u32(b"fdiv"), BLEND_MODE_DIVIDE);
        // norm returns NORMAL
        assert_eq!(separable_blend_mode_u32(b"norm"), BLEND_MODE_NORMAL);
        // non-separable returns NORMAL
        assert_eq!(separable_blend_mode_u32(b"colr"), BLEND_MODE_NORMAL);
        assert_eq!(separable_blend_mode_u32(b"hue "), BLEND_MODE_NORMAL);
    }

    #[test]
    fn gpu_separable_admits_all_separable() {
        let separable_keys = [
            b"norm", b"scrn", b"lddg", b"mul ", b"dark", b"idiv", b"lbrn", b"lite", b"div ",
            b"over", b"sLit", b"hLit", b"vLit", b"lLit", b"pLit", b"hMix", b"diff", b"excl",
            b"subt", b"fdiv",
        ];
        for key in &separable_keys {
            assert!(
                is_gpu_separable_blend(key),
                "key {:?} should be GPU-separable",
                core::str::from_utf8(*key)
            );
        }
    }

    #[test]
    fn gpu_separable_rejects_nonseparable() {
        let non_separable = [
            b"hue ", b"sat ", b"colr", b"lum ", b"dkCl", b"lgCl", b"diss", b"pass",
        ];
        for key in &non_separable {
            assert!(
                !is_gpu_separable_blend(key),
                "key {:?} should NOT be GPU-separable",
                core::str::from_utf8(*key)
            );
        }
    }
}
