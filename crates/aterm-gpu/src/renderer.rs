// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// GPU terminal renderer: glyph atlas + instanced cell quads, drawn offscreen and
// read back into an `aterm_render::Frame`. The output is built to MATCH the CPU
// `aterm_render::Renderer` exactly: same cell geometry (from the CPU renderer's
// metrics), same per-cell background fill, same glyph placement, and the same
// coverage blend (`out = fg*cov + bg*(1-cov)`, where TEXT coverage is first
// remapped by the W2 linear-corrected compensation when `text_blending` is the
// corrected default — see `fs_glyph`). Compositing is LINEAR-LIGHT, to
// match the CPU `blend` (which decodes sRGB→linear, lerps, re-encodes): the base
// OVER/REPLACE streams attach an `Rgba8UnormSrgb` VIEW of the offscreen, so the
// fixed-function `SrcAlpha`/`OneMinusSrcAlpha` blend decodes the dst, blends in
// linear, and re-encodes on store. The ADDITIVE streams (the LUMEN glow aurora,
// the sparkle-word `deco_add`) emit RAW colour into a `One`/`One` add. On NATIVE the
// offscreen is plain `Rgba8Unorm`, so that add is a byte-exact 8-bit add == the CPU
// `add_sat`. On DOWNLEVEL it lands in linear instead (see below). Storage is
// sRGB-encoded bytes either way.
//
// DOWNLEVEL FALLBACK: aliasing a plain `Rgba8Unorm` offscreen with an sRGB VIEW needs
// `DownlevelFlags::VIEW_FORMATS`, which GLES/WebGL2 lack (`srgb_offscreen` cap in
// `lib.rs`). There the offscreen texture is ITSELF `Rgba8UnormSrgb`: the base
// OVER/REPLACE passes attach its sRGB view and STILL composite in LINEAR light, so the
// base render matches native. The trade-off is the ADDITIVE streams (glow aurora,
// sparkle-add, bloom halo) — they share that single sRGB offscreen, so their One/One
// add lands in LINEAR rather than on raw sRGB bytes. With ONE offscreen you get a
// correct-linear base OVER xor a byte-exact (gamma) additive, not both; we keep the
// correct base and ACCEPT the additive approximation (a slightly brighter/hue-shifted
// dazzle, worst on near-black). It is cosmetic, confined to the optional embellishment
// layer, and never asserted byte-exact on downlevel (see `additive_is_byte_exact` + the
// glow parity guard). Native (VIEW_FORMATS) keeps BOTH exact — described below.
//
// On-glass present: the offscreen `Rgba8Unorm` texture is the SINGLE SOURCE OF
// TRUTH (parity-tested, and the exact buffer the AI snapshot/`image` introspection
// reads). The window path does NOT re-render into the swapchain; it BLITS that
// same texture into a swapchain texture with a fullscreen-triangle pass and
// presents on the GPU, so on-screen pixels are byte-identical to introspection
// (a hard invariant). The blit fragment can invert RGB for the visual bell.
//
// The common frame is ONE fused render pass on the sRGB view, in CPU draw order:
// bg → glyph → colour-emoji → inline-image → line-deco → undercurl →
// sparkle-paw → cursor (block fill / cut-out glyph / shapes). It SPLITS only when an additive
// stream is non-empty — sRGB(base) | view(glow) | sRGB(sparkle-paw) | view(sparkle-add)
// | sRGB(cursor) — the additive streams attach the offscreen's DEFAULT `view` (plain
// Unorm on native ⇒ byte-exact add; sRGB on downlevel ⇒ linear add, see DOWNLEVEL
// FALLBACK) while preserving the exact layering. A live `glow_under` (EMBERFORGE
// flame body, UNDER the glyph ink) additionally parts the base pass itself:
// sRGB(bg..trail) | view(glow_under) | sRGB(glyph..curl). Bloom (half-res blur) and the settings-card tray
// composite afterward in their own passes on the Unorm view. Cursor shapes follow
// DECSCUSR via the SAME geometry helper the CPU uses (`aterm_render::cursor_rects`);
// a block cursor is the CPU's exact recipe (cursor-colour bg quad + cut-out glyph).

use std::collections::{BTreeSet, HashMap};

// FxHashMap for the per-drawable-cell glyph atlas lookups (same fast non-DoS hasher the
// CPU renderer + the engine grid/core caches use): the glyph atlas `map` is keyed on the
// local `GlyphKey` once per drawable cell on the encode hot path; SipHash is wasted there.
use aterm_hash::FxHashMap;

// A-3: the GPU renderer no longer borrows `&Terminal` — it consumes only the
// engine-built `RenderInput`. `Terminal` is imported solely in the test modules
// (which build terminals + call `Terminal::cell_frame` to feed the renderer).
use aterm_core::terminal::{CursorStyle, UnderlineStyle};
use aterm_render::{
    DirtyDecision, Frame, GlyphImage, GlyphKey, Rasterizer, RenderInput, RenderView, Renderer,
    Theme, compute_dirty_rows, is_unchanged_frame,
};

use crate::GpuContext;

/// Atlas texture width in texels. A multiple of 256 so the R8 `bytes_per_row`
/// (== width) needs no extra padding on upload.
const ATLAS_WIDTH: u32 = 1024;

/// Extra texel rows the resident atlas TEXTURE carries beyond its currently
/// packed (occupied) height. New glyphs append into this headroom via a cheap
/// sub-region upload — no texture recreation — until it is exhausted, at which
/// point a glyph that would exceed it is genuine overflow (full repack into a
/// fresh, taller texture). Sized for several more shelves of typical glyphs so
/// the steady state grows in place. (Untouched headroom rows are never sampled:
/// no glyph slot points into them until an append writes them.)
const ATLAS_GROW_HEADROOM: u32 = 256;

/// Screen size + text-blend mode uniform (16 bytes for std140 alignment).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    screen: [f32; 2],
    /// W2: `!= 0.0` ⇒ `fs_glyph` applies the linear-corrected coverage remap
    /// (the float twin of the CPU `blend_text(.., corrected=true)`).
    text_blend: f32,
    _pad: f32,
}

/// One background quad: a pixel-space rect filled with an opaque colour.
///
/// PACKED LAYOUT (12 B, was 32 B): every bg rect is a NON-NEGATIVE INTEGER pixel
/// coord (x, y, w, h) — produced by `usize`/`u32` arithmetic and the `usize`-rect
/// helpers (`cursor_rects`/`underline_rects`/`strike_overline_rects`) — so it fits
/// `u16` exactly and decodes via `Uint16x4`→`vec4<u32>`→`vec4<f32>` with NO
/// precision loss at these magnitudes (integer→f32 is exact). The colour is an
/// opaque RGBA byte quad (a == 255) decoded by the fixed-function `Unorm8x4` path
/// as exactly `value/255.0` — the IDENTICAL IEEE-754 result `rgb4` used to compute,
/// so the rendered pixels stay byte-identical. `[u16;4]` (8 B) then `[u8;4]` (4 B)
/// pack with no padding (`#[repr(C)]`, 2-byte align): `size_of` == 12.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BgInstance {
    /// x, y, w, h in pixels (top-left origin, y down) — non-negative integers.
    rect: [u16; 4],
    /// r, g, b, a bytes (a == 255). Unorm8x4-decoded to the same `rgb4` floats.
    color: [u8; 4],
}

/// One glyph quad: a pixel-space dest rect, an atlas UV rect, a fg colour, and
/// the HOME-CELL background under it.
///
/// PACKED LAYOUT (40 B, was 48 B): ONLY the colours pack (`Unorm8x4`, exact
/// `value/255.0`). The rect and UV STAY `Float32x4` and MUST NOT pack: the glyph
/// rect's `gx0`/`gy0` can be NEGATIVE (font bearings, DEC double-size scaling) so
/// `Uint16` would corrupt them, and `Unorm16x4` UV quantization (`k/65535`) can
/// cross a texel-sample boundary at glyph edges — neither is byte-identical.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GlyphInstance {
    /// dest x, y, w, h in pixels (may be negative — bearings / DEC scaling).
    rect: [f32; 4],
    /// atlas u0, v0, du, dv in [0, 1].
    uv: [f32; 4],
    /// fg r, g, b, a bytes (a unused; coverage supplies alpha). Unorm8x4-decoded.
    color: [u8; 4],
    /// W2: the colour painted UNDER this quad (cell bg / selection band /
    /// cursor fill) — the luminance operand of `fs_glyph`'s linear-corrected
    /// coverage remap. Overhang texels reuse the HOME cell's bg (the CPU uses
    /// the true dst pixel there; divergence stays inside the AA parity
    /// tolerance, cell interiors are exact by construction). `[0; 4]` on the
    /// streams whose shaders ignore it (emoji / image / deco / scene).
    bg: [u8; 4],
}

// Lock in the packed strides (no `#[repr(C)]` padding surprises): the GPU
// `array_stride` is `size_of` of these, so a regression here would silently
// change the per-instance bandwidth. BgInstance: u16x4 (8) + u8x4 (4) = 12.
// GlyphInstance: f32x4 (16) + f32x4 (16) + u8x4 (4) + u8x4 (4) = 40.
const _: () = {
    assert!(std::mem::size_of::<BgInstance>() == 12);
    assert!(std::mem::size_of::<GlyphInstance>() == 40);
};

const BG_ATTRS: [wgpu::VertexAttribute; 2] = wgpu::vertex_attr_array![0 => Uint16x4, 1 => Unorm8x4];
const GLYPH_ATTRS: [wgpu::VertexAttribute; 4] =
    wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4, 2 => Unorm8x4, 3 => Unorm8x4];

/// PHOSPHOR rain bright-head halo instance: a [`BgInstance`]-shaped premultiplied
/// One/One quad PLUS the elliptical-falloff basis `(cx, cy, rx, ry)` in WINDOW
/// pixels. `fs_rain_glow` recomputes the SAME integer per-pixel weight the CPU
/// `draw_rain_add` uses, so the radial bloom stays byte-exact across backends.
/// `[u16;4] (8) + [u8;4] (4) + [u16;4] (8)` pack with no padding: `size_of` == 20.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct RainGlowInstance {
    /// x, y, w, h in pixels (top-left origin, y down) — the covered rect.
    rect: [u16; 4],
    /// r, g, b, a bytes (a == 255) — the PEAK premultiplied light.
    color: [u8; 4],
    /// cx, cy (window-pixel halo centre), rx, ry (falloff half-extents).
    falloff: [u16; 4],
}
const _: () = assert!(std::mem::size_of::<RainGlowInstance>() == 20);
const RAIN_GLOW_ATTRS: [wgpu::VertexAttribute; 3] =
    wgpu::vertex_attr_array![0 => Uint16x4, 1 => Unorm8x4, 2 => Uint16x4];

/// EMBERFORGE FirePatch instance: the covered rect PLUS the shared
/// pure-integer fire-field parameters, all in WINDOW pixels (the exact
/// operands the CPU `draw_fire_patch` feeds `aterm_render::fire_field`).
/// `fs_fire_add`/`fs_fire_over` recompute the field OP-FOR-OP per fragment,
/// so the flame art stays byte-exact across backends (the fire-parity
/// contract — `RainHalo`'s falloff trick at full art scale).
/// `[u16;4] (8) + [u16;4] (8) + u32 (4) + [u8;4] (4)` pack: `size_of` == 24.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FireInstance {
    /// x, y, w, h in pixels (top-left origin, y down) — the covered rect.
    rect: [u16; 4],
    /// base_y (window-px flame root), peak_h, cell_h, 0 (reserved).
    geo: [u16; 4],
    /// Churn phase in 1/1024 s (producer-quantized; identical on the CPU).
    phase: u32,
    /// temp, strength, cov_cap, lean (i8 bit pattern) bytes.
    tsl: [u8; 4],
}
const _: () = assert!(std::mem::size_of::<FireInstance>() == 24);
const FIRE_ATTRS: [wgpu::VertexAttribute; 4] =
    wgpu::vertex_attr_array![0 => Uint16x4, 1 => Uint16x4, 2 => Uint32, 3 => Uint8x4];

const SHADER: &str = r#"
// text_blend != 0.0 => fs_glyph applies the W2 linear-corrected coverage remap.
struct Uniforms { screen: vec2<f32>, text_blend: f32, pad: f32 };
@group(0) @binding(0) var<uniform> u: Uniforms;

// Unit quad corner for vertex index 0..6 (two CCW triangles).
fn corner(vi: u32) -> vec2<f32> {
    var c = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0), vec2<f32>(0.0, 1.0)
    );
    return c[vi];
}

// Pixel coords (top-left origin, y down) -> clip space (y up, row 0 at top).
fn to_ndc(px: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(2.0 * px.x / u.screen.x - 1.0, 1.0 - 2.0 * px.y / u.screen.y);
}

// sRGB-encoded channel -> linear-light (the proper piecewise curve). The base
// (OVER/REPLACE) passes render into an sRGB-typed target that re-encodes on store,
// so emitting linear here makes fixed-function blending composite in linear light,
// matching the CPU `blend`. Alpha/coverage are NOT sRGB and pass through. (The
// ADDITIVE glow/deco-add passes render to the Unorm view and must NOT decode.)
fn s2l(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(lo, hi, c > vec3<f32>(0.04045));
}

// Scalar sRGB decode/encode for the W2 corrected-alpha remap — the SAME
// piecewise curves (and thresholds) as the CPU `srgb_to_linear`/
// `linear_to_srgb`, so both backends remap coverage float-for-float.
fn s2l_s(c: f32) -> f32 {
    if (c <= 0.04045) { return c / 12.92; }
    return pow((c + 0.055) / 1.055, 2.4);
}
fn l2s_s(l: f32) -> f32 {
    if (l <= 0.0031308) { return 12.92 * l; }
    return 1.055 * pow(l, 1.0 / 2.4) - 0.055;
}

struct BgVsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_bg(@builtin(vertex_index) vi: u32,
         @location(0) rect_u: vec4<u32>,
         @location(1) color: vec4<f32>) -> BgVsOut {
    // Uint16x4 arrives as vec4<u32>; integer pixel coords -> exact f32 (no loss).
    let rect = vec4<f32>(rect_u);
    let k = corner(vi);
    let px = rect.xy + k * rect.zw;
    var o: BgVsOut;
    o.pos = vec4<f32>(to_ndc(px), 0.0, 1.0);
    o.color = color;
    return o;
}

@fragment
fn fs_bg(in: BgVsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(s2l(in.color.rgb), in.color.a);
}

// Glow (LUMEN aurora, One/One additive) emits its premultiplied colour RAW (no sRGB
// decode): over the offscreen's default view the add is byte-exact == the CPU `add_sat`
// on native, and lands in linear on downlevel (the accepted approximation — see the
// header DOWNLEVEL FALLBACK). Same vertex path as `fs_bg`, different fragment.
@fragment
fn fs_glow(in: BgVsOut) -> @location(0) vec4<f32> {
    return in.color;
}

// PHOSPHOR rain bright-head halo (One/One additive, RAW like fs_glow) with an
// ELLIPTICAL RADIAL FALLOFF. The weight is pure INTEGER math on window-pixel
// coords so it is byte-for-byte identical to the CPU `draw_rain_add`.
struct RainVsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) @interpolate(flat) fall: vec4<u32>,   // cx, cy, rx, ry (window px)
};

@vertex
fn vs_rain_glow(@builtin(vertex_index) vi: u32,
                @location(0) rect_u: vec4<u32>,
                @location(1) color: vec4<f32>,
                @location(2) fall_u: vec4<u32>) -> RainVsOut {
    let rect = vec4<f32>(rect_u);
    let k = corner(vi);
    let px = rect.xy + k * rect.zw;
    var o: RainVsOut;
    o.pos = vec4<f32>(to_ndc(px), 0.0, 1.0);
    o.color = color;
    o.fall = fall_u;
    return o;
}

@fragment
fn fs_rain_glow(in: RainVsOut) -> @location(0) vec4<f32> {
    // Frag pos is the pixel CENTRE (px + 0.5); floor -> the integer pixel index
    // the CPU walks. All arithmetic below mirrors `draw_rain_add` exactly.
    let px = i32(floor(in.pos.x));
    let py = i32(floor(in.pos.y));
    let cx = i32(in.fall.x);
    let cy = i32(in.fall.y);
    let rx = i32(in.fall.z);
    let ry = i32(in.fall.w);
    let dx = px - cx;
    let dy = py - cy;
    let nsq = (dx * dx * 256) / (rx * rx) + (dy * dy * 256) / (ry * ry);
    var wt = 256 - nsq;
    if (wt < 0) { wt = 0; }
    wt = (wt * wt) / 256;
    if (wt > 255) { wt = 255; }
    // mul8(premul_channel, wt) == (c*wt + 127)/255 — matches CPU `premul_rgb`.
    let cr = i32(round(in.color.r * 255.0));
    let cg = i32(round(in.color.g * 255.0));
    let cb = i32(round(in.color.b * 255.0));
    let r = f32((cr * wt + 127) / 255) / 255.0;
    let g = f32((cg * wt + 127) / 255) / 255.0;
    let b = f32((cb * wt + 127) / 255) / 255.0;
    return vec4<f32>(r, g, b, 1.0);
}

// HaloMode::Over radial VEIL (RAW like fs_rain_glow — same Unorm view): the
// SAME integer falloff weight, but emitted as the STRAIGHT (unpremultiplied)
// colour with the weight as the per-pixel ALPHA into the deco source-over
// blend state (SrcAlpha/OneMinusSrcAlpha):
//   out = rgb·(wt/255) + dst·(1 − wt/255)
// — the float twin of the CPU `over_rgb(dst, rgb, wt)` round-half integer
// composite. The fixed function rounds ONCE on store; exact ties are
// impossible (255k + 127.5 is never an integer), so the veil is byte-exact
// CPU==GPU wherever the One/One adds are (native Unorm offscreen).
@fragment
fn fs_rain_glow_over(in: RainVsOut) -> @location(0) vec4<f32> {
    let px = i32(floor(in.pos.x));
    let py = i32(floor(in.pos.y));
    let cx = i32(in.fall.x);
    let cy = i32(in.fall.y);
    let rx = i32(in.fall.z);
    let ry = i32(in.fall.w);
    let dx = px - cx;
    let dy = py - cy;
    let nsq = (dx * dx * 256) / (rx * rx) + (dy * dy * 256) / (ry * ry);
    var wt = 256 - nsq;
    if (wt < 0) { wt = 0; }
    wt = (wt * wt) / 256;
    if (wt > 255) { wt = 255; }
    // Per-pixel alpha CEILING (== CPU `wt.min(aterm_render::halo_over_cap)`):
    // the veil's cap rides the instance colour's ALPHA byte; 0 == uncapped
    // (255), so every historical veil (packed a == 0) is byte-identical.
    var cap = i32(round(in.color.a * 255.0));
    if (cap == 0) { cap = 255; }
    if (wt > cap) { wt = cap; }
    return vec4<f32>(in.color.rgb, f32(wt) / 255.0);
}

// ============================ EMBERFORGE FIRE ============================
// The WGSL twin of `aterm_render::fire_field` — OP-FOR-OP: every add, mul,
// shift, division and clamp below mirrors the Rust module exactly (pure
// i32/u32 math, wrapping u32 semantics), so the CPU rasterizer's pixel and
// this fragment's byte agree everywhere (the fire-parity contract). Any
// change here MUST be mirrored in fire_field.rs (and vice versa) — the
// fire_patch_parity differential is the referee.

fn fire_hash(x: u32, y: u32, seed: u32) -> u32 {
    var h = x * 0x9E3779B1u + y * 0x85EBCA77u + seed * 0xC2B2AE3Du;
    h ^= h >> 15u;
    h = h * 0x2C1B3C6Du;
    h ^= h >> 12u;
    h = h * 0x297A2D39u;
    h ^= h >> 15u;
    return h;
}

fn fire_fade(t: i32) -> i32 {
    return (t * t * (768 - 2 * t)) >> 16u;
}

fn fire_vnoise(x: u32, y: u32, ymask: u32, seed: u32) -> i32 {
    let ix = x >> 8u;
    let iy0 = (y >> 8u) & ymask;
    let iy1 = (iy0 + 1u) & ymask;
    let ix1 = ix + 1u;
    let fx = i32(x & 255u);
    let fy = i32(y & 255u);
    let n00 = i32(fire_hash(ix, iy0, seed) >> 24u);
    let n10 = i32(fire_hash(ix1, iy0, seed) >> 24u);
    let n01 = i32(fire_hash(ix, iy1, seed) >> 24u);
    let n11 = i32(fire_hash(ix1, iy1, seed) >> 24u);
    let ux = fire_fade(fx);
    let uy = fire_fade(fy);
    let a = n00 * (256 - ux) + n10 * ux;
    let b = n01 * (256 - ux) + n11 * ux;
    return (a * (256 - uy) + b * uy) >> 16u;
}

struct FireCoreOut {
    idx: i32,
    q: i32,
    edge: i32,
    body: i32,
    root: i32,
    rim: i32,
};

fn fire_core(px: i32, py: i32, base_y: i32, peak_h: i32, phase: u32,
             temp: i32, strength: i32, lean: i32, cell_h: i32) -> FireCoreOut {
    var o: FireCoreOut;
    o.idx = 0; o.q = 256; o.edge = 0; o.body = 0; o.root = 0; o.rim = 0;
    let ch = max(cell_h, 2);
    let chu = u32(ch);
    let peak = clamp(peak_h, 1, 2048);
    // ROOT SKIRT (CPU twin: fire_field fire_core_px): below the root the
    // envelope mirrors, compressed 5x — the dense base dissolves through the
    // glyph row's top instead of a hard cut.
    var v = base_y - py;
    if (v < 0) { v = -v * 5; }
    let vn = min((v * 256) / peak, 512);
    let shear = lean * vn / 4;
    let xq = bitcast<u32>(px * 256 + shear + 4194304);
    let sx = (xq * 4u) / (5u * chu);
    let ts = u32(300 + temp * 2);
    let tr = (phase * ts) >> 10u;
    let n0 = fire_vnoise(sx, tr, 0x3FFFu, 0x51F0A3B7u);
    let n1 = fire_vnoise(sx * 2u + 12799u, tr * 2u + 37199u, 0x7FFFu, 0x9D2C5680u);
    let n = (n0 * 9 + n1 * 7) >> 4u;
    let ridge = 255 - abs(2 * n - 255);
    let hs2 = (ridge * ridge) >> 8u;
    let hshape = (hs2 * (256 + ridge)) >> 9u;
    let hq = 30 + ((hshape * 260) >> 8u);
    let hcol = (peak * hq * strength) / 255;
    let vv = v * 256;
    let d = hcol - vv;
    if (d <= 0) { return o; }
    let aa = max(ch / 4, 3) * 256;
    let edge = min((d * 256) / aa, 256);
    let rim = clamp((2 * aa - d) * 256 / aa, 0, 256);
    let q = (vv * 256) / hcol;
    let rs = u32(350 + temp);
    let offr = (phase * rs) >> 10u;
    let sy = (u32(vv) * 2u) / (3u * chu);
    let by = sy - offr;
    let m0 = fire_vnoise(sx * 3u / 2u + 5023u, by, 0x3FFFu, 0xB5297A4Du);
    let m1 = fire_vnoise(sx * 3u + 9531u, by * 2u + 15913u, 0x7FFFu, 0x68E31DA4u);
    let m2 = fire_vnoise(sx * 6u + 26251u, by * 4u + 37633u, 0xFFFFu, 0x1B56C4E9u);
    let body0 = (m0 * 4 + m1 * 3 + m2) >> 3u;
    let body = 128 + (((body0 - 128) * edge) >> 8u);
    let heat = ((256 - q) * (112 + ((temp * 120) >> 8u))) >> 6u;
    let idx0 = clamp((heat * (150 + ((body * 212) >> 8u))) >> 8u, 0, 850);
    let root = min((v * 1536) / ch, 256);
    o.idx = (idx0 * (192 + (root >> 2u))) >> 8u;
    o.q = q;
    o.edge = edge;
    o.body = body;
    o.root = root;
    o.rim = rim;
    return o;
}

fn fire_pal_add(idx: i32) -> vec3<i32> {
    var pal = array<vec3<i32>, 5>(
        vec3<i32>(42, 0, 0), vec3<i32>(139, 26, 0), vec3<i32>(224, 74, 0),
        vec3<i32>(255, 176, 32), vec3<i32>(255, 240, 192));
    let seg = clamp(idx >> 8u, 0, 3);
    let f = idx - (seg << 8u);
    let a = pal[seg];
    let b = pal[seg + 1];
    return vec3<i32>((a.x * (256 - f) + b.x * f) >> 8u,
                     (a.y * (256 - f) + b.y * f) >> 8u,
                     (a.z * (256 - f) + b.z * f) >> 8u);
}

fn fire_pal_over(idx: i32) -> vec3<i32> {
    var pal = array<vec3<i32>, 5>(
        vec3<i32>(102, 24, 4), vec3<i32>(168, 40, 0), vec3<i32>(208, 70, 0),
        vec3<i32>(238, 110, 0), vec3<i32>(255, 166, 30));
    let seg = clamp(idx >> 8u, 0, 3);
    let f = idx - (seg << 8u);
    let a = pal[seg];
    let b = pal[seg + 1];
    return vec3<i32>((a.x * (256 - f) + b.x * f) >> 8u,
                     (a.y * (256 - f) + b.y * f) >> 8u,
                     (a.z * (256 - f) + b.z * f) >> 8u);
}

struct FireVsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) @interpolate(flat) geo: vec4<u32>,   // base_y, peak_h, cell_h, 0
    @location(1) @interpolate(flat) phase: u32,
    @location(2) @interpolate(flat) tsl: vec4<u32>,   // temp, strength, cov_cap, lean-bits
};

@vertex
fn vs_fire(@builtin(vertex_index) vi: u32,
           @location(0) rect_u: vec4<u32>,
           @location(1) geo: vec4<u32>,
           @location(2) phase: u32,
           @location(3) tsl: vec4<u32>) -> FireVsOut {
    let rect = vec4<f32>(rect_u);
    let k = corner(vi);
    let px = rect.xy + k * rect.zw;
    var o: FireVsOut;
    o.pos = vec4<f32>(to_ndc(px), 0.0, 1.0);
    o.geo = geo;
    o.phase = phase;
    o.tsl = tsl;
    return o;
}

// FireMode::Add — premultiplied field light on the One/One Unorm view:
// (palette·cov + 127)/255 per channel is the SINGLE rounding point, the exact
// CPU `fire_field_add` bytes, so the add is byte-exact (the fs_rain_glow
// argument).
@fragment
fn fs_fire_add(in: FireVsOut) -> @location(0) vec4<f32> {
    let px = i32(floor(in.pos.x));
    let py = i32(floor(in.pos.y));
    let lean = bitcast<i32>(in.tsl.w << 24u) >> 24u;
    let temp = i32(in.tsl.x);
    let c = fire_core(px, py, i32(in.geo.x), i32(in.geo.y), in.phase,
                      temp, i32(in.tsl.y), lean, i32(in.geo.z));
    // Coverage law == fire_cov_add: AA edge x temp density x pockets x root.
    let dens = 150 + ((temp * 106) >> 8u);
    let bodyc = 110 + ((c.body * 146) >> 8u);
    let cov = min((((((c.edge * dens) >> 8u) * bodyc) >> 8u) * c.root) >> 8u,
                  i32(in.tsl.z));
    // TOP-EDGE FADE == CPU fire_top_fade: dissolve toward the grid top (geo.w).
    let fade_px = max(i32(in.geo.z) * 2, 2);
    let ttop = clamp(((py - i32(in.geo.w)) * 255) / fade_px, 0, 255);
    let covf = (cov * fire_fade(ttop)) / 255;
    // Rim cooling: outline drops toward deep red, the core stays hot.
    let idx = (c.idx * (256 - ((c.rim * 112) >> 8u))) >> 8u;
    let rgb = fire_pal_add(idx);
    let r = f32((rgb.x * covf + 127) / 255) / 255.0;
    let g = f32((rgb.y * covf + 127) / 255) / 255.0;
    let b = f32((rgb.z * covf + 127) / 255) / 255.0;
    return vec4<f32>(r, g, b, 1.0);
}

// FireMode::Over — straight ink + field alpha through the deco source-over
// blend state on the same Unorm view (the fs_rain_glow_over byte-exactness
// argument: one rounding on store, no exact ties) == CPU `over_rgb` of
// `fire_field_over`.
@fragment
fn fs_fire_over(in: FireVsOut) -> @location(0) vec4<f32> {
    let px = i32(floor(in.pos.x));
    let py = i32(floor(in.pos.y));
    let lean = bitcast<i32>(in.tsl.w << 24u) >> 24u;
    let temp = i32(in.tsl.x);
    let c = fire_core(px, py, i32(in.geo.x), i32(in.geo.y), in.phase,
                      temp, i32(in.tsl.y), lean, i32(in.geo.z));
    // Alpha law == fire_alpha_over: pooled rims, dense root, wispy tips.
    let bodyc = 130 + ((c.body * 166) >> 8u);
    let tipf = 120 + (((256 - c.q) * 136) >> 8u);
    let pool = 256 + ((c.rim * 96) >> 8u);
    let a = min((((((((c.edge * bodyc) >> 8u) * c.root) >> 8u) * tipf) >> 8u) * pool) >> 8u,
                i32(in.tsl.z));
    // TOP-EDGE FADE == CPU fire_top_fade: dissolve toward the grid top (geo.w).
    let fade_px = max(i32(in.geo.z) * 2, 2);
    let ttop = clamp(((py - i32(in.geo.w)) * 255) / fade_px, 0, 255);
    let af = (a * fire_fade(ttop)) / 255;
    // Rim darkening: ink pools at the outline (the watercolor edge law).
    let idx = (c.idx * (256 - ((c.rim * 128) >> 8u))) >> 8u;
    let rgb = fire_pal_over(idx);
    return vec4<f32>(f32(rgb.x) / 255.0, f32(rgb.y) / 255.0, f32(rgb.z) / 255.0,
                     f32(af) / 255.0);
}

@group(1) @binding(0) var atlas_tex: texture_2d<f32>;
@group(1) @binding(1) var atlas_samp: sampler;

struct GlyphVsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) bg: vec4<f32>,
};

@vertex
fn vs_glyph(@builtin(vertex_index) vi: u32,
            @location(0) rect: vec4<f32>,
            @location(1) uv: vec4<f32>,
            @location(2) color: vec4<f32>,
            @location(3) bg: vec4<f32>) -> GlyphVsOut {
    let k = corner(vi);
    let px = rect.xy + k * rect.zw;
    var o: GlyphVsOut;
    o.pos = vec4<f32>(to_ndc(px), 0.0, 1.0);
    o.uv = uv.xy + k * uv.zw;
    o.color = color;
    o.bg = bg;
    return o;
}

// TEXT glyphs. W2 (`u.text_blend != 0.0`, the linear-corrected default): remap
// the coverage BEFORE the fixed-function sRGB-view blend so the blended
// LUMINANCE lands where a gamma-space blend would put it — ghostty's
// perceptual weight compensation, the float twin of the CPU
// `blend_text(.., corrected=true)`:
//   blend_l = s2l(l2s(fg_l)*cov + l2s(bg_l)*(1-cov))
//   cov'    = clamp((blend_l - bg_l) / (fg_l - bg_l), 0, 1)
// gated EXACTLY like the CPU `correction_applies`: interior coverage only
// (endpoints stay byte-exact) and a non-degenerate luminance gap only (the
// 0.001 literal == aterm_render::TEXT_BLEND_EPS). `in.bg` is the HOME cell's
// bg; the CPU uses the true dst pixel, which is identical on cell interiors
// and within the AA parity tolerance on overhangs.
@fragment
fn fs_glyph(in: GlyphVsOut) -> @location(0) vec4<f32> {
    var cov = textureSample(atlas_tex, atlas_samp, in.uv).r;
    let fg_lin = s2l(in.color.rgb);
    if (u.text_blend != 0.0 && cov > 0.0 && cov < 1.0) {
        let lum = vec3<f32>(0.2126, 0.7152, 0.0722);
        let fg_l = dot(fg_lin, lum);
        let bg_l = dot(s2l(in.bg.rgb), lum);
        if (abs(fg_l - bg_l) >= 0.001) {
            let blend_l = s2l_s(l2s_s(fg_l) * cov + l2s_s(bg_l) * (1.0 - cov));
            cov = clamp((blend_l - bg_l) / (fg_l - bg_l), 0.0, 1.0);
        }
    }
    return vec4<f32>(fg_lin, cov);
}

// Colour-emoji glyphs: the atlas (an RGBA8 texture bound in the SAME group-1
// slot) already holds the CPU renderer's final, cell-sized RGBA pixels, so we
// blit them straight through. ALPHA_BLENDING then does `rgb*a + dst*(1-a)` —
// byte-for-byte the CPU `blend(dst, rgb, a)` for the Rgba blit. The vertex
// `color` is ignored (the emoji carries its own colour).
@fragment
fn fs_glyph_color(in: GlyphVsOut) -> @location(0) vec4<f32> {
    let c = textureSample(atlas_tex, atlas_samp, in.uv);
    return vec4<f32>(s2l(c.rgb), c.a);
}

// Sparkle-word decorations sample a coverage sprite from the deco atlas and use
// the per-instance `color.a` as an opacity multiplier (text glyphs leave a unused
// at 1.0; these set it to the decoration's alpha). The effective coverage is
// `cov * color.a` — the float twin of the CPU `(cov * alpha + 127) / 255`.
//
// OVER (the feline cat-paw): output (rgb, a) into ALPHA_BLENDING ⇒
//   dst*(1-a) + rgb*a  == the CPU `blend(dst, color, a)`.
@fragment
fn fs_deco_over(in: GlyphVsOut) -> @location(0) vec4<f32> {
    let cov = textureSample(atlas_tex, atlas_samp, in.uv).r;
    let a = cov * in.color.a;
    return vec4<f32>(s2l(in.color.rgb), a);
}

// ADD (the profanity sparkle): output PREMULTIPLIED (rgb*a, a) into the One/One
// additive pipeline ⇒ dst + rgb*a == the CPU `add_sat(dst, premul_rgb(color, a))`.
@fragment
fn fs_deco_add(in: GlyphVsOut) -> @location(0) vec4<f32> {
    let cov = textureSample(atlas_tex, atlas_samp, in.uv).r;
    let a = cov * in.color.a;
    return vec4<f32>(in.color.rgb * a, a);
}

// SCENE ("Living Panels") sprites sample an RGBA8 scene atlas (the meadow/cat art).
// The per-instance `color.rgb` is a MULTIPLY tint (one grayscale cat → any fur colour)
// and `color.a` an opacity multiplier. OVER (the world): output (tint*atlas.rgb, a)
// into ALPHA_BLENDING on the sRGB view ⇒ dst*(1-a)+rgb*a == the CPU pass-1c sprite stamp.
@fragment
// The tint multiply and the opacity multiply are QUANTIZED to 8 bits with
// round-half BEFORE the sRGB decode, exactly reproducing the CPU stamp's
// intermediate byte: the cat/free path's mul8(c,f) = (c*f+127)/255 and the
// bilinear scene path's (t*tint*255 + 0.5) as u8. round(x255*y255/255) equals
// mul8 in the float domain (c = x255/255, f = y255/255), so the tinted colour
// lands on the SAME 8-bit lattice the CPU linearizes and the linear-light
// composite that follows is parity-exact rather than diverging by the ~1 LSB
// the un-quantized f32 product injected on mid-ramp tints (the design 9 tint
// divergence). At the cat's identity tint (0xFFFFFF, a=255) the quantization
// is a no-op (round(c*1*255)/255 == c), so cat parity is unchanged; the
// bilinear scene/free paths already quantize CPU-side, so this matches them.
fn fs_scene_over(in: GlyphVsOut) -> @location(0) vec4<f32> {
    let c = textureSample(atlas_tex, atlas_samp, in.uv);
    let tinted = round(c.rgb * in.color.rgb * 255.0) / 255.0;
    let a = round(c.a * in.color.a * 255.0) / 255.0;
    return vec4<f32>(s2l(tinted), a);
}

// SCENE additive light (sun/fireflies/comet): premultiplied One/One on the Unorm view,
// RAW like the glow ⇒ dst + (tint*atlas.rgb)*a == the CPU add_sat(dst, premul).
@fragment
fn fs_scene_add(in: GlyphVsOut) -> @location(0) vec4<f32> {
    let c = textureSample(atlas_tex, atlas_samp, in.uv);
    let a = c.a * in.color.a;
    return vec4<f32>(c.rgb * in.color.rgb * a, a);
}
"#;

/// On-glass blit: a fullscreen triangle generated from `@builtin(vertex_index)`
/// (3 verts, no vertex buffer) samples the offscreen frame with NEAREST (1:1,
/// no smear) and writes it straight to the swapchain. When `invert.flag != 0`
/// the RGB is inverted (`1.0 - rgb`) for the visual-bell flash — the GPU twin of
/// the CPU softbuffer `px ^ 0x00ffffff`.
const BLIT_SHADER: &str = r#"
struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// Oversized triangle covering the whole clip rect; UVs map the framebuffer 1:1.
@vertex
fn vs_blit(@builtin(vertex_index) vi: u32) -> VsOut {
    var uv = array<vec2<f32>, 3>(
        vec2<f32>(0.0, 0.0), vec2<f32>(2.0, 0.0), vec2<f32>(0.0, 2.0)
    );
    var o: VsOut;
    o.uv = uv[vi];
    // uv (0..2, y down) -> clip (x: -1..3, y: 1..-3).
    o.pos = vec4<f32>(o.uv.x * 2.0 - 1.0, 1.0 - o.uv.y * 2.0, 0.0, 1.0);
    return o;
}

@group(0) @binding(0) var src_tex: texture_2d<f32>;
@group(0) @binding(1) var src_samp: sampler;
// std140 layout — must match the Rust `BlitUniform` byte-for-byte (96 bytes).
struct Blit {
    flag: u32,        // bell-flash invert
    overlay: u32,     // drop-target highlight enabled
    border_px: f32,   // inset border thickness, device px
    encode_srgb: f32, // !=0: re-encode linear->sRGB (downlevel WebGL2 blit)
    accent: vec4<f32>,// overlay accent rgb (a unused), normalized 0..1
    dims: vec2<f32>,  // OFFSCREEN frame width,height in px
    wash_a: f32,      // interior wash alpha 0..1
    border_a: f32,    // border alpha 0..1
    band: vec4<f32>,  // W1 remainder-band colour (live terminal bg; a unused)
    content_off: vec2<f32>, // W1: frame top-left inside the swapchain, device px
    hdr: f32,         // M3: !=0: decode sRGB->linear, clamp <=1 (f16 EDR swapchain)
    translucent: f32, // M5: !=0: emit the offscreen/band ALPHA (translucent glass) instead of forcing 1.0
    sdr_white_scale: f32, // M3 (Windows scRGB): reference-white scale (SDRwhite/80); 1.0 on macOS/SDR
    visible_y: f32,   // first source row exposed by the frontend crop
    visible_h: f32,   // exposed source height; rows outside are remainder bands
    _p2: f32,
};
@group(0) @binding(2) var<uniform> b: Blit;

// Linear-light channel -> sRGB (the standard piecewise encode). Used only on the
// downlevel blit, which samples an sRGB-typed offscreen view (auto-decoded to linear)
// and must re-encode before writing to the non-sRGB swapchain.
fn l2s(c: f32) -> f32 {
    let cc = clamp(c, 0.0, 1.0);
    if (cc <= 0.0031308) { return 12.92 * cc; }
    return 1.055 * pow(cc, 1.0 / 2.4) - 0.055;
}

// M3 (EDR present): sRGB-encoded rgb -> linear light, CLAMPED to [0,1] — the
// GRID CLAMP LAW (aterm_render::hdr::hdr_grid_encode is the proven float twin):
// on the Rgba16Float extended-linear-sRGB swapchain the whole blit stream stays
// reference-white SDR; only the separate additive aurora pass may exceed 1.0.
// Same piecewise constants as the cell shader's s2l.
fn hdr_grid_encode3(c: vec3<f32>) -> vec3<f32> {
    let cc = clamp(c, vec3<f32>(0.0), vec3<f32>(1.0));
    let lo = cc / 12.92;
    let hi = pow((cc + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return clamp(select(lo, hi, cc > vec3<f32>(0.04045)), vec3<f32>(0.0), vec3<f32>(1.0));
}

@fragment
fn fs_blit(in: VsOut) -> @location(0) vec4<f32> {
    // W1 (kill the compositor stretch): the swapchain is the RAW window size;
    // the offscreen frame sits at `content_off` (the centred remainder split,
    // `aterm_render::band_offset`) and the surrounding bands are painted the
    // live terminal background — never sampled, never inverted, never washed
    // (they are chrome, matching the CPU `place_frame_bands` twin). `in.pos.xy` is the
    // swapchain pixel CENTER (x+0.5, y+0.5), so `p` floors to exact frame
    // texel coords: `textureLoad` is a 1:1 texel fetch — zero scaling ever.
    // At exact fit (`content_off == 0`, dst dims == frame dims) every pixel is
    // in-bounds and this is byte-identical to the historical NEAREST blit.
    let p = in.pos.xy - b.content_off;
    let visible_y1 = b.visible_y + b.visible_h;
    if (p.x < 0.0 || p.y < b.visible_y || p.x >= b.dims.x || p.y >= visible_y1) {
        // M3: the bands are chrome in the SAME stream as the frame — on the EDR
        // swapchain they take the same linear decode (and the same <=1.0 clamp).
        // M5: on a translucent window the remainder bands take the bg-quad alpha
        // (`b.band.a`) too, so the whole window reads as one sheet of glass; the
        // opaque default forces 1.0 (byte-identical chrome).
        let band_a = select(1.0, b.band.a, b.translucent != 0.0);
        if (b.hdr != 0.0) {
            return vec4<f32>(hdr_grid_encode3(b.band.rgb), band_a);
        }
        return vec4<f32>(b.band.rgb, band_a);
    }
    let c = textureLoad(src_tex, vec2<i32>(p), 0);
    var rgb = c.rgb;
    if (b.flag != 0u) {
        rgb = vec3<f32>(1.0) - rgb;
    }
    // Drag-and-drop drop-target highlight: faint accent wash + inset accent
    // border, relative to the CONTENT frame (`p` is the frame-local device
    // pixel). Gated on `overlay` so a normal present is byte-identical.
    if (b.overlay != 0u) {
        let visible_p = vec2<f32>(p.x, p.y - b.visible_y);
        let edge = min(
            min(visible_p.x, b.dims.x - visible_p.x),
            min(visible_p.y, b.visible_h - visible_p.y)
        );
        var a = b.wash_a;
        if (edge < b.border_px) { a = b.border_a; }
        rgb = mix(rgb, b.accent.rgb, a);
    }
    // Downlevel: the sampled value was auto-decoded sRGB->linear; re-encode for the
    // non-sRGB swapchain so the on-screen bytes match the readback (+ the CPU).
    if (b.encode_srgb != 0.0) {
        rgb = vec3<f32>(l2s(rgb.r), l2s(rgb.g), l2s(rgb.b));
    }
    // M3 (EDR present, native-only so mutually exclusive with encode_srgb): the
    // f16 swapchain is TAGGED extended-linear-sRGB, so decode the sRGB bytes to
    // linear light — clamped <= 1.0 (grid clamp law), applied AFTER the
    // gamma-space invert/overlay so the HDR frame is exactly the linear reading
    // of the SDR frame (identical on glass at SDR, per ColorSync).
    if (b.hdr != 0.0) {
        // Windows scRGB: scale reference-white to the SDR-white level (scRGB 1.0 == 80
        // nits fixed, desktop white is the user's setting) so the grid is not dim.
        // b.sdr_white_scale is 1.0 on macOS / SDR, so this is byte-identical there.
        rgb = hdr_grid_encode3(rgb) * b.sdr_white_scale;
    }
    // M5 true vibrancy: on a translucent window emit the offscreen alpha — the bg
    // quads carry `bg_quad_alpha(opacity)` and ink accumulated to opaque through
    // the base passes' OVER alpha blend, so the PostMultiplied swapchain composites
    // this STRAIGHT-alpha pixel over the window's NSVisualEffectView (bg turns to
    // blurred glass; text/decorations/images stay opaque). The opaque default
    // forces 1.0 — the byte-identical solid present.
    let out_a = select(1.0, c.a, b.translucent != 0.0);
    return vec4<f32>(rgb, out_a);
}
"#;

/// M3 phase B — the EDR AURORA pass: after the blit fills the `Rgba16Float`
/// extended-linear-sRGB swapchain with the (clamped ≤ 1.0) linear grid, this
/// pass re-emits the LUMEN glow quads ADDITIVELY (One/One) with linear values
/// ABOVE reference white — real light on an EDR panel. Per channel it is the
/// WGSL twin of the PROVEN `aterm_render::hdr::hdr_additive_encode`:
/// `min(s2l(colour) * boost, headroom)` with `headroom = sanitize(edr_max) - 1`
/// computed CPU-side, so the presented pixel is bounded by
/// `blit(≤1) + additive(≤headroom) ≤ edr_max` — the additive clamp law.
/// Same instance stream as the SDR aurora (`BgInstance` / `vs_bg` geometry),
/// with the swapchain-space uniform: quads are encoded in OFFSCREEN px, so the
/// vertex path adds the W1 `content_off` band placement before the NDC map.
const HDR_GLOW_SHADER: &str = r#"
struct HdrU {
    screen: vec2<f32>,      // SWAPCHAIN width,height in px (the NDC divisor)
    content_off: vec2<f32>, // W1 frame top-left inside the swapchain, device px
    boost: f32,             // linear emission boost (aterm_render::hdr::HDR_GLOW_BOOST)
    headroom: f32,          // sanitize(edr_max) - 1.0, >= 0 (proven CPU-side)
    _pad: vec2<f32>,
};
@group(0) @binding(0) var<uniform> hu: HdrU;

struct HdrVsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) color: vec4<f32>,
};

fn hdr_corner(vi: u32) -> vec2<f32> {
    var c = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0), vec2<f32>(0.0, 1.0)
    );
    return c[vi];
}

@vertex
fn vs_hdr_glow(@builtin(vertex_index) vi: u32,
               @location(0) rect_u: vec4<u32>,
               @location(1) color: vec4<f32>) -> HdrVsOut {
    let rect = vec4<f32>(rect_u);
    let k = hdr_corner(vi);
    // Offscreen px -> swapchain px (the W1 band placement) -> NDC.
    let px = rect.xy + k * rect.zw + hu.content_off;
    var o: HdrVsOut;
    o.pos = vec4<f32>(2.0 * px.x / hu.screen.x - 1.0, 1.0 - 2.0 * px.y / hu.screen.y, 0.0, 1.0);
    o.color = color;
    return o;
}

// Decode the premultiplied sRGB-space aurora colour to linear (same piecewise
// s2l as everywhere), boost, clamp to the headroom (never negative), and emit
// into the One/One add. COLOR write-mask: the blit's alpha stays 1.0.
@fragment
fn fs_hdr_glow(in: HdrVsOut) -> @location(0) vec4<f32> {
    let c = clamp(in.color.rgb, vec3<f32>(0.0), vec3<f32>(1.0));
    let lo = c / 12.92;
    let hi = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    let lin = select(lo, hi, c > vec3<f32>(0.04045));
    let bound = max(hu.headroom, 0.0);
    let add = max(min(lin * hu.boost, vec3<f32>(bound)), vec3<f32>(0.0));
    return vec4<f32>(add, 0.0);
}

// SDR twin of the boost (the swapchain-side crown on a NON-f16 present): scale
// the aurora colour by the BUDGET (hu.headroom carries it, <= 0.35 by the proven
// `sdr_glow_budget` bound; hu.boost is 1.0) and One/One-add RAW — the SDR
// swapchain is a non-sRGB Unorm view, so blending adds code values, the exact
// GPU twin of the CPU `add_sat` convention (no s2l decode). Scaling (not
// clamping) preserves the glow's gradient shape: peak = budget at full colour,
// no plateau. COLOR write-mask: the blit's alpha stays 1.0.
@fragment
fn fs_sdr_glow(in: HdrVsOut) -> @location(0) vec4<f32> {
    let c = clamp(in.color.rgb, vec3<f32>(0.0), vec3<f32>(1.0));
    let bound = max(hu.headroom, 0.0);
    return vec4<f32>(c * bound * max(hu.boost, 0.0), 0.0);
}
"#;

/// EDR aurora uniform — std140 32-byte layout matching WGSL `HdrU` (vec2 at 0
/// and 8, two f32 at 16/20, vec2 pad at 24). `screen` is the SWAPCHAIN size;
/// `content_off` the W1 frame placement; `boost`/`headroom` the proven
/// `aterm_render::hdr` emission parameters.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
struct HdrGlowUniform {
    screen: [f32; 2],
    content_off: [f32; 2],
    boost: f32,
    headroom: f32,
    _pad: [f32; 2],
}

/// Settings-card TRAY overlay: a single device-px-positioned, ALPHA-BLENDED quad
/// drawn OVER the blit, in the same present pass. Separate module from
/// `BLIT_SHADER` so its `@group(0)` bindings never collide with the blit's.
/// `rect` is device px (x,y,w,h, top-left origin, y-DOWN); `fb` is the framebuffer
/// size. NDC math matches the glyph path's `to_ndc`:
///   ndc_x = 2*px/fb_w - 1 ; ndc_y = 1 - 2*py/fb_h   (y-down px -> y-up clip).
/// The card RGBA is STRAIGHT (non-premultiplied) alpha, so ALPHA_BLENDING
/// (SrcAlpha / OneMinusSrcAlpha) reproduces the CPU `composite_tray` src-over.
const TRAY_SHADER: &str = r#"
struct TrayOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// std140 layout — must match the Rust `TrayUniform` byte-for-byte (32 bytes:
// vec4 at 0, vec2 at 16, vec2 pad at 24).
struct Tray {
    rect: vec4<f32>, // device-px x, y, w, h
    fb: vec2<f32>,   // framebuffer width, height in px
    pad: vec2<f32>,
};
@group(0) @binding(2) var<uniform> t: Tray;

// Unit quad as a 4-vert triangle-strip: (0,0) (1,0) (0,1) (1,1).
@vertex
fn vs_tray(@builtin(vertex_index) vi: u32) -> TrayOut {
    var corner = array<vec2<f32>, 4>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 1.0)
    );
    let k = corner[vi];
    let px = t.rect.xy + k * t.rect.zw;
    // y-down device px -> y-up NDC.
    let ndc = vec2<f32>(2.0 * px.x / t.fb.x - 1.0, 1.0 - 2.0 * px.y / t.fb.y);
    var o: TrayOut;
    o.pos = vec4<f32>(ndc, 0.0, 1.0);
    o.uv = k; // full-texture sample
    return o;
}

@group(0) @binding(0) var tray_tex: texture_2d<f32>;
@group(0) @binding(1) var tray_samp: sampler;

@fragment
fn fs_tray(in: TrayOut) -> @location(0) vec4<f32> {
    // Straight RGBA passthrough; ALPHA_BLENDING does the src-over composite.
    return textureSample(tray_tex, tray_samp, in.uv);
}
"#;

/// GPU-only cursor-comet BLOOM (the "more amazing on GPU" layer). The crisp comet
/// is rendered into a HALF-RES texture, gaussian-blurred, and additively composited
/// back over the offscreen — a soft radiant halo around the streak that host quads
/// can't cheaply do. This is a PRESENT-QUALITY embellishment LAYERED on top of the
/// (byte-parity-proven) base render: it only runs when `enable_bloom` is set, so the
/// CPU/GPU differential tests (which disable it) stay byte-exact.
///
/// `vs_fs` is the standard fullscreen triangle. `fs_bloom` does a separable-free
/// 5×5 gaussian over the half-res glow texture (cheap; the half-res + linear filter
/// already widen it) scaled by `strength`, returned for a One/One additive blend.
const BLOOM_SHADER: &str = r#"
struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_fs(@builtin(vertex_index) vi: u32) -> VsOut {
    var uv = array<vec2<f32>, 3>(
        vec2<f32>(0.0, 0.0), vec2<f32>(2.0, 0.0), vec2<f32>(0.0, 2.0)
    );
    var o: VsOut;
    o.uv = uv[vi];
    o.pos = vec4<f32>(o.uv.x * 2.0 - 1.0, 1.0 - o.uv.y * 2.0, 0.0, 1.0);
    return o;
}

@group(0) @binding(0) var bloom_src: texture_2d<f32>;
@group(0) @binding(1) var bloom_samp: sampler;
struct BloomU { texel: vec2<f32>, strength: f32, radius: f32 };
@group(0) @binding(2) var<uniform> bu: BloomU;

@fragment
fn fs_bloom(in: VsOut) -> @location(0) vec4<f32> {
    var sum = vec3<f32>(0.0, 0.0, 0.0);
    var wsum = 0.0;
    for (var j: i32 = -2; j <= 2; j = j + 1) {
        for (var i: i32 = -2; i <= 2; i = i + 1) {
            let off = vec2<f32>(f32(i), f32(j)) * bu.texel * bu.radius;
            let d2 = f32(i * i + j * j);
            let w = exp(-d2 / 4.0);
            sum = sum + textureSample(bloom_src, bloom_samp, in.uv + off).rgb * w;
            wsum = wsum + w;
        }
    }
    return vec4<f32>(sum / wsum * bu.strength, 1.0);
}
"#;

/// Bloom uniform — std140 16-byte layout matching WGSL `BloomU` (vec2 at 0, two
/// f32 at 8/12). `texel` is 1/half-res-dims; `strength`/`radius` tune the halo.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
struct BloomUniform {
    texel: [f32; 2],
    strength: f32,
    radius: f32,
}

/// GPU-only HEAT SHIMMER (the bloom parity class, present time only): the air
/// ABOVE burning cells refracts — a subtle per-pixel UV displacement of the
/// FINISHED frame (base + aurora + halo), exactly like heat haze over a road.
/// The hot region and per-column heat are derived host-side from the SAME
/// `cursor_glow_add` stream the bloom feeds on (zero new plumbing); the pass
/// samples a staged copy of the composed frame with a displaced UV whose
/// vertical ripple RISES with present time and fades with height above the hot
/// band. Like the bloom it is a documented parity-class exception: never in the
/// CPU/GPU byte differentials (they call [`GpuRenderer::set_shimmer`] with
/// `false`), ALWAYS in what introspection reads (offscreen readback, swapchain
/// video tap, virtual present), and absent on the CPU backend.
///
/// GUARDRAILS (the honesty contract's mechanical half):
/// * the displacement magnitude is `<= amp` BY CONSTRUCTION (the component
///   weights satisfy 0.94^2 + 0.30^2 < 1) and re-clamped anyway;
/// * the displaced sample is clamped to the frame interior (plus a ClampToEdge
///   sampler), so the pass can never read outside the frame;
/// * the pass is scissored to the derived region rect, so every pixel outside
///   it is byte-identical to the pre-shimmer frame (a hard zero beyond the
///   bound; the smooth rolloff lives INSIDE the rect).
///
/// TIME: `phase` is wall-clock seconds (wrapped at 100 s — every time
/// coefficient below is a two-decimal cycles/s rate, so `k * 100` is an integer
/// number of cycles and the wrap is seam-free). Wall-clock at present is the
/// accepted bloom-class exception, exactly like the SDR crown's attack
/// envelope; tests pin it via `set_shimmer_phase_for_test`.
const SHIMMER_SHADER: &str = r#"
struct VsOut {
    @builtin(position) pos: vec4<f32>,
};

@vertex
fn vs_fs(@builtin(vertex_index) vi: u32) -> VsOut {
    var xy = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0)
    );
    var o: VsOut;
    o.pos = vec4<f32>(xy[vi], 0.0, 1.0);
    return o;
}

@group(0) @binding(0) var shimmer_src: texture_2d<f32>;
@group(0) @binding(1) var shimmer_samp: sampler;
struct ShimmerU {
    frame: vec2<f32>,       // full frame dims, device px
    region_min: vec2<f32>,  // pass rect min (== scissor origin)
    region_max: vec2<f32>,  // pass rect max
    hot_top: f32,           // top edge of the hot band (px)
    rise: f32,              // haze height above the hot band (px)
    amp: f32,               // max displacement, device px (<= 1.5)
    period: f32,            // spatial ripple period (~cell height, px)
    phase: f32,             // present-time phase, seconds (wrapped at 100 s)
    band_x0: f32,           // heat-band strip origin (px)
    band_w: f32,            // heat-band width (px)
    rolloff: f32,           // horizontal edge rolloff width (px)
    _pad: vec2<f32>,
    heat: array<vec4<f32>, 16>, // 64 per-column-band heat proxies, 0..1
};
@group(0) @binding(2) var<uniform> su: ShimmerU;

const TAU: f32 = 6.28318530718;

fn heat_at(i: u32) -> f32 {
    return su.heat[i / 4u][i % 4u];
}

@fragment
fn fs_shimmer(in: VsOut) -> @location(0) vec4<f32> {
    let p = in.pos.xy;
    // Per-column heat, linearly interpolated between adjacent bands so the
    // haze strength follows the flame's silhouette without band steps.
    let xb = clamp((p.x - su.band_x0) / max(su.band_w, 1e-3) - 0.5, 0.0, 63.0);
    let i0 = u32(floor(xb));
    let heat = mix(heat_at(i0), heat_at(min(i0 + 1u, 63u)), fract(xb));
    // Height envelope: full strength at the hot band's top edge, smoothstep
    // fading to zero `rise` px above it (haze thins with altitude).
    let a = clamp((su.hot_top - p.y) / max(su.rise, 1.0), 0.0, 1.0);
    let vfade = 1.0 - a * a * (3.0 - 2.0 * a);
    // Horizontal rolloff: zero AT the region edges, easing in over `rolloff`
    // px, so the hard scissor bound is met by an already-zero displacement.
    let xr = min(p.x - su.region_min.x, su.region_max.x - p.x);
    let e = clamp(xr / max(su.rolloff, 1.0), 0.0, 1.0);
    let xfade = e * e * (3.0 - 2.0 * e);
    // The ripple field: two incommensurate plane waves RISING with time (the
    // +time phase moves constant-phase lines toward smaller y, i.e. upward)
    // plus a slow columnar wobble that breaks straight-line coherence — the
    // classic cheap heat-haze. All time rates are two-decimal cycles/s (see
    // the 100 s wrap note above).
    let ky = TAU / max(su.period, 4.0);
    let col = p.x * ky;
    let w1 = sin(ky * p.y + TAU * (0.55 * su.phase) + 1.7 * sin(0.37 * col + TAU * (0.09 * su.phase)));
    let w2 = sin(1.93 * ky * p.y + TAU * (0.83 * su.phase) + 0.61 * col);
    let wx = sin(1.31 * ky * p.y + TAU * (0.70 * su.phase) + 1.11 * col);
    let env = su.amp * clamp(heat, 0.0, 1.0) * vfade * xfade;
    var d = vec2<f32>(0.30 * wx, 0.94 * (0.62 * w1 + 0.38 * w2)) * env;
    // Belt-and-braces displacement cap: |d| <= amp by construction
    // (sqrt(0.94^2 + 0.30^2) < 1); re-clamp so no future retune can break it.
    let m = length(d);
    if (m > su.amp) {
        d = d * (su.amp / m);
    }
    // Never sample outside the frame: clamp the displaced point to the texel
    // interior (the ClampToEdge sampler is the second fence).
    let sp = clamp(p + d, vec2<f32>(0.5, 0.5), su.frame - vec2<f32>(0.5, 0.5));
    let c = textureSampleLevel(shimmer_src, shimmer_samp, sp / su.frame, 0.0);
    return vec4<f32>(c.rgb, 1.0);
}
"#;

/// Shimmer uniform — matches WGSL `ShimmerU` exactly: three vec2 (0/8/16),
/// seven f32 (24..52), a vec2 pad to 16-align the vec4 array at 64; 320 bytes.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
struct ShimmerUniform {
    frame: [f32; 2],
    region_min: [f32; 2],
    region_max: [f32; 2],
    hot_top: f32,
    rise: f32,
    amp: f32,
    period: f32,
    phase: f32,
    band_x0: f32,
    band_w: f32,
    rolloff: f32,
    _pad: [f32; 2],
    heat: [[f32; 4]; SHIMMER_BANDS / 4],
}

/// Blit uniform: the bell-flash invert flag plus the drag-and-drop drop-target
/// highlight parameters plus the W1 band placement (frame offset + band colour).
/// `#[repr(C)]` with a std140-compatible 96-byte layout matching the WGSL `Blit`
/// struct exactly (vec4 at offsets 16 and 48, vec2 at 32 and 64).
#[repr(C)]
#[derive(Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct BlitUniform {
    /// Non-zero inverts RGB (visual-bell flash); zero blits straight through.
    flag: u32,
    /// Non-zero paints the drop-target highlight (wash + inset border).
    overlay: u32,
    /// Inset border thickness in device pixels.
    border_px: f32,
    /// Non-zero on the downlevel (WebGL2) path: the blit samples an sRGB-typed view
    /// (which auto-decodes to linear), so it must re-encode linear→sRGB before writing
    /// to the non-sRGB swapchain. Zero on native (the blit samples raw Unorm bytes).
    encode_srgb: f32,
    /// Overlay accent, normalized 0..1 (rgb; a unused but kept for alignment).
    accent: [f32; 4],
    /// OFFSCREEN frame size in pixels (the content the blit fetches), for the
    /// in-shader bounds check + overlay edge-distance calc. ALWAYS set by the
    /// present path (the bounds check runs on every pixel).
    dims: [f32; 2],
    /// Interior wash alpha and border alpha, 0..1.
    wash_a: f32,
    border_a: f32,
    /// W1: the remainder-band colour (live terminal bg, device-RGB normalized
    /// 0..1; alpha unused). Painted outside the frame — never inverted/washed.
    band: [f32; 4],
    /// W1: the frame's top-left inside the swapchain, device px — the centred
    /// remainder split (`aterm_render::band_offset`; negative = centred crop).
    content_off: [f32; 2],
    /// M3: non-zero on the HDR (Rgba16Float extended-linear-sRGB) present — the
    /// blit decodes the sampled sRGB bytes to linear light, clamped ≤ 1.0 (the
    /// grid clamp law). Zero everywhere else, so the pre-M3 uniform bytes are
    /// unchanged (this slot WAS `_pad`, always 0.0).
    hdr: f32,
    /// M5 true vibrancy: non-zero when the window's `background_opacity < 1.0` and
    /// the swapchain is `PostMultiplied` — the blit then emits the offscreen frame
    /// alpha (content) and `band[3]` (remainder) so the compositor blends the
    /// window over its `NSVisualEffectView`. Zero (the solid default) forces the
    /// presented alpha to 1.0, so the uniform bytes stay the pre-M5 layout (this
    /// slot WAS `_pad`, always 0.0).
    translucent: f32,
    /// M3 (Windows scRGB): reference-white scale for the EDR present (`SDRwhite/80`).
    /// scRGB `1.0` == 80 nits FIXED, so the grid must scale to the user's SDR-white or
    /// it renders dim. `1.0` on macOS (the extended-linear layer auto-maps 1.0 → SDR
    /// white) and on every SDR present, so those uniform bytes are unchanged in effect.
    sdr_white_scale: f32,
    /// First raw source row exposed by the frontend's vertical crop.
    visible_y: f32,
    /// Height of the exposed source frame. Raw rows outside this interval are
    /// painted as destination bands (never inverted or overlay-washed).
    visible_h: f32,
    _pad2: f32,
}

impl BlitUniform {
    /// A plain blit (optionally bell-inverted) with no drop overlay. The caller
    /// MUST still fill `dims` / `band` / `content_off` (+ `encode_srgb`): the
    /// band bounds check consumes them on every present.
    fn bell(invert: bool) -> Self {
        Self {
            flag: invert as u32,
            overlay: 0,
            border_px: 0.0,
            encode_srgb: 0.0, // set by the caller from ctx.srgb_offscreen
            accent: [0.0; 4],
            dims: [0.0, 0.0],
            wash_a: 0.0,
            border_a: 0.0,
            band: [0.0; 4],
            content_off: [0.0, 0.0],
            hdr: 0.0,             // set by the caller from the HdrPlan (f16 swapchains only)
            translucent: 0.0,     // set by the present path when background_opacity < 1.0
            sdr_white_scale: 1.0, // 1.0 = no scaling (SDR / macOS); set on the Windows EDR present
            visible_y: 0.0,
            visible_h: 0.0,
            _pad2: 0.0,
        }
    }

    /// M5: mark this blit TRANSLUCENT and set the remainder-band alpha. `band_a`
    /// is `bg_quad_alpha(opacity)` normalized to 0..1 — the same alpha the bg
    /// quads carry — so the letterbox bands read as the same glass. A no-op on a
    /// solid window (the caller only sets it when `is_translucent`).
    fn with_translucency(mut self, band_a: f32) -> Self {
        self.translucent = 1.0;
        self.band[3] = band_a;
        self
    }

    /// Fill the W1 placement fields: frame `dims`, the centred `content_off`
    /// for a `dst_w`×`dst_h` swapchain, and the `band_rgb` (0x00RRGGBB live
    /// terminal bg) band colour. Shared by the real present and the test blit so the
    /// placement math has one owner (`aterm_render::band_offset`).
    fn with_bands(mut self, fw: u32, fh: u32, dst_w: u32, dst_h: u32, band_rgb: u32) -> Self {
        let chan = |s: u32| ((band_rgb >> s) & 0xff) as f32 / 255.0;
        self.dims = [fw as f32, fh as f32];
        self.visible_y = 0.0;
        self.visible_h = fh as f32;
        self.content_off = [
            aterm_render::band_offset(dst_w as usize, fw as usize) as f32,
            aterm_render::band_offset(dst_h as usize, fh as usize) as f32,
        ];
        self.band = [chan(16), chan(8), chan(0), 1.0];
        self
    }

    /// Restrict the raw offscreen to the frontend-visible vertical interval.
    /// The raw texture stays unchanged for renderer/cache compatibility; rows
    /// outside this interval become destination bands in the blit shader.
    fn with_source_crop(mut self, crop: Option<PresentCrop>) -> Self {
        if let Some(crop) = crop {
            let raw_h = self.dims[1].max(0.0) as u32;
            let y = crop.source_y.min(raw_h);
            let h = crop.height.min(raw_h.saturating_sub(y));
            self.visible_y = y as f32;
            self.visible_h = h as f32;
        }
        self
    }
}

/// Tray-overlay uniform: the device-px placement rect + framebuffer size, for
/// `vs_tray`. `#[repr(C)]` std140-compatible 32-byte layout matching the WGSL
/// `Tray` struct exactly (vec4 at offset 0, vec2 at 16, vec2 pad at 24).
#[repr(C)]
#[derive(Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct TrayUniform {
    /// Card placement in device px: x, y, w, h (top-left origin, y-down).
    rect: [f32; 4],
    /// Framebuffer size in px (the NDC divisor).
    fb: [f32; 2],
    _pad: [f32; 2],
}

/// Parameters for the drag-and-drop drop-target highlight, passed by the frontend
/// to [`Renderer::present_input`]. The alphas (the frontend's single source of
/// truth) are 0..=255; the border thickness is derived from the framebuffer size
/// inside the renderer (see `drop_border_px_gpu`, kept in sync with the GUI's
/// `app_render::drop_border_px`) so it matches the CPU/headless path.
#[derive(Clone, Copy)]
pub struct DropOverlay {
    /// Accent color, packed `0x00RRGGBB`.
    pub accent: u32,
    /// Interior wash alpha, 0..=255.
    pub wash_a: u8,
    /// Inset border alpha, 0..=255.
    pub border_a: u8,
}

/// Vertical interval of the renderer's raw offscreen that a frontend exposes.
/// This lets a GUI keep a legacy transport allocation while presenting/capturing
/// a shorter independently padded frame without leaking raw rows into bands,
/// visual-bell inversion, or the drop-overlay border.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PresentCrop {
    /// First visible row in raw source coordinates.
    pub source_y: u32,
    /// Number of visible source rows.
    pub height: u32,
}

/// A CPU-rasterized settings card to composite over the swapchain at present
/// time: a STRAIGHT-alpha RGBA8 buffer (`pw`×`ph`, `bytes_per_row = pw*4`) and a
/// device-px top-left placement (`dx`, `dy`). Passed by the frontend to
/// [`GpuRenderer::present_input`]; `None` (the default) draws nothing. The GPU
/// crate takes RAW bytes + rect, never the GUI's `SettingsCard` type.
#[derive(Clone, Copy)]
pub struct TrayQuad<'a> {
    /// Straight (non-premultiplied) RGBA8, length `pw*ph*4`.
    pub rgba: &'a [u8],
    /// Card width in px (also the texture width / `bytes_per_row/4`).
    pub pw: u32,
    /// Card height in px.
    pub ph: u32,
    /// Destination top-left X in device px.
    pub dx: u32,
    /// Destination top-left Y in device px.
    pub dy: u32,
}

/// Why an on-screen swapchain present did not acquire a drawable. The GUI
/// uses this distinction to avoid turning a persistent surface condition into
/// an unbounded `request_redraw` loop: reconfigured/timeout failures get a
/// bounded delayed retry, while an occluded or validation-failed surface parks
/// until a real external stimulus asks for another frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurfacePresentFailure {
    /// The surface was outdated or lost and has just been reconfigured.
    Reconfigured,
    /// Drawable acquisition timed out; retrying later may succeed.
    Timeout,
    /// The platform reports that the surface is not currently visible.
    Occluded,
    /// The surface transaction was rejected as invalid.
    Validation,
}

/// The resident settings-card overlay texture + its alpha-blend bind group,
/// per-window. Built on first use, REUSED at the same `(w, h)`, recreated only
/// when the card size changes — the `Offscreen`/`ImagePlane` lifecycle. The card
/// pixels are uploaded (`write_texture`) only when they DIFFER from the bytes the
/// texture already holds (`pixels` below is the exact-equality mirror): a
/// hover-stable settings frame driven by background PTY output re-presents an
/// identical raster, which now skips the whole-card upload. Cleared to `None` on
/// a card-free present so nothing is bound or drawn.
pub(crate) struct TrayOverlay {
    /// Resident RGBA8 card texture; the `write_texture` upload target for each new
    /// card frame (read at upload time, so NOT dead — unlike `view` below).
    texture: wgpu::Texture,
    /// The exact bytes resident in `texture` (the upload mirror). The next present
    /// skips `write_texture` iff the incoming card equals this byte-for-byte —
    /// exact equality, so a stale card is impossible (no fingerprint collisions).
    /// Empty right after a (re)create, so a fresh texture (undefined contents) is
    /// always uploaded into.
    pixels: Vec<u8>,
    /// Default view of `texture`; retained alongside it (also kept alive
    /// transitively by `bind`). Mirrors the `Offscreen` handle set.
    #[allow(dead_code)]
    view: wgpu::TextureView,
    /// Bind group on `tray_bgl`: card view (0) + LINEAR sampler (1) + the resident
    /// `tray_uniform_buf` (2).
    bind: wgpu::BindGroup,
    /// Resident texture dims; the reuse-vs-recreate key.
    w: u32,
    h: u32,
}

/// Inset border thickness in device px for a `w`×`h` framebuffer. MUST stay in
/// sync with `app_render::drop_border_px` so GPU and CPU/headless agree.
fn drop_border_px_gpu(w: u32, h: u32) -> u32 {
    (w.min(h) / 200).clamp(2, 6)
}

fn valid_present_crop(crop: PresentCrop, raw_height: u32) -> bool {
    crop.height > 0
        && crop.source_y < raw_height
        && crop.height <= raw_height.saturating_sub(crop.source_y)
}

/// Validate a frontend crop in the renderer's LOGICAL raw geometry, then
/// intersect it with the resident (device-clamped) offscreen.  Oversized grids
/// have always rendered as a clipped top prefix; keeping validation in logical
/// coordinates preserves that behaviour while ensuring the shader never sees
/// an interval beyond the actual texture.  An interval wholly below the
/// resident prefix is empty and therefore rejected.
fn normalize_present_crop(
    crop: PresentCrop,
    logical_raw_height: u32,
    resident_raw_height: u32,
) -> Option<PresentCrop> {
    if !valid_present_crop(crop, logical_raw_height) {
        return None;
    }
    let height = crop
        .height
        .min(resident_raw_height.saturating_sub(crop.source_y));
    (height > 0).then_some(PresentCrop {
        source_y: crop.source_y,
        height,
    })
}

/// Destination scissor for effects composited after the blit. Crown shaders use
/// raw source coordinates, so without this intersection they can re-light rows
/// the frontend deliberately cropped into background bands.
fn visible_source_scissor(
    uniform: &BlitUniform,
    destination_width: u32,
    destination_height: u32,
) -> Option<(u32, u32, u32, u32)> {
    let left = uniform.content_off[0] as i64;
    let top = (uniform.content_off[1] + uniform.visible_y) as i64;
    let right = left.saturating_add(uniform.dims[0] as i64);
    let bottom = top.saturating_add(uniform.visible_h as i64);
    let x0 = left.clamp(0, i64::from(destination_width));
    let y0 = top.clamp(0, i64::from(destination_height));
    let x1 = right.clamp(0, i64::from(destination_width));
    let y1 = bottom.clamp(0, i64::from(destination_height));
    (x1 > x0 && y1 > y0).then_some((x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32))
}

#[allow(
    clippy::too_many_arguments,
    reason = "the shared present uniform needs source/destination geometry plus the three independent present effects"
)]
fn present_blit_uniform(
    invert: bool,
    overlay: Option<DropOverlay>,
    source_crop: Option<PresentCrop>,
    source_width: u32,
    source_height: u32,
    destination_width: u32,
    destination_height: u32,
    band_rgb: u32,
) -> BlitUniform {
    let visible_height = source_crop.map_or(source_height, |crop| crop.height);
    match overlay {
        Some(overlay) => {
            let chan = |shift: u32| ((overlay.accent >> shift) & 0xff) as f32 / 255.0;
            BlitUniform {
                flag: invert as u32,
                overlay: 1,
                border_px: drop_border_px_gpu(source_width, visible_height) as f32,
                accent: [chan(16), chan(8), chan(0), 1.0],
                wash_a: f32::from(overlay.wash_a) / 255.0,
                border_a: f32::from(overlay.border_a) / 255.0,
                ..BlitUniform::bell(invert)
            }
        }
        None => BlitUniform::bell(invert),
    }
    .with_bands(
        source_width,
        source_height,
        destination_width,
        destination_height,
        band_rgb,
    )
    .with_source_crop(source_crop)
}

/// An on-screen presentation target: a configured wgpu swapchain surface that the
/// offscreen frame is blitted into. Opaque (fields private) — the frontend holds
/// it and passes it back to [`GpuRenderer::present_input`] / `resize_surface`.
pub struct GpuSurface {
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    /// M5 true vibrancy: whether this surface offers `CompositeAlphaMode::PostMultiplied`
    /// (the non-opaque composite the translucent present needs). Captured from the
    /// surface caps at attach so [`GpuRenderer::present_input`] can flip the swapchain
    /// between `Opaque` and `PostMultiplied` when `background_opacity` crosses 1.0
    /// WITHOUT re-querying the driver every frame. `false` ⇒ the platform has no
    /// non-opaque composite here, so a translucent request stays honestly solid.
    post_mult: bool,
    /// VIDEO introspection: whether the swapchain was configured with `COPY_SRC`
    /// (the surface caps offered it — always true on DX12 flip-model, common on
    /// Vulkan, often absent on Metal). Gates the `video` frame tap: where false,
    /// the verb replies unsupported rather than degrading to a lookalike capture.
    copyable: bool,
}

impl GpuSurface {
    /// M3 phase B: whether this swapchain is the EDR (`Rgba16Float`
    /// extended-linear) target — i.e. `hdr_glow` was on AND the surface offered
    /// the format at attach. The frontend uses it to pick the layer's colour-
    /// space tag (extended-linear-sRGB vs the config `window_colorspace`) and to
    /// know the per-window EDR headroom is worth (re)querying.
    #[must_use]
    pub fn is_hdr(&self) -> bool {
        self.config.format == wgpu::TextureFormat::Rgba16Float
    }
}

/// Where a glyph lives in the atlas, plus its placement offsets.
#[derive(Clone, Copy)]
struct GlyphSlot {
    ax: u32,
    ay: u32,
    gw: u32,
    gh: u32,
    xmin: i32,
    ymin: i32,
}

/// Which kind of glyph an [`Atlas`] holds — selects the bytes-per-texel and the
/// glyph-image variant a key is packed from. The mono (R8) and colour (RGBA8)
/// atlases share all packing logic; only this differs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AtlasKind {
    /// 8-bit coverage (R8), 1 byte/texel. Every requested key gets an entry —
    /// non-mono / empty glyphs pack as zero-sized slots (the mono atlas is the
    /// canonical place to look a key up).
    Mono,
    /// 32-bit colour (RGBA8), 4 bytes/texel. Only non-empty `Rgba` glyphs are
    /// packed; the rest live in the mono atlas.
    Color,
}

impl AtlasKind {
    /// Bytes per texel for this atlas's pixel format.
    fn bpp(self) -> u32 {
        match self {
            AtlasKind::Mono => 1,
            AtlasKind::Color => 4,
        }
    }
}

/// A packed coverage atlas (R8) or colour atlas (RGBA8) + per-glyph placement,
/// keyed by the CPU renderer's [`GlyphKey`] (face + class + char + style + size
/// — the full rasterization identity, so e.g. a styled variant of the same char
/// gets its own slot).
///
/// PERSISTENT across frames: the `data`/`map` and the shelf-packer cursor
/// (`px`,`py`,`shelf_h`) survive so new glyphs can be APPENDED into free space
/// without repacking. `height` is the height the bytes occupy; a glyph appends
/// only while it fits under `cap_h` (the resident GPU texture height) — past
/// that is genuine overflow and the caller repacks fresh.
struct Atlas {
    kind: AtlasKind,
    width: u32,
    height: u32,
    data: Vec<u8>,
    map: FxHashMap<GlyphKey, GlyphSlot>,
    // Shelf-packer cursor (next free position on the current shelf).
    px: u32,
    py: u32,
    shelf_h: u32,
}

impl Atlas {
    /// Whether this kind packs `img` as a real (non-zero) slot.
    fn packs(kind: AtlasKind, img: &GlyphImage) -> bool {
        let real = img.width() > 0 && img.height() > 0;
        match kind {
            // Mono atlas: only Mono coverage occupies real space (Rgba/empty
            // pack as zero slots, recorded for lookup).
            AtlasKind::Mono => real && matches!(img, GlyphImage::Mono { .. }),
            // Colour atlas: only non-empty Rgba glyphs are packed at all.
            AtlasKind::Color => real && matches!(img, GlyphImage::Rgba { .. }),
        }
    }

    /// Pack `key`/`img` into the current shelves, advancing the cursor and
    /// recording the slot. Returns `Some((ay, gh))` (the y band the glyph
    /// occupies) for a real slot, else `None`. Pure CPU bookkeeping — it does
    /// NOT touch `data`; the caller blits bytes for the returned band.
    fn place(&mut self, key: GlyphKey, img: &GlyphImage) -> Option<(u32, u32)> {
        let pad = 1u32;
        if !Self::packs(self.kind, img) {
            // Mono records every key (zero slot for non-mono/empty) so a lookup
            // there always resolves; Color simply skips.
            if self.kind == AtlasKind::Mono {
                self.map.insert(
                    key,
                    GlyphSlot {
                        ax: 0,
                        ay: 0,
                        gw: 0,
                        gh: 0,
                        xmin: img.xmin(),
                        ymin: img.ymin(),
                    },
                );
            }
            return None;
        }
        let (gw, gh) = (img.width() as u32, img.height() as u32);
        // A single glyph wider than the atlas can EVER hold has no shelf to wrap
        // onto (unlike the packed HEIGHT, which the caller rolls back / caps at
        // `cap_h` in `build_kind`/`grow_atlas`): the shelf-wrap below would reset
        // `px=0` and still record `gw > width`, so `blit`'s per-row
        // `copy_from_slice` of `gw*bpp` bytes would run past the row stride —
        // corrupting the next shelf (1024 < gw <= 2048) or panicking with
        // `dst + row_bytes > data.len()` (gw > 2048). Degrade it exactly like the
        // non-packable/empty branch above and return WITHOUT advancing the cursor:
        // Mono records a zero slot so the lookup still resolves (the cell paints
        // its background); Color simply skips. `self.width.saturating_sub(pad)`
        // avoids any u32 wrap on a pathological `gw + pad`.
        if gw > self.width.saturating_sub(pad) {
            if self.kind == AtlasKind::Mono {
                self.map.insert(
                    key,
                    GlyphSlot {
                        ax: 0,
                        ay: 0,
                        gw: 0,
                        gh: 0,
                        xmin: img.xmin(),
                        ymin: img.ymin(),
                    },
                );
            }
            return None;
        }
        if self.px + gw + pad > self.width {
            self.px = 0;
            self.py += self.shelf_h + pad;
            self.shelf_h = 0;
        }
        let (ax, ay) = (self.px, self.py);
        self.map.insert(
            key,
            GlyphSlot {
                ax,
                ay,
                gw,
                gh,
                xmin: img.xmin(),
                ymin: img.ymin(),
            },
        );
        self.px += gw + pad;
        self.shelf_h = self.shelf_h.max(gh);
        Some((ay, gh))
    }

    /// Blit one glyph's bytes into `data` at its slot (no-op for a zero slot).
    fn blit(&mut self, img: &GlyphImage, slot: &GlyphSlot) {
        if slot.gw == 0 || slot.gh == 0 {
            return;
        }
        let bpp = self.kind.bpp();
        let bytes = img.bytes();
        // A glyph row is `gw` contiguous texels in both source and destination
        // (rows are stored linearly, texels packed at `bpp` each), so each row
        // copies in one memcpy — byte-identical to the old per-texel loop, just
        // without the per-texel bounds-check / call overhead.
        let row_bytes = (slot.gw * bpp) as usize;
        for j in 0..slot.gh {
            let src = ((j * slot.gw) * bpp) as usize;
            let dst = (((slot.ay + j) * self.width + slot.ax) * bpp) as usize;
            self.data[dst..dst + row_bytes].copy_from_slice(&bytes[src..src + row_bytes]);
        }
    }

    /// The height (in texels) the shelves would occupy after packing everything
    /// placed so far, including 1px bottom padding.
    fn occupied_height(&self) -> u32 {
        (self.py + self.shelf_h + 1).max(1)
    }
}

/// Pack every requested glyph's image into one fresh R8 coverage atlas (shelf
/// packer, 1px padding), pulling the EXACT cached bytes from the CPU renderer
/// via [`Renderer::glyph_image`]. A full (re)pack — used on the first frame and
/// on genuine overflow; the steady state APPENDS instead (see `grow_atlas`).
///
/// A free function (not a `GpuRenderer` method) so the atlas-byte-identity
/// unit test can exercise it with no GPU device.
fn build_atlas(cpu: &mut Renderer, keys: &BTreeSet<GlyphKey>, cap_h: u32) -> Atlas {
    build_kind(cpu, keys, AtlasKind::Mono, cap_h)
}

/// Pack every colour-emoji (`GlyphImage::Rgba`) glyph into one fresh RGBA8 atlas
/// (shelf packer, 1px padding), pulling the EXACT cached pixels from the CPU
/// renderer. The CPU already scaled each emoji to its final on-cell size, so the
/// GPU blits these 1:1 with NEAREST sampling — exact bytes, like the mono path.
/// Mono and empty glyphs are skipped here (they live in the R8 atlas).
fn build_color_atlas(cpu: &mut Renderer, keys: &BTreeSet<GlyphKey>, cap_h: u32) -> Atlas {
    build_kind(cpu, keys, AtlasKind::Color, cap_h)
}

/// Shared full-pack for either [`AtlasKind`]: place every key, then blit its
/// bytes. `data` is sized to the occupied height once packing is known, so it
/// holds exactly the packed shelves (no slack) — byte-identical to the old
/// per-kind packers.
fn build_kind(cpu: &mut Renderer, keys: &BTreeSet<GlyphKey>, kind: AtlasKind, cap_h: u32) -> Atlas {
    let mut atlas = Atlas {
        kind,
        width: ATLAS_WIDTH,
        height: 1,
        data: Vec::new(),
        map: FxHashMap::default(),
        px: 0,
        py: 0,
        shelf_h: 0,
    };
    // Borrow each cached image just long enough to record placement; defer the
    // byte blit until `data` is allocated. Collect the (key, slot) pairs whose
    // bytes we still need so we can re-borrow the cache per glyph (no clone).
    let mut placed: Vec<GlyphKey> = Vec::new();
    for &key in keys {
        let img = cpu.glyph_image(key);
        // Snapshot the shelf cursor so a glyph that would push the packed height
        // past `cap_h` (the device's max 2D texture dimension) can be ROLLED BACK
        // and packing stopped — creating a texture taller than the GPU allows would
        // abort the device. The skipped glyphs find no slot (render nothing) only in
        // the pathological overflow case (thousands of distinct glyphs); for every
        // real workload `cap_h` is far above the packed height and nothing is
        // dropped, so this is byte-identical to the unbounded pack.
        let (sx, sy, sh) = (atlas.px, atlas.py, atlas.shelf_h);
        if atlas.place(key, img).is_some() {
            if atlas.occupied_height() > cap_h {
                atlas.map.remove(&key);
                atlas.px = sx;
                atlas.py = sy;
                atlas.shelf_h = sh;
                break;
            }
            placed.push(key);
        }
    }
    atlas.height = atlas.occupied_height();
    atlas.data = vec![0u8; (atlas.width * atlas.height * kind.bpp()) as usize];
    for key in placed {
        let slot = atlas.map[&key];
        let img = cpu.glyph_image(key);
        atlas.blit(img, &slot);
    }
    atlas
}

/// Append `new_keys` (each NOT already resident) into `atlas`'s free space,
/// keeping every existing slot put. `cap_h` is the resident GPU texture height
/// the bytes must still fit under. Returns `Some((dirty_y0, dirty_y1))` — the
/// half-open row band that changed and must be re-uploaded — on success, or
/// `None` on genuine overflow (the caller must full-repack into a taller
/// texture). On `None` the atlas is left UNMODIFIED.
fn grow_atlas(
    cpu: &mut Renderer,
    atlas: &mut Atlas,
    new_keys: &[GlyphKey],
    cap_h: u32,
) -> Option<(u32, u32)> {
    // Dry-run placement on a scratch copy of the cursor so a mid-list overflow
    // does not leave the atlas half-grown.
    let (sx, sy, sh) = (atlas.px, atlas.py, atlas.shelf_h);
    let mut probe = Atlas {
        kind: atlas.kind,
        width: atlas.width,
        height: atlas.height,
        data: Vec::new(),
        map: FxHashMap::default(),
        px: sx,
        py: sy,
        shelf_h: sh,
    };
    let mut dirty_lo = u32::MAX;
    let mut dirty_hi = 0u32;
    for &key in new_keys {
        let img = cpu.glyph_image(key);
        if let Some((ay, gh)) = probe.place(key, img) {
            dirty_lo = dirty_lo.min(ay);
            dirty_hi = dirty_hi.max(ay + gh);
        }
    }
    let need_h = probe.occupied_height();
    if need_h > cap_h {
        return None; // genuine overflow: caller repacks fresh into a new texture
    }
    if dirty_hi == 0 {
        // All new keys packed as zero slots (mono only) — record them, no upload.
        for &key in new_keys {
            let img = cpu.glyph_image(key);
            atlas.place(key, img);
        }
        return Some((0, 0));
    }

    // Commit: replay the placement on the real atlas (cursor advances exactly as
    // the probe did) and blit each new glyph's bytes. `data` is grown to the
    // occupied height first so the new band has backing storage.
    let new_total = (atlas.width * need_h * atlas.kind.bpp()) as usize;
    if atlas.data.len() < new_total {
        atlas.data.resize(new_total, 0);
    }
    atlas.height = need_h.max(atlas.height);
    for &key in new_keys {
        let img = cpu.glyph_image(key);
        // `place` records every key (zero slot for non-mono/empty too); only a
        // real slot needs its bytes blitted into the dirty band.
        if atlas.place(key, img).is_some() {
            let slot = atlas.map[&key];
            atlas.blit(img, &slot);
        }
    }
    Some((dirty_lo, dirty_hi))
}

// The deco sprite atlas is sized DYNAMICALLY to `DECO_ATLAS_SPRITES * cell_w`
// so the sprites always fit (a fixed width dropped all decorations at large
// fonts, diverging from the CPU path); the shared
// `aterm_render::undercurl_supported` predicate caps BOTH axes at
// `aterm_render::DECO_ATLAS_MAX_DIM` (the downlevel/WebGL2
// `max_texture_dimension_2d` of 2048), so the CPU renderer and this atlas can
// never disagree about whether the undercurl sprite exists.

/// The sparkle-word sprites, in atlas-column order. Index `i` lives at texel
/// `x = i * cell_w`. MUST stay in sync with [`deco_sprite_index`]. The atlas
/// carries ONE more sprite after these: the W7 AA undercurl tile at column
/// `aterm_render::UNDERCURL_SPRITE` (so `DECO_ATLAS_SPRITES == 8 sparkle + 1`).
const DECO_GLYPHS: [aterm_render::DecoGlyph; 8] = [
    aterm_render::DecoGlyph::Star4,
    aterm_render::DecoGlyph::Star5,
    aterm_render::DecoGlyph::Dot,
    aterm_render::DecoGlyph::Plus,
    aterm_render::DecoGlyph::Paw,
    aterm_render::DecoGlyph::Droplet,
    aterm_render::DecoGlyph::RingArc,
    aterm_render::DecoGlyph::Shade,
];

// The shared atlas layout: 8 sparkle sprites + the undercurl tile. A drift in
// either count is a CPU/GPU parity break, so pin both at compile time.
const _: () = assert!(DECO_GLYPHS.len() == aterm_render::UNDERCURL_SPRITE);
const _: () = assert!(DECO_GLYPHS.len() + 1 == aterm_render::DECO_ATLAS_SPRITES);

/// Atlas column index of a sprite (inverse of [`DECO_GLYPHS`]).
fn deco_sprite_index(g: aterm_render::DecoGlyph) -> usize {
    match g {
        aterm_render::DecoGlyph::Star4 => 0,
        aterm_render::DecoGlyph::Star5 => 1,
        aterm_render::DecoGlyph::Dot => 2,
        aterm_render::DecoGlyph::Plus => 3,
        aterm_render::DecoGlyph::Paw => 4,
        aterm_render::DecoGlyph::Droplet => 5,
        aterm_render::DecoGlyph::RingArc => 6,
        aterm_render::DecoGlyph::Shade => 7,
    }
}

/// The resident sparkle-word sprite atlas (R8 coverage) + its bind group, valid
/// for one cell size. Rebuilt on a cell-size change.
struct DecoAtlas {
    bind: wgpu::BindGroup,
    cw: usize,
    ch: usize,
    /// The undercurl band `(underline_y, underline_t)` the curl sprite was
    /// baked with — a font-table / adjust-knob change re-bakes the atlas even
    /// when the cell size is unchanged (W7).
    curl_band: (usize, usize),
    /// Actual atlas texture width = `DECO_ATLAS_SPRITES * cw` (grows with the
    /// cell so the sprites always fit); used to normalize the sample UVs.
    atlas_w: usize,
}

/// The uploaded RGBA8 SCENE atlas: the resident texture's bind group (sampled by the
/// scene pipelines), its texel dims (for UV normalization), and the source atlas
/// `version` so the upload is skipped on an unchanged frame and re-done on a rebake.
struct SceneTex {
    bind: wgpu::BindGroup,
    w: u32,
    h: u32,
    version: u64,
}

/// One persisted, on-GPU glyph atlas: the CPU-side packed [`Atlas`] (so we can
/// append new glyphs into free space) alongside its resident texture + bind
/// group and the texture's dims. Lives across frames — the whole point of the
/// atlas is that idle frames reuse it untouched.
struct ResidentAtlas {
    atlas: Atlas,
    tex: wgpu::Texture,
    bind: wgpu::BindGroup,
    /// The resident texture's height (texels). New glyphs may append while the
    /// packed shelves stay under this; beyond it is overflow → a new texture.
    tex_h: u32,
}

/// Per-window GPU state: the offscreen render target the window draws into and
/// blits from and its dirty-gate cache. One per logical window. The device,
/// glyph atlas, and pipelines live on the shared `GpuRenderer`; only these are
/// per-window, so N windows cost ~1 device + 1 atlas + N small offscreens.
#[derive(Default)]
pub struct WindowGpu {
    // CPU wall time spent encoding commands and calling `queue.submit` for the
    // most recent successful present, after swapchain acquisition and before
    // `present()`. This is NOT a GPU timestamp and does not measure completed
    // shader execution. Keeping the stamp per-window lets the frontend drive
    // load shedding without folding FIFO/nextDrawable pacing into the signal.
    last_present_work_ns: u64,
    // The resident offscreen render target + its blit-source bind group. `None`
    // until the first frame; reused at the same `(w, h)`, recreated only on a
    // dimension change. See `Offscreen`.
    pub(crate) offscreen: Option<Offscreen>,
    // DIRTY-GATE cache for the per-frame PRESENTATION hot path
    // (`render_input_cached`). Holds the previous frame's input + cursor state +
    // the pixels that were read back for it. When the next frame is PIXEL-
    // IDENTICAL (per `is_unchanged_frame`), we re-present these cached pixels and
    // do ZERO GPU work — no encode, no submit, no `device.poll`, no readback.
    pub(crate) gate_cache: Option<GpuGateCache>,
    // Decoded-image LRU for the inline-image (iTerm2 OSC 1337) pixel pass. Keyed
    // by `(arc_ptr, fp_w, fp_h)` like the CPU `ImageCache`, so each distinct
    // placement is decoded+scaled at most once. PER-WINDOW so window B's images
    // never leak into window A. Empty in the common image-free case.
    pub(crate) image_cache: GpuImageCache,
    // VIDEO introspection recording, `None` = off (one branch per present). The
    // tap copies the exact presented swapchain bytes — including the
    // swapchain-only glow/chrome passes every offscreen readback misses — into
    // a counted-drop ring. See `video_tap`.
    pub(crate) video: Option<crate::video_tap::VideoTap>,
    // The per-frame inline-image texture (every distinct visible image stacked)
    // and its bind group. `None` until the first image frame; rebuilt when the
    // set of visible images changes, reused otherwise. PER-WINDOW. Cleared to
    // `None` on an image-free frame so nothing is bound or drawn.
    pub(crate) image_plane: Option<ImagePlane>,
    // The per-window settings-card overlay (P3 frosted card). `None` until the
    // first present that carries a card; reused at the same size, recreated on a
    // size change, cleared to `None` on a card-free present. Composited over the
    // swapchain in the blit pass, AFTER the main blit, with ALPHA_BLENDING —
    // matching the CPU `composite_tray` straight-alpha src-over (screen ==
    // introspection). Feature-OFF by default: the caller passes `None` unless the
    // settings card is active.
    pub(crate) tray_overlay: Option<TrayOverlay>,
    // SCISSORED DIRTY-ROW REPAINT (the window present path). Holds the PREVIOUS
    // presented frame's input + the renderer cursor state it was drawn with, so
    // `encode_present_frame` can consult `compute_dirty_rows` against it and
    // re-encode only the dirty rows (LoadOp::Load + a scissor over the dirty
    // band) into this window's persistent offscreen — which still holds that
    // prior frame. `None` until the first present (forces a full repaint), and
    // reset on any geometry change (the offscreen is recreated, so its prior
    // contents are gone). PER-WINDOW: window B's prior frame must never be
    // diffed against window A's input on the SHARED `GpuRenderer`. See
    // `encode_frame` / `RepaintScope`.
    pub(crate) present_prev: Option<PresentPrev>,
    // The PREVIOUS presented frame's input snapshot, kept RESIDENT across frames
    // (never reallocated in steady state): `encode_present_frame` updates it via
    // `clone_from` (Vec::clone_from reuses the destination's grid allocation when
    // dims are stable — a changed frame at the same size does ZERO grid alloc,
    // just a memcpy into the retained per-row buffers; only an actual
    // cluster/combining cell allocates its Box<str>/Box<[char]>, never on the
    // ASCII path). It holds a VALID prior frame iff `present_prev` is `Some`; the
    // reset-on-`render_input` / `render_no_readback` path clears `present_prev`
    // (invalidating it) but keeps the buffer's capacity for the next present's
    // `clone_from`. PER-WINDOW alongside `present_prev`.
    pub(crate) prev_input: RenderInput,
    // M3 phase B: the RAW per-screen EDR maximum for the screen THIS window is
    // on (`NSScreen.maximumExtendedDynamicRangeColorComponentValue`), set by the
    // frontend at attach and re-queried on monitor changes. Read through the
    // PROVEN `aterm_render::hdr::additive_headroom` sanitizer at present, so the
    // `Default` 0.0 (never set / SDR platform) degrades to headroom 0 — the EDR
    // aurora pass then provably adds nothing. PER-WINDOW: two windows on two
    // panels get each panel's own headroom.
    pub(crate) edr_max: f32,
    // M3 (Windows scRGB): the display's reference-white scale, `SDRwhite_nits / 80`
    // (scRGB 1.0 == 80 nits fixed). Applied to the base grid (blit) + aurora on the
    // Windows EDR present so content isn't dim. `Default` 0.0 is clamped to 1.0 at
    // present (never set / macOS / SDR ⇒ no scaling, byte-identical).
    pub(crate) sdr_white_scale: f32,
    // M1b sub-row scroll: a scratch texture the size of the offscreen, used to
    // stage the grid-band pixel shift (an in-place overlapping texture copy is UB,
    // so the band is copied here then back, shifted). `None` until the first
    // fractional-scroll frame; reused at the same dims, recreated on a resize.
    // PER-WINDOW alongside the offscreen. See `shift_offscreen_band`.
    pub(crate) shift_scratch: Option<wgpu::Texture>,
    // M1b: the `scroll_frac_px` PRESENTED last frame. A fractional frame mutates
    // the offscreen (the band shift), so the scissored dirty-row diff must not
    // compare against it: a nonzero frac THIS frame OR last frame forces a full
    // repaint (fresh untranslated offscreen) before the shift. `0` on every
    // whole-row frame ⇒ the scissored present path is byte-identical to pre-M1b.
    pub(crate) prev_frac: i32,
    // SDR-bloom PRESENT compositing target. The GPU comet bloom is a soft additive
    // HALO; compositing it into `offscreen` would force every scissored aurora tick
    // to REBUILD the whole halo band (the halo re-adds over any Load-preserved row),
    // defeating the incremental present. Instead `offscreen` stays the CLEAN
    // base+aurora scissor base, and each present COPIES it here and composites the
    // halo over this throwaway texture (never a scissor base), which the blit then
    // samples. `None` until the first bloom present; reused at the same dims,
    // recreated on a resize. PER-WINDOW alongside the offscreen. See
    // `ensure_present_offscreen` / `encode_bloom_halo`.
    pub(crate) present_offscreen: Option<PresentOffscreen>,
    // HEAT-SHIMMER staging scratch: a frame-sized texture the shimmer pass
    // copies its source region into before resampling it with displaced UVs —
    // sampling and writing the SAME texture in one pass is impossible, and an
    // overlapping same-texture copy is UB (the `shift_scratch` rationale).
    // Holds the sample bind group so the hot path allocates nothing. `None`
    // until the first shimmer present; reused at the same dims, recreated on a
    // resize. PER-WINDOW alongside the offscreen. See `encode_shimmer`.
    pub(crate) shimmer_scratch: Option<ShimmerScratch>,
    // HEADLESS PRESENT-REAL: the persistent VIRTUAL "swapchain" a glass-less
    // window's `video` recording presents into — a plain texture standing where
    // the swapchain texture would (same `RENDER_ATTACHMENT | COPY_SRC` usage a
    // copyable swapchain configures, `Bgra8Unorm` — `pick_surface_format`'s
    // first choice), sized to the caller-selected present geometry (the legacy
    // arm uses the raw integer-cell frame; cropped frontends use their shorter
    // visible frame). `Some` ONLY while a
    // `virtual_begin`-armed recording is in flight (the frontend drops the whole
    // present target at finalize — a window at rest holds no virtual texture).
    // See `present_virtual`.
    pub(crate) virtual_target: Option<VirtualTarget>,
}

impl WindowGpu {
    pub fn new() -> Self {
        Self::default()
    }

    /// CPU wall time spent encoding commands and calling `queue.submit` for the
    /// most recent successful present. This is not a GPU-completion timestamp.
    /// Swapchain acquire and final `present()` are deliberately outside this
    /// measurement: either may wait for compositor pacing on a healthy GPU.
    #[must_use]
    pub fn last_present_work_ns(&self) -> u64 {
        self.last_present_work_ns
    }

    /// M3 phase B: record the screen's EDR maximum for this window (raw; the
    /// present path sanitizes via `aterm_render::hdr` — see the field doc).
    pub fn set_edr_max(&mut self, v: f32) {
        self.edr_max = v;
    }

    /// M3 (Windows scRGB): record the display's reference-white scale
    /// (`SDRwhite_nits / 80`). Clamped to `>= 1.0` so an unset/degenerate value can
    /// never darken the grid; `1.0` means no scaling (macOS / SDR).
    pub fn set_sdr_white_scale(&mut self, v: f32) {
        self.sdr_white_scale = v.max(1.0);
    }

    /// The recorded raw EDR maximum ([`Self::set_edr_max`]; `0.0` = never set).
    /// Lets the frontend PRESERVE the value across the cache-reset points that
    /// replace a window's `WindowGpu` wholesale (font/theme rebuilds), so an
    /// HDR window keeps its headroom without an OS re-query.
    #[must_use]
    pub fn edr_max(&self) -> f32 {
        self.edr_max
    }

    /// Drop this window's prior-frame validity so the NEXT `present_input` is a
    /// FULL repaint (Clear + all rows) rather than a scissored dirty-row diff.
    /// Needed after a theme change: the steady selection band, idle cursor, and
    /// padding border are theme-derived but NOT content, so the dirty-row diff
    /// would leave them painted in the OLD theme until cell content changes.
    pub fn invalidate_present(&mut self) {
        self.present_prev = None;
    }
}

/// One decoded + footprint-scaled inline image, ready to upload as an RGBA8
/// texture region. Mirrors the CPU renderer's `DecodedImage`: the same
/// `decode_image_to_footprint` bytes, so the GPU samples (NEAREST, per cell) the
/// exact pixels the CPU `blit_image_cell` copies — the CPU/GPU parity gate.
struct GpuDecodedImage {
    /// Footprint pixel width (`cols * cell_w`).
    w: u32,
    /// Footprint pixel height (`rows * cell_h`).
    h: u32,
    /// `w * h * 4` straight-alpha RGBA bytes, or empty if the decode failed
    /// (a cached negative result: the image draws nothing but is not re-decoded).
    rgba: Vec<u8>,
}

/// Decoded-image LRU for the GPU renderer's inline-image pass. Each entry HOLDS
/// an `Arc<ImageData>` clone of its source image plus the footprint pixel size, so
/// each distinct placement is decoded+scaled at most once and reused across frames
/// (idle image frames do no decode work). Holding the `Arc` is what makes pointer
/// identity a SOUND key (matching the CPU `ImageCache`): while a decode is cached
/// the source allocation can't be freed, so its address can't be reused by a
/// DIFFERENT image (the ABA hazard a bare `Arc::as_ptr` key had — a freed+reused
/// same-footprint address would otherwise hit the stale decode AND, since the
/// `placements` map is keyed on that same pointer, falsely reuse the resident
/// plane). Lookup matches by `Arc::ptr_eq` + footprint. Empty (zero entries) in
/// the common image-free case.
#[derive(Default)]
pub(crate) struct GpuImageCache {
    /// `(held image Arc, fp_w, fp_h) -> decoded`, MRU at the back.
    entries: Vec<(
        std::sync::Arc<aterm_core::grid::extra::ImageData>,
        usize,
        usize,
        GpuDecodedImage,
    )>,
}

impl GpuImageCache {
    /// Maximum distinct decoded images retained — matches the CPU cap.
    const MAX: usize = 8;

    /// Look up a decoded image by `Arc::ptr_eq` identity + footprint, promoting it
    /// to MRU on a hit. Matching the held `Arc` (not a freed-able raw pointer) is
    /// what makes the hit sound across image free+reallocation.
    fn get(
        &mut self,
        image: &std::sync::Arc<aterm_core::grid::extra::ImageData>,
        fp_w: usize,
        fp_h: usize,
    ) -> Option<&GpuDecodedImage> {
        let idx = self.entries.iter().position(|(a, w, h, _)| {
            std::sync::Arc::ptr_eq(a, image) && *w == fp_w && *h == fp_h
        })?;
        let entry = self.entries.remove(idx);
        self.entries.push(entry);
        self.entries.last().map(|(_, _, _, v)| v)
    }

    /// Immutable lookup that does NOT promote to MRU — for batch reads (the
    /// image-plane pack) that hold several entries borrowed at once, where a
    /// `&mut self` `get` per item would conflict. Recency is unaffected: the
    /// pack runs right after the placements loop already promoted every placed
    /// key in `order` sequence, so re-promoting here would be a no-op anyway.
    fn peek(
        &self,
        image: &std::sync::Arc<aterm_core::grid::extra::ImageData>,
        fp_w: usize,
        fp_h: usize,
    ) -> Option<&GpuDecodedImage> {
        self.entries
            .iter()
            .find(|(a, w, h, _)| std::sync::Arc::ptr_eq(a, image) && *w == fp_w && *h == fp_h)
            .map(|(_, _, _, v)| v)
    }

    /// Insert a freshly decoded image, holding an `Arc` clone of its source to pin
    /// the allocation, and evicting the LRU entry past the cap.
    fn put(
        &mut self,
        image: std::sync::Arc<aterm_core::grid::extra::ImageData>,
        fp_w: usize,
        fp_h: usize,
        value: GpuDecodedImage,
    ) {
        if self.entries.len() >= Self::MAX {
            self.entries.remove(0);
        }
        self.entries.push((image, fp_w, fp_h, value));
    }
}

/// The per-frame inline-image texture: every DISTINCT image placement visible
/// this frame, stacked vertically into one RGBA8 texture, plus a map from
/// `(arc_ptr, fp_w, fp_h)` to the y-row at which that image's footprint begins.
/// A covered cell's quad samples its tile at `(cell_col*cw, image_y0 + cell_row*ch)`.
/// Rebuilt only on frames that actually carry images; `None`/cleared otherwise, so
/// image-free frames bind nothing and stay byte-identical to the pre-image path.
pub(crate) struct ImagePlane {
    /// The bind group samples the per-frame image texture; it owns a
    /// `TextureView` of that texture (which keeps the texture itself alive), so no
    /// separate `tex` handle is retained here.
    bind: wgpu::BindGroup,
    /// Texture dims (texels) — the divisor for the sampled UVs.
    w: u32,
    h: u32,
    /// `(arc_ptr, fp_w, fp_h) -> (y0_in_texture, fp_w, fp_h)` for each placement.
    placements: HashMap<(usize, usize, usize), (u32, u32, u32)>,
    /// The `Arc<ImageData>` of every PLACED image, retained for this resident
    /// plane's lifetime. `placements` is keyed by raw `Arc::as_ptr`, and the
    /// reuse fast-path compares those keys; without holding the Arcs, a placed
    /// image whose address is freed could be reused by a new same-footprint image
    /// (ABA), making the key compare equal while the texels are stale. The decode
    /// cache only pins up to `GpuImageCache::MAX` images, but `placements` can
    /// reference more, so the plane must pin its own placed set. Never read —
    /// existence is the guarantee.
    _pinned_images: Vec<std::sync::Arc<aterm_core::grid::extra::ImageData>>,
}

/// GPU terminal renderer. Holds its own GPU device (via [`GpuContext`]) and a CPU
/// [`Renderer`] used purely for font metrics + glyph coverage, so geometry and
/// rasterization match the CPU renderer exactly.
pub struct GpuRenderer {
    ctx: GpuContext,
    cpu: Renderer,
    /// The configured font family (`font_family` config / `$ATERM_FONT`), kept so an
    /// IN-PLACE font/theme rebuild (`set_font_theme`: zoom, config hot-reload, Retina
    /// auto-scale) re-resolves the SAME family instead of silently falling back to the
    /// system monospace. Without this, a Retina rebuild on the first frame dropped a
    /// configured family out of the box on the GPU backend.
    // Read only by the native font-discovery rebuild (`set_font_theme`), which is
    // cfg'd out on wasm (no system fonts in the browser) — hence unused there.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    font_family: Option<String>,
    theme: Theme,
    uniform_buf: wgpu::Buffer,
    uniform_bg: wgpu::BindGroup,
    /// Last `(w, h, text_blend)` written into the SHARED screen-uniform buffer. The
    /// buffer is ONE per renderer while N windows encode through it, so the
    /// write-skip memo must live WITH the buffer (renderer-level, not per-window):
    /// two windows sharing the buffer each "matched" while it held the OTHER
    /// window's value (stale uniforms → a garbled frame that never self-corrected).
    /// W2: the key includes the text-blend mode (the uniform carries it), so a mode
    /// flip re-uploads. Value-keyed, so a single window (or same-size, same-mode
    /// windows) still skips the steady-state re-upload.
    uniform_written: Option<(u32, u32, u32)>,
    atlas_bgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    bg_pipeline: wgpu::RenderPipeline,
    /// The bg quad shader with ALPHA_BLENDING instead of REPLACE, for the
    /// TRANSLUCENT cursor fill (`cursor_opacity < 1`): the fill blends OVER the
    /// rendered cell in linear light (== CPU `blend_rect`), and its alpha
    /// composites Porter-Duff over the target alpha (== the CPU transmittance
    /// byte). Never bound at the default opacity, so the opaque path keeps the
    /// exact historical pipeline.
    cursor_blend_pipeline: wgpu::RenderPipeline,
    glyph_pipeline: wgpu::RenderPipeline,
    color_glyph_pipeline: wgpu::RenderPipeline,
    /// PREMULTIPLIED ADDITIVE pipeline for the LUMEN cursor aurora (One/One).
    glow_add_pipeline: wgpu::RenderPipeline,
    /// PHOSPHOR rain bright-head halos: the glow-add pipeline's radial twin
    /// (`vs_rain_glow`/`fs_rain_glow`, `RainGlowInstance` layout, same One/One
    /// blend + additive target) — the only stream with an elliptical falloff.
    rain_glow_pipeline: wgpu::RenderPipeline,
    /// `HaloMode::Over` radial VEILS (light-theme smoke/steam): the rain-glow
    /// pipeline with the deco source-over blend state (ALPHA_BLENDING) on the
    /// SAME additive target — `fs_rain_glow_over` emits the straight colour
    /// with the integer falloff weight as alpha, so the fixed-function
    /// source-over is byte-exact with the CPU `over_rgb` on native.
    rain_glow_over_pipeline: wgpu::RenderPipeline,
    /// EMBERFORGE FirePatch pipelines (see `build_cell_pipelines`): the
    /// per-pixel fire-field pair on the additive Unorm view — `fire_add`
    /// One/One (`fs_fire_add` == CPU `add_sat(fire_field_add)`), `fire_over`
    /// deco source-over (`fs_fire_over` == CPU `over_rgb(fire_field_over)`).
    fire_add_pipeline: wgpu::RenderPipeline,
    fire_over_pipeline: wgpu::RenderPipeline,
    /// GPU-only cursor-comet BLOOM: the additive composite pipeline + its
    /// bind-group layout, a LINEAR sampler for the half-res upsample, and the
    /// tunables uniform. See [`build_bloom_resources`].
    bloom_bgl: wgpu::BindGroupLayout,
    bloom_sampler: wgpu::Sampler,
    bloom_pipeline: wgpu::RenderPipeline,
    bloom_uniform_buf: wgpu::Buffer,
    /// Whether the GPU bloom layer runs. ON by default ("batteries included"); the
    /// CPU/GPU differential tests flip it OFF via [`GpuRenderer::set_bloom`] so the
    /// parity-critical base render stays byte-exact.
    enable_bloom: bool,
    /// M3 phase B: config `hdr_glow` (DEFAULT OFF). Feeds the two proven gate
    /// seams — `format_plan::hdr_swapchain_wants_f16` at surface attach and
    /// `format_plan::hdr_present_plan` at present. With this false everything
    /// M3-phase-B is inert by the SdrInvariance proof (HdrPresentGate).
    hdr_glow: bool,
    /// EDR aurora pass resources (pipeline + swapchain-space uniform), built
    /// lazily on the first HDR present (`ensure_hdr_glow_pipeline`) so the
    /// default (hdr off) path allocates nothing. The pipeline targets
    /// `Rgba16Float` (the only swapchain format the plan enables the pass for).
    hdr_glow_pipeline: Option<wgpu::RenderPipeline>,
    hdr_glow_uniform_buf: Option<wgpu::Buffer>,
    hdr_glow_bg: Option<wgpu::BindGroup>,
    /// The ONE bind-group layout both glow-boost pipelines (EDR f16 + SDR Unorm)
    /// build against, so the shared uniform bind group is compatible with either
    /// by object identity (no reliance on BGL deduplication). Created with the
    /// first boost pipeline.
    glow_boost_bgl: Option<wgpu::BindGroupLayout>,
    /// SDR glow-boost pass (the swapchain-side crown on a NON-f16 present):
    /// pipeline keyed by the actual swapchain format (rebuilt if it ever
    /// changes — an HDR toggle rebuilds the surface), sharing the EDR pass's
    /// uniform buffer + bind group (the two passes are mutually exclusive per
    /// present — `format_plan::sdr_boost_pass`). Lazy like the EDR resources.
    sdr_glow_pipeline: Option<(wgpu::TextureFormat, wgpu::RenderPipeline)>,
    /// Config `cursor_glow_sdr_boost` (0..=1; 0 = off). Combined with the theme
    /// background's luma into the per-present budget by
    /// `aterm_render::hdr::sdr_glow_budget` (proven bounded ≤ 0.35).
    sdr_glow_boost: f32,
    /// ATTACK-SMOOTHED SDR crown level: the budget actually applied this present,
    /// eased toward the computed target with a ~45ms rise so the crown BLOOMS IN
    /// over a few frames instead of strobing to full brightness on the first
    /// keystroke frame (the "cursor flashes / character looks late" report — the
    /// instant-onset wash also briefly masked the fresh glyph). Falls with the
    /// quads' own fade; resets to 0 when the glow stream empties, so every fresh
    /// burst re-blooms softly. Renderer-level (shared across windows — one
    /// focused cursor animates at a time; a cross-window blend is imperceptible).
    sdr_glow_level: f32,
    /// Timestamp of the last smoothing step (`None` until the first glow present).
    sdr_glow_level_at: Option<web_time::Instant>,
    /// Sparkle-word feline paw: textured coverage, ALPHA_BLENDING (== CPU `blend`).
    deco_over_pipeline: wgpu::RenderPipeline,
    /// Sparkle-word profanity sparkle: textured coverage, One/One premultiplied
    /// additive (== CPU `add_sat`).
    deco_add_pipeline: wgpu::RenderPipeline,
    /// The sparkle-word sprite coverage atlas (R8), rebuilt when the cell size
    /// changes. `None` until the first frame that carries decorations.
    deco_atlas: Option<DecoAtlas>,
    /// SCENE ("Living Panels") sprite pipelines: `scene_over` (RGBA8 atlas, src-over,
    /// sRGB view) draws the world UNDER the text; `scene_add` (One/One additive, Unorm
    /// view) draws the sun/firefly/comet glow. Both reuse `vs_glyph` + the glyph layout.
    scene_over_pipeline: wgpu::RenderPipeline,
    scene_add_pipeline: wgpu::RenderPipeline,
    /// The uploaded RGBA8 scene atlas texture + bind group, version-cached so it
    /// re-uploads only on a skin/theme rebake. `None` until the first scene frame.
    scene_atlas: Option<SceneTex>,
    /// LINEAR sampler for the scene atlas (smooth sprite scaling — unlike the NEAREST
    /// glyph `sampler`). Compatible with `atlas_bgl` (its sampler type is Filtering).
    scene_sampler: wgpu::Sampler,
    /// The uploaded RGBA8 peeking-CAT atlas (Sparkle Words v2 `cat_quads`) +
    /// bind group, version-cached like `scene_atlas` (re-uploads only when the
    /// host `CatBaker` bumps `SceneAtlas::version`). Bound with the NEAREST
    /// glyph `sampler` — cats are baked at exact destination size (1:1), so no
    /// filtering happens on either backend (the CPU stamp is integer-stepped
    /// NEAREST too). `None` until the first cat frame.
    cat_atlas: Option<SceneTex>,
    /// The uploaded RGBA8 FREE-sprite atlas (`RenderInput::free_atlas`, the
    /// arbitrary-rect `FreeSprite` layer) + bind group, version-cached like
    /// `cat_atlas`. Bound with the NEAREST glyph `sampler` — v1 free sprites are
    /// the cat regime (bake == dest size, 1:1; `FreeSampler::Linear` deferred).
    /// `None` until the first free-sprite frame.
    free_atlas: Option<SceneTex>,
    /// The uploaded RGBA8 PHOSPHOR rain-glyph atlas (`RenderInput::rain_atlas`,
    /// the `RainBaker` white-coverage tiles) + bind group, version-cached like
    /// `cat_atlas`. Bound with the NEAREST glyph `sampler` — rain tiles are
    /// baked at exact cell size (1:1, the cat regime), so the GPU must read the
    /// same unfiltered texel the CPU's integer-stepped NEAREST stamp reads.
    /// `None` until the first rain frame.
    rain_atlas: Option<SceneTex>,
    /// Bloom tunables (config `cursor_trail_bloom_strength`/`_radius`), set via
    /// [`GpuRenderer::set_bloom_params`]. Default to [`BLOOM_STRENGTH`]/[`BLOOM_RADIUS`].
    bloom_strength: f32,
    bloom_radius: f32,
    /// HEAT-SHIMMER pass resources (see [`SHIMMER_SHADER`]), built lazily on
    /// the first shimmer frame so a shimmer-off renderer allocates nothing.
    shimmer: Option<ShimmerResources>,
    /// Whether the heat shimmer runs. ON by default (quality-first; config
    /// `cursor_fire_shimmer`). The CPU/GPU differential + scissor byte-compare
    /// suites flip it OFF via [`GpuRenderer::set_shimmer`] — the same
    /// parity-class neutralization as `set_bloom(false)`, doubly necessary here
    /// because the phase is wall-clock (two renders never agree).
    enable_shimmer: bool,
    /// The shimmer's wall-clock origin: `phase = (now - epoch) % 100 s`. The
    /// deliberate present-time wall-clock term of this pass — the documented
    /// bloom-class exception, exactly like the SDR crown's attack envelope.
    shimmer_epoch: web_time::Instant,
    /// Test pin for the phase ([`GpuRenderer::set_shimmer_phase_for_test`]):
    /// `Some` replaces the wall clock so readbacks/arms are deterministic.
    shimmer_phase_pin: Option<f32>,
    // On-glass blit (the offscreen frame -> swapchain), built once and reused.
    blit_shader: wgpu::ShaderModule,
    blit_bgl: wgpu::BindGroupLayout,
    blit_layout: wgpu::PipelineLayout,
    blit_sampler: wgpu::Sampler,
    blit_uniform_buf: wgpu::Buffer,
    /// Last value written into the SHARED blit uniform buffer — the same
    /// renderer-level (buffer-keyed) memo rationale as `uniform_written`, so one
    /// window's bell invert / drop overlay never leaks into another's blit.
    blit_uniform_written: Option<BlitUniform>,
    /// Blit pipelines keyed by swapchain format. Built EAGERLY in
    /// `create_window_surface` (the format is known there) so the compile is off
    /// the first-FRAME path; `present_input` then only looks one up.
    /// `ensure_blit_pipeline` remains the idempotent lazy fallback (and the
    /// test/readback path's builder) for any format not seen at surface-create.
    blit_pipelines: HashMap<wgpu::TextureFormat, wgpu::RenderPipeline>,
    /// Settings-card TRAY overlay infra (separate module from the blit so bindings
    /// never collide). `tray_pipelines` is keyed by swapchain format like
    /// `blit_pipelines`, built lazily via `ensure_tray_pipeline`. The sampler is
    /// LINEAR (the card may be scaled); the uniform buf carries the per-present
    /// placement rect. All resident / shared across windows.
    tray_shader: wgpu::ShaderModule,
    tray_bgl: wgpu::BindGroupLayout,
    tray_layout: wgpu::PipelineLayout,
    tray_sampler: wgpu::Sampler,
    tray_uniform_buf: wgpu::Buffer,
    tray_pipelines: HashMap<wgpu::TextureFormat, wgpu::RenderPipeline>,
    /// Cached swapchain-format choice: the NON-sRGB format `pick_surface_format`
    /// picks is adapter/platform-stable (Bgra8Unorm on macOS/Metal, offered by
    /// every on-screen surface on this single adapter), so the blocking
    /// `surface.get_capabilities` round-trip is done ONCE and reused for later
    /// window-attaches. Clear this to restore per-attach querying.
    cached_surface_format: Option<wgpu::TextureFormat>,
    /// Persisted mono (R8) + colour (RGBA8) atlases — `None` until the first
    /// frame builds them. Reused untouched when a frame's glyph set is a subset
    /// of what's resident; grown incrementally on a miss.
    mono_res: Option<ResidentAtlas>,
    color_res: Option<ResidentAtlas>,
    /// The full glyph-key set currently resident across both atlases. A frame
    /// whose keys are a SUBSET of this skips all atlas work.
    resident_keys: BTreeSet<GlyphKey>,
    /// Count of atlas textures created (full (re)builds, not reuses). The
    /// persistence test asserts this does NOT advance across an unchanged frame.
    atlas_tex_creations: u64,
    // Persistent per-frame vertex streams. Previously these were allocated from
    // scratch every frame via `create_buffer_init` (one driver allocation per
    // stream per frame). Now each stream owns a buffer that is reused across
    // frames: we `write_buffer` into it when the frame's bytes fit the current
    // capacity, and only recreate (grow) the buffer when they don't. Capacities
    // are tracked alongside so we know when a grow is needed.
    vbufs: VertexBuffers,
    // Persistent per-frame instance streams + glyph-key set. Cleared (capacity
    // retained) at the top of each `encode_frame` instead of re-allocated. See
    // `Instances`.
    inst: Instances,
    // TEST/DIAGNOSTIC counters so a test can prove the gate is actually taken
    // (otherwise a "gate" that never fires would still pass a byte-identity
    // test). Counts gate-hits and gate-misses through `render_input_cached`.
    gate_hits: u64,
    gate_misses: u64,
    // NOTE: present_prev (Option<PresentPrev>) and prev_input (RenderInput) moved
    // to per-window `WindowGpu` so the scissored present path diffs each window's
    // input against ITS OWN prior frame — the shared GpuRenderer must hold no
    // per-window prior-frame state, or window B's present would diff against
    // window A's last frame and corrupt the scissor decision.
    // Persistent dirty-row scratch for the scissored present path. The
    // `&mut Vec<bool>` handed to `compute_dirty_rows` each present, taken out of
    // `self` across the encode and restored after. Resident across frames so a
    // stable-dimension changed frame allocates no per-call dirty Vec. The flags —
    // and thus the scissor decision — are byte-identical to the old per-call Vec.
    dirty_scratch: Vec<bool>,
    // Persistent per-row ligature-plan scratch for `encode_frame`. Taken out of
    // `self` across the encode (mem::take, like `dirty_scratch`) and restored
    // after the last reader. `row_glyph_plan` clears+resizes each inner Vec, so
    // reuse keeps inner capacities across frames and is byte-identical; both
    // readers are guarded by `row_active`, so stale rows are never observed.
    row_plans: Vec<Vec<aterm_render::ColumnGlyph>>,
    // Persistent decoration-rect scratch for `encode_frame`. Taken out of `self`
    // across the decoration loop (mem::take, like `row_plans`/`dirty_scratch`) and
    // restored after. The `underline_rects_into`/`strike_overline_rects_into`
    // variants clear the Vec before refilling, so reuse keeps its capacity across
    // frames and is byte-identical to the old per-frame `Vec::new()`.
    deco_rects_scratch: Vec<[usize; 4]>,
    // Persistent ink-skipped underline rect scratch (W7): the rects surviving
    // `deco::intersect_rect_spans`, swapped with `deco_rects_scratch` per cell.
    deco_skip_scratch: Vec<[usize; 4]>,
    // Persistent per-cell glyph-ink column scratch for the shared
    // `underline_keep_spans_into` (W7). Mirrors `deco_rects_scratch`.
    curl_ink_scratch: Vec<bool>,
    // Persistent kept-span scratch for descender ink-skip / undercurl quads
    // (W7). Mirrors `deco_rects_scratch`.
    curl_spans_scratch: Vec<(usize, usize)>,
    // Persistent ligature break-column scratch for `encode_frame`, threaded through
    // `ligature_break_cols_for_row_into` (mem::take, like `row_plans`). The `_into`
    // helper clears it before each row, so reuse across rows/frames keeps its
    // capacity and is byte-identical to the old per-row `Vec::new()`. A
    // selection-drag full repaint no longer allocates a break Vec per active row.
    ligature_break_scratch: Vec<usize>,
    // Persistent bloom-glow instance scratch for `build_bloom_glow` (the aurora
    // halo's extract source). Taken out of `self` (mem::take) and cleared per call,
    // like the other scratch streams above, so the default-on glow path — non-empty
    // on essentially every present while typing/fading — no longer heap-allocates a
    // fresh Vec<BgInstance> every frame. Byte-identical: the same instances are built
    // and uploaded; only the backing allocation is reused.
    bloom_glow_scratch: Vec<BgInstance>,
    // TEST/DIAGNOSTIC counters: how many `present_input` frames took the SCISSOR
    // (dirty-row) path vs a FULL repaint. The byte-identity test asserts the
    // scissor path is actually exercised on typing/cursor frames and that
    // DECDHL/DECDWL... no: DECDWL is safe; DECDHL/selection/scroll frames fall
    // back to full.
    scissor_taken: u64,
    full_repaints: u64,
    // TEST/DIAGNOSTIC: how many settings-card `write_texture` uploads actually
    // ran (an unchanged card skips the upload — see `ensure_tray_overlay`).
    tray_uploads: u64,
    // TEST/DIAGNOSTIC: total instances built in the LAST `encode_frame` (sum of
    // all eight streams). A scissored 1-row frame builds ~`1/rows` of a full
    // frame's instances — the proportional-to-dirty-rows win the benchmark
    // reports.
    last_instances: usize,
    // NOTE: image_cache (GpuImageCache) and image_plane (Option<ImagePlane>) moved
    // to per-window `WindowGpu` so window B's inline images never leak into window
    // A — the shared GpuRenderer must hold no per-window image state.
}

/// The previous presented frame's overlay state, for the scissored dirty-row
/// repaint. The persistent offscreen still holds this frame's pixels, so the next
/// present can update only the rows that differ from it.
///
/// The prior input SNAPSHOT itself lives in the always-resident per-window
/// `WindowGpu::prev_input` buffer (updated via `clone_from`, reusing its
/// allocation) rather than here — so a stable-dimension changed frame stores the
/// new prior frame with ZERO grid allocation. This struct, when `Some`, is the
/// validity flag for that buffer: it is `Some` exactly when `prev_input` holds a
/// VALID prior presented frame at the current offscreen dims.
pub(crate) struct PresentPrev {
    /// The blink phase that frame was drawn with.
    blink_phase: bool,
    /// The cursor-style override that frame was drawn with.
    cursor_style_override: Option<CursorStyle>,
    /// Grid Y origin used by the resident offscreen. A top-pad-only move keeps
    /// framebuffer dimensions stable, so geometry cannot be inferred from size.
    grid_top: usize,
}

/// What portion of the offscreen `encode_frame` must repaint:
///   * `Full` — clear the whole target and draw every row (the always-correct
///     path; byte-identical to the original encode).
///   * `Dirty(dirty)` — the persistent offscreen already holds the prior frame;
///     preserve it (`LoadOp::Load`), scissor to the dirty rows' bounding band, and
///     draw ONLY the dirty rows (`dirty[r]`). A re-shaded dirty row gets the
///     IDENTICAL instances the full path would build for it, so its pixels are
///     bit-identical; untouched rows are preserved by Load.
///
/// `Dirty` BORROWS the per-row flags from the caller's persistent dirty scratch
/// (`GpuRenderer::dirty_scratch`, taken out across the encode) rather than owning
/// a fresh `Vec`, so a changed frame allocates no dirty Vec. The flags are
/// byte-identical to the old owned form — only the allocation lifetime changed.
enum RepaintScope<'a> {
    Full,
    Dirty(&'a [bool]),
}

/// The GPU dirty-gate cache: the previous frame's `render_input_cached` inputs
/// and the pixels that were rendered + read back for them. Because the GPU has
/// no persistent CPU-side framebuffer to borrow (it renders on-device and reads
/// back), the gate must remember the prior frame's pixels itself so it can re-
/// present them on an unchanged frame.
pub(crate) struct GpuGateCache {
    /// The previous frame's input snapshot (cloned), for the gate comparison.
    input: RenderInput,
    /// The blink phase the previous frame was drawn with.
    blink_phase: bool,
    /// The cursor-style override the previous frame was drawn with.
    cursor_style_override: Option<CursorStyle>,
    /// Grid Y origin used for the cached readback pixels.
    grid_top: usize,
    /// The pixels read back for the previous frame (the cached framebuffer the
    /// gate re-presents verbatim on a hit). Byte-identical to what the GPU would
    /// re-render for an unchanged input.
    frame: Frame,
}

/// Concrete projection of both production GPU layout caches onto the
/// `AsymmetricPadLayout` cache-origin variables. Exposed only as a diagnostic
/// view; the cache records themselves remain private and authoritative.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AsymmetricPadGpuCacheProjection {
    pub grid_top: usize,
    pub present_cached_grid_top: Option<usize>,
    pub gate_cached_grid_top: Option<usize>,
}

/// The persistent offscreen render target (the frame the GPU draws into and the
/// blit samples from). Previously a fresh `Rgba8Unorm` texture + view was created
/// EVERY presented frame (~6.4 MB at 1080p), and the blit-source view + blit bind
/// group were rebuilt EVERY present. Now they are resident: a frame at the same
/// `(w, h)` reuses `tex`/`view`/`blit_bind` untouched; only a `None` field or a
/// dimension change (resize) recreates them. Lifetime-only change — the same draws
/// land in the same texture, so the swapchain blit stays byte-identical.
pub(crate) struct Offscreen {
    tex: wgpu::Texture,
    /// The base (`Rgba8Unorm`) view — present blit + readback (stored sRGB bytes,
    /// verbatim) and the glow/deco-ADD additive passes (raw 8-bit add == CPU `add_sat`).
    view: wgpu::TextureView,
    /// `Rgba8UnormSrgb` view aliasing the same texture, attached by the base
    /// bg/glyph/deco-over/cursor passes so fixed-function ALPHA_BLENDING composites
    /// in LINEAR light (matching the CPU linear `blend`). Storage stays sRGB-encoded,
    /// so blit/readback (which use `view`) are unchanged — screenshot==glass holds.
    view_srgb: wgpu::TextureView,
    /// The blit-source bind group (samples `tex` into the swapchain). Built ONCE
    /// when the offscreen is (re)created and reused every present.
    blit_bind: wgpu::BindGroup,
    w: u32,
    h: u32,
    /// Half-res bloom target: the comet glow is re-rendered into the bloom target's
    /// `view`, blurred, and additively composited back over the offscreen `view`. Its
    /// `bind` samples `tex` (group 0 of the bloom pipeline). `None` when bloom is off.
    /// Rebuilt with the offscreen on a resize. See `encode_frame`.
    bloom: Option<BloomTarget>,
}

/// The SDR-bloom PRESENT compositing target (see [`WindowGpu::present_offscreen`]).
/// A copy of the clean `offscreen` with the additive comet HALO composited over it,
/// which the on-glass blit (and the readback helpers) sample INSTEAD of `offscreen`
/// on a bloom present. Kept OFF the scissor path so `offscreen` never carries the
/// halo — the incremental dirty set stays proportional to real content change. Its
/// `blit_bind` is the drop-in replacement blit source (same layout/sampler/uniform
/// as the offscreen's), so the blit pipeline + uniform are byte-identical; only the
/// sampled texture differs. Resident: reused at the same dims, recreated on a resize.
pub(crate) struct PresentOffscreen {
    tex: wgpu::Texture,
    /// The default (`offscreen_format`) view — the halo composite's One/One draw
    /// target (same format as the offscreen's `view`, so the proven bloom pipeline
    /// attaches it unchanged).
    view: wgpu::TextureView,
    /// The blit-source bind group (samples `tex` into the swapchain). Built when the
    /// texture is (re)created; a drop-in for the offscreen's `blit_bind`.
    blit_bind: wgpu::BindGroup,
    w: u32,
    h: u32,
}

/// The HEAT-SHIMMER staging texture + its sample bind group (see
/// [`WindowGpu::shimmer_scratch`]): the shimmer pass copies the hot region
/// (plus [`SHIMMER_COPY_MARGIN`]) of its target here, then resamples it with
/// displaced UVs back into the target — the same stage-then-write shape as the
/// M1b `shift_scratch`. The bind group samples `tex` through the linear
/// ClampToEdge bloom sampler with the shimmer uniform (group 0 of the shimmer
/// pipeline). Resident: reused at the same dims, recreated on a resize.
pub(crate) struct ShimmerScratch {
    tex: wgpu::Texture,
    /// Group 0 of the shimmer pipeline: `tex` + linear sampler + uniform.
    bind: wgpu::BindGroup,
    w: u32,
    h: u32,
}

/// HEADLESS PRESENT-REAL (see [`WindowGpu::virtual_target`]): the persistent
/// offscreen texture a glass-less window's recording presents into via
/// [`GpuRenderer::present_virtual`] — the virtual twin of the swapchain
/// texture, with the same `RENDER_ATTACHMENT | COPY_SRC` usage a copyable
/// swapchain configures so the UNCHANGED [`crate::video_tap::VideoTap`] copies
/// its exact bytes. No `present()` ever runs on it — no photons, disclosed by
/// the recording's `mode` label.
pub(crate) struct VirtualTarget {
    tex: wgpu::Texture,
    w: u32,
    h: u32,
}

/// The format the virtual present target (and its tap) uses: the SDR default a
/// real swapchain gets (`pick_surface_format`'s first choice on every major
/// backend), so the virtual arm exercises the same blit pipeline / uniform
/// bytes as a real on-glass present. EDR/f16 virtual swapchains are explicitly
/// out of scope v1 (the recording's `format` field discloses this).
const VIRTUAL_PRESENT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8Unorm;

/// The DESTINATION of one present pass — the parameters over which
/// [`GpuRenderer::present_to_view`] abstracts the swapchain: the render-pass
/// attachment view, the texture the VIDEO tap copies (the same texture `view`
/// views), its `(w, h)` and format, and whether the destination composites
/// PostMultiplied (the M5 translucency arm; always `false` off-glass). Bundled
/// so both present arms hand the seam one value.
struct PresentDest<'a> {
    view: &'a wgpu::TextureView,
    tex: &'a wgpu::Texture,
    w: u32,
    h: u32,
    format: wgpu::TextureFormat,
    translucent: bool,
}

/// The half-resolution bloom render target + its composite bind group, resident on
/// the [`Offscreen`] and rebuilt only on a resize.
pub(crate) struct BloomTarget {
    /// The half-res texture. Held to keep it alive for `view`/`bind`; not read.
    #[allow(dead_code)]
    tex: wgpu::Texture,
    view: wgpu::TextureView,
    /// Samples `tex` + the linear sampler + the bloom uniform (the composite pass's
    /// group 0).
    bind: wgpu::BindGroup,
    bw: u32,
    bh: u32,
}

/// The persistent per-frame instance streams + glyph-key set. Previously fresh
/// `Vec`s and a `BTreeSet` were allocated EVERY `encode_frame`; now they are
/// hoisted and `.clear()`ed (capacity retained) at the start of each frame, so the
/// steady state does zero heap allocation for them. Identical contents built in
/// identical order → byte-identical. Field order mirrors `VertexBuffers`.
#[derive(Default)]
struct Instances {
    keys: BTreeSet<GlyphKey>,
    bg: Vec<BgInstance>,
    /// Inline-image (iTerm2 OSC 1337) cell tiles: one quad per image-covered cell
    /// sampling its tile of the per-frame image texture. Drawn AFTER the colour
    /// glyphs (the image owns the cell, so no glyph competes) and BEFORE the
    /// decorations/cursor — over the cell bg, alpha-blended, exactly like the CPU
    /// `blit_image_cell` straight-alpha-over composite. Empty (and so a no-op) for
    /// every image-free frame, keeping the text path byte-identical.
    image: Vec<GlyphInstance>,
    glyph: Vec<GlyphInstance>,
    /// EMBERFORGE GLYPH CONTRAST-HALO: the dark warm dilation ring around every
    /// glyph engulfed by the fire (a `fire_halo` cell in a `fire_patch` frame),
    /// drawn OVER the flame body and UNDER the (never-recoloured) glyph ink so
    /// the letterform stays legible against the flame. Each engulfed glyph
    /// pushes a small ring of instances (the glyph's own coverage quad shifted
    /// by the shared `aterm_render::HALO_DILATE_OFFSETS`, tinted
    /// `aterm_render::HALO_IN_FIRE_RGB` at the cell's engulfment-scaled
    /// `aterm_render::fire_halo_alpha`), drawn through the `deco_over_pipeline`
    /// binding the MONO atlas — straight source-over (`fs_deco_over` == CPU
    /// `blend`), the exact CPU dilation replicated as instanced quads. Empty for
    /// every fire-free frame, so ordinary text is byte-identical.
    glyph_halo: Vec<GlyphInstance>,
    color: Vec<GlyphInstance>,
    cursor: Vec<BgInstance>,
    deco: Vec<BgInstance>,
    /// W7 AA undercurl quads (textured coverage from the deco atlas' curl
    /// sprite, ALPHA_BLENDING via `fs_deco_over`), drawn AFTER the solid
    /// `deco` quads and BEFORE the aurora — the GPU twin of the CPU pass 3b
    /// `blend_undercurl`. Empty for a frame with no curly underline.
    curl: Vec<GlyphInstance>,
    /// Cursor motion-trail quads (the comet EMBER BED): pre-blended solid
    /// fills drawn through the bg pipeline (REPLACE) at the END of the base-bg
    /// half — UNDER the interpose light and UNDER the glyph ink (== the CPU
    /// phase B2b inside `composite_free`) — so the text the comet sweeps stays
    /// readable while the bed still sits under the live cursor. Empty (a
    /// no-op) for every frame with no active trail; the comet style fills it
    /// natively (`trail_is_comet`) and the web pipeline can drive it too; the
    /// shared `blend_rgb` makes the fill byte-identical to the CPU
    /// `draw_trail`.
    trail: Vec<BgInstance>,
    /// LUMEN aurora: PREMULTIPLIED ADDITIVE light quads (comet/bloom/ring/sparks),
    /// drawn through `glow_add_pipeline` (One/One) AFTER the glyphs/deco and UNDER
    /// the cursor. The colour is premultiplied, so the alpha lane is irrelevant.
    /// Empty when no aurora is live.
    glow_add: Vec<BgInstance>,
    /// GLOW-HALO cursor-effect radial light (`glow_halo`: EMBERFORGE round
    /// embers / crown): a [`RainGlowInstance`] One/One stream through the SAME
    /// `rain_glow_pipeline` as `rain_add`, drawn right AFTER the aurora and
    /// BEFORE the nova/rain (== the CPU `draw_glow_halo` after `draw_glow`).
    /// Row-gated by `row_active` — every halo lies in one row band. NOT fed
    /// into the bloom pass: the radial falloff is self-soft, so bloom stays
    /// `glow_add`-only for now. Empty for every halo-free frame.
    glow_halo: Vec<RainGlowInstance>,
    /// `HaloMode::Over` glow-halo VEILS (light-theme smoke/steam): the Over
    /// half of the `glow_halo` stream's per-mode split, drawn right AFTER the
    /// Add half through `rain_glow_over_pipeline` (deco source-over blend
    /// state on the Unorm view == CPU `over_rgb` — veils dim light, matching
    /// the CPU's Add-then-Over sweep). Row-gated by `row_active` like its Add
    /// sibling. Empty for every Over-free frame — then no extra draw is
    /// issued and the Add path is byte-identical to before.
    glow_halo_over: Vec<RainGlowInstance>,
    /// EMBERFORGE UNDER-GLYPH light (`glow_under`: the flame BODY the engulfed
    /// glyphs silhouette against): a [`BgInstance`] One/One stream through the
    /// SAME `glow_add_pipeline` as the aurora, but drawn in its OWN Unorm pass
    /// BETWEEN the base pass's bg/sprite draws and its glyph draws — under the
    /// letterforms, over everything below them (== the CPU phase-B3
    /// `draw_glow_under` between phases B2 and C). Row-gated by `row_active` —
    /// every quad lies in one row band. NOT fed into the bloom pass (bloom
    /// stays `glow_add`-only). Empty for every glow_under-free frame — and
    /// then NO extra pass opens (the fused base pass is byte-identical to
    /// before).
    glow_under: Vec<BgInstance>,
    /// EMBERFORGE per-pixel FIRE FIELD patches (`fire_patch`): the flame BODY
    /// at full art scale, drawn in the SAME A2 Unorm interpose pass right
    /// AFTER `glow_under` (== the CPU phase-B3b `draw_fire_patch` after
    /// `draw_glow_under`), split per mode — `fire_add` through the One/One
    /// `fire_add_pipeline`, then `fire_over` through the source-over
    /// `fire_over_pipeline` (the CPU's Add-then-Over sweep). Row-gated by
    /// `row_active`. Deliberately NOT fed into the bloom pass — the field is
    /// self-luminous (bloom stays `glow_add`-only).
    fire_add: Vec<FireInstance>,
    fire_over: Vec<FireInstance>,
    /// SUPERNOVA additive light (Sparkle Words v2 `nova_add`: crown / shockwave
    /// ring / rays): the second glow-shaped One/One stream, drawn through the
    /// SAME `glow_add_pipeline` right after the aurora on the Unorm view (== CPU
    /// `add_sat`, byte-exact over background — `draw_nova` runs right after
    /// `draw_glow`). Empty for every nova-free frame.
    nova_add: Vec<BgInstance>,
    /// PHOSPHOR rain bright-head halos (`rain_add`): the third glow-shaped
    /// One/One stream, drawn through the SAME `glow_add_pipeline` right after
    /// the nova on the Unorm view (== CPU `add_sat`; the host premultiplies the
    /// colour). Row-gated by `row_active` like the nova — every `GlowQuad` lies
    /// in one row band. Empty for every rain-free frame.
    rain_add: Vec<RainGlowInstance>,
    /// `HaloMode::Over` rain VEILS: the Over half of the `rain_add` stream's
    /// per-mode split, drawn right AFTER the Add half through
    /// `rain_glow_over_pipeline` — the same contract as `glow_halo_over`, so
    /// the two [`RainHalo`]-shaped streams cannot drift. Empty for every
    /// Over-free frame.
    rain_add_over: Vec<RainGlowInstance>,
    /// Sparkle-word FELINE paw quads (textured coverage, ALPHA_BLENDING), drawn
    /// after the aurora and UNDER the cursor — the GPU twin of the CPU `Over`
    /// branch of `draw_decorations`. Empty for a frame with no feline match.
    wdeco_over: Vec<GlyphInstance>,
    /// Sparkle-word PROFANITY sparkle quads (textured coverage, One/One additive),
    /// drawn with the paw and under the cursor — the GPU twin of the CPU `Add`
    /// branch. Empty for a frame with no profanity match.
    wdeco_add: Vec<GlyphInstance>,
    /// SCENE ("Living Panels") source-over sprite quads (the meadow/cat world), drawn
    /// UNDER the text via `scene_over_pipeline`. Empty for a frame with no active scene.
    scene_over: Vec<GlyphInstance>,
    /// SCENE additive-light sprite quads (sun/fireflies/comet), drawn via
    /// `scene_add_pipeline` (One/One). Empty when no scene light is live.
    scene_add: Vec<GlyphInstance>,
    /// PHOSPHOR rain glyph sprites (`rain_quads`), drawn UNDER the text in
    /// `emit_base_pre` BETWEEN `scene_over` and `cat_over` (cats walk on rain)
    /// — same src-over scene pipeline, sampling the RAIN atlas through the
    /// NEAREST sampler (bake == dest size, 1:1). Built UNCONDITIONALLY (no
    /// `row_active` filter), like cats/free: the dirty-band scissor clips, and
    /// `compute_dirty_rows`' band fill guarantees every admitted pixel lands on
    /// a rebuilt row. Empty for a rain-free frame.
    rain_under: Vec<GlyphInstance>,
    /// Peeking-CAT sprite quads (Sparkle Words v2 `cat_quads`), drawn UNDER the
    /// text in `emit_base_pre` right after `scene_over` — same src-over pipeline,
    /// but sampling the CAT atlas through the NEAREST sampler (bake == dest size,
    /// 1:1 — no filtering on either backend). Empty for a cat-free frame.
    cat_over: Vec<GlyphInstance>,
    /// FREE-floating UNDER-TEXT sprites (`FreeSprite` with `FreeZ::UnderText`),
    /// drawn in `emit_base_pre` right after `cat_over` and before the glyphs —
    /// same src-over pipeline, sampling the FREE atlas through the NEAREST
    /// sampler (v1 is NEAREST-only; `FreeSampler::Linear` is deferred). Empty
    /// for a frame with no free sprites.
    free_under: Vec<GlyphInstance>,
    /// FREE-floating OVER-TEXT sprites (`FreeZ::OverText`), drawn after the
    /// wdeco streams and immediately BEFORE the cursor. Empty when unused.
    free_over: Vec<GlyphInstance>,
    cursor_block: Vec<BgInstance>,
    cursor_glyph: Vec<GlyphInstance>,
    cursor_color: Vec<GlyphInstance>,
}

impl Instances {
    /// Empty all streams (retaining capacity) for a fresh frame.
    fn clear(&mut self) {
        self.keys.clear();
        self.bg.clear();
        self.image.clear();
        self.glyph.clear();
        self.glyph_halo.clear();
        self.color.clear();
        self.cursor.clear();
        self.deco.clear();
        self.curl.clear();
        self.trail.clear();
        self.glow_add.clear();
        self.glow_halo.clear();
        self.glow_halo_over.clear();
        self.glow_under.clear();
        self.fire_add.clear();
        self.fire_over.clear();
        self.nova_add.clear();
        self.rain_add.clear();
        self.rain_add_over.clear();
        self.wdeco_over.clear();
        self.wdeco_add.clear();
        self.scene_over.clear();
        self.scene_add.clear();
        self.rain_under.clear();
        self.cat_over.clear();
        self.free_under.clear();
        self.free_over.clear();
        self.cursor_block.clear();
        self.cursor_glyph.clear();
        self.cursor_color.clear();
    }
}

/// The eight persistent per-frame vertex streams (one `VertexBuffer` each).
/// Field order/labels mirror the instance vecs built in `encode_frame`.
struct VertexBuffers {
    bg: VertexBuffer,
    image: VertexBuffer,
    glyph: VertexBuffer,
    glyph_halo: VertexBuffer,
    color: VertexBuffer,
    cursor: VertexBuffer,
    deco: VertexBuffer,
    curl: VertexBuffer,
    trail: VertexBuffer,
    glow_add: VertexBuffer,
    glow_halo: VertexBuffer,
    glow_halo_over: VertexBuffer,
    glow_under: VertexBuffer,
    fire_add: VertexBuffer,
    fire_over: VertexBuffer,
    nova_add: VertexBuffer,
    rain_add: VertexBuffer,
    rain_add_over: VertexBuffer,
    /// The UNGATED aurora glow instances (every `cursor_glow_add` quad, not
    /// row-clipped to the scissor band) — the bloom halo's extract source. The
    /// halo is composited at PRESENT time over the WHOLE frame, so it must be
    /// spread from the full comet even on a scissored frame whose `glow_add`
    /// carries only the dirty-row subset. Built by `build_bloom_glow`; empty
    /// (allocates nothing) whenever there is no glow. See `encode_bloom_halo`.
    bloom_glow: VertexBuffer,
    wdeco_over: VertexBuffer,
    wdeco_add: VertexBuffer,
    scene_over: VertexBuffer,
    scene_add: VertexBuffer,
    rain_under: VertexBuffer,
    cat_over: VertexBuffer,
    free_under: VertexBuffer,
    free_over: VertexBuffer,
    cursor_block: VertexBuffer,
    cursor_glyph: VertexBuffer,
    cursor_color: VertexBuffer,
}

impl VertexBuffers {
    fn new(device: &wgpu::Device) -> Self {
        Self {
            bg: VertexBuffer::new(device, "aterm-gpu bg instances"),
            image: VertexBuffer::new(device, "aterm-gpu image instances"),
            glyph: VertexBuffer::new(device, "aterm-gpu glyph instances"),
            glyph_halo: VertexBuffer::new(device, "aterm-gpu glyph contrast-halo instances"),
            color: VertexBuffer::new(device, "aterm-gpu colour glyph instances"),
            cursor: VertexBuffer::new(device, "aterm-gpu cursor instances"),
            deco: VertexBuffer::new(device, "aterm-gpu decoration instances"),
            curl: VertexBuffer::new(device, "aterm-gpu undercurl instances"),
            trail: VertexBuffer::new(device, "aterm-gpu cursor trail instances"),
            glow_add: VertexBuffer::new(device, "aterm-gpu lumen glow-add instances"),
            glow_halo: VertexBuffer::new(device, "aterm-gpu glow halo instances"),
            glow_halo_over: VertexBuffer::new(device, "aterm-gpu glow halo over instances"),
            glow_under: VertexBuffer::new(device, "aterm-gpu emberforge glow-under instances"),
            fire_add: VertexBuffer::new(device, "aterm-gpu emberforge fire add instances"),
            fire_over: VertexBuffer::new(device, "aterm-gpu emberforge fire over instances"),
            nova_add: VertexBuffer::new(device, "aterm-gpu supernova nova-add instances"),
            rain_add: VertexBuffer::new(device, "aterm-gpu rain add instances"),
            rain_add_over: VertexBuffer::new(device, "aterm-gpu rain add over instances"),
            bloom_glow: VertexBuffer::new(device, "aterm-gpu bloom halo source (ungated glow)"),
            wdeco_over: VertexBuffer::new(device, "aterm-gpu sparkle-word paw instances"),
            wdeco_add: VertexBuffer::new(device, "aterm-gpu sparkle-word sparkle instances"),
            scene_over: VertexBuffer::new(device, "aterm-gpu scene over instances"),
            scene_add: VertexBuffer::new(device, "aterm-gpu scene add instances"),
            rain_under: VertexBuffer::new(device, "aterm-gpu rain under instances"),
            cat_over: VertexBuffer::new(device, "aterm-gpu cat over instances"),
            free_under: VertexBuffer::new(device, "aterm-gpu free under-text instances"),
            free_over: VertexBuffer::new(device, "aterm-gpu free over-text instances"),
            cursor_block: VertexBuffer::new(device, "aterm-gpu cursor block fill"),
            cursor_glyph: VertexBuffer::new(device, "aterm-gpu cursor cut-out glyph"),
            cursor_color: VertexBuffer::new(device, "aterm-gpu cursor colour glyph"),
        }
    }
}

/// A reusable `VERTEX | COPY_DST` buffer plus its byte capacity. Grows (recreates
/// the underlying buffer) only when a frame's contents exceed `capacity`.
struct VertexBuffer {
    buf: wgpu::Buffer,
    capacity: u64,
    label: &'static str,
}

impl VertexBuffer {
    /// Start at zero capacity; the first non-empty upload grows it. No GPU
    /// allocation happens for streams that are never used (e.g. colour-emoji
    /// buffers on a frame with no emoji).
    fn new(device: &wgpu::Device, label: &'static str) -> Self {
        Self {
            buf: Self::alloc(device, label, 0),
            capacity: 0,
            label,
        }
    }

    fn alloc(device: &wgpu::Device, label: &'static str, size: u64) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    /// Upload `bytes` into the buffer, growing it first if they don't fit.
    /// Returns a slice of exactly `bytes.len()` bytes ready to bind, or `None`
    /// when there is nothing to draw (empty stream) — the caller skips that pass,
    /// exactly as the old `Option<Buffer>` gating did. Identical contents and
    /// draw counts to the per-frame-allocated path; only the buffer's lifetime
    /// changes.
    fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bytes: &[u8],
    ) -> Option<wgpu::BufferSlice<'_>> {
        // The slice-precondition decision (the GpuEncode.tla `NeverSliceEmpty` rule):
        // a buffer is bound/sliced ONLY when it holds at least one instance. An empty
        // stream returns `None` so the caller skips the draw (wgpu panics on an empty
        // `buf.slice(..)` — the exact 4ab4eb9 bug). Factored through `should_slice`
        // so the real precondition is testable headlessly without a GPU.
        if !should_slice(bytes.len()) {
            return None;
        }
        // `write_buffer` requires the write size to be a multiple of
        // COPY_BUFFER_ALIGNMENT (4). Our instance structs are 16-byte aligned so
        // `bytes.len()` is already a multiple of 4, but round up defensively and
        // ensure capacity covers the padded length.
        let needed = align_up(bytes.len() as u64, wgpu::COPY_BUFFER_ALIGNMENT);
        if needed > self.capacity {
            // Absolute floor against the buffer-size DoS: `create_buffer` validates the
            // requested size against `max_buffer_size`, and with no `on_uncaptured_error`
            // handler installed a violation aborts the process. `encode_frame`'s
            // framebuffer-bounded loops already cap the instance count to the on-screen
            // area for any realistic cell size; this is the defense-in-depth net for the
            // pathological remainder (e.g. a sub-pixel cell on a MAX_GRID grid). If even
            // the tight `needed` won't fit, skip the draw — return `None` exactly like an
            // empty stream, so the caller omits this layer and the frame DEGRADES
            // (missing layer) instead of CRASHING.
            let max_buf = device.limits().max_buffer_size;
            if needed > max_buf {
                return None;
            }
            // Grow geometrically (next power of two, padded) to amortise the cost of
            // bursts that keep enlarging the stream, but never past `max_buffer_size`
            // (the pow2 rounding could otherwise overshoot the limit); `needed <= max_buf`
            // guarantees the cap still covers this write.
            let new_cap =
                align_up(needed.next_power_of_two(), wgpu::COPY_BUFFER_ALIGNMENT).min(max_buf);
            self.buf = Self::alloc(device, self.label, new_cap);
            self.capacity = new_cap;
        }
        queue.write_buffer(&self.buf, 0, bytes);
        Some(self.buf.slice(..bytes.len() as u64))
    }
}

/// The slice-precondition decision shared by every per-frame vertex stream — the
/// real implementation of `GpuEncode.tla`'s `NeverSliceEmpty` / `SliceImpliesFill`
/// rule: a stream is sliced/bound ONLY when it holds at least one instance
/// (`byte_len > 0`). The bg-instance path (the `Encode` action) calls
/// `bg_buf.slice(..)` exactly when this is `true`; an empty (zero-cell) frame
/// returns `None` from [`InstanceBuf::upload`] and draws nothing — the exact fix for
/// the wgpu "buffer slices can not be empty" panic (4ab4eb9). Pure + GPU-free so the
/// precondition is conformance-checked headlessly (`tests/conformance_gpuencode.rs`).
#[must_use]
pub fn should_slice(byte_len: usize) -> bool {
    byte_len != 0
}

/// Round `n` up to the next multiple of `align` (a power of two).
fn align_up(n: u64, align: u64) -> u64 {
    (n + align - 1) & !(align - 1)
}

/// Build the per-frame uniform buffer, its (vertex-visible) bind-group layout,
/// and the bind group that wires the buffer to binding 0. Extracted from
/// [`GpuRenderer::new_with_family`] verbatim.
fn build_uniform_resources(
    device: &wgpu::Device,
) -> (wgpu::Buffer, wgpu::BindGroupLayout, wgpu::BindGroup) {
    let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("aterm-gpu uniforms"),
        size: std::mem::size_of::<Uniforms>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let uniform_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("aterm-gpu uniform bgl"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            // W2: fs_glyph reads `u.text_blend` (the corrected-alpha remap
            // gate), so the uniform is visible to BOTH stages now.
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });
    let uniform_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("aterm-gpu uniform bg"),
        layout: &uniform_bgl,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: uniform_buf.as_entire_binding(),
        }],
    });

    (uniform_buf, uniform_bgl, uniform_bg)
}

/// Build the glyph-atlas bind-group layout (a fragment-visible texture +
/// sampler) and the NEAREST sampler used to read it. Extracted from
/// [`GpuRenderer::new_with_family`] verbatim.
fn build_atlas_resources(device: &wgpu::Device) -> (wgpu::BindGroupLayout, wgpu::Sampler) {
    let atlas_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("aterm-gpu atlas bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });

    // NEAREST: the atlas holds the exact CPU coverage bytes; nearest sampling
    // at texel centres reproduces them with no interpolation smear.
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("aterm-gpu nearest"),
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..Default::default()
    });

    (atlas_bgl, sampler)
}

/// Half-resolution divisor for the bloom texture: the glow halo is soft, so a
/// quarter of the pixels is plenty and keeps the blur cheap + naturally wider.
const BLOOM_DOWNSCALE: u32 = 2;
/// Bloom additive strength — fraction of the blurred glow added back over the frame.
const BLOOM_STRENGTH: f32 = 0.85;
/// Bloom blur radius in half-res texels — how far the radiant halo spreads.
const BLOOM_RADIUS: f32 = 2.2;

/// Build the GPU-only BLOOM resources: shader module, the bind-group layout
/// (half-res glow texture + linear sampler + `BloomUniform`), a LINEAR clamp
/// sampler (smooth half-res upsample), the additive composite pipeline (One/One
/// over the `Rgba8Unorm` offscreen, RGB-only write-mask so it never perturbs the
/// alpha the blit relies on), and the uniform buffer. See [`BLOOM_SHADER`].
fn build_bloom_resources(
    device: &wgpu::Device,
    target: wgpu::TextureFormat,
) -> (
    wgpu::BindGroupLayout,
    wgpu::Sampler,
    wgpu::RenderPipeline,
    wgpu::Buffer,
) {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("aterm-gpu bloom shader"),
        source: wgpu::ShaderSource::Wgsl(BLOOM_SHADER.into()),
    });
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("aterm-gpu bloom bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    });
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("aterm-gpu bloom linear"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..Default::default()
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("aterm-gpu bloom layout"),
        bind_group_layouts: &[Some(&bgl)],
        immediate_size: 0,
    });
    let add = wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::One,
        operation: wgpu::BlendOperation::Add,
    };
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("aterm-gpu bloom composite pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_fs"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_bloom"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: target,
                blend: Some(wgpu::BlendState {
                    color: add,
                    alpha: add,
                }),
                write_mask: wgpu::ColorWrites::COLOR,
            })],
        }),
        multiview_mask: None,
        cache: None,
    });
    let uniform = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("aterm-gpu bloom uniform"),
        size: std::mem::size_of::<BloomUniform>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    (bgl, sampler, pipeline, uniform)
}

/// Number of per-column heat bands the shimmer derives from the glow stream
/// (packed as 16 vec4s in the uniform). 64 bands over the hot region give
/// sub-cell horizontal resolution at any realistic comet width.
const SHIMMER_BANDS: usize = 64;
/// Hard cap on the shimmer displacement, device px. The effective amplitude is
/// `min(cell_h / 18, this)` so small fonts shimmer proportionally less; the
/// shader re-clamps the final vector to it either way.
const SHIMMER_AMP_PX: f32 = 1.5;
/// How far the haze rises above the hot band, in cell heights.
const SHIMMER_RISE_CELLS: f32 = 3.5;
/// Host-side gain on the premultiplied glow brightness feeding the heat proxy:
/// a fully-lit comet head (max channel ~0.8 after premul) reaches full shimmer
/// strength while the faint tail barely stirs the air.
const SHIMMER_HEAT_GAIN: f32 = 1.35;
/// Stage-copy margin around the pass rect, px: displacement (<= 1.5) plus the
/// bilinear footprint (1) plus slack, so a displaced sample never reads texels
/// the staging copy did not cover.
const SHIMMER_COPY_MARGIN: u32 = 4;
/// Wall-clock phase wrap, seconds. Every time coefficient in `SHIMMER_SHADER`
/// is a two-decimal cycles/s rate, so `rate * 100` is an integer cycle count
/// and the wrap is continuous; wrapping keeps `f32` phase precision bounded
/// over arbitrarily long uptimes.
const SHIMMER_PHASE_WRAP_S: f32 = 100.0;

/// The lazy HEAT-SHIMMER pass resources (see [`SHIMMER_SHADER`]): bind-group
/// layout (staged frame copy + linear sampler + `ShimmerUniform`), the REPLACE
/// pipeline over the offscreen format (write-mask COLOR — like the bloom it
/// never perturbs the alpha the blit relies on), and the uniform buffer. Built
/// on the first shimmer frame (`ensure_shimmer_resources`), so a shimmer-off
/// renderer allocates nothing. The linear ClampToEdge `bloom_sampler` is
/// reused for the staged-copy sample.
struct ShimmerResources {
    bgl: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
    uniform_buf: wgpu::Buffer,
}

/// Build the shimmer resources against the offscreen `target` format (the pass
/// draws into `off.view` / the present-offscreen view, exactly like the bloom
/// composite). Modeled on [`build_bloom_resources`].
fn build_shimmer_resources(device: &wgpu::Device, target: wgpu::TextureFormat) -> ShimmerResources {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("aterm-gpu shimmer shader"),
        source: wgpu::ShaderSource::Wgsl(SHIMMER_SHADER.into()),
    });
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("aterm-gpu shimmer bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("aterm-gpu shimmer layout"),
        bind_group_layouts: &[Some(&bgl)],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("aterm-gpu shimmer pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_fs"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_shimmer"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: target,
                // REPLACE (no blend): the fragment IS the refracted frame.
                blend: None,
                // RGB only — the blit's alpha is never perturbed (bloom rule).
                write_mask: wgpu::ColorWrites::COLOR,
            })],
        }),
        multiview_mask: None,
        cache: None,
    });
    let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("aterm-gpu shimmer uniform"),
        size: std::mem::size_of::<ShimmerUniform>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    ShimmerResources {
        bgl,
        pipeline,
        uniform_buf,
    }
}

/// The derived hot region for one frame's shimmer pass: the scissored pass
/// rect (`x0..x1`, `y0..y1` — strictly ABOVE the hot band), the hot band's top
/// edge + rise for the vertical envelope, and the per-column heat proxies.
/// Derived host-side from `input.cursor_glow_add` exactly as
/// [`GpuRenderer::build_bloom_glow`] derives the bloom source — zero new
/// plumbing. `None` (== no pass, byte-identical present) whenever the stream
/// is empty, dark, or has no air above it.
struct ShimmerRegion {
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
    /// The frame dims the region was derived against (the pass target's).
    fw: u32,
    fh: u32,
    hot_top: f32,
    rise: f32,
    band_x0: f32,
    band_w: f32,
    heat: [f32; SHIMMER_BANDS],
}

/// Build the six offscreen render pipelines that draw into the `Rgba8Unorm`
/// framebuffer: the per-cell background fill, the linear-light coverage-blended
/// mono-glyph pass, the straight-RGBA colour-emoji pass, the LUMEN additive glow
/// (One/One), and the sparkle-word `deco_over` (alpha) + `deco_add` (additive)
/// decoration passes. Extracted from [`GpuRenderer::new_with_family`] verbatim. The
/// bg/glow pipelines bind only the uniforms; the glyph/colour/deco pipelines also
/// bind the atlas. Base over/replace pipelines target the sRGB view when `srgb`
/// (downlevel/GLES falls back to the plain Unorm view — see header); the additive
/// pipelines always target the plain Unorm view.
/// The 13 per-cell-stream pipelines [`build_cell_pipelines`] returns, in
/// declaration order: bg, cursor_blend, glyph, color_glyph, glow_add,
/// rain_glow, rain_glow_over, fire_add, fire_over, deco_over, deco_add,
/// scene_over, scene_add.
type CellPipelines = (
    wgpu::RenderPipeline,
    wgpu::RenderPipeline,
    wgpu::RenderPipeline,
    wgpu::RenderPipeline,
    wgpu::RenderPipeline,
    wgpu::RenderPipeline,
    wgpu::RenderPipeline,
    wgpu::RenderPipeline,
    wgpu::RenderPipeline,
    wgpu::RenderPipeline,
    wgpu::RenderPipeline,
    wgpu::RenderPipeline,
    wgpu::RenderPipeline,
);

fn build_cell_pipelines(
    device: &wgpu::Device,
    shader: &wgpu::ShaderModule,
    uniform_bgl: &wgpu::BindGroupLayout,
    atlas_bgl: &wgpu::BindGroupLayout,
    // The base bg/glyph/deco-over/cursor passes attach off.view_srgb so fixed-function
    // ALPHA_BLENDING composites in LINEAR light (== CPU `blend`); the ADDITIVE passes
    // (glow_add/deco_add, One/One) attach off.view (== CPU `add_sat` on native; linear
    // on downlevel where off.view is sRGB). Both formats come from the single source
    // of truth (GpuContext::offscreen_srgb_view_format / offscreen_format) so the
    // pipeline target can never drift from its attachment — see crate::format_plan.
    base_target: wgpu::TextureFormat,
    additive_target: wgpu::TextureFormat,
) -> CellPipelines {
    let bg_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("aterm-gpu bg layout"),
        bind_group_layouts: &[Some(uniform_bgl)],
        immediate_size: 0,
    });
    let glyph_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("aterm-gpu glyph layout"),
        bind_group_layouts: &[Some(uniform_bgl), Some(atlas_bgl)],
        immediate_size: 0,
    });

    let bg_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("aterm-gpu bg pipeline"),
        layout: Some(&bg_layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_bg"),
            compilation_options: Default::default(),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<BgInstance>() as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &BG_ATTRS,
            }],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_bg"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: base_target,
                blend: Some(wgpu::BlendState::REPLACE),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    });

    // Identical to `bg_pipeline` except the blend state: ALPHA_BLENDING for the
    // translucent-cursor fill (see the `cursor_blend_pipeline` field doc).
    let cursor_blend_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("aterm-gpu cursor blend pipeline"),
        layout: Some(&bg_layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_bg"),
            compilation_options: Default::default(),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<BgInstance>() as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &BG_ATTRS,
            }],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_bg"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: base_target,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    });

    let glyph_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("aterm-gpu glyph pipeline"),
        layout: Some(&glyph_layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_glyph"),
            compilation_options: Default::default(),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<GlyphInstance>() as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &GLYPH_ATTRS,
            }],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_glyph"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: base_target,
                // out = fg*cov + dst*(1-cov) in LINEAR light (sRGB target).
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    });

    // Colour-emoji pipeline: same layout/vertex/blend as the mono glyph
    // pipeline, but the `fs_glyph_color` fragment samples an RGBA8 atlas
    // straight (no coverage tint). Reuses `atlas_bgl` — RGBA8Unorm is a
    // filterable float texture, so the layout is identical.
    let color_glyph_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("aterm-gpu colour-glyph pipeline"),
        layout: Some(&glyph_layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_glyph"),
            compilation_options: Default::default(),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<GlyphInstance>() as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &GLYPH_ATTRS,
            }],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_glyph_color"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: base_target,
                // out = rgb*a + dst*(1-a) in LINEAR light (sRGB target).
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    });

    // LUMEN aurora pipeline: PREMULTIPLIED ADDITIVE light. Reuses the bg layout +
    // `vs_bg`/`fs_bg` + `BG_ATTRS` verbatim (fs_bg already returns the full vec4);
    // the ONLY difference is the blend = One/One (out = src + dst) and a COLOR
    // write-mask (RGB only — never perturbs the offscreen alpha the blit relies on).
    // Over the linear Rgba8Unorm target this is min(255, src+dst) for 8-bit operands,
    // BIT-IDENTICAL to the CPU `add_sat` — so the dazzle costs nothing in parity.
    let add = wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::One,
        operation: wgpu::BlendOperation::Add,
    };
    let glow_add_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("aterm-gpu glow additive pipeline"),
        layout: Some(&bg_layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_bg"),
            compilation_options: Default::default(),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<BgInstance>() as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &BG_ATTRS,
            }],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: shader,
            // Glow is RAW (no sRGB decode): renders into the Unorm view so One/One
            // add stays byte-exact with the CPU `add_sat` (native); on WebGL2 it
            // targets the sRGB texture (add in linear) — see additive_target.
            entry_point: Some("fs_glow"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: additive_target,
                blend: Some(wgpu::BlendState {
                    color: add,
                    alpha: add,
                }),
                write_mask: wgpu::ColorWrites::COLOR,
            })],
        }),
        multiview_mask: None,
        cache: None,
    });

    // PHOSPHOR rain halo pipeline: the glow-add twin with the radial falloff
    // shaders + `RainGlowInstance` layout. Same `bg_layout`, One/One blend, and
    // additive target, so byte-parity with `fs_glow` holds wherever the flat
    // aurora does — only the per-pixel weight differs.
    let rain_glow_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("aterm-gpu rain halo pipeline"),
        layout: Some(&bg_layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_rain_glow"),
            compilation_options: Default::default(),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<RainGlowInstance>() as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &RAIN_GLOW_ATTRS,
            }],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_rain_glow"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: additive_target,
                blend: Some(wgpu::BlendState {
                    color: add,
                    alpha: add,
                }),
                write_mask: wgpu::ColorWrites::COLOR,
            })],
        }),
        multiview_mask: None,
        cache: None,
    });

    // HaloMode::Over radial-veil pipeline: the rain-glow pipeline with the deco
    // source-over blend state (wgpu::BlendState::ALPHA_BLENDING — the SAME state
    // `deco_over` proves against the CPU) swapped in, on the SAME additive
    // (Unorm) target with the COLOR write-mask. `fs_rain_glow_over` computes the
    // identical integer falloff weight and emits (straight rgb, wt/255), so the
    // fixed-function `src·a + dst·(1−a)` rounds to the CPU `over_rgb` byte on
    // native — light-theme smoke/steam veils, byte-exact like the adds.
    let rain_glow_over_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("aterm-gpu rain halo over pipeline"),
        layout: Some(&bg_layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_rain_glow"),
            compilation_options: Default::default(),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<RainGlowInstance>() as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &RAIN_GLOW_ATTRS,
            }],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_rain_glow_over"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: additive_target,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::COLOR,
            })],
        }),
        multiview_mask: None,
        cache: None,
    });

    // EMBERFORGE FirePatch pipelines: the per-pixel fire-field pair. Both run
    // `vs_fire` over the `FireInstance` layout on the SAME additive (Unorm)
    // target with the COLOR write-mask; they differ only in fragment + blend:
    // `fs_fire_add` is One/One premultiplied light (== CPU `add_sat` of
    // `fire_field_add`, the aurora contract) and `fs_fire_over` is the deco
    // source-over blend state (== CPU `over_rgb` of `fire_field_over`, the
    // rain_glow_over contract) — byte-exact wherever the One/One adds are.
    let fire_add_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("aterm-gpu fire add pipeline"),
        layout: Some(&bg_layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_fire"),
            compilation_options: Default::default(),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<FireInstance>() as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &FIRE_ATTRS,
            }],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_fire_add"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: additive_target,
                blend: Some(wgpu::BlendState {
                    color: add,
                    alpha: add,
                }),
                write_mask: wgpu::ColorWrites::COLOR,
            })],
        }),
        multiview_mask: None,
        cache: None,
    });
    let fire_over_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("aterm-gpu fire over pipeline"),
        layout: Some(&bg_layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_fire"),
            compilation_options: Default::default(),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<FireInstance>() as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &FIRE_ATTRS,
            }],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_fire_over"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: additive_target,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::COLOR,
            })],
        }),
        multiview_mask: None,
        cache: None,
    });

    // Sparkle-word decoration pipelines: both reuse the glyph layout + `vs_glyph`
    // + `GLYPH_ATTRS` (textured quad sampling the deco coverage atlas in group 1),
    // differing only in fragment shader + blend. `deco_over` is ALPHA_BLENDING
    // (the feline paw, == CPU `blend`); `deco_add` is One/One premultiplied additive
    // (the profanity sparkle, == CPU `add_sat`), COLOR write-mask like the aurora.
    let deco_over_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("aterm-gpu deco-over pipeline"),
        layout: Some(&glyph_layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_glyph"),
            compilation_options: Default::default(),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<GlyphInstance>() as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &GLYPH_ATTRS,
            }],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_deco_over"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: base_target,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    });
    let deco_add_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("aterm-gpu deco-add pipeline"),
        layout: Some(&glyph_layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_glyph"),
            compilation_options: Default::default(),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<GlyphInstance>() as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &GLYPH_ATTRS,
            }],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_deco_add"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: additive_target,
                blend: Some(wgpu::BlendState {
                    color: add,
                    alpha: add,
                }),
                write_mask: wgpu::ColorWrites::COLOR,
            })],
        }),
        multiview_mask: None,
        cache: None,
    });

    // SCENE pipelines (Living Panels): mirror the deco pipelines exactly — same glyph
    // layout/vertex/attrs, sampling an RGBA8 scene atlas bound in group 1. `scene_over`
    // is ALPHA_BLENDING on the sRGB view (== the CPU pass-1c src-over stamp); `scene_add` is
    // One/One premultiplied additive on the Unorm view, COLOR write-mask (== CPU add_sat)
    // for the sun / firefly / comet glow.
    let scene_over_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("aterm-gpu scene-over pipeline"),
        layout: Some(&glyph_layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_glyph"),
            compilation_options: Default::default(),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<GlyphInstance>() as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &GLYPH_ATTRS,
            }],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_scene_over"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: base_target,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    });
    let scene_add_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("aterm-gpu scene-add pipeline"),
        layout: Some(&glyph_layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_glyph"),
            compilation_options: Default::default(),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<GlyphInstance>() as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &GLYPH_ATTRS,
            }],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_scene_add"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: additive_target,
                blend: Some(wgpu::BlendState {
                    color: add,
                    alpha: add,
                }),
                write_mask: wgpu::ColorWrites::COLOR,
            })],
        }),
        multiview_mask: None,
        cache: None,
    });

    (
        bg_pipeline,
        cursor_blend_pipeline,
        glyph_pipeline,
        color_glyph_pipeline,
        glow_add_pipeline,
        rain_glow_pipeline,
        rain_glow_over_pipeline,
        fire_add_pipeline,
        fire_over_pipeline,
        deco_over_pipeline,
        deco_add_pipeline,
        scene_over_pipeline,
        scene_add_pipeline,
    )
}

/// Build the format-independent on-glass blit infrastructure: the blit shader,
/// its bind-group layout (texture + sampler + invert uniform), the pipeline
/// layout, the NEAREST blit sampler, and the invert uniform buffer. The blit
/// pipeline itself depends on the swapchain format, so it is built lazily per
/// surface format in `present_input` and cached in `blit_pipelines`. Extracted
/// from [`GpuRenderer::new_with_family`] verbatim.
fn build_blit_resources(
    device: &wgpu::Device,
) -> (
    wgpu::ShaderModule,
    wgpu::BindGroupLayout,
    wgpu::PipelineLayout,
    wgpu::Sampler,
    wgpu::Buffer,
) {
    let blit_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("aterm-gpu blit shader"),
        source: wgpu::ShaderSource::Wgsl(BLIT_SHADER.into()),
    });
    let blit_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("aterm-gpu blit bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    });
    let blit_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("aterm-gpu blit layout"),
        bind_group_layouts: &[Some(&blit_bgl)],
        immediate_size: 0,
    });
    // NEAREST: a 1:1 framebuffer->swapchain blit, no interpolation smear.
    let blit_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("aterm-gpu blit nearest"),
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..Default::default()
    });
    let blit_uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("aterm-gpu blit invert uniform"),
        size: std::mem::size_of::<BlitUniform>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    (
        blit_shader,
        blit_bgl,
        blit_layout,
        blit_sampler,
        blit_uniform_buf,
    )
}

/// Build the tray-overlay shader, bind-group layout, pipeline layout, LINEAR
/// sampler, and placement uniform buffer. Mirrors [`build_blit_resources`]; the
/// per-format pipelines are built lazily in `ensure_tray_pipeline`.
fn build_tray_resources(
    device: &wgpu::Device,
) -> (
    wgpu::ShaderModule,
    wgpu::BindGroupLayout,
    wgpu::PipelineLayout,
    wgpu::Sampler,
    wgpu::Buffer,
) {
    let tray_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("aterm-gpu tray shader"),
        source: wgpu::ShaderSource::Wgsl(TRAY_SHADER.into()),
    });
    // Same triple as the blit bgl (texture + filtering sampler + uniform).
    let tray_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("aterm-gpu tray bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                // vs_tray reads the placement uniform.
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    });
    let tray_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("aterm-gpu tray layout"),
        bind_group_layouts: &[Some(&tray_bgl)],
        immediate_size: 0,
    });
    // LINEAR: the card may be scaled, so smooth interpolation rather than the
    // NEAREST blit smear.
    let tray_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("aterm-gpu tray linear"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..Default::default()
    });
    let tray_uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("aterm-gpu tray placement uniform"),
        size: std::mem::size_of::<TrayUniform>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    (
        tray_shader,
        tray_bgl,
        tray_layout,
        tray_sampler,
        tray_uniform_buf,
    )
}

impl GpuRenderer {
    /// Acquire a GPU and a CPU font face. `px`/`theme` must match the CPU
    /// renderer you want to reproduce.
    ///
    /// NATIVE ONLY: uses `pollster::block_on` (GPU init) + `std::thread::spawn`
    /// (font load) + system font discovery, none of which exist on the browser
    /// wasm target. The wasm path builds a [`GpuContext`] asynchronously and a CPU
    /// [`Renderer`] from injected font bytes, then assembles the renderer via
    /// [`GpuRenderer::from_parts`].
    #[cfg(not(target_arch = "wasm32"))]
    pub fn new(px: f32, theme: Theme) -> Result<Self, String> {
        Self::new_with_family(None, px, theme)
    }

    /// Like [`GpuRenderer::new`], but resolves a configured font FAMILY first
    /// (then `$ATERM_FONT`, then the built-in candidates), mirroring the CPU
    /// renderer's [`Renderer::from_system_with_family`]. `None` is identical to
    /// [`GpuRenderer::new`]. NATIVE ONLY (see [`GpuRenderer::new`]).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn new_with_family(family: Option<&str>, px: f32, theme: Theme) -> Result<Self, String> {
        // Cold-launch overlap: font resolution + rasterization (CPU-bound, GPU-
        // independent) and GPU adapter/device init (blocking driver round-trips)
        // are the two dominant serial costs of building the renderer. Run the font
        // load on a background thread while the GPU device initializes on this
        // thread, then join. They share no state (the font path touches no GPU
        // object), so this is pure scheduling — no work is eliminated, the two
        // legs just overlap, saving ~min(gpu_init, font_load) off cold start.
        let family_owned = family.map(String::from);
        let font_handle = std::thread::spawn(move || {
            let mut cpu = Renderer::from_system_with_family(family_owned.as_deref(), px, theme)?;
            // Warm the printable-ASCII glyph cache here (still off the critical path,
            // overlapping GPU init) so the first frame's atlas build doesn't
            // rasterize them on the hot path. Byte-identical output (cache fill only).
            cpu.prewarm_ascii();
            Some(cpu)
        });
        let ctx = GpuContext::new()?;
        let cpu = font_handle
            .join()
            .map_err(|_| "font-load thread panicked".to_string())?
            .ok_or("no system monospace font")?;
        Self::from_parts(ctx, cpu, family.map(String::from), theme)
    }

    /// Assemble a `GpuRenderer` from an already-acquired [`GpuContext`] and a
    /// pre-built CPU [`Renderer`] (font face). This is the PORTABLE core that does
    /// no GPU acquisition, no threads, and no font discovery — every wgpu pipeline
    /// is built here. The native constructors call it after their blocking init;
    /// the wasm WebGPU path calls it after awaiting the device + building the CPU
    /// face from injected font bytes (`Renderer::from_bytes`).
    pub fn from_parts(
        ctx: GpuContext,
        cpu: Renderer,
        font_family: Option<String>,
        theme: Theme,
    ) -> Result<Self, String> {
        let device = &ctx.device;

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("aterm-gpu shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });

        let (uniform_buf, uniform_bgl, uniform_bg) = build_uniform_resources(device);
        let (atlas_bgl, sampler) = build_atlas_resources(device);
        // LINEAR sampler for the SCENE atlas — the sprites scale to the band, so bilinear
        // reads keep the cats/sky smooth (the glyph `sampler` is NEAREST for coverage
        // exactness). `atlas_bgl`'s sampler binding is Filtering, so this is compatible.
        let scene_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("aterm-gpu scene linear"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });

        // Single source of truth for every offscreen colour-target format (see
        // crate::format_plan): base OVER/REPLACE + cursor + deco_over passes attach
        // off.view_srgb; the additive glow/deco-add, bloom, and tray passes attach
        // off.view. A pipeline and its attachment can no longer drift apart (C1/C2).
        let base_target = ctx.offscreen_srgb_view_format();
        let additive_target = ctx.offscreen_format();
        let (
            bg_pipeline,
            cursor_blend_pipeline,
            glyph_pipeline,
            color_glyph_pipeline,
            glow_add_pipeline,
            rain_glow_pipeline,
            rain_glow_over_pipeline,
            fire_add_pipeline,
            fire_over_pipeline,
            deco_over_pipeline,
            deco_add_pipeline,
            scene_over_pipeline,
            scene_add_pipeline,
        ) = build_cell_pipelines(
            device,
            &shader,
            &uniform_bgl,
            &atlas_bgl,
            base_target,
            additive_target,
        );

        let (blit_shader, blit_bgl, blit_layout, blit_sampler, blit_uniform_buf) =
            build_blit_resources(device);

        let (tray_shader, tray_bgl, tray_layout, tray_sampler, tray_uniform_buf) =
            build_tray_resources(device);

        // The bloom composite + extract attach off.view, so they use offscreen_format.
        let (bloom_bgl, bloom_sampler, bloom_pipeline, bloom_uniform_buf) =
            build_bloom_resources(device, ctx.offscreen_format());

        let vbufs = VertexBuffers::new(device);

        Ok(Self {
            ctx,
            cpu,
            font_family,
            theme,
            uniform_buf,
            uniform_bg,
            uniform_written: None,
            atlas_bgl,
            sampler,
            bg_pipeline,
            cursor_blend_pipeline,
            glyph_pipeline,
            color_glyph_pipeline,
            glow_add_pipeline,
            rain_glow_pipeline,
            rain_glow_over_pipeline,
            fire_add_pipeline,
            fire_over_pipeline,
            bloom_bgl,
            bloom_sampler,
            bloom_pipeline,
            bloom_uniform_buf,
            enable_bloom: true,
            hdr_glow: false,
            hdr_glow_pipeline: None,
            hdr_glow_uniform_buf: None,
            hdr_glow_bg: None,
            glow_boost_bgl: None,
            sdr_glow_pipeline: None,
            sdr_glow_boost: 0.0,
            sdr_glow_level: 0.0,
            sdr_glow_level_at: None,
            deco_over_pipeline,
            deco_add_pipeline,
            deco_atlas: None,
            scene_over_pipeline,
            scene_add_pipeline,
            scene_atlas: None,
            scene_sampler,
            cat_atlas: None,
            free_atlas: None,
            rain_atlas: None,
            bloom_strength: BLOOM_STRENGTH,
            bloom_radius: BLOOM_RADIUS,
            shimmer: None,
            enable_shimmer: true,
            shimmer_epoch: web_time::Instant::now(),
            shimmer_phase_pin: None,
            blit_shader,
            blit_bgl,
            blit_layout,
            blit_sampler,
            blit_uniform_buf,
            blit_uniform_written: None,
            blit_pipelines: HashMap::new(),
            tray_shader,
            tray_bgl,
            tray_layout,
            tray_sampler,
            tray_uniform_buf,
            tray_pipelines: HashMap::new(),
            cached_surface_format: None,
            mono_res: None,
            color_res: None,
            resident_keys: BTreeSet::new(),
            atlas_tex_creations: 0,
            vbufs,
            inst: Instances::default(),
            gate_hits: 0,
            gate_misses: 0,
            dirty_scratch: Vec::new(),
            row_plans: Vec::new(),
            deco_rects_scratch: Vec::new(),
            deco_skip_scratch: Vec::new(),
            curl_ink_scratch: Vec::new(),
            curl_spans_scratch: Vec::new(),
            ligature_break_scratch: Vec::new(),
            bloom_glow_scratch: Vec::new(),
            scissor_taken: 0,
            full_repaints: 0,
            tray_uploads: 0,
            last_instances: 0,
        })
    }

    /// Rebuild the font/theme IN PLACE without recreating the wgpu device — the
    /// device, all pipelines, and every window's swapchain stay valid (dropping the
    /// device would orphan every other window's surface). Only the CPU face and the
    /// glyph atlas are font-dependent: rebuild the face at the new px/theme and
    /// invalidate the atlas so the next frame re-rasterizes on the SAME device.
    ///
    /// NATIVE ONLY: re-resolves the face via system font discovery. The wasm path
    /// rebuilds the face from injected font bytes via [`GpuRenderer::set_face`].
    #[cfg(not(target_arch = "wasm32"))]
    pub fn set_font_theme(&mut self, px: f32, theme: Theme) -> Result<(), String> {
        self.set_font_family_theme(self.font_family.clone(), px, theme)
    }

    /// Atomically rebuild the primary face, configured family, and theme.
    ///
    /// Font discovery is fallible. Resolve the candidate face before publishing
    /// either the new family or theme so a failed live config reload leaves the
    /// complete renderer authority unchanged. This is the family-changing twin of
    /// [`Self::set_font_theme`]; callers must not stage `font_family` separately.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn set_font_family_theme(
        &mut self,
        family: Option<String>,
        px: f32,
        theme: Theme,
    ) -> Result<(), String> {
        self.set_font_family_theme_with(family, px, theme, Renderer::from_system_with_family)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn set_font_family_theme_with(
        &mut self,
        family: Option<String>,
        px: f32,
        theme: Theme,
        resolve: impl FnOnce(Option<&str>, f32, Theme) -> Option<Renderer>,
    ) -> Result<(), String> {
        let cpu = resolve(family.as_deref(), px, theme).ok_or("no system monospace font")?;
        self.font_family = family;
        self.set_face(cpu, theme);
        Ok(())
    }

    /// Swap in an already-built CPU face + theme IN PLACE (no device rebuild, no
    /// font discovery). The portable core of [`set_font_theme`]; the wasm path
    /// calls it directly with a face built from injected font bytes.
    /// Forward of the inner CPU rasterizer's
    /// [`fallback_parse_pending`](Renderer::fallback_parse_pending): true while
    /// a background fallback-face parse is in flight (provisional `.notdef`
    /// cells may be on glass). The frontend re-arms a redraw while pending and
    /// invalidates the window's present cache once it lands — the per-window
    /// damage diff cannot see the CPU renderer's `font_epoch`.
    pub fn fallback_parse_pending(&mut self) -> bool {
        self.cpu.fallback_parse_pending()
    }

    /// Forward of the inner CPU rasterizer's
    /// [`take_missing_font_classes`](Renderer::take_missing_font_classes) —
    /// glyph selection happens on the wrapped CPU face during atlas build, so
    /// the miss bits accumulate there.
    pub fn take_missing_font_classes(&mut self) -> u8 {
        self.cpu.take_missing_font_classes()
    }

    /// Forward of the inner CPU rasterizer's
    /// [`set_runtime_font_discovery`](Renderer::set_runtime_font_discovery).
    pub fn set_runtime_font_discovery(&mut self, enabled: bool) {
        self.cpu.set_runtime_font_discovery(enabled);
    }

    /// TEST/DEBUG forward of the inner rasterizer's
    /// [`debug_block_on_lazy_fallbacks`](Renderer::debug_block_on_lazy_fallbacks):
    /// parity/pixel tests block on the lazy fallback parses up front so the
    /// CPU-vs-GPU comparison is deterministic (no provisional `.notdef` race).
    #[doc(hidden)]
    pub fn debug_block_on_lazy_fallbacks(&mut self) {
        self.cpu.debug_block_on_lazy_fallbacks();
    }

    pub fn set_face(&mut self, cpu: Renderer, theme: Theme) {
        // CARRY OVER the active text-shaping config: a freshly-built `cpu` starts at
        // `TextShapingConfig::default()` (ligatures Enabled, no features), so without
        // this a zoom / config-reload / Retina-rescale rebuild would silently revert
        // the user's ligature/font-feature choice to the default. Re-apply the prior
        // shaping onto the new face (it also rebuilds `resolved_features`).
        let prior_shaping = self.cpu.text_shaping().clone();
        // Same carry-over for the W2 typography knobs: a fresh face starts at
        // their defaults, so a zoom/reload/Retina rebuild would silently revert
        // a configured `text_blending`/`font_thicken`/`stem_gamma`.
        let prior_blending = self.cpu.text_blending();
        let prior_thicken = self.cpu.font_thicken();
        let prior_stem_gamma = self.cpu.stem_gamma();
        // W9: carry the variation config too, so a zoom/reload rebuild keeps
        // the user's `font_variation`/`font_weight`/`font_weight_dark_nudge`.
        let prior_variations = self.cpu.font_variation_requests().to_vec();
        let prior_dark_nudge = self.cpu.font_weight_dark_nudge();
        self.cpu = cpu;
        self.cpu.set_text_shaping(prior_shaping);
        self.cpu.set_text_blending(prior_blending);
        self.cpu.set_font_thicken(prior_thicken);
        self.cpu.set_stem_gamma(prior_stem_gamma);
        self.cpu
            .set_font_variations(&prior_variations, prior_dark_nudge);
        self.theme = theme;
        self.resident_keys.clear();
        self.mono_res = None;
        self.color_res = None;
        // Cat/free atlases: same belt as `invalidate_atlas` — version-keyed, so
        // this only costs one re-upload on the next cat/free frame.
        self.cat_atlas = None;
        self.free_atlas = None;
    }

    /// Install the text-shaping config (ligature mode + OpenType font features) on
    /// the wrapped CPU face and drop the resident atlas so the next present
    /// re-rasterizes with the new shaped glyphs. A ligature/feature flip changes
    /// which glyph ids the shaper selects, so the atlas (content-addressed by glyph
    /// id) must be invalidated for the change to reach the screen. Mirrors the CPU
    /// [`Renderer::set_text_shaping`]; the GPU wrapper previously had no passthrough,
    /// so a configured ligature/feature setting could never reach the GPU backend.
    pub fn set_text_shaping(&mut self, shaping: aterm_render::TextShapingConfig) {
        self.cpu.set_text_shaping(shaping);
        self.invalidate_atlas();
    }

    /// Configured primary-face `font_features` tags the active font cannot apply
    /// (forwards to the wrapped CPU face — see [`Renderer::unsupported_user_feature_tags`]).
    #[must_use]
    pub fn unsupported_user_feature_tags(&self) -> Vec<[u8; 4]> {
        self.cpu.unsupported_user_feature_tags()
    }

    /// Install the coverage blend mode (config `text_blending`, W2) on the
    /// wrapped CPU face — the single source of truth `encode_frame` reads when
    /// writing the shader uniform, so CPU and GPU remap identically. Blend-time
    /// only (no atlas dependence); the HOST must invalidate the present
    /// (appearance-only, not content) for it to redraw, like a theme edit.
    pub fn set_text_blending(&mut self, mode: aterm_render::TextBlending) {
        self.cpu.set_text_blending(mode);
    }

    /// The active coverage blend mode (off the wrapped CPU face).
    #[must_use]
    pub fn text_blending(&self) -> aterm_render::TextBlending {
        self.cpu.text_blending()
    }

    /// Set macOS `font_thicken` (CoreText font smoothing, W2) on the wrapped
    /// CPU face and drop the resident atlas: the coverage BYTES change, so the
    /// atlas (which holds rasterized coverage) must re-upload for the change to
    /// reach the screen. A same-value call still drops the atlas only when the
    /// face reports a change — mirror of [`Renderer::set_font_thicken`].
    pub fn set_font_thicken(&mut self, on: bool) {
        if self.cpu.font_thicken() != on {
            self.cpu.set_font_thicken(on);
            self.invalidate_atlas();
        }
    }

    /// Install the W9 variable-font instantiation config (`font_variation` /
    /// `font_weight` requests + `font_weight_dark_nudge`) on the wrapped CPU
    /// face and drop the resident atlas when the resolved coords actually
    /// changed — the atlas coverage (and possibly the cell geometry, which
    /// the caller's rebuild path re-grids for) belongs to the new instance.
    /// Mirror of [`Renderer::set_font_variations`]; an unchanged config is a
    /// free no-op.
    pub fn set_font_variations(&mut self, requests: &[(u32, f32)], dark_nudge: f32) {
        if self.cpu.set_font_variations(requests, dark_nudge) {
            self.invalidate_atlas();
        }
    }

    /// Set the aesthetic stem gamma (config `stem_gamma` / `ATERM_STEM_GAMMA`,
    /// W2) on the wrapped CPU face and drop the resident atlas on a change (stem
    /// darkening bakes into the cached coverage bytes the atlas holds).
    pub fn set_stem_gamma(&mut self, gamma: f32) {
        let before = self.cpu.stem_gamma();
        self.cpu.set_stem_gamma(gamma);
        if (self.cpu.stem_gamma() - before).abs() > f32::EPSILON {
            self.invalidate_atlas();
        }
    }

    /// Replace just the fg/bg/cursor/selection theme live (host theme change) on both
    /// the GPU presentation state and the CPU face, so a pane re-themes without a
    /// device/face rebuild. Glyphs are coverage masks coloured at draw time, so no
    /// atlas invalidation is needed — EXCEPT under the W9 dark-weight nudge,
    /// where a light↔dark polarity flip re-instantiates the variable primary
    /// (new coverage bytes; the safety gate keeps the cell geometry fixed, so
    /// only the atlas needs to follow).
    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
        let coords_before: Option<Vec<(u32, f32)>> = self.cpu.variation_coords().map(<[_]>::to_vec);
        self.cpu.set_theme(theme);
        if self.cpu.variation_coords() != coords_before.as_deref() {
            self.invalidate_atlas();
        }
    }

    /// Explicit selected-text foreground (theme `selectionForeground`), or `None`
    /// for the contrast-floor default. Routed through the wrapped CPU face so the
    /// GPU and CPU selection-glyph colours stay byte-identical.
    pub fn set_selection_fg(&mut self, fg: Option<u32>) {
        self.cpu.set_selection_fg(fg);
    }

    /// Per-cell minimum contrast ratio (xterm's `minimumContrastRatio`; `<= 1.0`
    /// = off, the default). Routed through the wrapped CPU face — `encode_frame`
    /// reads it off `self.cpu.minimum_contrast()`, so the GPU and CPU floor
    /// per-cell glyph fg identically (parity by construction). The host must
    /// invalidate the present (appearance-only, not content) for it to redraw.
    pub fn set_minimum_contrast(&mut self, ratio: f32) {
        self.cpu.set_minimum_contrast(ratio);
    }

    /// DEFAULT-background opacity (0..=1; `1.0` = opaque, the default —
    /// byte-identical output; the M5 vibrancy knob). Routed through the wrapped
    /// CPU face — `encode_frame` reads it off `self.cpu.background_opacity()` and
    /// emits it as literal clear/quad alpha on default-bg cells only (via the ONE
    /// policy [`aterm_render::vibrancy::bg_quad_alpha`]), so the GPU and CPU
    /// (transmittance byte) agree by construction. M5's WCAG-AA legibility floor
    /// reads the same value off the CPU face. M5 TRUE VIBRANCY (now wired): below
    /// `1.0` the offscreen's STRAIGHT alpha carries that policy (bg scaled, ink
    /// opaque — [`aterm_render::vibrancy::ink_quad_alpha`]); `present_input`
    /// reconciles the swapchain to `CompositeAlphaMode::PostMultiplied` (where the
    /// surface offers it) and the blit emits that alpha, so the frame composites
    /// over the window's `NSVisualEffectView` as real see-through glass. The host
    /// must invalidate the present (appearance-only, not content) for it to redraw.
    pub fn set_background_opacity(&mut self, opacity: f32) {
        self.cpu.set_background_opacity(opacity);
    }

    /// The configured default-background opacity (off the wrapped CPU face).
    #[must_use]
    pub fn background_opacity(&self) -> f32 {
        self.cpu.background_opacity()
    }

    /// CURSOR-fill opacity (0..=1; `1.0` = opaque fill + block cut-out, the
    /// default — byte-identical output). Routed through the wrapped CPU face —
    /// `encode_frame` reads it off `self.cpu.cursor_opacity()`; below 1.0 the
    /// cursor fill draws through the ALPHA_BLENDING `cursor_blend_pipeline`
    /// and the block cut-out is skipped, mirroring the CPU. The host must
    /// invalidate the present (appearance-only, not content) for it to redraw.
    pub fn set_cursor_opacity(&mut self, opacity: f32) {
        self.cpu.set_cursor_opacity(opacity);
    }

    /// The configured cursor-fill opacity (off the wrapped CPU face).
    #[must_use]
    pub fn cursor_opacity(&self) -> f32 {
        self.cpu.cursor_opacity()
    }

    /// Mark the pane unfocused (`true`) / focused (`false`) for selection theming.
    /// Routed through the wrapped CPU face — `encode_frame` reads the band colour
    /// off `self.cpu.effective_selection_bg()`, so the GPU and CPU agree. The host
    /// must invalidate the present (appearance-only, not content) for it to redraw.
    pub fn set_selection_inactive(&mut self, inactive: bool) {
        self.cpu.set_selection_inactive(inactive);
    }

    /// Inactive (unfocused) selection bg (0x00RRGGBB), or `None` to derive it from
    /// the theme. Routed through the wrapped CPU face (single source of truth).
    pub fn set_selection_inactive_bg(&mut self, bg: Option<u32>) {
        self.cpu.set_selection_inactive_bg(bg);
    }

    /// Re-rasterize at a new pixel size (host DPI / devicePixelRatio change): update
    /// the wrapped CPU face's metrics + glyph caches and drop the GPU atlas so the
    /// next frame re-uploads glyphs at the new size. The host then resizes the grid.
    pub fn set_px(&mut self, px: f32) {
        self.cpu.set_px(px);
        self.invalidate_atlas();
    }

    /// LIGHT per-window size switch (W12 mixed-DPI): re-target the active
    /// rasterization size to `px` on the wrapped CPU face WITHOUT invalidating
    /// the resident glyph atlas. The persisted atlas keys on the full
    /// [`aterm_render::GlyphKey`] (which embeds a quantized `px_q`), so glyphs
    /// for different window sizes COEXIST in one texture — `ensure_atlases`
    /// simply GROWS the atlas with the new size's keys on the next encode,
    /// keeping every other window's warm glyphs put. This is the GPU analog of
    /// [`aterm_render::Renderer::activate_px`]: no atlas teardown, no re-upload of
    /// the sizes still in use. No-op when `px` is unchanged (the CPU switch is a
    /// no-op, and the atlas is untouched either way).
    pub fn activate_px(&mut self, px: f32) {
        self.cpu.activate_px(px);
        // Deliberately NO `invalidate_atlas()`: the atlas is content-addressed by
        // GlyphKey and grows to hold both sizes (see `ensure_atlases`).
    }

    /// PURE per-window cell geometry at `px` — `(cell_w, cell_h, baseline)` —
    /// delegated to the wrapped CPU face so the GPU and CPU resolve a window's
    /// metrics identically while another size is active. See
    /// [`aterm_render::Renderer::cell_geometry`].
    pub fn cell_geometry(&self, px: f32) -> (usize, usize, i32) {
        self.cpu.cell_geometry(px)
    }

    /// Inject a broad-coverage (CJK + symbols) fallback face into the GPU's CPU
    /// face from font bytes and invalidate the atlas so the next frame
    /// re-rasterizes the new coverage. The browser GPU path has no system-font
    /// discovery, so the host pushes OS font bytes in.
    pub fn set_fallback_font_bytes(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.cpu.set_fallback_bytes(bytes)?;
        self.invalidate_atlas();
        Ok(())
    }

    /// APPEND a broad-coverage fallback face to the chain (mirrors
    /// [`set_fallback_font_bytes`], but adds rather than resets). Lets the host push
    /// several OS fallbacks so a script the first face misses (Arabic/Devanagari/
    /// Thai/Hebrew vs a CJK face) still reaches a covering face. Atlas invalidated so
    /// the next frame re-rasterizes with the added coverage.
    pub fn add_fallback_font_bytes(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.cpu.add_fallback_bytes(bytes)?;
        self.invalidate_atlas();
        Ok(())
    }

    /// Inject a colour-emoji (sbix) face into the GPU's CPU face from font bytes
    /// and invalidate the atlas so the colour atlas re-rasterizes with the new
    /// emoji coverage. Mirrors [`set_fallback_font_bytes`].
    pub fn set_emoji_font_bytes(&mut self, bytes: Vec<u8>) -> Result<(), String> {
        self.cpu.set_color_font_bytes(bytes)?;
        self.invalidate_atlas();
        Ok(())
    }

    /// Inject a REAL bold weight into the GPU's CPU face so SGR-bold cells render
    /// as a true heavier weight (`FaceId::BoldPrimary`) instead of synthetic
    /// embolden. Atlas invalidated so bold glyphs re-rasterize from the new face.
    pub fn set_bold_font_bytes(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.cpu.set_bold_font(bytes)?;
        self.invalidate_atlas();
        Ok(())
    }

    /// Inject a broad-coverage SYMBOL fallback face (config `symbol_font`, W6)
    /// into the GPU's CPU face from font bytes and invalidate the atlas so the
    /// next frame re-rasterizes the new symbol coverage. Mirrors
    /// [`set_fallback_font_bytes`], the byte-injection sibling of the path-based
    /// [`set_config_symbol_font`], for hosts with no system-font discovery.
    pub fn set_symbol_font_bytes(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.cpu.set_symbol_fallback_bytes(bytes)?;
        self.invalidate_atlas();
        Ok(())
    }

    /// Inject a REAL styled face (`slot` 0 = bold, 1 = italic, 2 = bold-italic;
    /// W6 config `font_family_italic` / `font_family_bold_italic`) into the GPU's
    /// CPU face. Atlas invalidated so styled glyphs + ligature runs re-rasterize.
    pub fn set_styled_font_bytes(&mut self, slot: usize, bytes: &[u8]) -> Result<(), String> {
        self.cpu.set_styled_font_bytes(slot, bytes)?;
        self.invalidate_atlas();
        Ok(())
    }

    /// Enable/disable SYNTHETIC bold/italic on the wrapped CPU face (config
    /// `font_synthetic_style`, W6). Atlas invalidated only on a real change (the
    /// CPU setter reports it), so re-applying the same value is free.
    pub fn set_synthetic_styles(&mut self, on: bool) {
        if self.cpu.set_synthetic_styles(on) {
            self.invalidate_atlas();
        }
    }

    /// Install the CONFIG fallback-font chain (TOML `fallback_fonts`, W6) on the
    /// wrapped CPU face — config > `$ATERM_FALLBACK_FONT` > built-ins, the proven
    /// `fallback_chain_order` law. Atlas invalidated only on a real change.
    pub fn set_config_fallback_fonts(&mut self, paths: &[String]) {
        if self.cpu.set_config_fallback_fonts(paths) {
            self.invalidate_atlas();
        }
    }

    /// Install the CONFIG symbol-fallback font (TOML `symbol_font`, W6).
    /// Atlas invalidated only on a real change.
    pub fn set_config_symbol_font(&mut self, path: Option<&str>) {
        if self.cpu.set_config_symbol_font(path) {
            self.invalidate_atlas();
        }
    }

    /// Install the CONFIG colour-emoji font (TOML `emoji_font`, W6).
    /// Atlas invalidated only on a real change.
    pub fn set_config_emoji_font(&mut self, path: Option<&str>) {
        if self.cpu.set_config_emoji_font(path) {
            self.invalidate_atlas();
        }
    }

    /// Swap the PRIMARY face of the GPU's CPU face (the host's `terminalFontFamily`)
    /// and invalidate the atlas so every glyph re-rasterizes from the new face. The
    /// host resizes the grid afterwards if the cell metrics changed.
    pub fn set_primary_font_bytes(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.cpu.set_primary_font(bytes)?;
        self.invalidate_atlas();
        Ok(())
    }

    /// Scale the cell BOX height (the host's `terminalLineHeight`) without changing
    /// the glyph px. Atlas invalidated because procedural (box-drawing) glyphs are
    /// rasterized cell-exact to the new height; the host resizes the grid after.
    pub fn set_line_height(&mut self, scale: f32) {
        self.cpu.set_line_height(scale);
        self.invalidate_atlas();
    }

    /// Baseline escape hatch (config `adjust_baseline`, W5a): shift the derived
    /// baseline by a signed px delta on the wrapped CPU face. Placement-only
    /// (glyph quads read `cpu.baseline()` at encode time), so the atlas stays;
    /// the host invalidates the present (appearance-only) for it to redraw.
    pub fn set_adjust_baseline(&mut self, px: i32) {
        self.cpu.set_adjust_baseline(px);
    }

    /// Underline escape hatches (config `adjust_underline_position` /
    /// `adjust_underline_thickness`, W7) on the wrapped CPU face. Draw-time
    /// only for the solid bands; the undercurl sprite re-bakes automatically
    /// (`ensure_deco_atlas` keys on the resolved band).
    pub fn set_adjust_underline(&mut self, pos_px: i32, thick_px: i32) {
        self.cpu.set_adjust_underline(pos_px, thick_px);
    }

    /// Descender ink-skip toggle (config `underline_skip_descenders`, W7) on
    /// the wrapped CPU face — the deco loop reads the skip spans off the CPU
    /// renderer, so this is the single switch for both paths.
    pub fn set_underline_skip_descenders(&mut self, on: bool) {
        self.cpu.set_underline_skip_descenders(on);
    }

    /// The wrapped CPU face's resolved PRIMARY font — `(bytes, collection
    /// index)` — for the GUI chrome (tray/overlay text). See
    /// [`Renderer::chrome_primary_face`].
    #[must_use]
    pub fn chrome_primary_face(&self) -> Option<(std::sync::Arc<[u8]>, u32)> {
        self.cpu.chrome_primary_face()
    }

    /// The wrapped CPU face's real BOLD sibling of the primary family for the
    /// GUI chrome. See [`Renderer::chrome_bold_face`].
    #[must_use]
    pub fn chrome_bold_face(&mut self) -> Option<(std::sync::Arc<[u8]>, u32)> {
        self.cpu.chrome_bold_face()
    }

    /// Fork the wrapped CPU font engine for a renderer-native semantic
    /// surface. The fork shares immutable parsed fallback assets and carries
    /// the exact live font-routing/shaping knobs; it owns no GPU resources.
    #[must_use]
    pub fn fork_semantic_surface(&self, px: f32, theme: Theme) -> Option<aterm_render::Renderer> {
        self.cpu.fork_semantic_surface(px, theme)
    }

    /// Drop the resident atlases + key set so the next present rebuilds them with
    /// the current CPU face's coverage (mirrors [`set_face`]'s invalidation).
    /// The CAT atlas is dropped too: it is version-keyed off the host `CatBaker`
    /// (which itself rebakes on cell-metric change), so dropping it here only
    /// forces one re-upload — a cheap belt that keeps every resident texture
    /// covered by the same invalidation hook.
    fn invalidate_atlas(&mut self) {
        self.resident_keys.clear();
        self.mono_res = None;
        self.color_res = None;
        self.cat_atlas = None;
        self.free_atlas = None;
    }

    /// TEST/DIAGNOSTIC: number of `render_input_cached` calls that took the
    /// dirty-gate (re-presented cached pixels with ZERO GPU work).
    #[doc(hidden)]
    #[must_use]
    pub fn gate_hits(&self) -> u64 {
        self.gate_hits
    }

    /// TEST/DIAGNOSTIC: number of `render_input_cached` calls that MISSED the
    /// dirty-gate (ran a full encode + readback).
    #[doc(hidden)]
    #[must_use]
    pub fn gate_misses(&self) -> u64 {
        self.gate_misses
    }

    /// TEST/DIAGNOSTIC: number of `present_input` frames that took the SCISSORED
    /// dirty-row repaint (LoadOp::Load + scissor over the dirty band, only dirty
    /// rows re-encoded) instead of a full Clear+all-rows repaint.
    #[doc(hidden)]
    #[must_use]
    pub fn scissor_taken(&self) -> u64 {
        self.scissor_taken
    }

    /// TEST/DIAGNOSTIC: number of `present_input` frames that did a FULL repaint
    /// (Clear + all rows) — the first frame, a geometry/scrollback/selection
    /// change, a double-HEIGHT row, etc. (the conservative always-correct path).
    #[doc(hidden)]
    #[must_use]
    pub fn full_repaints(&self) -> u64 {
        self.full_repaints
    }

    /// Project the exact `PresentPrev` and `GpuGateCache` origin stamps used by
    /// the `AsymmetricPadLayout` refinement anchors.
    #[doc(hidden)]
    #[must_use]
    pub fn project_asymmetric_pad_layout(
        &self,
        win: &WindowGpu,
    ) -> AsymmetricPadGpuCacheProjection {
        AsymmetricPadGpuCacheProjection {
            grid_top: self.cpu.grid_top(),
            present_cached_grid_top: win.present_prev.as_ref().map(|cache| cache.grid_top),
            gate_cached_grid_top: win.gate_cache.as_ref().map(|cache| cache.grid_top),
        }
    }

    /// TEST/DIAGNOSTIC: number of settings-card `write_texture` uploads that
    /// actually ran. A present whose card bytes equal the resident texture's
    /// (`TrayOverlay::pixels`) skips the upload and does not advance this.
    #[doc(hidden)]
    #[must_use]
    pub fn tray_uploads(&self) -> u64 {
        self.tray_uploads
    }

    /// TEST/DIAGNOSTIC: total instances built in the LAST encoded frame (sum of
    /// the eight per-frame streams). The scissored path builds ~`dirty_rows/rows`
    /// of a full frame's instances.
    #[doc(hidden)]
    #[must_use]
    pub fn last_instances(&self) -> usize {
        self.last_instances
    }

    /// Cell size (pixels), straight from the CPU renderer so geometry matches.
    pub fn cell_size(&self) -> (usize, usize) {
        self.cpu.cell_size()
    }

    /// Interior padding (px per edge), delegated to the inner CPU renderer — the
    /// single source of `pad`, so the GPU encode and the CPU encode always agree.
    pub fn pad(&self) -> usize {
        self.cpu.pad()
    }

    /// Set the interior padding on the inner CPU renderer. The GPU `encode_frame`
    /// reads `self.cpu.pad()` each frame, so this takes effect on the next present.
    /// This inherits the CPU contract: a changed base pad clears the top override,
    /// while a same-value call is a complete no-op and preserves it.
    pub fn set_pad(&mut self, pad: usize) {
        self.cpu.set_pad(pad);
    }

    /// Chrome headroom (px above the padded grid), delegated to the inner CPU
    /// renderer — the single source of `head`, matching the `pad` delegation.
    pub fn head(&self) -> usize {
        self.cpu.head()
    }

    /// Set the chrome headroom on the inner CPU renderer. The GPU `encode_frame`
    /// reads `self.cpu.head()` each frame, so this takes effect on the next present.
    pub fn set_head(&mut self, head_px: usize) {
        self.cpu.set_head(head_px);
    }

    /// Effective TOP interior pad (px), delegated to the inner CPU renderer.
    pub fn pad_top(&self) -> usize {
        self.cpu.pad_top()
    }

    /// Override the TOP-only interior pad on the inner CPU renderer. `encode_frame`
    /// reads `self.cpu.grid_top()` each frame, so this takes effect next present
    /// without changing the framebuffer size. The refinement anchors live on the
    /// CPU mutator below this delegate; anchoring both would bind the same concrete
    /// transition twice.
    pub fn set_pad_top(&mut self, pad_top: usize) {
        self.cpu.set_pad_top(pad_top);
    }

    /// Padded pixel size of a `rows`×`cols` grid (`cols·cell_w + 2·pad`, etc.) —
    /// the size to configure the swapchain / window to. Mirrors the CPU renderer.
    pub fn frame_size(&self, rows: usize, cols: usize) -> (usize, usize) {
        self.cpu.frame_size(rows, cols)
    }

    /// Whether this renderer's GPU device has been reported lost (driver update, TDR
    /// reset, eGPU unplug). Delegates to [`GpuContext::device_lost`]. The frontend
    /// polls this after presenting a window; when it returns `true` the device is
    /// dead and every further `present_input` is a no-op (the swapchain only ever
    /// `get_current_texture() -> Lost`s), so the caller must rebuild the GPU stack or
    /// downgrade to the CPU backend rather than freeze at the last frame.
    #[inline]
    pub fn device_lost(&self) -> bool {
        self.ctx.device_lost()
    }

    /// Clamp a framebuffer/surface pixel dimension to `1..=max_texture_dimension_2d`.
    ///
    /// The SINGLE clamp every swapchain-surface and offscreen-texture pixel size on
    /// the GPU path routes through, so the blit SOURCE (offscreen) and DESTINATION
    /// (swapchain) — plus the cache-dim comparisons — can never drift. wgpu validates
    /// each texture/surface against `max_texture_dimension_2d` (8192 on many GPUs via
    /// `Limits::default()`, which is what `GpuContext` requests on native), and with
    /// NO `on_uncaptured_error` handler installed anywhere a violation hits wgpu's
    /// default handler, which PANICS and aborts the process. The control-socket
    /// `resize` verb accepts up to 4096 rows/cols but does NOT clamp to pixels, so a
    /// padded framebuffer of `cols·cell_w + 2·pad` px can exceed the limit; clamping
    /// renders a CLIPPED view (graceful) instead of crashing. Mirrors the atlas/image
    /// clamps (`create_atlas_texture`, `rebuild_atlases`, `build_image_plane`) — same
    /// limit, same `.min(max_tex_dim)` idiom. Grid dimensions are untouched.
    #[inline]
    fn clamp_fb_dim(&self, d: u32) -> u32 {
        d.max(1)
            .min(self.ctx.device.limits().max_texture_dimension_2d)
    }

    /// Number of glyph-atlas TEXTURES created so far (full (re)packs only — a
    /// reuse or incremental sub-region append creates none). The persistence
    /// test asserts an unchanged-glyph frame does not advance this.
    #[cfg(test)]
    fn atlas_tex_creations(&self) -> u64 {
        self.atlas_tex_creations
    }

    /// The `(width, height)` of the resident mono + colour atlas textures (for
    /// the persistence test: identity check that the SAME textures are reused).
    #[cfg(test)]
    fn atlas_tex_dims(&self) -> Option<((u32, u32), (u32, u32))> {
        let m = self.mono_res.as_ref()?;
        let c = self.color_res.as_ref()?;
        Some((
            (m.tex.width(), m.tex.height()),
            (c.tex.width(), c.tex.height()),
        ))
    }

    /// The adapter/backend the GPU device is running on (for diagnostics).
    pub fn adapter(&self) -> (&str, &str) {
        (&self.ctx.adapter_name, &self.ctx.backend)
    }

    /// Whether the GPU additive (One/One) streams — the LUMEN glow aurora, the
    /// sparkle-add decoration, and the bloom halo — composite BYTE-EXACT with the CPU
    /// `add_sat`. True on native (the offscreen is plain `Rgba8Unorm`, so the One/One
    /// add is a raw 8-bit add); FALSE on downlevel (GLES/WebGL2), where the single
    /// offscreen is `Rgba8UnormSrgb` and the add lands in linear (a cosmetic
    /// approximation — see the header DOWNLEVEL FALLBACK). Parity tests gate their
    /// byte-exact additive assertions on this so they never fire on the downlevel path.
    pub fn additive_is_byte_exact(&self) -> bool {
        self.ctx.srgb_offscreen
    }

    /// Mirror of [`Renderer::set_cursor_blink_phase`]: `false` skips drawing
    /// the cursor for the frame, but ONLY for the `Blinking*` DECSCUSR styles.
    /// Defaults to `true`. State lives on the inner CPU renderer so both paths
    /// always agree.
    pub fn set_cursor_blink_phase(&mut self, on: bool) {
        self.cpu.set_cursor_blink_phase(on);
    }

    /// Mirror of [`Renderer::set_cursor_style_override`]: when set, the cursor
    /// is drawn in THIS style instead of the terminal's DECSCUSR style (the
    /// windowed frontend forces `HollowBlock` while unfocused).
    pub fn set_cursor_style_override(&mut self, style: Option<CursorStyle>) {
        self.cpu.set_cursor_style_override(style);
    }

    /// Whether a drawable glyph lives in this cell (not wide-continuation, not a
    /// space, not a control char) — mirrors the CPU renderer's `blit` guard.
    fn drawable(cell: &aterm_core::terminal::RenderCell) -> bool {
        !cell.wide && cell.ch != ' ' && !cell.ch.is_control()
    }

    /// Resolve a cell to its glyph key via the SAME dispatch the CPU `blit`
    /// uses: a shaped emoji cluster (ZWJ / skin-tone / keycap) first, then a
    /// VS16 emoji-presentation base, then ordinary text. Keeps the GPU atlas key
    /// set and the per-cell instance lookup in lockstep with the CPU, so `❤️`,
    /// `👨‍👩‍👧`, `👍🏽`, `1️⃣` key to the colour atlas on both paths.
    fn cell_key(
        &mut self,
        cluster: Option<&str>,
        cell: &aterm_core::terminal::RenderCell,
    ) -> GlyphKey {
        self.cpu.resolve_cell_key(cluster, cell)
    }

    /// Take ownership of a freshly packed `atlas`, create its GPU texture (sized
    /// to it) + bind group, upload `atlas.data` in full, and return the resident
    /// bundle. Counts as ONE texture creation — used on a full (re)pack, NOT on a
    /// reuse or incremental append. The format follows the atlas kind (R8Unorm /
    /// RGBA8Unorm); both bind through `atlas_bgl` with the NEAREST sampler.
    fn create_atlas_texture(&mut self, atlas: Atlas) -> ResidentAtlas {
        let device = &self.ctx.device;
        let bpp = atlas.kind.bpp();
        let (format, label, bg_label) = match atlas.kind {
            AtlasKind::Mono => (
                wgpu::TextureFormat::R8Unorm,
                "aterm-gpu atlas",
                "aterm-gpu atlas bg",
            ),
            AtlasKind::Color => (
                wgpu::TextureFormat::Rgba8Unorm,
                "aterm-gpu colour atlas",
                "aterm-gpu colour atlas bg",
            ),
        };
        // Allocate the texture TALLER than the packed data (headroom) so later
        // glyphs append via sub-region upload instead of recreating the texture.
        // Clamp to the device's max 2D texture dimension: the packer already bounds
        // `atlas.height` to this same limit (so the upload, whose Extent3d height ==
        // atlas.height, always fits), and this stops the +headroom from ever pushing
        // the texture past the limit — which would abort the device.
        let max_tex_dim = device.limits().max_texture_dimension_2d;
        let tex_h = (atlas.height + ATLAS_GROW_HEADROOM).min(max_tex_dim);
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: atlas.width,
                height: tex_h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.atlas_tex_creations += 1;
        // Upload only the OCCUPIED rows (`atlas.height`); the headroom stays
        // unwritten (never sampled until an append fills it). bytes_per_row ==
        // width * bpp: width is 1024, so 1024 (R8) and 4096 (RGBA8) are both
        // multiples of 256 — no row padding needed.
        self.ctx.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &atlas.data[..(atlas.width * atlas.height * bpp) as usize],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(atlas.width * bpp),
                rows_per_image: Some(atlas.height),
            },
            wgpu::Extent3d {
                width: atlas.width,
                height: atlas.height,
                depth_or_array_layers: 1,
            },
        );
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(bg_label),
            layout: &self.atlas_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        ResidentAtlas {
            atlas,
            tex,
            bind,
            tex_h,
        }
    }

    /// Ensure the deco sprite atlas (sparkle words + the W7 undercurl tile)
    /// exists for the current cell size + resolved underline band, rebuilding
    /// it (R8 coverage) when absent or when either changed. The `DECO_GLYPHS`
    /// sprites are packed left-to-right at `i * cw` (the atlas width auto-grows
    /// with the table and the cell); sampled 1:1 (NEAREST) at the dest cell so
    /// the GPU reads the EXACT CPU coverage byte (byte-parity with
    /// `draw_decorations` / `blend_undercurl`).
    fn ensure_deco_atlas(&mut self, cw: usize, ch: usize) {
        // Size the atlas to the cell: a FIXED width dropped ALL decorations once
        // `DECO_ATLAS_SPRITES * cw` exceeded it (cw>64 at the old 384) while the CPU
        // `draw_decorations` kept drawing them — a GPU/CPU parity break at large
        // fonts. Grow with `cw` and only bail past a texture-size-safe cap (an
        // absurd cell size no real font reaches), where both backends drop it:
        // the bail condition below is EXACTLY the shared
        // `aterm_render::undercurl_supported` predicate, so the CPU renderer
        // falls back to the square-wave curly rects iff this atlas is absent.
        // BOTH axes are capped at `DECO_ATLAS_MAX_DIM` (== the downlevel/WebGL2
        // `max_texture_dimension_2d` of 2048): the WIDTH via `atlas_w` and the HEIGHT
        // via `ch` (the `Extent3d.height` below). Without the height guard a large
        // font whose `ch` exceeds the limit — while `atlas_w` (~9·cw) stays small, so
        // the width guard doesn't fire — would hand `create_texture` an oversized
        // height and abort. Dropping the atlas (`deco_atlas = None`) is the same
        // graceful fallback the width-overflow path already uses.
        let atlas_w = aterm_render::DECO_ATLAS_SPRITES * cw;
        if !aterm_render::undercurl_supported(cw, ch) {
            self.deco_atlas = None;
            return;
        }
        let dm = self.cpu.deco_metrics();
        let curl_band = (dm.underline_y, dm.underline_t);
        if let Some(d) = &self.deco_atlas
            && d.cw == cw
            && d.ch == ch
            && d.curl_band == curl_band
        {
            return;
        }
        let mut data = vec![0u8; atlas_w * ch];
        for (i, g) in DECO_GLYPHS.iter().enumerate() {
            let x0 = i * cw;
            let mask = aterm_render::procedural::deco_coverage(*g, cw, ch);
            for my in 0..ch {
                for mx in 0..cw {
                    data[my * atlas_w + x0 + mx] = mask[my * cw + mx];
                }
            }
        }
        // The W7 undercurl tile, from the SAME shared mask the CPU blends —
        // one source of shape, so the wave can never diverge between paths.
        {
            let x0 = aterm_render::UNDERCURL_SPRITE * cw;
            let mask = self.cpu.undercurl_mask();
            for my in 0..ch {
                for mx in 0..cw {
                    data[my * atlas_w + x0 + mx] = mask[my * cw + mx];
                }
            }
        }
        let device = &self.ctx.device;
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aterm-gpu deco atlas"),
            size: wgpu::Extent3d {
                width: atlas_w as u32,
                height: ch as u32,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.ctx.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(atlas_w as u32),
                rows_per_image: Some(ch as u32),
            },
            wgpu::Extent3d {
                width: atlas_w as u32,
                height: ch as u32,
                depth_or_array_layers: 1,
            },
        );
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("aterm-gpu deco atlas bg"),
            layout: &self.atlas_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        self.deco_atlas = Some(DecoAtlas {
            bind,
            cw,
            ch,
            curl_band,
            atlas_w,
        });
    }

    /// Re-upload only the changed row band `[y0, y1)` of a resident atlas — the
    /// incremental-append fast path. Writes FULL atlas rows (origin x=0, full
    /// width) so `bytes_per_row` stays a multiple of 256, and creates no new
    /// texture (the whole point of the optimisation). No-op for an empty band.
    fn upload_atlas_rows(&self, res: &ResidentAtlas, y0: u32, y1: u32) {
        if y1 <= y0 {
            return;
        }
        let bpp = res.atlas.kind.bpp();
        let row = (res.atlas.width * bpp) as usize;
        let start = y0 as usize * row;
        let end = y1 as usize * row;
        self.ctx.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &res.tex,
                mip_level: 0,
                origin: wgpu::Origin3d { x: 0, y: y0, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            &res.atlas.data[start..end],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(res.atlas.width * bpp),
                rows_per_image: Some(y1 - y0),
            },
            wgpu::Extent3d {
                width: res.atlas.width,
                height: y1 - y0,
                depth_or_array_layers: 1,
            },
        );
    }

    /// Ensure both glyph atlases hold every key in `keys`, reusing the resident
    /// textures untouched when `keys` is already a subset, appending new glyphs
    /// into free space on a small miss, and full-repacking into a fresh texture
    /// only on genuine overflow (or the first frame). Pure GPU/CPU bookkeeping
    /// — the render passes below just bind `mono_res`/`color_res`.
    fn ensure_atlases(&mut self, keys: &BTreeSet<GlyphKey>) {
        // Fast path: every requested key already resident in BOTH atlases → the
        // resident textures + bind groups are exactly what this frame needs.
        // (`resident_keys` is the union packed across both atlases.)
        if self.mono_res.is_some()
            && self.color_res.is_some()
            && keys.iter().all(|k| self.resident_keys.contains(k))
        {
            return;
        }

        // Which keys are genuinely new this frame.
        let new_keys: Vec<GlyphKey> = keys
            .iter()
            .copied()
            .filter(|k| !self.resident_keys.contains(k))
            .collect();

        // First frame (or a prior overflow cleared the cache): full pack both.
        if self.mono_res.is_none() || self.color_res.is_none() {
            self.rebuild_atlases(keys);
            return;
        }

        // Try to grow both resident atlases in place. A capacity miss in either
        // forces a full repack of BOTH from the complete key set (simplest
        // correct fallback; overflow is rare).
        let cap_mono = self.mono_res.as_ref().unwrap().tex_h;
        let cap_color = self.color_res.as_ref().unwrap().tex_h;
        let mono_band = {
            let res = self.mono_res.as_mut().unwrap();
            grow_atlas(&mut self.cpu, &mut res.atlas, &new_keys, cap_mono)
        };
        let color_band = {
            let res = self.color_res.as_mut().unwrap();
            grow_atlas(&mut self.cpu, &mut res.atlas, &new_keys, cap_color)
        };
        match (mono_band, color_band) {
            (Some(mb), Some(cb)) => {
                // Both grew (or stayed) within capacity: upload only the dirty
                // bands; no new texture is created.
                let mono_res = self.mono_res.as_ref().unwrap();
                self.upload_atlas_rows(mono_res, mb.0, mb.1);
                let color_res = self.color_res.as_ref().unwrap();
                self.upload_atlas_rows(color_res, cb.0, cb.1);
                self.resident_keys.extend(new_keys);
            }
            _ => self.rebuild_atlases(keys),
        }
    }

    /// Full (re)pack of both atlases from the complete key set, replacing the
    /// resident textures. Used on the first frame and on genuine overflow.
    fn rebuild_atlases(&mut self, keys: &BTreeSet<GlyphKey>) {
        // Cap the packed atlas height at the device's max 2D texture dimension so a
        // very large distinct-glyph set can never ask wgpu to create a texture
        // taller than the GPU allows (which aborts the device). Far above any real
        // workload; only the pathological case is bounded.
        let cap_h = self.ctx.device.limits().max_texture_dimension_2d;
        let mono = build_atlas(&mut self.cpu, keys, cap_h);
        self.mono_res = Some(self.create_atlas_texture(mono));
        let color = build_color_atlas(&mut self.cpu, keys, cap_h);
        self.color_res = Some(self.create_atlas_texture(color));
        self.resident_keys = keys.clone();
    }

    /// Pack each placed footprint's RGBA rows into a stacked `tw`×`th` plane
    /// buffer. `items` is `(y0, dw, dh, rgba)` per footprint. When a footprint
    /// spans the full plane width (`dw == tw`) its rows are contiguous in both
    /// source and destination, so the whole footprint copies in ONE memcpy;
    /// otherwise rows are copied individually (right padding stays transparent).
    /// Byte-identical either way — factored out of [`build_image_plane`] so the
    /// copy can be unit-tested and benchmarked in isolation from GPU upload.
    ///
    /// `#[doc(hidden)] pub` ONLY so the `image_plane` bench can call it directly;
    /// not part of the public API.
    #[doc(hidden)]
    #[must_use]
    pub fn pack_image_plane(items: &[(u32, u32, u32, &[u8])], tw: u32, th: u32) -> Vec<u8> {
        let mut data = vec![0u8; (tw * th * 4) as usize];
        for &(y0, dw, dh, rgba) in items {
            if dw == tw {
                // Footprint spans the full plane width: rows are contiguous in both
                // source and destination, so collapse the `dh` per-row copies into
                // one memcpy (the common cell-sized / single-image case).
                let dst = (y0 * tw) as usize * 4;
                let len = (dw * dh * 4) as usize;
                data[dst..dst + len].copy_from_slice(&rgba[..len]);
            } else {
                // Narrower than the plane: each row has right padding — copy row by row.
                for y in 0..dh {
                    let src = (y * dw * 4) as usize;
                    let dst = ((y0 + y) * tw) as usize * 4;
                    data[dst..dst + (dw * 4) as usize]
                        .copy_from_slice(&rgba[src..src + (dw * 4) as usize]);
                }
            }
        }
        data
    }

    /// Build the per-frame inline-image texture (iTerm2 OSC 1337) from every
    /// DISTINCT image visible in `input`'s rows (whole grid — every image cell is
    /// repainted; the scissor path falls back to FULL whenever images differ, see
    /// `compute_dirty_rows`). Each distinct `(arc_ptr, fp_w, fp_h)` footprint is
    /// decoded+scaled via `aterm_render::decode_image_to_footprint` (the SAME
    /// bytes the CPU `blit_image_cell` copies, cached by the same key) and stacked
    /// vertically into ONE RGBA8 texture; a covered cell then samples its tile
    /// NEAREST, so the GPU pixels match the CPU per-cell copy (the parity gate).
    ///
    /// Sets `self.image_plane` to the resident texture + placement map, or to
    /// `None` when no decodable image is visible (so an image-free frame binds and
    /// draws nothing — the text path stays byte-identical).
    fn build_image_plane(&mut self, win: &mut WindowGpu, input: &RenderInput) {
        let (cw, ch) = self.cpu.cell_size();
        // Distinct placements: keyed like the CPU cache. Insertion order is
        // deterministic (row-major over the grid), so the packed texture layout
        // is stable frame to frame for the same image set.
        let mut order: Vec<(
            std::sync::Arc<aterm_core::grid::extra::ImageData>,
            usize,
            usize,
        )> = Vec::new();
        let mut seen: HashMap<(usize, usize, usize), ()> = HashMap::new();
        for row in &input.images {
            for (_c, image) in row {
                let fp_w = image.image.cols as usize * cw;
                let fp_h = image.image.rows as usize * ch;
                if fp_w == 0 || fp_h == 0 {
                    continue;
                }
                let key = (std::sync::Arc::as_ptr(&image.image) as usize, fp_w, fp_h);
                if seen.insert(key, ()).is_none() {
                    // Carry the live `Arc` (not just its raw pointer): the decode cache
                    // holds an `Arc` clone to PIN the allocation, so the pointer-keyed
                    // `placements`/plane-reuse below can't be fooled by a freed+reused
                    // address (the ABA hazard).
                    order.push((image.image.clone(), fp_w, fp_h));
                }
            }
        }
        if order.is_empty() {
            // No images this frame: drop any prior plane so nothing is bound.
            win.image_plane = None;
            return;
        }

        // Decode each distinct image to its footprint RGBA (cached), and lay them
        // out top-to-bottom in one texture. Failed decodes (empty rgba) are kept
        // in the cache as a negative result but contribute no texture rows; a cell
        // referencing one simply emits no quad (its bg shows through, == CPU).
        // Bound the stacked plane to the device's max 2D texture dimension on BOTH
        // axes: stacking enough (or wide enough) inline images to exceed the limit
        // would otherwise ask wgpu for an oversized texture and abort the device. An
        // image that doesn't fit emits no quad (its bg shows through) — the same
        // graceful fallback a failed decode already uses.
        let max_tex_dim = self.ctx.device.limits().max_texture_dimension_2d;
        let mut placements: HashMap<(usize, usize, usize), (u32, u32, u32)> = HashMap::new();
        let mut total_h: u32 = 0;
        let mut max_w: u32 = 0;
        for (image, fp_w, fp_h) in &order {
            let key = (std::sync::Arc::as_ptr(image) as usize, *fp_w, *fp_h);
            if win.image_cache.get(image, *fp_w, *fp_h).is_none() {
                // Decode directly from the live `Arc` we already hold — no grid re-scan.
                let decoded = decode_for_key(image, *fp_w, *fp_h);
                win.image_cache.put(image.clone(), *fp_w, *fp_h, decoded);
            }
            let decoded = win
                .image_cache
                .get(image, *fp_w, *fp_h)
                .expect("just inserted");
            if decoded.rgba.is_empty() || decoded.w == 0 || decoded.h == 0 {
                continue;
            }
            // Skip a single footprint that exceeds the limit on its own, and stop
            // once the next image would push the stacked height past the limit.
            if decoded.w > max_tex_dim || decoded.h > max_tex_dim {
                continue;
            }
            if total_h + decoded.h > max_tex_dim {
                break;
            }
            placements.insert(key, (total_h, decoded.w, decoded.h));
            total_h += decoded.h;
            max_w = max_w.max(decoded.w);
        }
        if placements.is_empty() || max_w == 0 || total_h == 0 {
            win.image_plane = None;
            return;
        }

        // One straight-RGBA buffer holding every footprint, stacked. Unused right
        // padding (when footprints differ in width) stays zero/transparent and is
        // never sampled — a cell only reads its own `(cell_col*cw, y0+cell_row*ch)`
        // tile, fully inside its footprint.
        let (tw, th) = (max_w, total_h);
        // Reuse the resident plane when the visible image set and layout are
        // byte-identical to last frame (the documented `image_plane` contract):
        // an equal placement map + dimensions imply byte-identical texels, since
        // the keys embed `Arc::as_ptr(image)` + footprint dims and the decode is
        // deterministic. This pointer-keyed equality is sound because the RESIDENT
        // plane holds an `Arc` to each placed image (`_pinned_images`), so every
        // address in `p.placements` stays live while the plane exists — a freed
        // address cannot be reused by a different image and alias a key here.
        // Skips the pack/alloc/upload/bind below entirely.
        if let Some(p) = &win.image_plane
            && p.w == tw
            && p.h == th
            && p.placements == placements
        {
            return;
        }
        // Gather each placed footprint's source rows, then pack them in one pass
        // (extracted to `pack_image_plane` so the copy is unit-tested + benched in
        // isolation from GPU upload).
        let mut items: Vec<(u32, u32, u32, &[u8])> = Vec::with_capacity(order.len());
        // Retain the Arc of every PLACED image for the plane's lifetime so its
        // raw-pointer key cannot be a freed address reused by a different image
        // (ABA) on the next frame's `placements`-equality reuse check. Includes
        // images evicted from the decode cache but still present in `placements`
        // — those are precisely the ones the cache no longer pins.
        let mut pinned_images: Vec<std::sync::Arc<aterm_core::grid::extra::ImageData>> =
            Vec::with_capacity(placements.len());
        for (image, fp_w, fp_h) in &order {
            let key = (std::sync::Arc::as_ptr(image) as usize, *fp_w, *fp_h);
            let Some(&(y0, dw, dh)) = placements.get(&key) else {
                continue;
            };
            pinned_images.push(std::sync::Arc::clone(image));
            // The decoded LRU is bounded (`GpuImageCache::MAX`) while `placements`
            // is not: when more than that many DISTINCT images are visible, the
            // layout loop above evicted this already-placed key before we re-read
            // it here. Skip such a footprint — its reserved rows stay transparent,
            // so the covered cell's bg shows through — instead of panicking. This
            // is the same graceful fallback a failed decode uses; a frame never
            // aborts on a crowded inline-image screen.
            let Some(decoded) = win.image_cache.peek(image, *fp_w, *fp_h) else {
                continue;
            };
            items.push((y0, dw, dh, decoded.rgba.as_slice()));
        }
        let data = Self::pack_image_plane(&items, tw, th);

        let device = &self.ctx.device;
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aterm-gpu image plane"),
            size: wgpu::Extent3d {
                width: tw,
                height: th,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.ctx.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(tw * 4),
                rows_per_image: Some(th),
            },
            wgpu::Extent3d {
                width: tw,
                height: th,
                depth_or_array_layers: 1,
            },
        );
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("aterm-gpu image plane bg"),
            layout: &self.atlas_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        win.image_plane = Some(ImagePlane {
            bind,
            w: tw,
            h: th,
            placements,
            _pinned_images: pinned_images,
        });
    }

    /// Enable/disable the GPU-only cursor-comet bloom (ON by default — "batteries
    /// included"). Set BEFORE the first frame: the per-window bloom target is built
    /// alongside the offscreen, so a toggle takes full effect on the next offscreen
    /// (re)create (e.g. a resize). The CPU/GPU differential tests call this with
    /// `false` so the parity-critical base render stays byte-exact.
    pub fn set_bloom(&mut self, on: bool) {
        self.enable_bloom = on;
    }

    /// Whether the GPU bloom layer is currently enabled.
    #[must_use]
    pub fn bloom_enabled(&self) -> bool {
        self.enable_bloom
    }

    /// Set the bloom tunables (config `cursor_trail_bloom_strength`/`_radius`):
    /// `strength` is how much of the blurred glow is added back, `radius` how far the
    /// halo spreads (half-res texels). Applied on the next frame; no reallocation.
    pub fn set_bloom_params(&mut self, strength: f32, radius: f32) {
        self.bloom_strength = strength;
        self.bloom_radius = radius;
    }

    /// Enable/disable the GPU-only HEAT SHIMMER (ON by default — quality-first,
    /// config `cursor_fire_shimmer`): the present-time refraction of the air
    /// above burning cells, the bloom's parity class (see [`SHIMMER_SHADER`]).
    /// The CPU/GPU differential and scissor byte-compare tests call this with
    /// `false` — like `set_bloom(false)`, and doubly needed here because the
    /// shimmer phase is wall-clock. Takes effect on the next frame.
    pub fn set_shimmer(&mut self, on: bool) {
        self.enable_shimmer = on;
    }

    /// Whether the GPU heat-shimmer layer is currently enabled.
    #[must_use]
    pub fn shimmer_enabled(&self) -> bool {
        self.enable_shimmer
    }

    /// TEST HOOK: pin the shimmer's present-time phase (`Some(seconds)`) so
    /// readbacks and present arms are deterministic — the analogue of
    /// `reset_glow_ease_for_test` for this pass's one wall-clock term. `None`
    /// restores the wall clock.
    #[doc(hidden)]
    pub fn set_shimmer_phase_for_test(&mut self, phase: Option<f32>) {
        self.shimmer_phase_pin = phase;
    }

    /// TEST HELPER: the shimmer pass rect `(x0, y0, x1, y1)` this input would
    /// derive at its own frame size (the same clamp `encode_frame` applies), or
    /// `None` when no pass would run. Lets tests assert byte-identity OUTSIDE
    /// the bound and visibility inside it without duplicating the derivation.
    #[doc(hidden)]
    #[must_use]
    pub fn shimmer_region_for_test(&self, input: &RenderInput) -> Option<(u32, u32, u32, u32)> {
        let (fw, fh) = self.frame_size(input.rows, input.cols);
        let w = self.clamp_fb_dim(fw as u32);
        let h = self.clamp_fb_dim(fh as u32);
        self.shimmer_region(input, w, h)
            .map(|r| (r.x0, r.y0, r.x1, r.y1))
    }

    /// Whether the heat shimmer runs for THIS frame: the flag is on AND the
    /// frame carries a live FIRE FIELD (`fire_patch` — only the EMBERFORGE
    /// fire style emits it) AND a glow stream to derive the hot region from.
    /// The FIRE gate is the point: the shimmer is fire's heat haze (config
    /// `cursor_fire_shimmer`, "refraction above burning cells"), but the old
    /// gate keyed on `cursor_glow_add` alone — which EVERY glow style fills —
    /// so phaser/laser/water/comet typing paid a frame-region copy + resample
    /// pass per present AND had the air above the cursor wobble their glyphs
    /// (a legibility break with no flame in sight). No flame ⇒ no haze ⇒ the
    /// present is byte-identical to shimmer-off. The SINGLE gate for every
    /// shimmer arm (in-place, present-offscreen, readback routing).
    fn shimmer_live(&self, input: &RenderInput) -> bool {
        self.enable_shimmer && !input.fire_patch.is_empty() && !input.cursor_glow_add.is_empty()
    }

    /// M3 phase B — config `hdr_glow` (DEFAULT OFF): opt the EDR aurora in. Set
    /// BEFORE a window's surface is created to get the `Rgba16Float`
    /// extended-linear swapchain (attach seam); the present seam re-checks it
    /// each frame, so flipping it off live disables the >1.0 emission
    /// immediately while an already-f16 swapchain keeps decoding correctly
    /// (grid clamped at reference white). See `format_plan::hdr_present_plan`
    /// for the proven gating.
    pub fn set_hdr_glow(&mut self, on: bool) {
        self.hdr_glow = on;
    }

    /// Whether the EDR aurora is opted in (config `hdr_glow`).
    #[must_use]
    pub fn hdr_glow_enabled(&self) -> bool {
        self.hdr_glow
    }

    /// Set the SDR glow-boost strength (config `cursor_glow_sdr_boost`, 0..=1;
    /// 0 disables). Combined with the live theme background's luma into the
    /// per-present budget (`aterm_render::hdr::sdr_glow_budget`, proven bounded)
    /// at present time, so a theme reload re-shapes it automatically. The pass
    /// draws into the SWAPCHAIN only — the offscreen the readback/differential
    /// suites consume is untouched at ANY strength.
    pub fn set_sdr_glow_boost(&mut self, strength: f32) {
        self.sdr_glow_boost = strength;
    }

    /// VIDEO introspection: start recording this window's presented frames
    /// with the client's [`crate::video_tap::CaptureOpts`]. Errs plainly where
    /// the backend's surface offers no `COPY_SRC` (the tap never degrades to a
    /// lookalike capture). See [`crate::video_tap`].
    pub fn video_begin(
        &self,
        win: &mut WindowGpu,
        surf: &GpuSurface,
        opts: crate::video_tap::CaptureOpts,
    ) -> Result<(), String> {
        if !surf.copyable {
            return Err("surface not copyable on this backend (no COPY_SRC)".into());
        }
        let tap = crate::video_tap::VideoTap::new(
            &self.ctx.device,
            surf.config.width,
            surf.config.height,
            surf.config.format,
            opts,
        )?;
        win.video = Some(tap);
        Ok(())
    }

    /// VIDEO introspection, the HEADLESS arm ("offscreen-present-real"): start
    /// recording a GLASS-LESS window's presents through a persistent
    /// [`VirtualTarget`] instead of a swapchain — sibling of
    /// [`Self::video_begin`], feeding the UNCHANGED [`crate::video_tap::VideoTap`]
    /// (same ring, counted-drop, budget, resize-finalize disciplines). `(w, h)`
    /// is the caller's chosen present geometry: normally the raw integer-cell
    /// frame, or the visible destination used with
    /// [`Self::present_virtual_cropped`]. It is clamped by the same device limit
    /// the encode clamps to, so the tap matches the corresponding first present.
    /// `video_after_present` / `video_finish` / `video_status` work unchanged on
    /// `win.video`. The HONESTY contract: this arm must be attached ONLY where
    /// no glass exists (the frontend's mode law pins it) — a windowed surface
    /// records through `video_begin`'s swapchain tap, never a lookalike.
    pub fn virtual_begin(
        &self,
        win: &mut WindowGpu,
        w: u32,
        h: u32,
        opts: crate::video_tap::CaptureOpts,
    ) -> Result<(), String> {
        let w = self.clamp_fb_dim(w.max(1));
        let h = self.clamp_fb_dim(h.max(1));
        let tap =
            crate::video_tap::VideoTap::new(&self.ctx.device, w, h, VIRTUAL_PRESENT_FORMAT, opts)?;
        win.virtual_target = Some(self.make_virtual_target(w, h));
        win.video = Some(tap);
        Ok(())
    }

    /// VIDEO introspection post-present hook: stamp the just-presented frame
    /// with the caller's same-clock time and harvest completed copies
    /// (non-blocking). No-op when not recording.
    pub fn video_after_present(&self, win: &mut WindowGpu, t_us: u64) {
        if let Some(tap) = win.video.as_mut() {
            tap.after_present(&self.ctx.device, t_us);
        }
    }

    /// VIDEO introspection: finalize the recording (blocking drain — off the
    /// hot path by definition) and hand the frames out. `None` if not recording.
    pub fn video_finish(&self, win: &mut WindowGpu) -> Option<crate::video_tap::VideoTake> {
        win.video.take().map(|t| t.finish(&self.ctx.device))
    }

    /// VIDEO introspection status: `(frames_so_far, resized_early_stop)`.
    #[must_use]
    pub fn video_status(&self, win: &WindowGpu) -> Option<(usize, bool)> {
        win.video.as_ref().map(|t| (t.frames_so_far(), t.resized()))
    }

    /// Render a [`RenderInput`] snapshot (built by the engine via
    /// [`aterm_core::terminal::Terminal::cell_frame_into`]) on the GPU and read it
    /// back — no `&Terminal` borrow, so the frontend renders after dropping the
    /// lock. As of REARCH A-3 the GPU renderer is a PURE consumer of the snapshot;
    /// the engine emits it and both CPU/GPU paths consume the identical value.
    /// `tray` (P3) bakes the settings card into the offscreen before readback.
    pub fn render_input(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
        tray: Option<TrayQuad<'_>>,
    ) -> Frame {
        // FULL repaint (Clear + all rows) — the snapshot / readback / oracle path.
        // It overwrites the offscreen with this (possibly unrelated) input, so it
        // invalidates the scissored present sequence's prior-frame tracking: a
        // subsequent `present_input` must NOT diff against a frame it never drew.
        // Per-window: reset THIS window's prior-frame validity.
        win.present_prev = None;
        let (w, h) = self.encode_frame(win, input, &RepaintScope::Full);
        // GPU comet BLOOM: the additive halo is no longer composited inside
        // `encode_frame` (that would force every scissored present to rebuild the
        // whole halo band). Composite it into the offscreen HERE — before the tray /
        // sub-row scroll, preserving the halo-under-chrome z-order and the shift —
        // so the readback / introspection frame is base+aurora+halo, byte-identical
        // to the pre-change in-encode bloom. Safe in-place: `render_input`
        // invalidates `present_prev`, so this haloed offscreen is never a scissor
        // base. A glow-free frame is a no-op (byte-identical to the pre-bloom path).
        if self.enable_bloom && !input.cursor_glow_add.is_empty() {
            self.composite_bloom_in_place(win, input);
        }
        // HEAT SHIMMER (bloom parity class): refract the air above the hot
        // region of the FINISHED frame (halo included — hence after the bloom)
        // so the readback / introspection sees exactly what the present
        // composites. In-place is safe for the same reason as the bloom above:
        // `present_prev` is invalidated, so this offscreen is never a scissor
        // base. A region-less frame (no/dark glow, fire on the top row) or a
        // fire-free style (`shimmer_live`'s `fire_patch` gate) runs no pass —
        // byte-identical to the pre-shimmer path.
        if self.shimmer_live(input) {
            self.shimmer_offscreen_in_place(win, input);
        }
        // Bake the settings card INTO the offscreen before readback, so the
        // introspection (snapshot / image) sees exactly what `present_input` blits.
        if let Some(t) = tray {
            self.draw_tray_into_offscreen(win, t);
        }
        // M1b sub-row scroll: shift the terminal-content grid band of the offscreen
        // by the SIGNED `scroll_frac_px` (up for a glide, down for an overscroll
        // bounce) BEFORE the readback, so the oracle/introspection frame is the SAME
        // pixel-shifted frame the CPU present translate produces (the CPU==GPU parity
        // contract for a fractional-scroll frame). A no-op at frac 0 / no band ⇒
        // byte-identical to the pre-M1b readback.
        self.shift_offscreen_band(win, input);
        // The freshly rendered target is resident on `win.offscreen`.
        let tex = win
            .offscreen
            .as_ref()
            .expect("encode_frame sets offscreen")
            .tex
            .clone();
        self.ctx.read_back(&tex, w, h)
    }

    /// M1b SUB-ROW SCROLL — the GPU twin of the CPU present translate
    /// ([`aterm_render::scroll_translate::translate_grid_band_in_place`]): shift the
    /// TERMINAL-CONTENT grid band `[grid_top_row, grid_bot_row)` of this window's
    /// offscreen by the SIGNED `input.scroll_frac_px` device pixels — UP for a
    /// positive glide residual, DOWN for a negative elastic-overscroll bounce — IN
    /// the offscreen texture, so both the on-glass blit and the readback see the
    /// identical pixel-shifted frame. Chrome rows (the spliced tab strip / HUD /
    /// dividers, all OUTSIDE the band) are never touched — the chrome-invariance
    /// theorem.
    ///
    /// It is the SAME integer row memmove over the SAME rendered pixels as the CPU:
    /// the untranslated GPU frame already matches the CPU within the parity
    /// tolerance, so the shifted frame matches too, and the exposed `frac`-px strip
    /// (bottom for up, top for down) keeps the band's own pixels as a placeholder on
    /// BOTH backends (the raster-exact incoming-row fetch is documented-deferred). A
    /// same-texture overlapping `copy_texture_to_texture` is UB, so a resident
    /// scratch texture stages the move (band → scratch, scratch → band shifted); no
    /// shader, no readback, no encode of cells. `frac == 0`, an empty band, or a
    /// fully-exposed band (`moved == 0`) is a literal no-op.
    fn shift_offscreen_band(&mut self, win: &mut WindowGpu, input: &RenderInput) {
        // SIGNED: positive shifts the band UP (glide), negative shifts it DOWN (the
        // elastic-overscroll bounce). The magnitude drives the copy; the sign picks
        // which edge exposes the placeholder strip (bottom for up, top for down).
        let frac = i64::from(input.scroll_frac_px);
        if frac == 0 {
            return;
        }
        let mag = frac.unsigned_abs() as usize;
        let Some(off) = win.offscreen.as_ref() else {
            return;
        };
        let (w, h) = (off.w, off.h);
        // The renderer's own row→px mapping, clamped to the framebuffer — the SAME
        // helper the CPU present path calls, so the two bands cannot drift.
        let (y0, y1) = aterm_render::scroll_translate::grid_band_px(
            self.cpu.grid_top(),
            self.cell_size().1,
            input.grid_top_row,
            input.grid_bot_row,
            h as usize,
        );
        let band_h = y1.saturating_sub(y0);
        let moved = band_h.saturating_sub(mag); // destination rows that pull in-band
        if moved == 0 {
            return;
        }
        // UP: stage the band's lower `moved` rows `[y0+mag, y1)` and lay them back at
        // `[y0, y1-mag)` (bottom strip exposed). DOWN: stage the upper `moved` rows
        // `[y0, y1-mag)` and lay them back at `[y0+mag, y1)` (top strip exposed). A
        // distinct scratch texture stages the move either way, so no overlap UB and
        // the copy order is irrelevant — byte-identical to the CPU bottom-up/top-down
        // in-place walk (`translate_grid_band_in_place`).
        let (src_y, dst_y) = if frac > 0 {
            (y0 + mag, y0)
        } else {
            (y0, y0 + mag)
        };
        let off_tex = off.tex.clone();
        // Reuse the resident scratch when its dims already match; (re)create on the
        // first fractional frame or after a resize. COPY_DST (dest of the down-copy)
        // + COPY_SRC (source of the up-copy); same format as the offscreen so the
        // texture-to-texture copy is valid.
        let scratch_ok = win
            .shift_scratch
            .as_ref()
            .is_some_and(|t| t.width() == w && t.height() == h);
        if !scratch_ok {
            win.shift_scratch = Some(self.ctx.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("aterm-gpu m1b scroll shift scratch"),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: self.ctx.offscreen_format(),
                usage: wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            }));
        }
        let scratch = win.shift_scratch.as_ref().expect("just ensured");
        let mut enc = self
            .ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aterm-gpu m1b scroll shift"),
            });
        let copy_size = wgpu::Extent3d {
            width: w,
            height: moved as u32,
            depth_or_array_layers: 1,
        };
        // 1. Stage the SOURCE band `[src_y, src_y+moved)` into the scratch (a
        //    distinct texture ⇒ no overlap UB).
        enc.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &off_tex,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: src_y as u32,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: scratch,
                mip_level: 0,
                origin: wgpu::Origin3d { x: 0, y: 0, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            copy_size,
        );
        // 2. Copy it back shifted into `[dst_y, dst_y+moved)`. The exposed strip at
        //    the opposite edge is left as-is (the placeholder), exactly like the CPU
        //    `translate_grid_band_in_place`.
        enc.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: scratch,
                mip_level: 0,
                origin: wgpu::Origin3d { x: 0, y: 0, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &off_tex,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: dst_y as u32,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            copy_size,
        );
        self.ctx.queue.submit(std::iter::once(enc.finish()));
    }

    /// Create an on-screen presentation surface for `target` (e.g. an
    /// `Arc<winit::window::Window>`) at the given pixel size, on the SAME
    /// instance/adapter as the offscreen renderer.
    ///
    /// The swapchain format is chosen to be NON-sRGB (preferring `Bgra8Unorm`) so
    /// the raw 8-bit colours blitted from the `Rgba8Unorm` offscreen frame land on
    /// screen byte-identical to the readback the AI introspection sees. An `*Srgb`
    /// surface would re-encode every channel and break that invariant.
    pub fn create_window_surface(
        &mut self,
        target: impl Into<wgpu::SurfaceTarget<'static>>,
        width: u32,
        height: u32,
    ) -> Result<GpuSurface, String> {
        let surface = self
            .ctx
            .instance
            .create_surface(target)
            .map_err(|e| format!("create_surface failed: {e}"))?;
        // M3 phase B: with `hdr_glow` opted in, prefer the `Rgba16Float` EDR
        // swapchain when this surface offers it (wgpu's Metal backend then
        // auto-sets `wantsExtendedDynamicRangeContent`). Decided by the PROVEN
        // pure gate (`format_plan::hdr_swapchain_wants_f16` — the HdrPresentGate
        // Attach action): hdr off ⇒ this arm is never taken and the legacy pick
        // below runs byte-identically. Queries caps per attach (window creation
        // is rare) and deliberately does NOT touch `cached_surface_format`,
        // which stays the SDR-pick cache.
        if self.hdr_glow {
            let caps = surface.get_capabilities(&self.ctx.adapter);
            let supports_f16 = caps.formats.contains(&wgpu::TextureFormat::Rgba16Float);
            if crate::format_plan::hdr_swapchain_wants_f16(self.hdr_glow, supports_f16) {
                let format = wgpu::TextureFormat::Rgba16Float;
                self.ensure_blit_pipeline(format);
                let post_mult = Self::caps_support_post_multiplied(&caps);
                let (usage, copyable) = Self::surface_usage(&caps);
                let config = wgpu::SurfaceConfiguration {
                    usage,
                    format,
                    width: self.clamp_fb_dim(width),
                    height: self.clamp_fb_dim(height),
                    // Same tear-free choice as the SDR path below.
                    present_mode: Self::pick_present_mode(&caps),
                    // M5: translucent glass composites even on the EDR swapchain (the
                    // grid stays reference-white SDR; only the aurora exceeds 1.0).
                    alpha_mode: Self::present_alpha_mode(self.cpu.background_opacity(), post_mult),
                    view_formats: vec![],
                    // Same latency as the SDR paths below (default 1, honoring the
                    // ATERM_GPU_FRAME_LATENCY override): the 2→1 latency win applies
                    // on the EDR swapchain too, not only SDR — a terminal's cheap
                    // on-demand frames gain nothing from a deeper queue but pay a
                    // refresh of keypress-to-glass latency for it.
                    desired_maximum_frame_latency: Self::desired_frame_latency(),
                };
                surface.configure(&self.ctx.device, &config);
                // On DX12 an f16 swapchain is only CORRECT if we can tag it scRGB
                // (extended-linear) — which succeeds iff Windows HDR is ON for this
                // output. If not (SDR mode, or a non-DX12 backend) fall through to the
                // SDR pick below so the EDR present is never shown washed-out. Off
                // Windows this always succeeds (macOS/Metal auto-sets EDR for f16).
                if Self::tag_swapchain_scrgb(&surface) {
                    if std::env::var_os("ATERM_VERBOSE").is_some() {
                        eprintln!(
                            "aterm-gpu: EDR swapchain (Rgba16Float, scRGB), present mode = {:?}",
                            config.present_mode
                        );
                    }
                    return Ok(GpuSurface {
                        surface,
                        config,
                        post_mult,
                        copyable,
                    });
                }
                if std::env::var_os("ATERM_VERBOSE").is_some() {
                    eprintln!(
                        "aterm-gpu: f16 requested but scRGB unsupported (Windows HDR off?) — SDR swapchain"
                    );
                }
                // fall through to the SDR pick below (reconfigures this surface 8-bit)
            }
            // hdr_glow on but the surface has no f16: fall through to the
            // legacy SDR pick (the plan then keeps every HDR arm off).
        }
        // Query the surface capabilities once (the present-mode pick below needs
        // them regardless); the NON-sRGB FORMAT choice is adapter/platform-stable,
        // so it is still cached across windows to skip the `pick_surface_format`
        // re-scan. `caps` also carries the M5 alpha-mode support.
        let caps = surface.get_capabilities(&self.ctx.adapter);
        let format = match self.cached_surface_format {
            Some(f) => {
                // The cache assumes the NON-sRGB format choice is stable across every
                // surface on this single adapter (true on macOS/Metal). Guard that
                // assumption in debug builds at zero release cost: if a surface ever
                // did NOT support the cached format (e.g. a future multi-adapter/
                // -surface backend), this fires loudly so the cache can be re-keyed
                // (clear `cached_surface_format`) instead of silently mis-configuring.
                debug_assert!(
                    caps.formats.contains(&f),
                    "cached surface format {f:?} is not supported by this surface \
                     (supported: {:?}); the per-surface-stable assumption broke — \
                     clear `cached_surface_format` to re-query per attach",
                    caps.formats
                );
                f
            }
            None => {
                let f = Self::pick_surface_format(&caps)?;
                self.cached_surface_format = Some(f);
                f
            }
        };
        // Compile the blit pipeline for this format NOW (off the first-FRAME path);
        // `present_input` then only looks it up. Idempotent if already built.
        self.ensure_blit_pipeline(format);
        // M5: does this surface offer the non-opaque composite? Captured on the
        // GpuSurface so the per-frame alpha-mode reconcile never re-queries.
        let post_mult = Self::caps_support_post_multiplied(&caps);
        let (usage, copyable) = Self::surface_usage(&caps);
        let config = wgpu::SurfaceConfiguration {
            usage,
            format,
            // Clamp to the device limit (same helper as `resize_surface`): an oversized
            // INITIAL surface size would otherwise abort in `surface.configure` — see
            // `clamp_fb_dim`.
            width: self.clamp_fb_dim(width),
            height: self.clamp_fb_dim(height),
            // Tear-free AND smooth: Mailbox (newest-frame-wins, low-latency) where the
            // surface supports it, else Fifo (vsync). NOT AutoNoVsync — on Vulkan/X11 it
            // resolves to Immediate, which TEARS the moving cursor aurora (the "glow
            // isn't smooth" report); Mailbox keeps both smoothness and low latency. See
            // `pick_present_mode`.
            present_mode: Self::pick_present_mode(&caps),
            // M5 true vibrancy: PostMultiplied when the window is translucent (blend
            // the STRAIGHT-alpha frame over its NSVisualEffectView), else Opaque —
            // the byte-identical solid default.
            alpha_mode: Self::present_alpha_mode(self.cpu.background_opacity(), post_mult),
            view_formats: vec![],
            desired_maximum_frame_latency: Self::desired_frame_latency(),
        };
        surface.configure(&self.ctx.device, &config);
        if std::env::var_os("ATERM_VERBOSE").is_some() {
            eprintln!("aterm-gpu: present mode = {:?}", config.present_mode);
        }
        Ok(GpuSurface {
            surface,
            config,
            post_mult,
            copyable,
        })
    }

    /// Configure an ALREADY-CREATED `wgpu::Surface` for presentation at the given
    /// size, on this renderer's adapter/device. Same NON-sRGB format selection as
    /// [`create_window_surface`](Self::create_window_surface) — split out because
    /// the WebGL backend must create the canvas surface BEFORE the adapter exists
    /// (the adapter is enumerated against that surface), so the surface and the
    /// renderer are assembled in the opposite order from native. Takes `&self` (no
    /// first-attach format cache / eager blit-pipeline build — the wasm path makes
    /// exactly one surface and `present_input` lazily ensures the blit pipeline).
    pub fn configure_window_surface(
        &self,
        surface: wgpu::Surface<'static>,
        width: u32,
        height: u32,
    ) -> Result<GpuSurface, String> {
        let caps = surface.get_capabilities(&self.ctx.adapter);
        let format = Self::pick_surface_format(&caps)?;
        let post_mult = Self::caps_support_post_multiplied(&caps);
        let (usage, copyable) = Self::surface_usage(&caps);
        let config = wgpu::SurfaceConfiguration {
            usage,
            format,
            // Clamp to the device limit (same helper as `resize_surface`); the web path
            // (aterm-gpu-web) feeds `frame_size` straight in here, so this covers it too.
            width: self.clamp_fb_dim(width),
            height: self.clamp_fb_dim(height),
            // Tear-free + smooth (Mailbox else Fifo); see `pick_present_mode`.
            present_mode: Self::pick_present_mode(&caps),
            // M5: opacity-aware composite (Opaque at the 1.0 default). The canvas host
            // reaches translucency through its own alpha path, so on the web this
            // stays the solid default unless the surface offers PostMultiplied.
            alpha_mode: Self::present_alpha_mode(self.cpu.background_opacity(), post_mult),
            view_formats: vec![],
            desired_maximum_frame_latency: Self::desired_frame_latency(),
        };
        surface.configure(&self.ctx.device, &config);
        Ok(GpuSurface {
            surface,
            config,
            post_mult,
            copyable,
        })
    }

    /// Pick a NON-sRGB swapchain format: `Bgra8Unorm` if offered (the macOS/Metal
    /// native), else `Rgba8Unorm`, else the first non-`*Srgb` format the surface
    /// supports. Errs only if the surface offers *exclusively* sRGB formats (it
    /// won't on Metal), since presenting through one would gamma-shift the colours.
    /// Choose a TEAR-FREE present mode that also keeps animations (the cursor aurora)
    /// SMOOTH. Prefer `Mailbox` — newest-frame-wins, tear-free AND low-latency, the
    /// best of both — else `Fifo` (plain vsync: tear-free, +~1 refresh of latency).
    /// Deliberately AVOIDS `Immediate`, which `AutoNoVsync` selects FIRST on Vulkan/X11
    /// and which TEARS a moving glow (the "cursor glow isn't smooth" report). `Fifo` is
    /// guaranteed on every backend, so this never fails. On macOS/Metal, Mailbox is NOT
    /// offered (wgpu-hal's Metal backend exposes only Fifo/Immediate), so this selects
    /// Fifo — plain vsync, tear-free.
    /// The swapchain usage: `RENDER_ATTACHMENT`, plus `COPY_SRC` when the surface
    /// caps offer it — the VIDEO-introspection frame tap copies the presented
    /// texture (the exact bytes handed to present, including the swapchain-only
    /// glow/chrome passes every offscreen readback structurally misses). Set once
    /// at configure; on DX12 flip-model backbuffers are natively copyable so the
    /// flag adds no per-frame cost. Returns (usage, copyable).
    fn surface_usage(caps: &wgpu::SurfaceCapabilities) -> (wgpu::TextureUsages, bool) {
        let copyable = caps.usages.contains(wgpu::TextureUsages::COPY_SRC);
        let usage = if copyable {
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC
        } else {
            wgpu::TextureUsages::RENDER_ATTACHMENT
        };
        (usage, copyable)
    }

    fn pick_present_mode(caps: &wgpu::SurfaceCapabilities) -> wgpu::PresentMode {
        // Mailbox everywhere it exists — NON-BLOCKING present (newest-frame-wins),
        // the lowest-latency mode: a keystroke echo acquires an already-free buffer
        // and presents in ~1ms, never waiting on vblank. (An earlier experiment
        // forced Fifo on Windows to vsync-lock the effect animation to 60fps, but
        // Fifo BLOCKS the UI thread each present and, with the continuous effect
        // pump, backed the echo pipeline up to ~180ms input->present — unusable
        // typing. Input latency wins over animation smoothness, unconditionally.)
        if caps.present_modes.contains(&wgpu::PresentMode::Mailbox) {
            wgpu::PresentMode::Mailbox
        } else {
            wgpu::PresentMode::Fifo
        }
    }

    /// Swapchain frame-latency budget (`desired_maximum_frame_latency`). A terminal's
    /// frames are cheap and rendered on demand, so queueing more than ONE only adds
    /// up to a refresh of keypress-to-glass latency (on DX12 this maps straight to
    /// the DXGI maximum frame latency). Default 1 — the lowest tear-free latency the
    /// backend offers alongside Mailbox — overridable via `ATERM_GPU_FRAME_LATENCY`
    /// (clamped to 1..=3) for pipelining experiments; wgpu further clamps to what
    /// the swapchain supports.
    fn desired_frame_latency() -> u32 {
        // Default 2 on Metal (touch-to-glass audit): macOS has NO Mailbox —
        // the swapchain is FIFO with maximumDrawableCount = latency + 1 and an
        // UNBOUNDED nextDrawable. At latency 1 (2 drawables) a typing-hot TUI
        // repaint storm exhausts the pool and parks the event loop — keyDowns
        // queued behind the park measured up to ~84ms to reach the PTY. A third
        // drawable absorbs the in-flight frame (pool non-empty ⇒ acquire returns
        // immediately) and adds ZERO latency while un-exhausted; FIFO still
        // paces the glass. Non-Metal backends with real Mailbox keep 1 via env.
        let default = if cfg!(target_os = "macos") { 2 } else { 1 };
        std::env::var("ATERM_GPU_FRAME_LATENCY")
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok())
            .map_or(default, |v| v.clamp(1, 3))
    }

    fn pick_surface_format(
        caps: &wgpu::SurfaceCapabilities,
    ) -> Result<wgpu::TextureFormat, String> {
        use wgpu::TextureFormat::{Bgra8Unorm, Rgba8Unorm};
        if caps.formats.contains(&Bgra8Unorm) {
            return Ok(Bgra8Unorm);
        }
        if caps.formats.contains(&Rgba8Unorm) {
            return Ok(Rgba8Unorm);
        }
        caps.formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
            .ok_or_else(|| "surface offers no non-sRGB format".to_string())
    }

    /// M5: whether this surface offers the non-opaque composite the translucent
    /// present needs (`CompositeAlphaMode::PostMultiplied` — STRAIGHT alpha, which
    /// matches the offscreen's non-premultiplied bytes). Metal offers it; a backend
    /// that does not keeps the window honestly solid.
    fn caps_support_post_multiplied(caps: &wgpu::SurfaceCapabilities) -> bool {
        caps.alpha_modes
            .contains(&wgpu::CompositeAlphaMode::PostMultiplied)
    }

    /// M5: the swapchain composite-alpha mode for a given default-background
    /// `opacity`. `PostMultiplied` (blend the STRAIGHT-alpha frame over the
    /// window's `NSVisualEffectView`) when the window is translucent AND the
    /// surface offers it; otherwise `Opaque` — the byte-identical solid default,
    /// and the honest fallback where no non-opaque composite exists.
    fn present_alpha_mode(opacity: f32, post_mult: bool) -> wgpu::CompositeAlphaMode {
        if post_mult && aterm_render::vibrancy::is_translucent(opacity) {
            wgpu::CompositeAlphaMode::PostMultiplied
        } else {
            wgpu::CompositeAlphaMode::Opaque
        }
    }

    /// Resize an on-screen surface to a new pixel size and reconfigure it (clamped
    /// to a minimum of 1×1, which wgpu requires, and to a MAXIMUM of the device's
    /// `max_texture_dimension_2d`).
    ///
    /// The upper clamp is a DoS guard, routed through [`clamp_fb_dim`](Self::clamp_fb_dim)
    /// (see there for the full rationale): the control-socket `resize` verb accepts up
    /// to 4096 rows/cols but not pixels, so the padded framebuffer can exceed
    /// `max_texture_dimension_2d`; `surface.configure` would then hit wgpu's default
    /// uncaptured-error handler and abort the process. Clamping the swapchain (blit
    /// DESTINATION) with the SAME helper as the offscreen (blit SOURCE) keeps them 1:1.
    pub fn resize_surface(&self, surf: &mut GpuSurface, width: u32, height: u32) {
        let w = self.clamp_fb_dim(width);
        let h = self.clamp_fb_dim(height);
        // Idempotent: the control-echo follow-up `Resized` (and any resize event that
        // did not change the clamped surface size) must NOT re-`configure` — that
        // would needlessly recreate the swapchain (a visible hitch) for no size
        // change. The frontend sizes the surface to the true window client size on
        // every resize, so this no-ops the common "already this size" re-entry.
        if surf.config.width == w && surf.config.height == h {
            return;
        }
        surf.config.width = w;
        surf.config.height = h;
        surf.surface.configure(&self.ctx.device, &surf.config);
        // A reconfigure rebuilds the DX12 swapchain, which reverts to the DXGI
        // gamma-2.2 default colorspace; re-tag scRGB so an EDR (f16) present stays
        // correct across resizes (no-op on SDR swapchains / off DX12).
        if surf.config.format == wgpu::TextureFormat::Rgba16Float {
            let _ = Self::tag_swapchain_scrgb(&surf.surface);
        }
    }

    /// Windows/DX12: tag the swapchain scRGB (extended-linear,
    /// `DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709`) so an `Rgba16Float` EDR present
    /// composites correctly instead of being reinterpreted as gamma-2.2 (washed
    /// out, with >1.0 clipped by DWM). wgpu 0.29's DX12 backend never calls
    /// `SetColorSpace1`, so we reach the raw `IDXGISwapChain3` via `as_hal`.
    ///
    /// Returns whether scRGB is SUPPORTED + was set — which is exactly "Windows HDR
    /// is ON for this output". Fail-safe: a non-DX12 backend (Vulkan), any COM
    /// error, or HDR-off returns `false` on Windows so the caller keeps the SDR
    /// swapchain; off Windows it returns `true` (macOS/Metal auto-sets EDR for an
    /// f16 layer, so the existing behavior is preserved).
    #[cfg(not(windows))]
    fn tag_swapchain_scrgb(_surface: &wgpu::Surface) -> bool {
        true
    }

    #[cfg(windows)]
    fn tag_swapchain_scrgb(surface: &wgpu::Surface) -> bool {
        use windows::Win32::Graphics::Dxgi::Common::{
            DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709, DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020,
        };
        use windows::Win32::Graphics::Dxgi::{
            DXGI_SWAP_CHAIN_COLOR_SPACE_SUPPORT_FLAG_PRESENT, IDXGIOutput6,
        };
        use windows::core::Interface as _;
        // SAFETY: the hal-surface guard borrows the live surface for this scope
        // only; we read its `IDXGISwapChain3` and issue result-checked COM calls,
        // then drop the guard. `as_hal::<Dx12>` yields `None` on any non-DX12 backend
        // (e.g. Vulkan), where DXGI colorspace tagging does not apply.
        unsafe {
            let Some(hal_surf) = surface.as_hal::<wgpu::hal::api::Dx12>() else {
                return false;
            };
            let Some(sc) = hal_surf.swap_chain() else {
                return false;
            };
            // GATE ON WINDOWS HDR ACTUALLY BEING ON for this output. `CheckColorSpaceSupport`
            // alone is too loose: AMD/Intel report scRGB PRESENT-support even on an SDR
            // desktop, so tagging + presenting the f16 EDR swapchain there hands DWM
            // extended-linear data on an sRGB pipeline — the grid renders washed-out / colour-
            // cast (the "yellow text" bug on the 780M). The containing output's CURRENT colour
            // space is the canonical HDR-toggle signal: HDR10 (G2084 P2020) when Windows HDR is
            // on, sRGB (G22 P709) when off. Require it before taking the EDR path; any query
            // failure (unusual multi-GPU / remoting) fails SAFE to the correct 8-bit present.
            let hdr_on = sc
                .GetContainingOutput()
                .ok()
                .and_then(|o| o.cast::<IDXGIOutput6>().ok())
                .and_then(|o6| o6.GetDesc1().ok())
                .map(|d| d.ColorSpace == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020)
                .unwrap_or(false);
            if !hdr_on {
                return false;
            }
            let cs = DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709;
            let supported = matches!(
                sc.CheckColorSpaceSupport(cs),
                Ok(flags) if (flags & DXGI_SWAP_CHAIN_COLOR_SPACE_SUPPORT_FLAG_PRESENT.0 as u32) != 0
            );
            supported && sc.SetColorSpace1(cs).is_ok()
        }
    }

    /// Render the frame offscreen (the single source of truth) and PRESENT it on
    /// the GPU by blitting that texture into `surf`'s swapchain — no CPU readback,
    /// no softbuffer copy. `invert` flips RGB for the visual-bell flash.
    ///
    /// This is the thin SWAPCHAIN ARM of the present: reconcile the composite-
    /// alpha mode, acquire the swapchain texture, run the shared
    /// [`Self::present_to_view`] compose-and-blit seam against it, and
    /// `present()`. The HEADLESS twin ([`Self::present_virtual`]) runs the SAME
    /// seam against a window's [`VirtualTarget`] — byte parity between the two
    /// is by construction (one body), and pinned by the present-real theorem
    /// test in `tests/present_real.rs`.
    pub fn present_input(
        &mut self,
        win: &mut WindowGpu,
        surf: &mut GpuSurface,
        input: &RenderInput,
        invert: bool,
        overlay: Option<DropOverlay>,
        // Settings-card overlay (P3). `None` (the default) draws nothing — the
        // feature is OFF unless the caller passes a card.
        tray: Option<TrayQuad<'_>>,
    ) -> Result<(), SurfacePresentFailure> {
        self.present_input_with_crop(win, surf, input, invert, overlay, tray, None)
    }

    /// [`Self::present_input`] with an explicit frontend-visible source interval.
    /// The raw renderer allocation is unchanged; rows outside `crop` are treated
    /// as destination bands by the shared blit/present seam.
    #[allow(
        clippy::too_many_arguments,
        reason = "the cropped public twin preserves present_input's established presentation tuple and adds one source interval"
    )]
    pub fn present_input_cropped(
        &mut self,
        win: &mut WindowGpu,
        surf: &mut GpuSurface,
        input: &RenderInput,
        invert: bool,
        overlay: Option<DropOverlay>,
        tray: Option<TrayQuad<'_>>,
        crop: PresentCrop,
    ) -> Result<(), SurfacePresentFailure> {
        self.present_input_with_crop(win, surf, input, invert, overlay, tray, Some(crop))
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the shared swapchain seam carries present_input's established tuple plus the optional source crop"
    )]
    fn present_input_with_crop(
        &mut self,
        win: &mut WindowGpu,
        surf: &mut GpuSurface,
        input: &RenderInput,
        invert: bool,
        overlay: Option<DropOverlay>,
        tray: Option<TrayQuad<'_>>,
        source_crop: Option<PresentCrop>,
    ) -> Result<(), SurfacePresentFailure> {
        let source_crop = if let Some(crop) = source_crop {
            let (_, logical_raw_height) = self.frame_size(input.rows, input.cols);
            let logical_raw_height = u32::try_from(logical_raw_height).unwrap_or(u32::MAX).max(1);
            let resident_raw_height = self.clamp_fb_dim(logical_raw_height);
            Some(
                normalize_present_crop(crop, logical_raw_height, resident_raw_height)
                    .ok_or(SurfacePresentFailure::Validation)?,
            )
        } else {
            None
        };
        // Returns `Ok(())` only when a frame was presented. A typed failure lets
        // the caller choose a bounded retry policy instead of immediately
        // requesting redraw forever while the surface stays unavailable.
        //
        // M5 true vibrancy: reconcile the swapchain composite-alpha mode with the
        // LIVE `background_opacity` (a config hot-reload can flip it either way).
        // PostMultiplied when translucent (blend the STRAIGHT-alpha frame over the
        // window's NSVisualEffectView), else Opaque. Reconfigure only on a real
        // change (rare), so a steady present never re-creates the swapchain. The
        // matching `layer.opaque` / NSVisualEffectView toggles are driven from the
        // frontend (aterm-gui `apply_render_knob` / window attach).
        let want_alpha = Self::present_alpha_mode(self.cpu.background_opacity(), surf.post_mult);
        if surf.config.alpha_mode != want_alpha {
            surf.config.alpha_mode = want_alpha;
            surf.surface.configure(&self.ctx.device, &surf.config);
        }

        // Acquire the next swapchain texture BEFORE any compose work, so a dropped
        // acquire leaves the window's offscreen/scissor state untouched (the retry
        // redraw re-runs the whole compose). On Outdated/Lost the surface config no
        // longer matches; reconfigure and skip this frame (the next redraw
        // presents). Timeout/Occluded/Validation: skip this frame.
        use wgpu::CurrentSurfaceTexture as C;
        let frame = match surf.surface.get_current_texture() {
            C::Success(f) | C::Suboptimal(f) => f,
            C::Outdated | C::Lost => {
                surf.surface.configure(&self.ctx.device, &surf.config);
                // DROPPED: config no longer matches (common on a fresh window whose
                // CAMetalLayer is not yet composited). Reconfigured; retry next redraw.
                return Err(SurfacePresentFailure::Reconfigured);
            }
            C::Timeout => return Err(SurfacePresentFailure::Timeout),
            C::Occluded => return Err(SurfacePresentFailure::Occluded),
            C::Validation => return Err(SurfacePresentFailure::Validation),
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let work_started = std::time::Instant::now();
        self.present_to_view(
            win,
            input,
            invert,
            overlay,
            tray,
            source_crop,
            PresentDest {
                view: &view,
                tex: &frame.texture,
                w: surf.config.width,
                h: surf.config.height,
                format: surf.config.format,
                translucent: want_alpha == wgpu::CompositeAlphaMode::PostMultiplied,
            },
        );
        win.last_present_work_ns =
            u64::try_from(work_started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        frame.present();
        Ok(())
    }

    /// The COMPOSE-AND-BLIT body of a present, extracted VERBATIM from
    /// `present_input` and parameterized over the destination ([`PresentDest`]:
    /// view + texture + `(w, h)` + format + composite mode) so the swapchain arm
    /// and the headless [`Self::present_virtual`] arm share ONE body — the
    /// present-real theorem's "byte parity by construction" seam. Encodes the
    /// offscreen frame (scissored dirty-row repaint), composites bloom/tray,
    /// shifts the sub-row scroll band, runs the letterbox blit + the EDR-aurora /
    /// SDR-crown pass into `dest.view`, appends the VIDEO tap's copy of
    /// `dest.tex` to the SAME encoder, and submits. The caller owns
    /// acquire/alpha-mode/`present()` (swapchain) or the virtual target
    /// (headless).
    #[allow(
        clippy::too_many_arguments,
        reason = "the common glass/headless encoder needs the existing frame effects, source crop, and typed destination"
    )]
    fn present_to_view(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
        invert: bool,
        overlay: Option<DropOverlay>,
        tray: Option<TrayQuad<'_>>,
        source_crop: Option<PresentCrop>,
        dest: PresentDest<'_>,
    ) {
        // 1. Offscreen render (submits). SCISSORED DIRTY-ROW REPAINT: when the
        //    persistent offscreen still holds the prior presented frame and only
        //    some rows differ, re-encode ONLY those rows (LoadOp::Load + a scissor
        //    over the dirty band) — proportional to the change, not the screen.
        //    Otherwise a full Clear+all-rows repaint (the always-correct path).
        //    The rendered target + its blit-source bind group are resident on
        //    `win.offscreen` (built once, reused across presents at the same
        //    dimensions; rebuilt only on a resize), so this present allocates no
        //    per-frame texture / view / blit bind group.
        // The tray is baked into the PERSISTENT offscreen below; the scissored
        // dirty-row path would otherwise re-blend it over itself (alpha compounding)
        // or leave stale card pixels when the card moves/closes. Force a full clean
        // repaint while a card is shown, and on the present that first drops it. Cheap
        // (a card shows only during settings UI, never on the typing hot path).
        let tray_full = tray.is_some() || win.tray_overlay.is_some();
        if tray_full {
            win.present_prev = None;
        }
        // M1b sub-row scroll: a fractional frame MUTATES the offscreen (the grid-band
        // shift below), so the scissored dirty-row diff must never compare against
        // it. Force a full clean repaint (fresh UNtranslated offscreen) whenever the
        // shift runs this frame OR ran last frame (so the frame returning to frac 0
        // also re-renders clean). SIGNED: a negative frac (overscroll bounce) mutates
        // the offscreen just like a positive one, so the gate keys on `!= 0` for
        // EITHER sign — otherwise successive down-shifts would compound on a stale
        // offscreen. `0` on every whole-row frame ⇒ byte-identical to the pre-M1b
        // scissored path.
        let frac = input.scroll_frac_px;
        let shift_full = frac != 0 || win.prev_frac != 0;
        if shift_full {
            win.present_prev = None;
        }
        let (fw, fh) = self.encode_present_frame(win, input);

        let bloom_glow_present = self.enable_bloom && !input.cursor_glow_add.is_empty();
        // HEAT SHIMMER rides the identical present-time parity class off the
        // SAME glow stream, gated to the FIRE style (`shimmer_live`: a live
        // `fire_patch` — reduced-motion / unfocused windows emit no quads ⇒
        // no region ⇒ no shimmer, automatically). Both effects share the
        // in-place vs present-offscreen routing below; the shimmer always runs
        // AFTER the halo so it refracts the finished frame.
        let shimmer_present = self.shimmer_live(input);
        let fx_present = bloom_glow_present || shimmer_present;
        // The tray / sub-row-scroll frames MUTATE the offscreen in place (card
        // composite, grid-band shift) and are already forced to a Full repaint, so
        // the offscreen is not reused as a scissor base. On those frames composite
        // the comet halo IN PLACE into the offscreen — BEFORE the tray/shift, so the
        // halo sits under the chrome and rides the scroll exactly as the pre-change
        // in-encode bloom did — then invalidate `present_prev` at the end so the
        // haloed offscreen is never diffed against. The hot typing path (no tray, no
        // shift) keeps the offscreen a CLEAN halo-free scissor base and composites
        // the halo over a throwaway `present_offscreen` below.
        let bake_in_place = fx_present && (tray_full || shift_full);
        // When a bloom composite below builds + uploads the UNGATED glow
        // instances (`vbufs.bloom_glow`), remember the count — the EDR/SDR
        // crown passes read the same buffer and must not build it twice.
        let mut ungated_built: Option<u32> = None;
        if bake_in_place {
            if bloom_glow_present {
                ungated_built = Some(self.composite_bloom_in_place(win, input));
            }
            if shimmer_present {
                self.shimmer_offscreen_in_place(win, input);
            }
        }

        // Composite the settings card INTO the offscreen (the single source of
        // truth) so the on-glass blit below shows exactly what `render_input` reads
        // back. A `None` tray drops any resident overlay and draws nothing.
        match tray {
            Some(t) => self.draw_tray_into_offscreen(win, t),
            None => win.tray_overlay = None,
        }

        // M1b sub-row scroll: shift the terminal-content grid band by the SIGNED
        // `frac` (up for a glide residual, down for an overscroll bounce) on the
        // offscreen AFTER the tray composite (chrome stays pinned), so the blit
        // below — and any concurrent readback (screen == introspection) — presents
        // the pixel-shifted frame the CPU translate produces. `prev_frac` is stamped
        // so next frame's full-repaint gate (above) fires exactly once after motion.
        self.shift_offscreen_band(win, input);
        win.prev_frac = frac;

        // HOT PATH: the offscreen is a CLEAN base+aurora scissor base; composite the
        // comet halo over a throwaway copy of it (`present_offscreen`) which the blit
        // samples instead — so the scissored dirty set stays proportional to real
        // content change (no halo-band rebuild per aurora tick) while the presented
        // frame stays byte-identical to the old in-offscreen bloom.
        //
        // TYPING-2 revisited: the halo composite used to be DEFERRED on an
        // `input_hot` keystroke-echo frame (a whole-framebuffer copy + second
        // submit on the latency path). That was imperceptible while the bloom was
        // an opt-in rarity — but with the bloom ON BY DEFAULT and the halo
        // continuously present during typing, deferring it BLINKS the halo off
        // for exactly the echo frame of every keystroke (the reported
        // "little flash when I type"). The composite now runs on echo frames too:
        // the GPU-internal copy costs ~0.1-0.3 ms — far under perception and far
        // under the 16.7 ms frame budget — and every frame is halo-consistent.
        // The offscreen stays a clean halo-free scissor base either way, so
        // `present_prev` diffing is unaffected.
        let use_present_off = fx_present && !bake_in_place;
        if use_present_off && let Some(n) = self.compose_present_offscreen(win, input) {
            ungated_built = Some(n);
        }

        // 2. Ensure a blit pipeline for this destination format exists. The tray is
        //    already composited into the offscreen (step above), so the blit just
        //    blits the offscreen-with-card — no swapchain-side tray pass.
        let format = dest.format;
        self.ensure_blit_pipeline(format);
        // M3 phase B: the PROVEN present gate (HdrPresentGate.Present — see
        // format_plan). Keyed on the swapchain's ACTUAL format, so with
        // `hdr_glow` off (or an SDR swapchain) both arms are provably inert.
        // Resolved BEFORE the `&self.blit_pipelines` borrow below because the
        // aurora-pass resources build lazily (`&mut self`).
        //
        // The crown passes read the UNGATED aurora stream (`build_bloom_glow`),
        // NOT the row-gated `inst.glow_add`: glow frames STAY on the scissored
        // path (pinned by `bloom_glow_at_grid_edge_scissors_and_matches`), and
        // the gated stream is only complete because `compute_dirty_rows` marks
        // every prev∪cur glow row dirty whenever the glow CHANGES — a
        // byte-stable non-empty glow coinciding with an unrelated dirty row
        // would gate down to a partial (possibly empty) subset and blink the
        // crown off for that present (the exact hazard `build_bloom_glow` is
        // ungated for). Presence is decided from the raw input stream with the
        // same rows-bound skip; the instances are built below (or reused from
        // the bloom composite above) only when a crown pass will fire.
        let glow_present = input
            .cursor_glow_add
            .iter()
            .any(|q| (q.row as usize) < input.rows);
        let plan = crate::format_plan::hdr_present_plan(
            self.hdr_glow,
            format == wgpu::TextureFormat::Rgba16Float,
            glow_present,
        );
        // The EDR aurora pass (>1.0 emission) — bounded by the per-window panel
        // headroom (the proven sanitize/clamp chain in `aterm_render::hdr`). An
        // SDR panel (headroom 0.0) provably adds nothing, so skip the pass.
        let headroom = aterm_render::hdr::additive_headroom(win.edr_max);
        let run_hdr_glow = plan.glow_boost_pass && headroom > 0.0;
        if run_hdr_glow {
            self.ensure_hdr_glow_pipeline();
        }
        // SDR twin (the crown on a NON-f16 present): budget from the proven
        // `sdr_glow_budget` over the LIVE terminal background (OSC 11 wins over
        // theme — the same value the letterbox clear uses below), gated by the
        // pure `format_plan::sdr_boost_pass` (mutually exclusive with the EDR
        // pass; off at zero budget: strength 0, light themes, poisoned inputs).
        // Swapchain-only — the offscreen readback/differential source of truth
        // is untouched at any budget.
        let live_bg = if input.default_bg == aterm_core::render::COLOR_UNSET {
            self.theme.bg
        } else {
            input.default_bg
        };
        let sdr_budget_target = aterm_render::hdr::sdr_glow_budget(
            aterm_render::hdr::packed_luma(live_bg),
            self.sdr_glow_boost,
        );
        // ATTACK ENVELOPE: ease the applied budget toward the target with a ~45ms
        // rise (1 - e^(-dt/45ms)) so the crown blooms in over ~3 frames instead of
        // strobing to full brightness on the first keystroke frame. Decay rides the
        // glow quads' own fade; an EMPTY stream resets the level so the next burst
        // re-blooms softly. The eased level can only ever be <= the proven-bounded
        // target, so the SDR_GLOW_BUDGET_BASE bound is preserved by construction.
        let sdr_budget = if !glow_present {
            self.sdr_glow_level = 0.0;
            self.sdr_glow_level_at = None;
            0.0
        } else {
            let now = web_time::Instant::now();
            let dt = self
                .sdr_glow_level_at
                .map_or(0.016, |t| now.saturating_duration_since(t).as_secs_f32());
            self.sdr_glow_level_at = Some(now);
            if sdr_budget_target > self.sdr_glow_level {
                let a = 1.0 - (-dt / 0.045).exp();
                self.sdr_glow_level += (sdr_budget_target - self.sdr_glow_level) * a;
            } else {
                self.sdr_glow_level = sdr_budget_target;
            }
            self.sdr_glow_level
        };
        let run_sdr_glow = crate::format_plan::sdr_boost_pass(
            format == wgpu::TextureFormat::Rgba16Float,
            glow_present,
            sdr_budget,
        );
        if run_sdr_glow {
            self.ensure_sdr_glow_pipeline(format);
        }
        // The crown's ungated instances: reuse the bloom composite's build, or
        // build + upload now (bloom off / shimmer-only frames). `glow_present`
        // guarantees a non-zero count, so the passes below always draw.
        let crown_count = if run_hdr_glow || run_sdr_glow {
            match ungated_built {
                Some(n) => n,
                None => self.build_bloom_glow(input).0,
            }
        } else {
            0
        };
        let pipeline = &self.blit_pipelines[&format];

        // 4. Build the blit uniform (bell invert + drop-target highlight + the W1
        //    band placement) and upload it ONLY when it differs from what the SHARED
        //    buffer holds — a steady present (stable invert, no hover, stable size)
        //    re-uploads nothing, while windows presenting interleaved values each
        //    rewrite it (the memo is renderer-level, keyed to the buffer, NOT the
        //    window). The offscreen texture is already bound as the blit source by
        //    the resident `blit_bind`. W1: the swapchain is sized to the true window
        //    client area, so the grid frame is letterboxed and the leftover bands
        //    are painted the live terminal background in this same blit pass — the
        //    compositor never rescales. At exact grid fit there is no remainder and
        //    the blit is byte-identical to the historical full-surface present.
        let mut want = present_blit_uniform(
            invert,
            overlay,
            source_crop,
            fw,
            fh,
            dest.w,
            dest.h,
            live_bg,
        );
        // Downlevel (sRGB-typed offscreen): the blit samples a view that auto-decodes to
        // linear, so it must re-encode to sRGB for the non-sRGB swapchain.
        want.encode_srgb = if self.ctx.srgb_offscreen { 0.0 } else { 1.0 };
        // M3: linear-decode iff the swapchain is the f16 EDR target (see the
        // plan above). With hdr off this stays 0.0 — the uniform bytes equal
        // the pre-M3 layout (`hdr` was the zero pad).
        want.hdr = if plan.blit_linear_encode { 1.0 } else { 0.0 };
        // M3 (Windows scRGB): scale reference-white to the SDR-white level on the EDR
        // present so the grid isn't dim (scRGB 1.0 == 80 nits). 1.0 off the f16 path.
        want.sdr_white_scale = if plan.blit_linear_encode {
            win.sdr_white_scale.max(1.0)
        } else {
            1.0
        };
        // M5 true vibrancy: when the swapchain is actually PostMultiplied (the
        // window is translucent AND the surface offered it), the blit emits the
        // offscreen/band alpha so the compositor blends over the NSVisualEffectView.
        // Keyed on the CONFIGURED alpha mode (reconciled by the swapchain arm),
        // never merely on the opacity, so a surface with no non-opaque composite
        // stays solid — no stray alpha on an Opaque swapchain (and the virtual
        // arm, always opaque, never emits it). The band alpha is `bg_quad_alpha`
        // normalized, matching the bg quads.
        if dest.translucent {
            let band_a = f32::from(aterm_render::vibrancy::bg_quad_alpha(
                self.cpu.background_opacity(),
            )) / 255.0;
            want = want.with_translucency(band_a);
        }
        let crown_scissor = visible_source_scissor(&want, dest.w, dest.h);
        // Renderer-level memo (buffer-keyed, not per-window): the blit uniform is
        // re-uploaded only when it changes (invert flip, hover, resize, EDR mode).
        if self.blit_uniform_written != Some(want) {
            self.ctx
                .queue
                .write_buffer(&self.blit_uniform_buf, 0, bytemuck::bytes_of(&want));
            self.blit_uniform_written = Some(want);
        }
        if run_hdr_glow {
            let hu = HdrGlowUniform {
                screen: [dest.w as f32, dest.h as f32],
                content_off: want.content_off,
                // scRGB: scale the aurora to the same reference-white as the grid so its
                // peak reaches the panel max (min(lin*boost*s, headroom*s) == s*min(..)).
                boost: aterm_render::hdr::HDR_GLOW_BOOST * win.sdr_white_scale.max(1.0),
                headroom: headroom * win.sdr_white_scale.max(1.0),
                _pad: [0.0, 0.0],
            };
            self.ctx.queue.write_buffer(
                self.hdr_glow_uniform_buf
                    .as_ref()
                    .expect("ensure_hdr_glow_pipeline sets the uniform buf"),
                0,
                bytemuck::bytes_of(&hu),
            );
        } else if run_sdr_glow {
            // SDR: `headroom` carries the BUDGET (≤ 0.35, proven); boost 1.0 —
            // `fs_sdr_glow` scales the colour by the budget (gradient-preserving,
            // peak = budget), no reference-white involvement on an SDR present.
            let hu = HdrGlowUniform {
                screen: [dest.w as f32, dest.h as f32],
                content_off: want.content_off,
                boost: 1.0,
                headroom: sdr_budget,
                _pad: [0.0, 0.0],
            };
            self.ctx.queue.write_buffer(
                self.hdr_glow_uniform_buf
                    .as_ref()
                    .expect("ensure_sdr_glow_pipeline sets the uniform buf"),
                0,
                bytemuck::bytes_of(&hu),
            );
        }
        // On the hot bloom path the blit samples the `present_offscreen` (clean copy
        // + halo); otherwise the offscreen itself (which carries the in-place halo on
        // a tray/scroll frame, or no halo at all). Both blit binds are byte-identical
        // in layout/sampler/uniform, so the blit pipeline + uniform are unchanged.
        let bind = if use_present_off {
            &win.present_offscreen
                .as_ref()
                .expect("compose_present_offscreen set it")
                .blit_bind
        } else {
            &win.offscreen
                .as_ref()
                .expect("encode_frame sets offscreen")
                .blit_bind
        };

        // 3. LETTERBOX BLIT (M3: on an EDR present the aurora boost draws in the
        //    SAME pass, One/One over the just-blitted frame — see `run_hdr_glow`
        //    below). The swapchain is sized to the TRUE window client area,
        //    which is rarely an exact multiple of the cell size (a maximized / snapped
        //    window essentially never is), but the offscreen is the integer-cell grid
        //    (`fw`×`fh`). Resolve the whole destination in the band-aware shader: it
        //    places the offscreen 1:1 at the centred content offset and fills every
        //    surrounding sub-cell remainder pixel with the terminal background, so
        //    the WSI
        //    (DX12 `DXGI_SCALING_STRETCH` / Vulkan) never bilinearly stretches the
        //    glyphs to the odd window size — the "shimmery/blurry text" defect. When
        //    the window IS an exact cell multiple the content covers the whole
        //    swapchain and this is byte-identical to the old full-surface blit. The
        //    legacy VIRTUAL target is grid-sized too; its cropped sibling instead
        //    uses the frontend's explicit visible destination.
        let (sw, sh) = (dest.w, dest.h);
        // Letterbox margin colour = the live terminal background (OSC 11 / DECSCNM,
        // else the theme), so the margin matches the offscreen's own padding band and
        // reads as an even border, not a seam. The swapchain is non-sRGB (raw bytes),
        // so the packed sRGB byte / 255 lands verbatim — the same formula the padding
        // quads use. (`live_bg` was resolved above, shared with the SDR-glow budget.)
        let bg = live_bg;
        let clear = wgpu::Color {
            r: ((bg >> 16) & 0xff) as f64 / 255.0,
            g: ((bg >> 8) & 0xff) as f64 / 255.0,
            b: (bg & 0xff) as f64 / 255.0,
            a: 1.0,
        };
        let mut enc = self
            .ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aterm-gpu blit"),
            });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("aterm-gpu blit pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: dest.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // Clear the WHOLE swapchain to the terminal bg first. The
                        // full-destination blit below overwrites every pixel; the clear
                        // remains a defensive initialization if a future pass skips a draw.
                        load: wgpu::LoadOp::Clear(clear),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, bind, &[]);
            // Cover the WHOLE destination. `fs_blit` performs the 1:1 placement itself:
            // destination pixels outside `content_off + dims` take the live background,
            // while in-bounds pixels use an exact `textureLoad`. Restricting this to the
            // source extent clips the trailing content after a centred odd remainder and
            // exposes the raw clear (especially visible on an f16/EDR swapchain). The HDR
            // and SDR crown draws below share this destination-space viewport as well.
            pass.set_viewport(0.0, 0.0, sw as f32, sh as f32, 0.0, 1.0);
            pass.set_scissor_rect(0, 0, sw, sh);
            pass.draw(0..3, 0..1);
            if run_hdr_glow && let Some((x, y, width, height)) = crown_scissor {
                pass.set_scissor_rect(x, y, width, height);
                // The UNGATED aurora instances for THIS frame are resident in
                // the bloom_glow vertex buffer (built above whenever a crown
                // pass fires — the row-gated `glow_add` buffer can hold a
                // scissored subset; see the `glow_present` note).
                pass.set_pipeline(
                    self.hdr_glow_pipeline
                        .as_ref()
                        .expect("ensure_hdr_glow_pipeline sets the pipeline"),
                );
                pass.set_bind_group(
                    0,
                    self.hdr_glow_bg
                        .as_ref()
                        .expect("ensure_hdr_glow_pipeline sets the bind group"),
                    &[],
                );
                pass.set_vertex_buffer(0, self.vbufs.bloom_glow.buf.slice(..));
                pass.draw(0..6, 0..crown_count);
            } else if run_sdr_glow && let Some((x, y, width, height)) = crown_scissor {
                pass.set_scissor_rect(x, y, width, height);
                // SDR crown: same instances, same shared bind group, the
                // budget-scaled fragment on the actual swapchain format.
                pass.set_pipeline(
                    &self
                        .sdr_glow_pipeline
                        .as_ref()
                        .expect("ensure_sdr_glow_pipeline sets the pipeline")
                        .1,
                );
                pass.set_bind_group(
                    0,
                    self.hdr_glow_bg
                        .as_ref()
                        .expect("ensure_sdr_glow_pipeline sets the bind group"),
                    &[],
                );
                pass.set_vertex_buffer(0, self.vbufs.bloom_glow.buf.slice(..));
                pass.draw(0..6, 0..crown_count);
            }
        }
        // VIDEO introspection tap: copy the EXACT presented bytes (post-blit,
        // post-glow, post-chrome) into the recording ring — appended to the SAME
        // encoder after the pass closed, so it captures precisely what present()
        // ships (or, on the virtual arm, precisely what it WOULD have shipped —
        // the tap still copies the exact bytes of the texture it is aimed at; the
        // recording's `mode` label discloses whether that texture reached
        // photons). One `Option` branch when off; counted-drop when the ring is
        // saturated (never blocks). See `video_tap`.
        if let Some(tap) = win.video.as_mut() {
            tap.enqueue_copy(&mut enc, dest.tex);
        }
        self.ctx.queue.submit([enc.finish()]);

        // A tray/scroll frame composited the halo IN PLACE into the offscreen, so it
        // is no longer a clean base+aurora frame: the NEXT present must not scissor
        // against it (the halo would be Load-preserved and re-added). Force a clean
        // FULL repaint next present. (One extra Full frame after the rare
        // settings-card / scroll-end tick; the hot typing path never bakes in place.)
        if bake_in_place {
            win.present_prev = None;
        }
    }

    /// HEADLESS PRESENT-REAL: present `input` into this window's persistent
    /// [`VirtualTarget`] — the glass-less twin of [`Self::present_input`],
    /// running the SAME [`Self::present_to_view`] compose-and-blit seam (encode,
    /// bloom, tray, sub-row shift, letterbox blit, SDR-crown/EDR pass, VIDEO-tap
    /// copy in the same encoder) and submitting, with no `present()` — nothing
    /// reaches photons, which the recording's `"offscreen-present-real"` mode
    /// label discloses. This legacy arm tracks the exact raw integer-cell frame
    /// of the CURRENT input (the same `frame_size` + device-clamp math
    /// `encode_frame` uses), so its letterbox pass degenerates to a full-viewport
    /// blit. [`Self::present_virtual_cropped`] supplies independent visible
    /// geometry for asymmetric-padding frontends. A mid-recording size change
    /// recreates the target, and the tap then finalizes honestly on the mismatch
    /// — the exact swapchain-resize semantics.
    /// Returns `true` (the virtual acquire cannot fail); the shape mirrors
    /// `present_input` so the frontend treats both arms uniformly.
    pub fn present_virtual(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
        invert: bool,
        overlay: Option<DropOverlay>,
        tray: Option<TrayQuad<'_>>,
    ) -> bool {
        let (fw, fh) = self.frame_size(input.rows, input.cols);
        self.present_virtual_with_crop(
            win,
            input,
            invert,
            overlay,
            tray,
            None,
            (fw as u32, fh as u32),
        )
    }

    /// Glass-less present with explicit visible source and destination geometry.
    /// Used by frontends whose renderer transport is intentionally larger than
    /// the frame exposed by image/dims/video introspection.
    #[allow(
        clippy::too_many_arguments,
        reason = "the cropped public twin preserves present_virtual's established presentation tuple and adds source/destination geometry"
    )]
    pub fn present_virtual_cropped(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
        invert: bool,
        overlay: Option<DropOverlay>,
        tray: Option<TrayQuad<'_>>,
        crop: PresentCrop,
        destination: (u32, u32),
    ) -> bool {
        self.present_virtual_with_crop(win, input, invert, overlay, tray, Some(crop), destination)
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the internal shared virtual-present seam carries the existing present tuple plus source crop and destination geometry"
    )]
    fn present_virtual_with_crop(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
        invert: bool,
        overlay: Option<DropOverlay>,
        tray: Option<TrayQuad<'_>>,
        source_crop: Option<PresentCrop>,
        destination: (u32, u32),
    ) -> bool {
        let source_crop = if let Some(crop) = source_crop {
            let (_, logical_raw_height) = self.frame_size(input.rows, input.cols);
            let logical_raw_height = u32::try_from(logical_raw_height).unwrap_or(u32::MAX).max(1);
            let resident_raw_height = self.clamp_fb_dim(logical_raw_height);
            let Some(crop) = normalize_present_crop(crop, logical_raw_height, resident_raw_height)
            else {
                return false;
            };
            Some(crop)
        } else {
            None
        };
        let w = self.clamp_fb_dim(destination.0.max(1));
        let h = self.clamp_fb_dim(destination.1.max(1));
        if win
            .virtual_target
            .as_ref()
            .is_none_or(|t| t.w != w || t.h != h)
        {
            win.virtual_target = Some(self.make_virtual_target(w, h));
        }
        let tex = win
            .virtual_target
            .as_ref()
            .expect("ensured above")
            .tex
            .clone();
        // A fresh per-present view of the persistent texture — exactly how the
        // swapchain arm views its acquired frame.
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        let work_started = std::time::Instant::now();
        self.present_to_view(
            win,
            input,
            invert,
            overlay,
            tray,
            source_crop,
            PresentDest {
                view: &view,
                tex: &tex,
                w,
                h,
                format: VIRTUAL_PRESENT_FORMAT,
                translucent: false,
            },
        );
        win.last_present_work_ns =
            u64::try_from(work_started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        true
    }

    /// Build the persistent virtual present target (see [`VirtualTarget`]).
    fn make_virtual_target(&self, w: u32, h: u32) -> VirtualTarget {
        let tex = self.ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aterm-gpu virtual present target"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: VIRTUAL_PRESENT_FORMAT,
            // The usage a COPYABLE swapchain configures (`surface_usage` with
            // COPY_SRC offered): render attachment for the blit pass, copy
            // source for the tap.
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        VirtualTarget { tex, w, h }
    }

    /// Build + cache the EDR aurora pass resources (M3 phase B) if absent: a
    /// dedicated shader module (`HDR_GLOW_SHADER`), its uniform buffer + bind
    /// group, and a One/One additive pipeline over the `BgInstance` stream
    /// targeting `Rgba16Float` — the only swapchain format
    /// `format_plan::hdr_present_plan` enables the pass for. COLOR write-mask:
    /// the blit's alpha is never perturbed. Lazy so the default (hdr off) path
    /// allocates nothing.
    fn ensure_hdr_glow_pipeline(&mut self) {
        if self.hdr_glow_pipeline.is_some() {
            return;
        }
        self.ensure_glow_boost_shared();
        let pipeline = self.build_glow_boost_pipeline(
            "aterm-gpu hdr-glow pipeline",
            "fs_hdr_glow",
            wgpu::TextureFormat::Rgba16Float,
        );
        self.hdr_glow_pipeline = Some(pipeline);
    }

    /// Build + cache the SDR glow-boost pipeline for the ACTUAL (non-f16)
    /// swapchain `format` — the SDR twin of [`Self::ensure_hdr_glow_pipeline`],
    /// sharing its uniform buffer/bind group (the two passes are mutually
    /// exclusive per present — `format_plan::sdr_boost_pass`). Keyed by format:
    /// an HDR toggle rebuilds the surface with a different format, which
    /// rebuilds this pipeline on the next SDR present.
    fn ensure_sdr_glow_pipeline(&mut self, format: wgpu::TextureFormat) {
        if matches!(&self.sdr_glow_pipeline, Some((f, _)) if *f == format) {
            return;
        }
        self.ensure_glow_boost_shared();
        let pipeline =
            self.build_glow_boost_pipeline("aterm-gpu sdr-glow pipeline", "fs_sdr_glow", format);
        self.sdr_glow_pipeline = Some((format, pipeline));
    }

    /// The resources BOTH boost pipelines share: the bind-group layout (one
    /// object, so the bind group is compatible with either pipeline by
    /// identity), the `HdrGlowUniform` buffer, and its bind group. Lazy; built
    /// by whichever boost pass runs first.
    fn ensure_glow_boost_shared(&mut self) {
        if self.glow_boost_bgl.is_some() {
            return;
        }
        let device = &self.ctx.device;
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("aterm-gpu glow-boost bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aterm-gpu glow-boost uniform"),
            size: std::mem::size_of::<HdrGlowUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("aterm-gpu glow-boost bg"),
            layout: &bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buf.as_entire_binding(),
            }],
        });
        self.glow_boost_bgl = Some(bgl);
        self.hdr_glow_uniform_buf = Some(uniform_buf);
        self.hdr_glow_bg = Some(bg);
    }

    /// One glow-boost pipeline over the `BgInstance` stream: `vs_hdr_glow` +
    /// the given fragment entry, One/One additive, COLOR write-mask (the blit's
    /// alpha is never perturbed), targeting `format`. Caller picks the fragment
    /// (EDR headroom clamp vs SDR budget scale) and the target format.
    fn build_glow_boost_pipeline(
        &self,
        label: &str,
        fs_entry: &str,
        format: wgpu::TextureFormat,
    ) -> wgpu::RenderPipeline {
        let device = &self.ctx.device;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("aterm-gpu glow-boost shader"),
            source: wgpu::ShaderSource::Wgsl(HDR_GLOW_SHADER.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("aterm-gpu glow-boost layout"),
            bind_group_layouts: &[Some(
                self.glow_boost_bgl
                    .as_ref()
                    .expect("ensure_glow_boost_shared sets the bgl"),
            )],
            immediate_size: 0,
        });
        let add = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Add,
        };
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_hdr_glow"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<BgInstance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &BG_ATTRS,
                }],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some(fs_entry),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState {
                        color: add,
                        alpha: add,
                    }),
                    write_mask: wgpu::ColorWrites::COLOR,
                })],
            }),
            multiview_mask: None,
            cache: None,
        })
    }

    /// Compute the SCISSORED dirty-row scope for THIS presented frame against the
    /// previous one (the persistent offscreen still holds it), encode it, then
    /// record this frame as the new `present_prev`. Shared by `present_input` (the
    /// window blit path) and the readback test helper, so the scissor decision is
    /// computed in exactly one place.
    ///
    /// The cardinal rule: when in ANY doubt, FULL REPAINT. The scissor activates
    /// ONLY when a prior presented frame exists, the offscreen still holds it at
    /// the right dims, and `compute_dirty_rows` says the frame is reusable with a
    /// non-empty dirty set. Everything else (first frame, resize, scrollback /
    /// selection / double-height change) falls back to the full Clear+all-rows
    /// path — byte-identical to the original encode.
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "AsymmetricPadLayout",
            action = "PrimeLayoutCache",
            project = "aterm_gpu::GpuRenderer::project_asymmetric_pad_layout"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "AsymmetricPadLayout",
            action = "RenderWithLayoutCache",
            project = "aterm_gpu::GpuRenderer::project_asymmetric_pad_layout"
        )
    )]
    fn encode_present_frame(&mut self, win: &mut WindowGpu, input: &RenderInput) -> (u32, u32) {
        let cur_blink = self.cpu.cursor_blink_phase();
        let cur_override = self.cpu.cursor_style_override();
        let (cw, ch) = self.cpu.cell_size();
        // Match `encode_frame`'s PADDED offscreen dims (`cols*cw + 2*pad`): the
        // offscreen is created/stored at the inset size, so the gate below must
        // compare against the same padded extent. Reading the unpadded size here
        // made `offscreen_holds_prev` ALWAYS false whenever `pad > 0` (the windowed
        // GUI runs at `pad = pad_for_scale(..) > 0`), silently forcing every present
        // to a Full repaint and disabling the scissored dirty-row path entirely. At
        // `pad == 0` the `+ 2*pad` terms drop out, so this is unchanged there.
        let pad = self.cpu.pad();
        let head = self.cpu.head();
        // Clamp with the SAME helper `encode_frame` uses to size/store the offscreen.
        // Without this an oversized grid computes an UNCLAMPED (w, h) here, so
        // `offscreen_holds_prev` below (`o.w == w`) never matches the clamped stored
        // `Offscreen { w, h }` and EVERY present falls back to a Full repaint.
        let (w, h) = (
            self.clamp_fb_dim((input.cols * cw + 2 * pad) as u32),
            self.clamp_fb_dim((input.rows * ch + 2 * pad + head) as u32),
        );

        // The offscreen must already hold the previous frame at THESE dims for a
        // scissored Load to be safe. A dimension change recreates the texture
        // (its prior contents are gone), so any dims mismatch forces Full.
        let offscreen_holds_prev = matches!(&win.offscreen, Some(o) if o.w == w && o.h == h);

        // Take the persistent dirty scratch out of `self` (swapping in an empty Vec
        // — no allocation) so `compute_dirty_rows` can write into it while the
        // per-window `prev_input`/`present_prev` are read, and `encode_frame` can
        // borrow `win` mutably without aliasing it. The `Dirty` scope BORROWS this
        // local across the encode; it is restored to `self.dirty_scratch` after.
        // Take the per-window prior-frame state OUT of `win` too (swapping in
        // empties) so the scope can hold a `&` into them while `encode_frame`
        // mutably borrows `win`; both are written back below.
        let mut dirty = std::mem::take(&mut self.dirty_scratch);
        let prev_present = win.present_prev.take();
        let prev_input = std::mem::take(&mut win.prev_input);
        let scope = match (&prev_present, offscreen_holds_prev) {
            (Some(prev), true) if prev.grid_top == self.cpu.grid_top() => match compute_dirty_rows(
                // The prior presented frame's snapshot lives in this window's
                // resident `prev_input` buffer; `present_prev` being `Some` is its
                // validity.
                &prev_input,
                input,
                prev.blink_phase,
                prev.cursor_style_override,
                cur_blink,
                cur_override,
                ch,
                &mut dirty,
            ) {
                // Reusable: scissor the dirty band. The GPU comet BLOOM no longer
                // widens or band-fills the dirty set (change #2): the halo is a soft
                // additive layer composited at PRESENT time over a THROWAWAY copy of
                // this clean offscreen (`present_offscreen`), never into the scissor
                // base, so a Load-preserved row can never accumulate halo light and
                // the dirty set stays proportional to real content change. A moving
                // comet + one keystroke therefore scissors the few changed rows
                // instead of rebuilding the whole halo band every aurora tick. A live
                // radius change (`set_bloom_params`) needs no special arm either: the
                // present-time halo is recomputed fresh from the CURRENT radius each
                // frame over the clean base, so it can neither strand old halo light
                // nor accumulate. The zero-dirty-row case (a gate-class idle frame) is
                // handled correctly downstream — Load preserves the prior frame and
                // the empty dirty set draws nothing, the cheapest possible encode.
                DirtyDecision::Rows(_) => RepaintScope::Dirty(&dirty),
                // Not reusable (geometry / scrollback / selection / double-height):
                // the conservative full repaint.
                DirtyDecision::FullRepaint => RepaintScope::Full,
            },
            // No prior frame, or the offscreen no longer holds it: full repaint.
            _ => RepaintScope::Full,
        };

        match &scope {
            RepaintScope::Dirty(_) => self.scissor_taken += 1,
            RepaintScope::Full => self.full_repaints += 1,
        }
        let dims = self.encode_frame(win, input, &scope);
        // Done with the borrowed scope; restore the scratch (capacity retained).
        self.dirty_scratch = dirty;

        // This frame is now resident on THIS window's offscreen; remember it (+ the
        // state it was drawn with) ON THE WINDOW so the NEXT present of THIS window
        // can diff against it. The snapshot goes into the window's RESIDENT
        // `prev_input` buffer (taken out above) via `clone_from`, which reuses its
        // grid allocation when the dims are stable — a stable-dims changed frame
        // does ZERO grid allocation here, just a memcpy into the retained buffers.
        let mut prev_input = prev_input;
        prev_input.clone_from(input);
        win.prev_input = prev_input;
        win.present_prev = Some(PresentPrev {
            blink_phase: cur_blink,
            cursor_style_override: cur_override,
            grid_top: self.cpu.grid_top(),
        });
        dims
    }

    /// Build + upload the UNGATED aurora glow instances (every `cursor_glow_add`
    /// quad, in offscreen pixel space) into `vbufs.bloom_glow` — the bloom halo's
    /// extract source. Uses the SAME rect/colour transform as the aurora emission in
    /// `encode_frame`, but WITHOUT the scissor-band row gate, so the halo spreads
    /// from the WHOLE comet even on a scissored frame (whose `glow_add` carries only
    /// the dirty-row subset). Returns `(count, bbox, extract_first)`: `count == 0`
    /// ⇒ no glow, no upload, the halo is a no-op. On a FULL frame the exact set
    /// equals `inst.glow_add`, so the composited halo is byte-identical to the
    /// pre-change in-offscreen bloom.
    ///
    /// HALF-RES STABILITY (`extract_first`): the bloom extract rasterizes these
    /// quads into a half-res target, where a 1px quad (a beam's AA edge
    /// hairline) is only half a texel wide — it covers a texel CENTRE only at
    /// odd x/y, so as the beam moves the hairline's bloom energy blinks in and
    /// out (halo flicker on thin diagonals). When any quad is thinner than one
    /// half-res texel ([`BLOOM_DOWNSCALE`] px) on either axis, a SECOND copy of
    /// the whole set is appended with each thin axis widened to exactly one
    /// texel and the premultiplied colour scaled down by the same factor
    /// (energy-conserving under the gaussian): a `DOWNSCALE`-px quad covers
    /// exactly ONE texel centre at every alignment, so the halo is
    /// parity-stable. The EXTRACT draws instances
    /// `extract_first..extract_first + count`; the crown passes keep drawing
    /// the EXACT set `0..count` (full-res, no adjustment wanted). With no thin
    /// quad `extract_first == 0` and the buffer holds the single exact set —
    /// byte-identical to the pre-change build.
    fn build_bloom_glow(&mut self, input: &RenderInput) -> (u32, Option<[u32; 4]>, u32) {
        let rows = input.rows;
        let ds = BLOOM_DOWNSCALE as u16;
        // Reuse the resident scratch (mem::take + clear, restored below) instead of a
        // fresh per-frame allocation — the same pattern as the other instance streams.
        let mut v = std::mem::take(&mut self.bloom_glow_scratch);
        v.clear();
        // Accumulate the window-space bounding box of the glow so the composite
        // pass can scissor to it (the bloom is a small localized region; the rest
        // of the frame adds nothing). x1/y1 are exclusive. Thin axes accumulate
        // their WIDENED extract extent (over-covering the scissor is byte-safe:
        // beyond the blur's true reach the added light is exactly 0).
        let (mut bx0, mut by0, mut bx1, mut by1) = (u16::MAX, u16::MAX, 0u16, 0u16);
        for q in &input.cursor_glow_add {
            // Mirror the GPU aurora emission: skip quads past the grid's last row.
            if (q.row as usize) >= rows {
                continue;
            }
            // Effect-stream quads arrive WINDOW-ABSOLUTE from the producer — no
            // renderer-side pad offset (mirrors the aurora emission below).
            let rx = q.x;
            let ry = q.y;
            let ew = if q.w > 0 { q.w.max(ds) } else { 0 };
            let eh = if q.h > 0 { q.h.max(ds) } else { 0 };
            bx0 = bx0.min(rx);
            by0 = by0.min(ry);
            bx1 = bx1.max(rx.saturating_add(ew));
            by1 = by1.max(ry.saturating_add(eh));
            v.push(BgInstance {
                rect: [rx, ry, q.w, q.h],
                color: rgb4_u32(q.color),
            });
        }
        let n = v.len();
        let any_thin = v
            .iter()
            .any(|i| (i.rect[2] > 0 && i.rect[2] < ds) || (i.rect[3] > 0 && i.rect[3] < ds));
        let extract_first = if any_thin {
            for i in 0..n {
                let mut inst = v[i];
                let [x, y, w0, h0] = inst.rect;
                let (w1, num_w, den_w) = if w0 > 0 && w0 < ds {
                    (ds, u32::from(w0), u32::from(ds))
                } else {
                    (w0, 1, 1)
                };
                let (h1, num_h, den_h) = if h0 > 0 && h0 < ds {
                    (ds, u32::from(h0), u32::from(ds))
                } else {
                    (h0, 1, 1)
                };
                let (num, den) = (num_w * num_h, den_w * den_h);
                if den > 1 {
                    for c in &mut inst.color[..3] {
                        *c = ((u32::from(*c) * num + den / 2) / den) as u8;
                    }
                }
                inst.rect = [x, y, w1, h1];
                v.push(inst);
            }
            n as u32
        } else {
            0
        };
        let n = n as u32;
        if n > 0 {
            let device = &self.ctx.device;
            let queue = &self.ctx.queue;
            self.vbufs
                .bloom_glow
                .upload(device, queue, bytemuck::cast_slice(&v));
        }
        // Hand the (grown) allocation back for the next frame to reuse.
        self.bloom_glow_scratch = v;
        let bbox = (n > 0).then_some([bx0 as u32, by0 as u32, bx1 as u32, by1 as u32]);
        (n, bbox, extract_first)
    }

    /// (Re)create this window's [`PresentOffscreen`] when absent or resized, at the
    /// same `(w, h)` as the offscreen. Its `blit_bind` is a drop-in for the
    /// offscreen's (same layout / sampler / shared blit uniform), so the blit
    /// pipeline + uniform stay byte-identical — only the sampled texture differs.
    fn ensure_present_offscreen(&mut self, win: &mut WindowGpu, w: u32, h: u32) {
        if matches!(&win.present_offscreen, Some(p) if p.w == w && p.h == h) {
            return;
        }
        let tex = self.ctx.offscreen_texture(w, h);
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        let src_view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        let blit_bind = self
            .ctx
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("aterm-gpu present-offscreen blit bg"),
                layout: &self.blit_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&src_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.blit_sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: self.blit_uniform_buf.as_entire_binding(),
                    },
                ],
            });
        win.present_offscreen = Some(PresentOffscreen {
            tex,
            view,
            blit_bind,
            w,
            h,
        });
    }

    /// Encode the GPU comet BLOOM halo: re-render the ungated glow
    /// (`vbufs.bloom_glow`, `glow_count` instances from [`Self::build_bloom_glow`])
    /// into the half-res `bt` target, gaussian-blur it, and additively composite the
    /// radiant halo over `target_view` (One/One, RGB-only — the blit alpha is never
    /// perturbed). This is the SAME extract → blur → composite the bloom used to run
    /// INSIDE the offscreen encode, now hoisted to PRESENT time over a throwaway
    /// target so the scissor base stays halo-free. Composited over the WHOLE frame
    /// (no scissor): `target_view` is either the offscreen itself on a readback /
    /// tray / scroll frame (`present_prev` already invalidated) or a fresh copy of it
    /// (`present_offscreen`), never a Load-preserved scissor band, so there is
    /// nothing to clip and the light cannot accumulate. `glow_count > 0` is the
    /// caller's precondition.
    #[allow(
        clippy::too_many_arguments,
        reason = "one GPU pass encoder threading the encoder, target, uniforms and glyph buffers"
    )]
    fn encode_bloom_halo(
        &self,
        enc: &mut wgpu::CommandEncoder,
        target_view: &wgpu::TextureView,
        bt: &BloomTarget,
        glow_count: u32,
        bbox: Option<[u32; 4]>,
        dims: (u32, u32),
        extract_first: u32,
    ) {
        // Refresh the tunables for this target's half-res texel size.
        let bu = BloomUniform {
            texel: [1.0 / bt.bw as f32, 1.0 / bt.bh as f32],
            strength: self.bloom_strength,
            radius: self.bloom_radius,
        };
        self.ctx
            .queue
            .write_buffer(&self.bloom_uniform_buf, 0, bytemuck::bytes_of(&bu));
        // 1) Extract: draw the glow (additive) into the cleared half-res tex.
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("aterm-gpu bloom extract pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &bt.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.glow_add_pipeline);
            pass.set_bind_group(0, &self.uniform_bg, &[]);
            pass.set_vertex_buffer(0, self.vbufs.bloom_glow.buf.slice(..));
            // The EXTRACT set: the half-res-stable copy when thin quads forced
            // one (`extract_first > 0`), else the exact set (see
            // `build_bloom_glow`). The crown passes keep drawing `0..count`.
            pass.draw(0..6, extract_first..extract_first + glow_count);
        }
        // 2) Blur + composite: additively add the blurred glow over the frame.
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("aterm-gpu bloom composite pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.bloom_pipeline);
            pass.set_bind_group(0, &bt.bind, &[]);
            // SCISSOR the composite to the glow's bounding box dilated by the
            // blur's full reach: the half-res source is cleared to 0 everywhere
            // outside the glow. The gaussian taps reach 2·radius half-res
            // texels, and the LINEAR filter at each tap interpolates one texel
            // further — so the true reach is (2·radius + 1) texels ⇒
            // (2·radius + 1)·DOWNSCALE full-res px (the old `2·radius·DOWNSCALE`
            // under-dilated by a texel and clipped the halo's outer fringe to a
            // straight edge on a long jump). Outside that the added light is
            // exactly 0 (One/One additive of 0 is a no-op), so this is
            // BYTE-EXACT — it just skips shading the dead majority of the
            // frame. A tiny/edge bbox that collapses falls back to no scissor.
            if let Some([x0, y0, x1, y1]) = bbox {
                let d =
                    ((2.0 * self.bloom_radius + 1.0) * BLOOM_DOWNSCALE as f32).ceil() as u32 + 1;
                let (w, h) = dims;
                let sx0 = x0.saturating_sub(d);
                let sy0 = y0.saturating_sub(d);
                let sx1 = (x1 + d).min(w);
                let sy1 = (y1 + d).min(h);
                if sx1 > sx0 && sy1 > sy0 {
                    pass.set_scissor_rect(sx0, sy0, sx1 - sx0, sy1 - sy0);
                }
            }
            pass.draw(0..3, 0..1);
        }
    }

    /// Produce this window's [`PresentOffscreen`] for a bloom/shimmer present:
    /// COPY the clean `offscreen` (base + aurora, never the halo) into it,
    /// composite the comet halo over the copy (when bloom is on), then refract
    /// the air above the hot region (when the shimmer is on and derives one).
    /// The blit (and the readback helpers) sample this instead of the
    /// offscreen, so the presented frame is base+aurora+halo+haze —
    /// byte-identical to the pre-change in-offscreen bloom when the shimmer is
    /// off — while the offscreen stays a clean, effect-free scissor base.
    /// Precondition: (`enable_bloom` or `enable_shimmer`) + live glow +
    /// offscreen resident. Returns the UNGATED glow instance count when the
    /// bloom built + uploaded `vbufs.bloom_glow` (the crown passes reuse the
    /// buffer), `None` when nothing was built (shimmer-only, or no offscreen).
    fn compose_present_offscreen(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
    ) -> Option<u32> {
        let (w, h) = win.offscreen.as_ref().map(|o| (o.w, o.h))?;
        self.ensure_present_offscreen(win, w, h);
        // Only the bloom consumes the ungated glow upload; a shimmer-only
        // present (bloom off) skips the build entirely.
        let (glow_count, glow_bbox, extract_first) = if self.enable_bloom {
            self.build_bloom_glow(input)
        } else {
            (0, None, 0)
        };
        let shimmer_region = if self.shimmer_live(input) {
            self.shimmer_region(input, w, h)
        } else {
            None
        };
        if shimmer_region.is_some() {
            self.ensure_shimmer_scratch(win, w, h);
        }
        let mut enc = self
            .ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aterm-gpu present-offscreen bloom composite"),
            });
        let off = win.offscreen.as_ref().expect("offscreen resident");
        let po = win
            .present_offscreen
            .as_ref()
            .expect("ensure_present_offscreen set it");
        // The clean base+aurora → present_offscreen (byte-exact same-format copy).
        enc.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &off.tex,
                mip_level: 0,
                origin: wgpu::Origin3d { x: 0, y: 0, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &po.tex,
                mip_level: 0,
                origin: wgpu::Origin3d { x: 0, y: 0, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        // Composite the halo over the copy (whole frame — a throwaway target, so no
        // scissor and no accumulation). An empty glow leaves the copy as the clean
        // frame, exactly what a no-halo present blits.
        if glow_count > 0
            && let Some(bt) = off.bloom.as_ref()
        {
            self.encode_bloom_halo(
                &mut enc,
                &po.view,
                bt,
                glow_count,
                glow_bbox,
                (w, h),
                extract_first,
            );
        }
        // HEAT SHIMMER, after the halo: the haze refracts the FINISHED frame.
        // Same throwaway-target argument as the halo; a `None` region encodes
        // nothing, leaving the copy byte-identical to a shimmer-off present.
        if let Some(region) = &shimmer_region {
            let scratch = win.shimmer_scratch.as_ref().expect("ensured above");
            self.encode_shimmer(&mut enc, scratch, &po.tex, &po.view, region);
        }
        self.ctx.queue.submit([enc.finish()]);
        self.enable_bloom.then_some(glow_count)
    }

    /// Composite the comet halo directly INTO this window's `offscreen` — the
    /// readback / tray / sub-row-scroll frames, where `present_prev` is invalidated
    /// so the haloed offscreen is never reused as a scissor base. Byte-identical to
    /// the pre-change in-`encode_frame` bloom: the SAME extract → blur → composite
    /// over the SAME offscreen view, whole-frame. A no-op when there is no glow or no
    /// bloom target. Returns the UNGATED glow instance count it built + uploaded
    /// into `vbufs.bloom_glow` (the crown passes reuse the buffer).
    fn composite_bloom_in_place(&mut self, win: &mut WindowGpu, input: &RenderInput) -> u32 {
        let (glow_count, glow_bbox, extract_first) = self.build_bloom_glow(input);
        if glow_count == 0 {
            return glow_count;
        }
        let mut enc = self
            .ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aterm-gpu in-place bloom composite"),
            });
        let Some(off) = win.offscreen.as_ref() else {
            return glow_count;
        };
        let dims = (off.w, off.h);
        let Some(bt) = off.bloom.as_ref() else {
            return glow_count;
        };
        self.encode_bloom_halo(
            &mut enc,
            &off.view,
            bt,
            glow_count,
            glow_bbox,
            dims,
            extract_first,
        );
        self.ctx.queue.submit([enc.finish()]);
        glow_count
    }

    /// The shimmer's present-time phase, seconds: the test pin when set, else
    /// wall clock wrapped at [`SHIMMER_PHASE_WRAP_S`] (seam-free — see the
    /// [`SHIMMER_SHADER`] rate note). The wall clock is this pass's ONE
    /// deliberate nondeterminism, the documented bloom-class exception (the
    /// SDR crown envelope precedent).
    fn shimmer_phase(&self) -> f32 {
        self.shimmer_phase_pin
            .unwrap_or_else(|| self.shimmer_epoch.elapsed().as_secs_f32() % SHIMMER_PHASE_WRAP_S)
    }

    /// Derive the shimmer's hot region for this frame from the SAME
    /// `cursor_glow_add` stream the bloom feeds on (the [`Self::build_bloom_glow`]
    /// convention: window-absolute quads, rows past the grid skipped — zero new
    /// host plumbing). The pass rect spans the quads' x-range widened one cell
    /// each side (the shader's rolloff runs INSIDE it) and rises
    /// [`SHIMMER_RISE_CELLS`] above the hottest top edge — strictly ABOVE the
    /// burning cells, clamped to the `w`x`h` frame (the `y0` clamp bottoms out
    /// at the window's top edge, so the haze may rise into the head band —
    /// intended). Per-column heat = the max premultiplied glow brightness over
    /// each of the [`SHIMMER_BANDS`] bands, gain-lifted and 1-2-1 smoothed.
    /// `None` ⇒ no pass runs and the present is byte-identical (empty/dark
    /// stream, or no air above the fire).
    fn shimmer_region(&self, input: &RenderInput, w: u32, h: u32) -> Option<ShimmerRegion> {
        let (cw, ch) = self.cpu.cell_size();
        let (cw, ch) = (cw as u32, ch as u32);
        let rows = input.rows;
        let quad_brightness = |q: &aterm_render::GlowQuad| -> u32 {
            if (q.row as usize) >= rows {
                return 0;
            }
            ((q.color >> 16) & 0xff)
                .max((q.color >> 8) & 0xff)
                .max(q.color & 0xff)
        };
        let (mut hx0, mut hx1, mut hot_top) = (u32::MAX, 0u32, u32::MAX);
        for q in &input.cursor_glow_add {
            if quad_brightness(q) == 0 {
                continue;
            }
            let qx0 = u32::from(q.x);
            hx0 = hx0.min(qx0);
            hx1 = hx1.max(qx0 + u32::from(q.w));
            hot_top = hot_top.min(u32::from(q.y));
        }
        if hot_top == u32::MAX || hx1 <= hx0 {
            return None;
        }
        let rise = SHIMMER_RISE_CELLS * ch as f32;
        let x0 = hx0.saturating_sub(cw).min(w);
        let x1 = (hx1.saturating_add(cw)).min(w);
        let y1 = hot_top.min(h);
        let y0 = hot_top.saturating_sub(rise.ceil() as u32).min(y1);
        if x0 >= x1 || y0 >= y1 {
            return None;
        }
        // Per-column heat over the pass rect (the widened margins naturally
        // carry zero heat, so the haze dies out before the rolloff even acts).
        let band_w = (x1 - x0) as f32 / SHIMMER_BANDS as f32;
        let mut heat = [0f32; SHIMMER_BANDS];
        for q in &input.cursor_glow_add {
            let b = quad_brightness(q);
            if b == 0 {
                continue;
            }
            let strength = (b as f32 / 255.0 * SHIMMER_HEAT_GAIN).min(1.0);
            let qx0 = u32::from(q.x) as f32 - x0 as f32;
            let qx1 = qx0 + f32::from(q.w);
            let lo = ((qx0 / band_w).floor().max(0.0) as usize).min(SHIMMER_BANDS);
            let hi = ((qx1 / band_w).ceil().max(0.0) as usize).min(SHIMMER_BANDS);
            for slot in &mut heat[lo..hi] {
                *slot = slot.max(strength);
            }
        }
        // Two 1-2-1 smoothing passes: the haze eases past the flame edges and
        // never steps band-to-band.
        for _ in 0..2 {
            let prev = heat;
            for (i, slot) in heat.iter_mut().enumerate() {
                let l = prev[i.saturating_sub(1)];
                let r = prev[(i + 1).min(SHIMMER_BANDS - 1)];
                *slot = 0.25 * l + 0.5 * prev[i] + 0.25 * r;
            }
        }
        Some(ShimmerRegion {
            x0,
            y0,
            x1,
            y1,
            fw: w,
            fh: h,
            hot_top: hot_top as f32,
            rise,
            band_x0: x0 as f32,
            band_w,
            heat,
        })
    }

    /// Build the lazy [`ShimmerResources`] if absent (first shimmer frame).
    fn ensure_shimmer_resources(&mut self) {
        if self.shimmer.is_none() {
            self.shimmer = Some(build_shimmer_resources(
                &self.ctx.device,
                self.ctx.offscreen_format(),
            ));
        }
    }

    /// (Re)create this window's [`ShimmerScratch`] when absent or resized, at
    /// the target's `(w, h)`. Ensures the shared resources first (the bind
    /// group references their layout + uniform buffer).
    fn ensure_shimmer_scratch(&mut self, win: &mut WindowGpu, w: u32, h: u32) {
        self.ensure_shimmer_resources();
        if matches!(&win.shimmer_scratch, Some(s) if s.w == w && s.h == h) {
            return;
        }
        let sr = self.shimmer.as_ref().expect("ensured above");
        let tex = self.ctx.offscreen_texture(w, h);
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        let bind = self
            .ctx
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("aterm-gpu shimmer scratch bg"),
                layout: &sr.bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        // The linear ClampToEdge sampler the bloom upsample uses —
                        // exactly the filtering the displaced resample wants.
                        resource: wgpu::BindingResource::Sampler(&self.bloom_sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: sr.uniform_buf.as_entire_binding(),
                    },
                ],
            });
        win.shimmer_scratch = Some(ShimmerScratch { tex, bind, w, h });
    }

    /// Encode the HEAT-SHIMMER refraction over `target_view`: stage the pass
    /// rect (+ [`SHIMMER_COPY_MARGIN`]) of `target_tex` into the scratch at the
    /// SAME origin, refresh the uniform (region, heat, amplitude, wall-clock /
    /// pinned phase), then run the scissored resample pass. Outside the
    /// scissor rect the target is untouched — byte-identical by construction;
    /// inside, the displaced sample never exceeds `amp` px nor leaves the
    /// frame (shader clamps). Preconditions: resources + scratch ensured,
    /// `region` derived for the target's exact dims (`region.fw`/`region.fh`).
    fn encode_shimmer(
        &self,
        enc: &mut wgpu::CommandEncoder,
        scratch: &ShimmerScratch,
        target_tex: &wgpu::Texture,
        target_view: &wgpu::TextureView,
        region: &ShimmerRegion,
    ) {
        let (w, h) = (region.fw, region.fh);
        let sr = self
            .shimmer
            .as_ref()
            .expect("ensure_shimmer_resources set it");
        // 1) Stage: copy the rect the pass may SAMPLE (region + displacement +
        //    bilinear footprint) — a fresh scratch's uncovered texels are never
        //    read because the shader's sample clamp stays within this margin.
        let m = SHIMMER_COPY_MARGIN;
        let cx0 = region.x0.saturating_sub(m);
        let cy0 = region.y0.saturating_sub(m);
        let cx1 = region.x1.saturating_add(m).min(w);
        let cy1 = region.y1.saturating_add(m).min(h);
        enc.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: target_tex,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: cx0,
                    y: cy0,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &scratch.tex,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: cx0,
                    y: cy0,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: cx1 - cx0,
                height: cy1 - cy0,
                depth_or_array_layers: 1,
            },
        );
        // 2) This frame's uniform. Amplitude scales with the cell height and is
        //    hard-capped at SHIMMER_AMP_PX; period is the cell height (the
        //    "cell-height scale" ripple), rolloff one cell width.
        let (cw, ch) = self.cpu.cell_size();
        let mut heat = [[0f32; 4]; SHIMMER_BANDS / 4];
        for (i, v) in region.heat.iter().enumerate() {
            heat[i / 4][i % 4] = *v;
        }
        let su = ShimmerUniform {
            frame: [w as f32, h as f32],
            region_min: [region.x0 as f32, region.y0 as f32],
            region_max: [region.x1 as f32, region.y1 as f32],
            hot_top: region.hot_top,
            rise: region.rise,
            amp: (ch as f32 / 18.0).clamp(0.75, SHIMMER_AMP_PX),
            period: (ch as f32).max(4.0),
            phase: self.shimmer_phase(),
            band_x0: region.band_x0,
            band_w: region.band_w,
            rolloff: (cw as f32).max(1.0),
            _pad: [0.0; 2],
            heat,
        };
        self.ctx
            .queue
            .write_buffer(&sr.uniform_buf, 0, bytemuck::bytes_of(&su));
        // 3) The refraction pass, scissored to the region rect: every pixel
        //    outside it is untouched (the no-op / outside-the-bound law).
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("aterm-gpu shimmer pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&sr.pipeline);
            pass.set_bind_group(0, &scratch.bind, &[]);
            pass.set_scissor_rect(
                region.x0,
                region.y0,
                region.x1 - region.x0,
                region.y1 - region.y0,
            );
            pass.draw(0..3, 0..1);
        }
    }

    /// Refract the air above the hot region directly INTO this window's
    /// `offscreen` — the readback / tray / sub-row-scroll frames, where
    /// `present_prev` is invalidated so the shimmered offscreen is never
    /// reused as a scissor base (the [`Self::composite_bloom_in_place`] safety
    /// argument, verbatim). Runs AFTER the bloom composite so the haze
    /// refracts the FINISHED frame, halo included. A no-op (no encoder, no
    /// submit) when the stream yields no region.
    fn shimmer_offscreen_in_place(&mut self, win: &mut WindowGpu, input: &RenderInput) {
        let (w, h) = match win.offscreen.as_ref() {
            Some(o) => (o.w, o.h),
            None => return,
        };
        let Some(region) = self.shimmer_region(input, w, h) else {
            return;
        };
        self.ensure_shimmer_scratch(win, w, h);
        let mut enc = self
            .ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aterm-gpu in-place shimmer"),
            });
        let off = win.offscreen.as_ref().expect("checked above");
        let scratch = win.shimmer_scratch.as_ref().expect("ensured above");
        self.encode_shimmer(&mut enc, scratch, &off.tex, &off.view, &region);
        self.ctx.queue.submit([enc.finish()]);
    }

    /// TEST HELPER (byte-identity gate): run the SCISSORED present-path encode for
    /// `input` exactly as [`present_input`](Self::present_input) does — same
    /// `compute_dirty_rows` decision, same `present_prev` tracking, same persistent
    /// offscreen — then read the PRESENTED pixels back into a [`Frame`]. This is the
    /// path the `scissor_repaint` test asserts is byte-identical to a fresh full
    /// render. With the GPU comet bloom live, the halo is composited over a throwaway
    /// copy of the clean offscreen (`present_offscreen`, exactly as the on-glass
    /// present blits), so read THAT back on a bloom frame and the plain offscreen
    /// otherwise — the offscreen itself stays a halo-free scissor base.
    #[doc(hidden)]
    pub fn present_input_readback(&mut self, win: &mut WindowGpu, input: &RenderInput) -> Frame {
        let (w, h) = self.encode_present_frame(win, input);
        // Faithful to `present_input`: the halo composites on EVERY bloom frame —
        // including `input_hot` keystroke echoes (the TYPING-2 deferral was removed:
        // with bloom default-on it blinked the halo off per keystroke) — so this
        // reads back exactly what the on-glass blit samples. The heat shimmer
        // shares the routing (the `fx_present` gate in `present_to_view`).
        let tex = if (self.enable_bloom && !input.cursor_glow_add.is_empty())
            || self.shimmer_live(input)
        {
            self.compose_present_offscreen(win, input);
            win.present_offscreen
                .as_ref()
                .expect("compose_present_offscreen set it")
                .tex
                .clone()
        } else {
            win.offscreen
                .as_ref()
                .expect("encode_frame sets offscreen")
                .tex
                .clone()
        };
        self.ctx.read_back(&tex, w, h)
    }

    /// TEST/BENCH HELPER: run the SCISSORED present-path encode for `input` and
    /// BLOCK until the GPU finishes, but do NOT read the pixels back. Isolates the
    /// changed-frame ENCODE + instance-build + GPU fill cost (the readback, which
    /// is identical for any scope, would otherwise swamp the scissor's saving).
    #[doc(hidden)]
    pub fn present_encode_poll(&mut self, win: &mut WindowGpu, input: &RenderInput) {
        let _ = self.encode_present_frame(win, input);
        self.ctx
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("GPU poll failed");
    }

    /// TEST HELPER (the present-real theorem's SWAPCHAIN arm): run the REAL
    /// swapchain present — the SAME [`Self::present_to_view`] compose-and-blit
    /// seam `present_input` runs after its acquire — against a fresh stand-in
    /// texture carrying exactly a copyable swapchain's configuration (the
    /// `surface_usage` `RENDER_ATTACHMENT | COPY_SRC`, the `pick_surface_format`
    /// SDR default), sized `(w, h)` like the true window client area. The
    /// window's VIDEO tap (arm it with [`Self::video_begin_standin_for_test`])
    /// copies the presented bytes in the same encoder, exactly as on glass; only
    /// `present()` — pure WSI, no pixel effect — is absent, since a headless
    /// test has no compositor to hand the frame to.
    #[doc(hidden)]
    pub fn present_swapchain_standin_for_test(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
        invert: bool,
        overlay: Option<DropOverlay>,
        tray: Option<TrayQuad<'_>>,
        (w, h): (u32, u32),
    ) {
        let tex = self.ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aterm-gpu test present dst (copyable swapchain stand-in)"),
            size: wgpu::Extent3d {
                width: w.max(1),
                height: h.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: VIRTUAL_PRESENT_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        self.present_to_view(
            win,
            input,
            invert,
            overlay,
            tray,
            None,
            PresentDest {
                view: &view,
                tex: &tex,
                w: w.max(1),
                h: h.max(1),
                format: VIRTUAL_PRESENT_FORMAT,
                translucent: false,
            },
        );
    }

    /// TEST HELPER: arm the window's VIDEO tap for the stand-in swapchain above
    /// (same geometry/format), without needing a real `GpuSurface` — what
    /// `video_begin` does after its `copyable` gate.
    #[doc(hidden)]
    pub fn video_begin_standin_for_test(
        &self,
        win: &mut WindowGpu,
        w: u32,
        h: u32,
        opts: crate::video_tap::CaptureOpts,
    ) -> Result<(), String> {
        let tap =
            crate::video_tap::VideoTap::new(&self.ctx.device, w, h, VIRTUAL_PRESENT_FORMAT, opts)?;
        win.video = Some(tap);
        Ok(())
    }

    /// TEST HELPER: reset the SDR glow-boost ATTACK-ENVELOPE ease (renderer-
    /// global, wall-clock driven) so two presents compared by a differential
    /// test start from the identical eased budget — the one intentionally
    /// time-dependent term in the present pass.
    #[doc(hidden)]
    pub fn reset_glow_ease_for_test(&mut self) {
        self.sdr_glow_level = 0.0;
        self.sdr_glow_level_at = None;
    }

    /// TEST HELPER (on-glass blit coverage): run the REAL on-glass blit — the
    /// SAME `vs_blit`/`fs_blit` pipeline, the SAME `blit_sampler` (NEAREST), and
    /// the SAME `BlitUniform` that [`present_input`](Self::present_input) uses —
    /// against a fresh READABLE `Rgba8Unorm` target (the window swapchain isn't
    /// readable headless) and read the result back into a [`Frame`].
    ///
    /// The blit SOURCE is the renderer's CURRENT resident offscreen (whatever the
    /// last `render_input`/`present_input` drew), so the caller renders a frame,
    /// captures the offscreen pixels with the existing readback, then calls this
    /// to obtain the blitted pixels and compare. `invert == false` is the
    /// straight-through (byte-exact) present; `invert == true` is the visual-bell
    /// `1.0 - rgb` flash.
    ///
    /// Production-inert: only test/bench code reaches it, it builds its own target
    /// + bind group, and it leaves `win.offscreen` / `present_prev` untouched
    /// (it records what it wrote in `blit_uniform_written`, so the next
    /// `present_input` still skips the uniform write iff the buffer already holds
    /// its value). The `Rgba8Unorm` target format matches one
    /// of the two formats `pick_surface_format` chooses for a real swapchain, so
    /// the blit pipeline it exercises is exactly a real present pipeline.
    #[doc(hidden)]
    pub fn blit_to_offscreen_for_test(&mut self, win: &mut WindowGpu, invert: bool) -> Frame {
        let (w, h) = {
            let src = win
                .offscreen
                .as_ref()
                .expect("render a frame before blitting");
            (src.w, src.h)
        };
        self.blit_to_sized_for_test(win, invert, w, h)
    }

    /// TEST HELPER (W1 band placement): like
    /// [`blit_to_offscreen_for_test`](Self::blit_to_offscreen_for_test) but with
    /// an EXPLICIT destination size — the RAW-window-sized swapchain stand-in.
    /// Runs the REAL band-aware present blit: the frame lands at the centred
    /// [`aterm_render::band_offset`] and the remainder bands are painted the
    /// renderer's theme background. Production `present_input` supplies the
    /// live terminal background to this same shader instead.
    /// `dst == src` dims is the exact-fit present (byte-identical passthrough).
    #[doc(hidden)]
    pub fn blit_to_sized_for_test(
        &mut self,
        win: &mut WindowGpu,
        invert: bool,
        dst_w: u32,
        dst_h: u32,
    ) -> Frame {
        self.blit_to_sized_with_options_for_test(win, invert, None, None, dst_w, dst_h)
    }

    /// TEST HELPER (frontend crop/effect coverage): the production
    /// [`present_blit_uniform`] and real `fs_blit` path with an explicit visible
    /// source interval and optional drop overlay, rendered into a readable
    /// destination.  This is the cropped sibling of
    /// [`Self::blit_to_sized_for_test`].
    #[doc(hidden)]
    pub fn blit_to_sized_cropped_for_test(
        &mut self,
        win: &mut WindowGpu,
        invert: bool,
        overlay: Option<DropOverlay>,
        crop: PresentCrop,
        dst_w: u32,
        dst_h: u32,
    ) -> Frame {
        self.blit_to_sized_with_options_for_test(win, invert, overlay, Some(crop), dst_w, dst_h)
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the test seam mirrors the production blit's independent effect and geometry inputs"
    )]
    fn blit_to_sized_with_options_for_test(
        &mut self,
        win: &mut WindowGpu,
        invert: bool,
        overlay: Option<DropOverlay>,
        source_crop: Option<PresentCrop>,
        dst_w: u32,
        dst_h: u32,
    ) -> Frame {
        let src = win
            .offscreen
            .as_ref()
            .expect("render a frame before blitting");
        let (fw, fh) = (src.w, src.h);
        assert!(
            source_crop.is_none_or(|crop| valid_present_crop(crop, fh)),
            "test crop must be a non-empty interval inside the resident source"
        );
        let (w, h) = (dst_w.max(1), dst_h.max(1));
        let src_view = src.tex.create_view(&wgpu::TextureViewDescriptor::default());

        // Fresh readable plain-Unorm target — a faithful stand-in for the NON-sRGB
        // swapchain the real present blits into, on BOTH backends. It must NOT route
        // through `offscreen_texture()` (which is sRGB-typed on downlevel): the blit
        // pipeline targets `Rgba8Unorm`, so an sRGB dst view would mismatch it and
        // wgpu would reject the pass (the C1/C2 format-drift class) — see
        // crate::format_plan. A dedicated Unorm texture keeps pipeline == attachment.
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let dst = self.ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aterm-gpu test-blit dst (swapchain stand-in)"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let dst_view = dst.create_view(&wgpu::TextureViewDescriptor::default());

        // Write the production crop/invert/overlay/band uniform through the REAL
        // shared buffer and keep its memo coherent for the next present.
        let mut want =
            present_blit_uniform(invert, overlay, source_crop, fw, fh, w, h, self.theme.bg);
        // Downlevel (sRGB-typed offscreen): re-encode linear→sRGB in the blit (see above).
        want.encode_srgb = if self.ctx.srgb_offscreen { 0.0 } else { 1.0 };
        self.ctx
            .queue
            .write_buffer(&self.blit_uniform_buf, 0, bytemuck::bytes_of(&want));
        self.blit_uniform_written = Some(want);

        // The REAL blit bind group: source view + the REAL `blit_sampler` (NEAREST)
        // + the REAL `blit_uniform_buf`, under the REAL `blit_bgl`.
        let bind = self
            .ctx
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("aterm-gpu test blit bg"),
                layout: &self.blit_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&src_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.blit_sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: self.blit_uniform_buf.as_entire_binding(),
                    },
                ],
            });

        // The REAL blit pipeline (vs_blit + fs_blit) for this format.
        self.ensure_blit_pipeline(format);
        let pipeline = &self.blit_pipelines[&format];

        let mut enc = self
            .ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aterm-gpu test blit"),
            });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("aterm-gpu test blit pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &dst_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..3, 0..1);
        }
        self.ctx.queue.submit([enc.finish()]);
        self.ctx.read_back(&dst, w, h)
    }

    /// TEST HELPER (M3 EDR clamp laws): run the REAL present-path encode for
    /// `input`, then the REAL HDR present — the same `fs_blit` `hdr` arm and
    /// (when `boost_pass`) the same EDR aurora pass, gated/parameterised by the
    /// SAME `format_plan::hdr_present_plan` + `aterm_render::hdr` chain the
    /// on-glass present uses with this window's `edr_max` — against a readable
    /// `Rgba16Float` target (the real swapchain isn't readable headless), and
    /// decode the raw half-float channels to f32. Returns `(rgba_linear, w, h)`
    /// with `rgba_linear.len() == (w*h*4) as usize`, channel order RGBA.
    ///
    /// Production-inert like `blit_to_sized_for_test`: builds its own target +
    /// bind group and only updates the same uniform bookkeeping the real
    /// present would. `self.hdr_glow` must be set by the caller — the plan
    /// (`hdr_glow`, f16 target, glow presence) is honoured, so a caller with
    /// `hdr_glow == false` gets NO aurora pass regardless of `boost_pass` (the
    /// SdrInvariance arm, testable directly).
    #[doc(hidden)]
    pub fn present_hdr_for_test(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
        boost_pass: bool,
    ) -> (Vec<f32>, u32, u32) {
        let (w, h) = self.encode_present_frame(win, input);
        let format = wgpu::TextureFormat::Rgba16Float;

        // The REAL present gate, with the REAL swapchain format this helper
        // stands in for. `boost_pass = false` models a glow-free present.
        // Like the real present, the crown reads the UNGATED stream (built by
        // `build_bloom_glow` when the pass fires), not the row-gated one.
        let glow_present = input
            .cursor_glow_add
            .iter()
            .any(|q| (q.row as usize) < input.rows);
        let plan = crate::format_plan::hdr_present_plan(
            self.hdr_glow,
            true, // the stand-in target IS Rgba16Float
            glow_present,
        );
        let headroom = aterm_render::hdr::additive_headroom(win.edr_max);
        let run_hdr_glow = boost_pass && plan.glow_boost_pass && headroom > 0.0;
        let glow_count = if run_hdr_glow {
            self.build_bloom_glow(input).0
        } else {
            0
        };
        if run_hdr_glow {
            self.ensure_hdr_glow_pipeline();
        }
        self.ensure_blit_pipeline(format);

        let src_view = win
            .offscreen
            .as_ref()
            .expect("encode_present_frame sets offscreen")
            .tex
            .create_view(&wgpu::TextureViewDescriptor::default());
        let dst = self.ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aterm-gpu test-hdr dst (EDR swapchain stand-in)"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let dst_view = dst.create_view(&wgpu::TextureViewDescriptor::default());

        // The REAL blit uniform, exact-fit, with the REAL plan-driven hdr flag.
        let mut want = BlitUniform::bell(false).with_bands(w, h, w, h, self.theme.bg);
        want.encode_srgb = if self.ctx.srgb_offscreen { 0.0 } else { 1.0 };
        want.hdr = if plan.blit_linear_encode { 1.0 } else { 0.0 };
        want.sdr_white_scale = if plan.blit_linear_encode {
            win.sdr_white_scale.max(1.0)
        } else {
            1.0
        };
        self.ctx
            .queue
            .write_buffer(&self.blit_uniform_buf, 0, bytemuck::bytes_of(&want));
        // Renderer-level (buffer-keyed) memo — the shared buffer now holds `want`.
        self.blit_uniform_written = Some(want);
        if run_hdr_glow {
            let hu = HdrGlowUniform {
                screen: [w as f32, h as f32],
                content_off: want.content_off,
                boost: aterm_render::hdr::HDR_GLOW_BOOST * win.sdr_white_scale.max(1.0),
                headroom: headroom * win.sdr_white_scale.max(1.0),
                _pad: [0.0, 0.0],
            };
            self.ctx.queue.write_buffer(
                self.hdr_glow_uniform_buf
                    .as_ref()
                    .expect("ensure_hdr_glow_pipeline sets the uniform buf"),
                0,
                bytemuck::bytes_of(&hu),
            );
        }

        let bind = self
            .ctx
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("aterm-gpu test hdr bg"),
                layout: &self.blit_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&src_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.blit_sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: self.blit_uniform_buf.as_entire_binding(),
                    },
                ],
            });
        let pipeline = &self.blit_pipelines[&format];

        let mut enc = self
            .ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aterm-gpu test hdr present"),
            });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("aterm-gpu test hdr pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &dst_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..3, 0..1);
            if run_hdr_glow {
                pass.set_pipeline(
                    self.hdr_glow_pipeline
                        .as_ref()
                        .expect("ensure_hdr_glow_pipeline sets the pipeline"),
                );
                pass.set_bind_group(
                    0,
                    self.hdr_glow_bg
                        .as_ref()
                        .expect("ensure_hdr_glow_pipeline sets the bind group"),
                    &[],
                );
                pass.set_vertex_buffer(0, self.vbufs.bloom_glow.buf.slice(..));
                pass.draw(0..6, 0..glow_count);
            }
        }

        // Half-float readback: 8 bytes/px, rows padded to wgpu's 256-byte
        // alignment, decoded to f32 with the local `f16_bits_to_f32`.
        let padded = {
            let raw = (w as usize) * 8;
            raw.div_ceil(256) * 256
        };
        let buffer = self.ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aterm-gpu test hdr readback"),
            size: (padded * h as usize) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &dst,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded as u32),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        self.ctx.queue.submit([enc.finish()]);
        let slice = buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        self.ctx
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("GPU poll failed");
        let data = slice.get_mapped_range();
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for row in 0..h as usize {
            let base = row * padded;
            for col in 0..w as usize {
                let p = base + col * 8;
                for ch in 0..4 {
                    let bits = u16::from_le_bytes([data[p + ch * 2], data[p + ch * 2 + 1]]);
                    out.push(f16_bits_to_f32(bits));
                }
            }
        }
        drop(data);
        buffer.unmap();
        (out, w, h)
    }

    /// Build + cache the blit render pipeline for a swapchain `format` if absent.
    fn ensure_blit_pipeline(&mut self, format: wgpu::TextureFormat) {
        if self.blit_pipelines.contains_key(&format) {
            return;
        }
        let pipeline = self
            .ctx
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("aterm-gpu blit pipeline"),
                layout: Some(&self.blit_layout),
                vertex: wgpu::VertexState {
                    module: &self.blit_shader,
                    entry_point: Some("vs_blit"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &self.blit_shader,
                    entry_point: Some("fs_blit"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(wgpu::BlendState::REPLACE),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            });
        self.blit_pipelines.insert(format, pipeline);
    }

    /// Lazily build (and cache) the ALPHA_BLENDING tray pipeline for `format`.
    /// Mirrors [`Self::ensure_blit_pipeline`]; differs only in entry points, blend
    /// (ALPHA_BLENDING vs REPLACE) and primitive topology (TriangleStrip).
    fn ensure_tray_pipeline(&mut self, format: wgpu::TextureFormat) {
        if self.tray_pipelines.contains_key(&format) {
            return;
        }
        let pipeline = self
            .ctx
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("aterm-gpu tray pipeline"),
                layout: Some(&self.tray_layout),
                vertex: wgpu::VertexState {
                    module: &self.tray_shader,
                    entry_point: Some("vs_tray"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleStrip,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &self.tray_shader,
                    entry_point: Some("fs_tray"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        // Straight-alpha src-over == CPU `composite_tray`.
                        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            });
        self.tray_pipelines.insert(format, pipeline);
    }

    /// Ensure `win.tray_overlay` holds a `w`×`h` RGBA8 card texture whose contents
    /// are `rgba`. RESIDENT reuse at the same `(w, h)`; recreate (new texture +
    /// view + bind group) on a size change; the upload is SKIPPED when the resident
    /// texture already holds exactly these bytes (see `TrayOverlay::pixels`).
    /// Follows the `ImagePlane` upload precedent (`bytes_per_row = w*4`). `rgba`
    /// MUST be `w*h*4` straight-alpha bytes.
    fn ensure_tray_overlay(&mut self, win: &mut WindowGpu, rgba: &[u8], w: u32, h: u32) {
        let recreate = match &win.tray_overlay {
            Some(t) => t.w != w || t.h != h,
            None => true,
        };
        if recreate {
            let device = &self.ctx.device;
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("aterm-gpu tray overlay"),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("aterm-gpu tray overlay bg"),
                layout: &self.tray_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.tray_sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: self.tray_uniform_buf.as_entire_binding(),
                    },
                ],
            });
            win.tray_overlay = Some(TrayOverlay {
                texture,
                view,
                bind,
                w,
                h,
                // Empty ⇒ can never equal a real card ⇒ the fresh texture
                // (undefined contents) is always uploaded into below.
                pixels: Vec::new(),
            });
        }
        // Upload the card pixels — but ONLY when they differ from what the
        // resident texture already holds. The GUI caches the raster and re-sends
        // the IDENTICAL bytes on every hover-stable present (background PTY
        // output keeps presenting while the card is open), so the exact-equality
        // mirror turns those into a memcmp instead of a whole-card upload.
        // TEXTURE_BINDING|COPY_DST write_texture has no 256-byte row constraint,
        // so the caller's tight `w*4` rows go straight in.
        let overlay = win.tray_overlay.as_mut().expect("tray_overlay set above");
        if !recreate && overlay.pixels.as_slice() == rgba {
            return;
        }
        self.ctx.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &overlay.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w * 4),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        overlay.pixels.clear();
        overlay.pixels.extend_from_slice(rgba);
        self.tray_uploads += 1;
    }

    /// Composite the settings-card TRAY into the OFFSCREEN target (the single
    /// source of truth) so BOTH the readback (`render_input`) and the on-glass
    /// blit (`present_input`) show it identically. Runs AFTER the offscreen encode
    /// has already submitted; this is a SEPARATE encoder + submit, so single-queue
    /// submission order guarantees the tray lands ON TOP of the finished frame.
    ///
    /// The tray uniform `fb` is the OFFSCREEN's own padded dims (read straight off
    /// `win.offscreen`, == the `(w, h)` that `encode_frame`/`encode_present_frame`
    /// return); `dx/dy/pw/ph` are already device-px in that space.
    fn draw_tray_into_offscreen(&mut self, win: &mut WindowGpu, tray: TrayQuad<'_>) {
        // The tray pass attaches off.view, so it builds with the offscreen format from
        // the single source of truth (Rgba8Unorm native / Rgba8UnormSrgb downlevel) —
        // the pipeline can't drift from the attachment (C1/C2). Compositing in the
        // offscreen's own space matches the CPU `composite_tray` src-over either way.
        let format = self.ctx.offscreen_format();

        // (1) ALL `&mut self` / `&mut win` work FIRST — before any immutable borrow
        //     of `win.offscreen` / `win.tray_overlay` is taken below.
        self.ensure_tray_pipeline(format);
        self.ensure_tray_overlay(win, tray.rgba, tray.pw, tray.ph);

        // (2) Placement uniform. `fb` is the OFFSCREEN's padded dims.
        let (fw, fh) = {
            let off = win.offscreen.as_ref().expect("encode_frame sets offscreen");
            (off.w, off.h)
        };
        let want = TrayUniform {
            rect: [
                tray.dx as f32,
                tray.dy as f32,
                tray.pw as f32,
                tray.ph as f32,
            ],
            fb: [fw as f32, fh as f32],
            _pad: [0.0, 0.0],
        };
        self.ctx
            .queue
            .write_buffer(&self.tray_uniform_buf, 0, bytemuck::bytes_of(&want));

        // (3) NOW the immutable borrows: the offscreen view (render target) and the
        //     resident overlay bind. A SEPARATE encoder; LoadOp::Load PRESERVES the
        //     just-rendered cells, the ALPHA_BLENDING quad composites straight-alpha
        //     src-over on top.
        let view = &win
            .offscreen
            .as_ref()
            .expect("encode_frame sets offscreen")
            .view;
        let mut enc = self
            .ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aterm-gpu tray-into-offscreen"),
            });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("aterm-gpu tray offscreen pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            let t = win
                .tray_overlay
                .as_ref()
                .expect("ensure_tray_overlay set it above");
            pass.set_pipeline(&self.tray_pipelines[&format]);
            pass.set_bind_group(0, &t.bind, &[]);
            pass.draw(0..4, 0..1); // 4-vert triangle-strip quad
        }
        self.ctx.queue.submit([enc.finish()]);
    }

    /// THE GPU DIRTY-GATE — the per-frame PRESENTATION hot path.
    ///
    /// On an UNCHANGED frame this does ZERO GPU work: no encode, no submit, no
    /// `device.poll`, no readback. It re-presents the previous frame's already-
    /// read-back pixels (a [`RenderView::Borrowed`] over the gate cache). The
    /// gate-hit decision is the SHARED [`is_unchanged_frame`] predicate — the
    /// SAME one the CPU [`Renderer::render_input_cached`] uses — so the GPU and
    /// CPU gates cannot diverge, and on a hit the cached pixels ARE exactly what
    /// the GPU would re-render for that input (nothing changed since we read them
    /// back).
    ///
    /// On a MISS it runs the full GPU render + readback (exactly
    /// [`render_input`](Self::render_input)), stores the resulting `Frame` plus
    /// this frame's input / blink / style-override in the gate cache, and returns
    /// a borrow of the freshly-cached pixels.
    ///
    /// The owned-`Frame` [`render_input`](Self::render_input) path (snapshot /
    /// image / headless verbs) is UNCHANGED and still does its full readback.
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "AsymmetricPadLayout",
            action = "PrimeLayoutCache",
            project = "aterm_gpu::GpuRenderer::project_asymmetric_pad_layout"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "AsymmetricPadLayout",
            action = "RenderWithLayoutCache",
            project = "aterm_gpu::GpuRenderer::project_asymmetric_pad_layout"
        )
    )]
    pub fn render_input_cached<'a>(
        &mut self,
        win: &'a mut WindowGpu,
        input: &RenderInput,
    ) -> RenderView<'a> {
        // Current cursor state lives on the inner CPU renderer (the frontend
        // forwards blink/override there), exactly as the CPU gate reads it.
        let cur_blink = self.cpu.cursor_blink_phase();
        let cur_override = self.cpu.cursor_style_override();

        // Expected pixel dims for this frame, to cross-check the cached frame.
        // The cached frame is encoded at the PADDED size (the grid plus the
        // interior `pad` border on every edge), so the gate must compare against
        // `+ 2*pad` — otherwise any non-zero padding makes the dims never match
        // and the gate never hits, silently defeating the cache on this path.
        let (cw, ch) = self.cpu.cell_size();
        let pad2 = 2 * self.cpu.pad();
        let head = self.cpu.head();
        // Clamp with the SAME helper that sizes the cached frame (`encode_frame` →
        // `read_back` stores `Frame { width, height }` at the clamped size). Without
        // this an oversized grid's unclamped (w, h) never equals `c.frame.{width,
        // height}` (clamped), silently defeating the gate → full readback every frame.
        let (w, h) = (
            self.clamp_fb_dim((input.cols * cw + pad2) as u32) as usize,
            self.clamp_fb_dim((input.rows * ch + pad2 + head) as u32) as usize,
        );

        // GATE-HIT: a prior frame exists, it is pixel-identical to this input,
        // AND its cached pixels are the right size (defensive — `is_unchanged_
        // frame` already requires equal rows/cols, which fixes the dims, but we
        // assert the buffer we are about to hand back genuinely matches).
        let hit = match &win.gate_cache {
            Some(c) => {
                c.frame.width == w
                    && c.frame.height == h
                    && c.grid_top == self.cpu.grid_top()
                    && is_unchanged_frame(
                        &c.input,
                        c.blink_phase,
                        c.cursor_style_override,
                        input,
                        cur_blink,
                        cur_override,
                        ch,
                    )
            }
            None => false,
        };

        if hit {
            self.gate_hits += 1;
            let frame = &win.gate_cache.as_ref().expect("hit implies Some").frame;
            return RenderView::Borrowed {
                width: frame.width,
                height: frame.height,
                pixels: &frame.pixels,
            };
        }

        // MISS: full GPU render + readback, then refresh the gate cache to THIS
        // frame's pixels + state so the next unchanged frame can take the gate.
        self.gate_misses += 1;
        let frame = self.render_input(win, input, None);
        win.gate_cache = Some(GpuGateCache {
            input: input.clone(),
            blink_phase: cur_blink,
            cursor_style_override: cur_override,
            grid_top: self.cpu.grid_top(),
            frame,
        });
        let frame = &win.gate_cache.as_ref().expect("just stored").frame;
        RenderView::Borrowed {
            width: frame.width,
            height: frame.height,
            pixels: &frame.pixels,
        }
    }

    /// Encode + submit the GPU work and BLOCK until the GPU finishes, but do NOT
    /// read the pixels back. This is the on-screen render cost (a window presents
    /// the texture instead of copying it to CPU) — i.e. what the readback path
    /// adds on top is pure verification overhead. Returns nothing; time the call.
    ///
    /// Takes a pre-built [`RenderInput`] (A-3: the engine emits the snapshot via
    /// [`aterm_core::terminal::Terminal::cell_frame_into`]); the renderer never
    /// borrows `&Terminal`.
    pub fn render_no_readback(&mut self, win: &mut WindowGpu, input: &RenderInput) {
        win.present_prev = None;
        let _ = self.encode_frame(win, input, &RepaintScope::Full);
        self.ctx
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("GPU poll failed");
    }

    /// Build the atlas + instances, encode the single render pass onto the
    /// RESIDENT offscreen target (`win.offscreen`, reused at the same `(w, h)`
    /// and rebuilt only on a resize), and submit. Returns the frame's `(w, h)`;
    /// the rendered texture (+ its blit-source bind group) live on
    /// `win.offscreen` for the caller to read back or present.
    ///
    /// `scope` selects FULL (Clear + every row — the always-correct path,
    /// byte-identical to the original encode) or SCISSORED (`RepaintScope::Dirty`):
    /// the offscreen already holds the prior frame, so we preserve it with
    /// `LoadOp::Load`, build instances ONLY for the dirty rows, and scissor the
    /// pass to the dirty rows' bounding band. A re-shaded dirty row gets the
    /// IDENTICAL instances (same values, same order) the full path would build for
    /// it, and rows are disjoint vertical bands (double-HEIGHT, the only cross-band
    /// case, forces FULL), so the scissored band is bit-identical to a full render
    /// and the untouched rows are preserved verbatim.
    ///
    /// SPEC: the bg-instance path of this method is the real implementation of the
    /// external `GpuEncode.tla` model (TRUST_NATIVE_TLA Phase 2, GPU FRAME-ENCODE
    /// safety). The CPU cell walk that `bg_inst.push(BgInstance { … })` per
    /// non-default-bg cell is the spec's `Append`; the frame encode that uploads +
    /// slices `bg_buf` is `Encode`, gated by [`should_slice`] (the real
    /// `NeverSliceEmpty` precondition — slice ONLY when `bgInst > 0`, the exact
    /// 4ab4eb9 fix for the empty-buffer wgpu panic). Tier-1 conformance drives the
    /// real [`should_slice`] decision over the bg-instance count
    /// (`tests/conformance_gpuencode.rs`); the full GPU encode needs a live device,
    /// so the slice DECISION is what is bound, which is exactly the modeled property.
    // PROJECTION (TRUST_VACUITY_GATE §2.2 / finding 2): both actions project the real
    // bg-instance buffer onto the spec's `<<bgInst, sliced>>` — `Append` bumps the
    // instance count, `Encode` is the `should_slice`-gated slice decision. The
    // projection `conformance_gpuencode.rs` drives is named `aterm_gpu::renderer::
    // project_bg_encode`; L2 requires the projection NAME be present (Trust does not
    // execute it — the slice DECISION is the aterm-side Tier-1 binding).
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "gpu_encode",
            action = "Append",
            project = "aterm_gpu::renderer::project_bg_encode"
        )
    )]
    /// (Re)upload the SCENE atlas texture when the frame carries one whose `version`
    /// differs from the resident copy (a skin/theme rebake), or clear it when the frame
    /// has no scene atlas. RGBA8Unorm, sampled by the scene pipelines through `atlas_bgl`
    /// with the LINEAR `scene_sampler`. The atlas width (512) makes `bytes_per_row` (2048)
    /// a multiple of 256, so no row padding is needed.
    fn ensure_scene_atlas(&mut self, input: &RenderInput) {
        let Some(src) = input.scene_atlas.as_deref() else {
            self.scene_atlas = None;
            return;
        };
        let need = (src.width as usize)
            .saturating_mul(src.height as usize)
            .saturating_mul(4);
        if src.width == 0 || src.height == 0 || src.rgba.len() < need {
            self.scene_atlas = None;
            return;
        }
        if matches!(&self.scene_atlas, Some(s) if s.version == src.version && s.w == src.width && s.h == src.height)
        {
            return;
        }
        let device = &self.ctx.device;
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aterm-gpu scene atlas"),
            size: wgpu::Extent3d {
                width: src.width,
                height: src.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.ctx.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &src.rgba[..need],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(src.width * 4),
                rows_per_image: Some(src.height),
            },
            wgpu::Extent3d {
                width: src.width,
                height: src.height,
                depth_or_array_layers: 1,
            },
        );
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        let bind = self
            .ctx
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("aterm-gpu scene atlas bind"),
                layout: &self.atlas_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.scene_sampler),
                    },
                ],
            });
        self.scene_atlas = Some(SceneTex {
            bind,
            w: src.width,
            h: src.height,
            version: src.version,
        });
    }

    /// (Re)upload the peeking-CAT atlas (Sparkle Words v2 `cat_quads`) when the frame
    /// carries one whose `version` differs from the resident copy (a `CatBaker` rebake —
    /// blink frame, cell-metric change), or clear it when the frame has no cat atlas.
    /// The `ensure_scene_atlas` pattern verbatim, with ONE deliberate difference: the
    /// bind group carries the NEAREST glyph `sampler`, not the LINEAR `scene_sampler` —
    /// cats are baked at exact destination size (1:1), so the GPU must read the same
    /// unfiltered texel the CPU's integer-stepped NEAREST stamp reads (§5.1's two
    /// sampling regimes).
    fn ensure_cat_atlas(&mut self, input: &RenderInput) {
        let Some(src) = input.cat_atlas.as_deref() else {
            self.cat_atlas = None;
            return;
        };
        let need = (src.width as usize)
            .saturating_mul(src.height as usize)
            .saturating_mul(4);
        if src.width == 0 || src.height == 0 || src.rgba.len() < need {
            self.cat_atlas = None;
            return;
        }
        if matches!(&self.cat_atlas, Some(s) if s.version == src.version && s.w == src.width && s.h == src.height)
        {
            return;
        }
        let device = &self.ctx.device;
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aterm-gpu cat atlas"),
            size: wgpu::Extent3d {
                width: src.width,
                height: src.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.ctx.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &src.rgba[..need],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(src.width * 4),
                rows_per_image: Some(src.height),
            },
            wgpu::Extent3d {
                width: src.width,
                height: src.height,
                depth_or_array_layers: 1,
            },
        );
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        let bind = self
            .ctx
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("aterm-gpu cat atlas bind"),
                layout: &self.atlas_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        // NEAREST — the 1:1 cat regime (see the fn doc).
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            });
        self.cat_atlas = Some(SceneTex {
            bind,
            w: src.width,
            h: src.height,
            version: src.version,
        });
    }

    /// (Re)upload the FREE-sprite atlas (`RenderInput::free_atlas`, the
    /// arbitrary-rect `FreeSprite` layer) when the frame carries one whose
    /// `version` differs from the resident copy, or clear it when the frame has
    /// no free atlas. The `ensure_cat_atlas` pattern verbatim: ONE bind group
    /// over `atlas_bgl` carrying the NEAREST glyph `sampler` — v1 free sprites
    /// are the cat regime (bake == dest size, 1:1; `FreeSampler::Linear` and its
    /// LINEAR bind group are deferred).
    fn ensure_free_atlas(&mut self, input: &RenderInput) {
        let Some(src) = input.free_atlas.as_deref() else {
            self.free_atlas = None;
            return;
        };
        let need = (src.width as usize)
            .saturating_mul(src.height as usize)
            .saturating_mul(4);
        if src.width == 0 || src.height == 0 || src.rgba.len() < need {
            self.free_atlas = None;
            return;
        }
        if matches!(&self.free_atlas, Some(s) if s.version == src.version && s.w == src.width && s.h == src.height)
        {
            return;
        }
        let device = &self.ctx.device;
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aterm-gpu free atlas"),
            size: wgpu::Extent3d {
                width: src.width,
                height: src.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.ctx.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &src.rgba[..need],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(src.width * 4),
                rows_per_image: Some(src.height),
            },
            wgpu::Extent3d {
                width: src.width,
                height: src.height,
                depth_or_array_layers: 1,
            },
        );
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        let bind = self
            .ctx
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("aterm-gpu free atlas bind"),
                layout: &self.atlas_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        // NEAREST — the v1 free-sprite regime (see the fn doc).
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            });
        self.free_atlas = Some(SceneTex {
            bind,
            w: src.width,
            h: src.height,
            version: src.version,
        });
    }

    /// (Re)upload the PHOSPHOR rain-glyph atlas (`RenderInput::rain_atlas`, the
    /// `RainBaker` white-coverage tiles) when the frame carries one whose
    /// `version` differs from the resident copy (a rebake — cell-metric or ramp
    /// change), or clear it when the frame has no rain atlas. The
    /// `ensure_cat_atlas` pattern verbatim: ONE bind group over `atlas_bgl`
    /// carrying the NEAREST glyph `sampler` — rain tiles are baked at exact
    /// cell size (1:1, the cat regime), so the GPU must read the same
    /// unfiltered texel the CPU's integer-stepped NEAREST stamp reads.
    fn ensure_rain_atlas(&mut self, input: &RenderInput) {
        let Some(src) = input.rain_atlas.as_deref() else {
            self.rain_atlas = None;
            return;
        };
        let need = (src.width as usize)
            .saturating_mul(src.height as usize)
            .saturating_mul(4);
        if src.width == 0 || src.height == 0 || src.rgba.len() < need {
            self.rain_atlas = None;
            return;
        }
        if matches!(&self.rain_atlas, Some(s) if s.version == src.version && s.w == src.width && s.h == src.height)
        {
            return;
        }
        let device = &self.ctx.device;
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aterm-gpu rain atlas"),
            size: wgpu::Extent3d {
                width: src.width,
                height: src.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.ctx.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &src.rgba[..need],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(src.width * 4),
                rows_per_image: Some(src.height),
            },
            wgpu::Extent3d {
                width: src.width,
                height: src.height,
                depth_or_array_layers: 1,
            },
        );
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        let bind = self
            .ctx
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("aterm-gpu rain atlas bind"),
                layout: &self.atlas_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        // NEAREST — the 1:1 rain-tile regime (see the fn doc).
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            });
        self.rain_atlas = Some(SceneTex {
            bind,
            w: src.width,
            h: src.height,
            version: src.version,
        });
    }

    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "gpu_encode",
            action = "Encode",
            project = "aterm_gpu::renderer::project_bg_encode"
        )
    )]
    fn encode_frame(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
        scope: &RepaintScope,
    ) -> (u32, u32) {
        // Bound the shared shaping/glyph caches on the GPU path too. Glyph planning
        // below (row_glyph_plan / resolve_cell_key / glyph_image) inserts into the
        // inner CPU `Renderer`'s caches, but the GPU backend never routes through
        // `Renderer::render_input_cached`, the sole place the CPU path evicts — so
        // without this the caches would grow for the whole process lifetime on a long
        // session streaming varied text. Cheap: a length check that clears past the cap.
        self.cpu.evict_caches_if_large();
        let (rows, cols) = (input.rows, input.cols);
        let (cw, ch) = self.cpu.cell_size();
        let baseline = self.cpu.baseline();
        // Interior padding (px per edge), read from the inner CPU renderer so the
        // GPU grid is inset by the SAME amount as the CPU path: the framebuffer is
        // `2·pad` larger on each axis and every cell origin shifts by
        // `(pad, grid_top)`. With `pad == 0` this is byte-identical to before (the
        // `+ pad` terms drop out). Keeping the GPU and CPU pad in lockstep
        // preserves CPU/GPU parity AND the image-vs-window parity within the GPU
        // backend (the offscreen `render_input` and the on-glass `present_input`
        // both run this encode).
        let pad = self.cpu.pad();
        // Chrome headroom (px above the padded grid), also from the CPU renderer:
        // the framebuffer grows by `head` on Y and the grid interior's top-left
        // moves to `(pad, grid_top)`. With `head == 0` this is byte-identical to
        // before (`grid_top == pad`).
        let head = self.cpu.head();
        // Single-source the grid's Y-origin from the CPU renderer so a top-only
        // `pad_top` override (grid_top == pad_top + head) stays byte-identical
        // across backends. `pad` (X inset, fb width) and `head` (fb height) are
        // still read above; only the vertical grid origin consults `pad_top`.
        let grid_top = self.cpu.grid_top();
        // Clamp the padded framebuffer to the device limit via `clamp_fb_dim` (see
        // there). Clamping HERE (not only inside `offscreen_texture`) keeps the whole
        // encode consistent: the offscreen TEXTURE, the `screen` projection uniform,
        // the scissor rects, and the stored `Offscreen { w, h }` all use the clamped
        // size, so an extreme grid renders a CLIPPED view instead of crashing, and the
        // blit SOURCE (this offscreen) matches the DESTINATION (swapchain, clamped by
        // the SAME helper) 1:1.
        let fb_w = (cols * cw + 2 * pad) as u32;
        let fb_h = (rows * ch + 2 * pad + head) as u32;
        let w = self.clamp_fb_dim(fb_w);
        let h = self.clamp_fb_dim(fb_h);
        // BOUND THE PER-FRAME INSTANCE STREAMS to the clamped framebuffer. The encode
        // loops below iterate the FULL `input.rows × input.cols`; a cell whose pixel
        // ORIGIN lies outside the clamped framebuffer emits no visible pixels (it is
        // scissored / NDC-clipped), so skipping it is byte-identical — yet WITHOUT
        // skipping, a densely-filled oversized grid (up to MAX_GRID 4096×4096) would
        // build ~16.7M GlyphInstances (36 B each) → a > 256 MiB vertex buffer →
        // `create_buffer` validation error → the (uninstalled) uncaptured-error handler
        // aborts the process (a DoS the framebuffer clamp above merely relocated here
        // from `create_texture`). Bounding to the on-screen cell count sizes the streams
        // by the framebuffer AREA (≈ `(w/cw)·(h/ch)`), not the grid.
        //
        // ROWS: `vis_rows` caps the OUTER loops. Row pitch is a uniform `ch`, so this is
        // width-mode independent; for a grid that FITS (`h == fb_h`) it is `>= rows`, so
        // every `.take(vis_rows)` is a no-op → byte-identical (parity). `+2` covers the
        // `pad`/integer-division slack and glyph ascent across the boundary.
        // COLS: per-row `rcw` varies (DECDWL), so columns are bounded by a per-cell
        // `break` GATED on `clip_cols` — true only when the width was actually clamped
        // (`w < fb_w`). A grid that fits keeps the full column iteration (parity,
        // including the double-width rows whose far cells are already off-screen-clipped
        // exactly as today). The `+ rcw` margin keeps a boundary cell's glyph bearing.
        let vis_rows = (h as usize / ch).saturating_add(2).min(rows);
        let clip_cols = w < fb_w;

        // Deco sprite atlas (sparkle words + the W7 undercurl tile):
        // build/refresh for this cell size + resolved underline band before
        // the pass borrows `self` immutably. Unconditional since W7 — a curly
        // underline can appear on any frame, and the rebuild check is a cheap
        // per-frame no-op once the atlas matches the geometry.
        self.ensure_deco_atlas(cw, ch);
        // SCENE atlas upload (version-cached) — done HERE, before the `&self` atlas/glyph
        // borrows below, since it takes `&mut self`. A no-op when the frame has no scene.
        self.ensure_scene_atlas(input);
        // CAT atlas upload, beside the scene's (version-keyed on `cat_atlas.version`).
        self.ensure_cat_atlas(input);
        // FREE-sprite atlas upload, the same version-keyed pattern.
        self.ensure_free_atlas(input);
        // PHOSPHOR rain atlas upload, the same version-keyed pattern.
        self.ensure_rain_atlas(input);

        // Which rows to (re)build instances for. FULL: every row. Dirty: only the
        // flagged rows (others are preserved on the offscreen by LoadOp::Load).
        // A closure over the scope so every per-row loop below shares ONE filter.
        let row_active = |r: usize| -> bool {
            match scope {
                RepaintScope::Full => true,
                RepaintScope::Dirty(dirty) => dirty.get(r).copied().unwrap_or(false),
            }
        };
        // The scissor band: (first, last) dirty row under a Dirty scope. `None`
        // under Full or when nothing is dirty. Computed ONCE here because both the
        // band-edge pad strips below and the scissor rect need it.
        let dirty_band = match scope {
            RepaintScope::Dirty(dirty) => {
                let mut first = None;
                let mut last = 0usize;
                for (r, &d) in dirty.iter().enumerate() {
                    if d {
                        first.get_or_insert(r);
                        last = r;
                    }
                }
                first.map(|f| (f, last))
            }
            RepaintScope::Full => None,
        };

        // Visible rows, already resolved by `extract` under the lock.
        let rendered: &[Vec<aterm_core::terminal::RenderCell>] = &input.cells;

        let (cr, cc) = (input.cursor_row, input.cursor_col);
        let cursor_in = cr < rows && cc < cols;
        // Cursor shape for THIS frame: DECSCUSR or the frontend's override,
        // gated by DECTCEM and the blink phase — read from the inner CPU
        // renderer so the suppression rules are byte-for-byte the CPU's.
        let style = self
            .cpu
            .cursor_style_override()
            .unwrap_or(input.cursor_style);
        let cursor_drawn = cursor_in
            && input.cursor_visible
            && aterm_render::cursor_shown(style, self.cpu.cursor_blink_phase());
        let block_cursor =
            cursor_drawn && matches!(style, CursorStyle::BlinkingBlock | CursorStyle::SteadyBlock);

        // Reset the persistent per-frame instance streams + key set (capacity
        // retained — no per-frame allocation in the steady state). They are
        // rebuilt below with identical contents in identical order.
        self.inst.clear();

        // Atlas for every drawable glyph across the grid: each char resolves
        // to its full glyph identity (face/style/size) via the CPU renderer's
        // cached dispatch, so the set — and the packing — is per-glyph. The set
        // is moved OUT of `self.inst` (swapping in an empty placeholder) so the
        // `self.cell_key`/`self.cpu.glyph_key` calls below can borrow `self`
        // freely; it is moved back before being read.
        // Ligature plan per active row, built via the SHARED CPU planner so the
        // GPU keys + quads the IDENTICAL `mono_gid` glyph at the IDENTICAL column
        // the CPU blits — the CPU==GPU byte-identical invariant. Built BEFORE the
        // atlas/key loops so those can borrow `self.cpu` (the planner needs `&mut`).
        // Empty rows / globally-off ligatures yield all-`PerCell` plans (the
        // pre-ligature key set, byte-identical).
        // Reuse the persistent plan scratch (mem::take, like `dirty_scratch`) so a
        // stable-dimension frame allocates no per-row plan Vecs after warmup;
        // `row_glyph_plan` clears+resizes each inner Vec, so retained capacities
        // are reused and the contents stay byte-identical. Restored to
        // `self.row_plans` after the last reader (the glyph loop) below.
        let mut row_plans = std::mem::take(&mut self.row_plans);
        row_plans.resize_with(rendered.len(), Vec::new);
        // Reuse a single persistent break-column scratch across every active row
        // (mem::take, like `row_plans`); `_into` clears it per row, so a
        // selection-drag full repaint allocates no per-row break Vec after warmup
        // and the plan stays byte-identical. Restored after the loop.
        let mut break_cols = std::mem::take(&mut self.ligature_break_scratch);
        for (r, plan) in row_plans.iter_mut().enumerate() {
            if !row_active(r) {
                continue;
            }
            self.cpu
                .ligature_break_cols_for_row_into(input, r, &mut break_cols);
            self.cpu.row_glyph_plan(input, r, &break_cols, plan);
        }
        self.ligature_break_scratch = break_cols;

        // W4 (cursor ink integrity): the block-cursor cut-out geometry, computed
        // through the SAME shared helpers as the CPU `draw_cursor` so both paths
        // repaint identical slices.
        //
        // Cursor-row cell width (doubled on any DEC double-size line).
        let cur_cw = if cr < rows {
            aterm_render::row_cell_w(input.line_sizes[cr], cw)
        } else {
            cw
        };
        // A block cursor on a WIDE lead (CJK/emoji) covers the glyph's whole
        // 2-cell footprint (the CPU's `cur_w`), so the ideograph inverts as one.
        let cursor_wide_lead = block_cursor
            && rendered
                .get(cr)
                .and_then(|row| row.get(cc + 1))
                .is_some_and(|n| n.wide);
        let cur_block_w = if cursor_wide_lead { 2 * cur_cw } else { cur_cw };
        // Cut-out source columns: the cursor cell plus (when the cursor sits
        // inside a ligated run) the whole contiguous run — the run's covering
        // glyph may live on a NEIGHBOUR column and span the cursor cell. Gated
        // on the CPU's drawability guard (blank / wide-continuation cursor cells
        // cut nothing), byte-for-byte the CPU `draw_cursor` predicate.
        let cursor_cell = rendered.get(cr).and_then(|row| row.get(cc));
        let cut_out_on = block_cursor
            && cursor_cell
                .is_some_and(|cell| !cell.wide && cell.ch != ' ' && !cell.ch.is_control());
        let (cut_lo, cut_hi) = if cut_out_on {
            aterm_render::cursor_cutout_cols(row_plans.get(cr).map_or(&[][..], Vec::as_slice), cc)
        } else {
            // Sentinel empty range (lo > hi): no column contributes a slice.
            (1, 0)
        };
        // The cursor rect's x-span: every cut-out slice is clipped to it, so a
        // glyph whose ink exits the rect (a ligature spanning its lead cells)
        // never has its OUTSIDE ink re-drawn in bg — partition/no-bleed.
        let (cut_x0, cut_x1) = if cut_out_on {
            let x0 = (pad + cc * cur_cw) as i32;
            (x0, x0 + cur_block_w as i32)
        } else {
            (0, 0)
        };
        // The cut-out recolours in the CURSOR cell's bg over the cursor fill —
        // the CPU's `cut_color`/`cursor_fill` operand pair.
        let cutout_color = cursor_cell.map_or([0, 0, 0, 0], |cell| rgb4(cell.bg));

        let mut keys = std::mem::take(&mut self.inst.keys);
        for (r, cells) in rendered.iter().enumerate() {
            // A scissored Dirty repaint only re-encodes its dirty rows (the instance
            // loops below skip the rest via `row_active`), so it only needs THOSE
            // rows' atlas keys; the resident atlases already hold the untouched rows'
            // glyphs. Under Full, `row_active` is true for every row (unchanged).
            if !row_active(r) {
                continue;
            }
            let plan = &row_plans[r];
            // Cell advance for THIS row (DECDWL doubles it) — the shade-phase
            // fold below needs the row's true pixel origins.
            let rcw = aterm_render::row_cell_w(
                input
                    .line_sizes
                    .get(r)
                    .copied()
                    .unwrap_or(aterm_core::grid::LineSize::SingleWidth),
                cw,
            );
            for (c, cell) in cells.iter().take(cols).enumerate() {
                // An image-covered cell skips its glyph (image-vs-glyph
                // precedence, mirroring the CPU `image_covers` guard), so it
                // contributes no atlas key — UNLESS the image is z<0 (behind text),
                // where the glyph still draws (shared `image_hides_glyph_at`).
                if Self::drawable(cell) && !input.image_hides_glyph_at(r, c) {
                    // A ligature-owned column contributes the shaped `mono_gid`
                    // key (matching the CPU plan); other columns the per-cell key.
                    let key = match plan
                        .get(c)
                        .copied()
                        .unwrap_or(aterm_render::ColumnGlyph::PerCell)
                    {
                        aterm_render::ColumnGlyph::Ligated(gid) => {
                            self.cpu.ligature_key(gid, aterm_render::cell_style(cell))
                        }
                        // M4: a collapsed (Cascadia N:1) cell keys the wide glyph's
                        // per-cell slice tile — IDENTICAL key to the CPU cache, so the
                        // atlas holds the same cell-local coverage (parity).
                        aterm_render::ColumnGlyph::LigatedSlice { gid, k, .. } => self
                            .cpu
                            .ligature_slice_key(gid, k, aterm_render::cell_style(cell)),
                        aterm_render::ColumnGlyph::PerCell => {
                            self.cell_key(input.cluster_at(r, c), cell)
                        }
                    };
                    // Shade dithers key on the cell's ABSOLUTE pixel parity
                    // (no-op otherwise) — identical fold to the CPU blit and
                    // the quad-emission loop below, so the atlas holds the
                    // exact phase variants those quads will reference.
                    let key = aterm_render::shade_phase_key(key, pad + c * rcw, grid_top + r * ch);
                    keys.insert(key);
                    // Combining-mark glyphs share the mono atlas.
                    if input.cluster_at(r, c).is_none()
                        && let Some(marks) = input.combining_at(r, c)
                    {
                        for &m in marks {
                            keys.insert(self.cpu.glyph_key(m));
                        }
                    }
                }
            }
        }
        // Persist the atlases across frames: a subset frame (the steady state —
        // including idle cursor-blink ticks) reuses the resident textures + bind
        // groups untouched; a miss grows them incrementally; only genuine
        // overflow recreates a texture. This is the G-1 fix — no per-frame
        // rebuild/re-upload. After this, `mono_res`/`color_res` are Some and hold
        // every key in `keys`. (`keys` is still the moved-out local here, so
        // `ensure_atlases`' `&mut self` doesn't alias it; it returns to
        // `self.inst.keys` right after.)
        self.ensure_atlases(&keys);
        self.inst.keys = keys;
        // Build the per-frame inline-image texture (iTerm2 OSC 1337). `&mut self`,
        // so it runs BEFORE the resident-atlas borrows below. A no-op (drops any
        // prior plane) for image-free frames — the common path is untouched.
        self.build_image_plane(win, input);
        let mono_res = self
            .mono_res
            .as_ref()
            .expect("ensure_atlases sets mono_res");
        let color_res = self
            .color_res
            .as_ref()
            .expect("ensure_atlases sets color_res");
        let atlas = &mono_res.atlas;
        let color_atlas = &color_res.atlas;
        // UVs normalise against the RESIDENT TEXTURE height (`tex_h`), which is
        // what the GPU samples — after an incremental append the CPU `atlas.height`
        // may exceed neither but the texture is unchanged, so `tex_h` is the only
        // correct divisor. (Slot `ay` are absolute texture rows.)
        let (aw, ah) = (atlas.width as f32, mono_res.tex_h as f32);
        let (caw, cah) = (color_atlas.width as f32, color_res.tex_h as f32);
        let atlas_bind = &mono_res.bind;
        let color_bind = &color_res.bind;
        let deco_bind = self.deco_atlas.as_ref().map(|d| &d.bind);

        // Selection highlight, exactly the CPU rule: selection rows are
        // live-screen coords, viewport row r shows live row (r - display_offset),
        // and a selected cell's bg fill becomes the theme selection colour.
        let selection = &input.selection;
        let display_offset = input.display_offset;

        // Instances. BG: one opaque quad per cell. GLYPH: one alpha quad per
        // drawable glyph — including the block-cursor cell's own (its full quad
        // stays in the fg pass, exactly like the CPU base pass). These push into
        // the persistent (cleared) streams on `self.inst`; disjoint-field
        // borrows keep them split from `self.cpu`/`self.theme`/`self.mono_res`/
        // `self.color_res` used in the same loop. W4: the opaque cursor fill is
        // drawn AFTER the glyph passes (covering neighbour glyph overflow and
        // the in-rect fg ink, exactly like the CPU paints the block cursor
        // last), and each cut-out source glyph contributes a SLICE clipped to
        // the cursor rect (bg-coloured, drawn after the fill) — so the
        // complement of the rect is byte-identical to the no-cursor frame.
        let bg_inst = &mut self.inst.bg;
        let glyph_inst = &mut self.inst.glyph;
        // EMBERFORGE CONTRAST-HALO stream (disjoint `self.inst` field): the dark
        // warm dilation ring around each glyph engulfed by the fire (a
        // `fire_halo` cell — the colour-free strength stream; the ink itself is
        // never recoloured). Only a frame carrying a live fire field engulfs
        // glyphs, so the halo is also gated on `fire_present` — a `fire_halo`
        // cell WITHOUT fire (a decayed field) pushes NO halo, and `char_fg`
        // alone (the pure ember-recolour path pinned byte-exact by
        // `glow_under`/`present_real`) never keys the ring.
        let glyph_halo_inst = &mut self.inst.glyph_halo;
        let fire_present = !input.fire_patch.is_empty();
        // The shared CONTRAST-HALO ink as a deco-over instance colour: RGB from
        // `aterm_render::HALO_IN_FIRE_RGB`; the per-copy straight source-over
        // alpha rides per instance in `.a` (`fs_deco_over` multiplies it by the
        // sampled glyph coverage — the CPU `blit_mono_halo` blend, replicated)
        // as the SHARED `fire_halo_alpha(strength)` byte, so both backends
        // derive the identical engulfment-scaled alpha.
        let halo_rgb = {
            let rgb = aterm_render::HALO_IN_FIRE_RGB;
            [
                ((rgb >> 16) & 0xff) as u8,
                ((rgb >> 8) & 0xff) as u8,
                (rgb & 0xff) as u8,
            ]
        };
        let color_inst = &mut self.inst.color;
        let cursor_glyph_inst = &mut self.inst.cursor_glyph;
        let cursor_color_inst = &mut self.inst.cursor_color;
        // LIVE default-bg / cursor colour for this frame (OSC 11/111, OSC 12/112,
        // DECSCNM) when the host resolved them, else the configured theme — read off
        // `self.theme` only (a disjoint field from the `&mut self.inst` borrows above),
        // mirroring the CPU `frame_bg`/`frame_cursor`. `COLOR_UNSET` (non-windowed
        // paths) → theme, byte-identical to before.
        let frame_bg = if input.default_bg == aterm_core::render::COLOR_UNSET {
            self.theme.bg
        } else {
            input.default_bg
        };
        let frame_cursor = if input.cursor_color == aterm_core::render::COLOR_UNSET {
            self.theme.cursor
        } else {
            input.cursor_color
        };
        // Background/cursor opacity (read off the CPU face — the single source
        // of truth, like min_contrast). `bg_alpha` is the literal alpha byte a
        // DEFAULT-bg quad (or the clear) writes: 255 at opacity 1.0 (the
        // historical bytes), matching the CPU's `255 - transmittance` exactly.
        // `cursor_cov == 255` keeps the opaque cursor fill + block cut-out path
        // byte-identical; below 255 the fill BLENDS over the rendered cell and
        // the cut-out is skipped (the glyph stays in the normal streams) —
        // mirroring the CPU `draw_cursor`.
        let bg_opacity = self.cpu.background_opacity();
        // M5: the ONE background-quad alpha policy (`round(clamp01(o) * 255)`,
        // opaque at o >= 1) — the same function the ink-opacity proof and the
        // present path use, so the byte a bg quad carries has a single owner.
        let bg_alpha: u8 = aterm_render::vibrancy::bg_quad_alpha(bg_opacity);
        let cursor_cov: u8 = (self.cpu.cursor_opacity() * 255.0).round() as u8;
        let cursor_opaque = cursor_cov == 255;
        // The full-row/default-bg quad colour: the frame default bg with the
        // background-opacity alpha (255 at the default — byte-identical).
        let theme_bg = {
            let mut c = rgb4_u32(frame_bg);
            c[3] = bg_alpha;
            c
        };
        // The selection band colour for THIS frame — active when focused, the dimmer
        // inactive bg when unfocused. Read off the CPU face (the single source of
        // truth) so the GPU band matches the CPU pixel-for-pixel. Captured once so
        // the per-cell selection-fg floor below doesn't borrow self inside the loop
        // (where self.cpu is borrowed for glyph-key resolution).
        let theme_selection = self.cpu.effective_selection_bg();
        // Explicit selectionForeground override (read off the CPU face — the single
        // source of truth — so GPU/CPU selected-glyph colour stays identical).
        let selection_fg = self.cpu.selection_fg();
        // Per-cell minimum-contrast floor (read off the CPU face — the single
        // source of truth; `<= 1.0` = off and the floor returns the raw fg,
        // keeping the disabled path byte-identical). Matches the CPU seam.
        let min_contrast = self.cpu.minimum_contrast();
        // W5b: with the min-contrast knob on, the cursor fill is floored against
        // the bg of the cell it sits on (an OSC-12 cursor near bg can't vanish);
        // off, it is exactly `frame_cursor`. The SAME value feeds the block
        // fill, the non-block cursor quads, and the cut-out remap operand —
        // the shared `floor_cursor_fill`, so CPU/GPU derive one pixel.
        // The typing-reactive rainbow cursor overrides the BLOCK fill only (mirrors the
        // CPU `draw_cursor`): the host's evolving colour, still floored against the cell
        // CPU twin (draw_cursor): the fill override paints EVERY cursor shape —
        // block, bar, underline, and the BOLT (laser lightning cursor) alike. The
        // old block-only gate left a TUI's bar cursor theme green (the owner's
        // "green flash" opening Claude); per-style hosts already gate which fill
        // is Some, so at most one arrives and every shape may wear it.
        let base_cursor = match input.cursor_fill_override {
            Some(fill) => fill,
            None => frame_cursor,
        };
        let cursor_fill = aterm_render::floor_cursor_fill(
            base_cursor,
            cursor_cell.map_or(frame_bg, |cell| aterm_render::rgb_to_u32(cell.bg)),
            min_contrast,
        );
        // SCISSORED PATH ONLY: when the dirty band touches the grid's FIRST/LAST
        // row, the scissor is extended into the top/bottom strip — the head band
        // plus pad above, the pad below (see the band calc) — and these quads
        // re-establish that strip from bg, exactly the bytes FULL's whole-target
        // clear gives it. Needed because effects (bloom halo, fire in the head
        // band) draw there under LoadOp::Load; without the reset the drawn light
        // would be preserved and re-added every frame (accumulation). The per-row
        // full-width quad below already resets the LEFT/RIGHT pad of every dirty
        // row the same way.
        if matches!(scope, RepaintScope::Dirty(_))
            && let Some((f, last)) = dirty_band
        {
            if f == 0 && grid_top > 0 {
                bg_inst.push(BgInstance {
                    rect: [0, 0, w as u16, grid_top as u16],
                    color: theme_bg,
                });
            }
            let grid_bot = ((grid_top + rows * ch) as u32).min(h);
            if last + 1 >= rows && grid_bot < h {
                bg_inst.push(BgInstance {
                    rect: [0, grid_bot as u16, w as u16, (h - grid_bot) as u16],
                    color: theme_bg,
                });
            }
        }
        for (r, cells) in rendered.iter().enumerate().take(vis_rows) {
            if !row_active(r) {
                continue;
            }
            // Integer pixel row top for the packed-u16 bg rects (inset by
            // `grid_top`). Saturating: an oversized grid's off-screen rows land at
            // u16::MAX (clipped) rather than wrapping into view — see `sat_pos_u16`.
            let y0u = sat_pos_u16(grid_top + r * ch);
            let sel_row = r as i32 - display_offset;
            // DEC line size (DECDWL/DECDHL): the cell advance, glyph NEAREST
            // enlargement and dest-row clip come from the SAME helpers the CPU
            // blit uses, so the quads reproduce it exactly.
            let line_size = input.line_sizes[r];
            let rcw = aterm_render::row_cell_w(line_size, cw);
            let (scale, anchor_y) =
                aterm_render::row_scale(line_size, grid_top + r * ch, ch, r + 1 == input.rows);
            // Animated-ink fg overrides for THIS row: the SAME once-per-row
            // slice + lockstep merge-walk as the CPU `render_row` (shared
            // `ink_row_slice`/`InkWalk`, one compare per cell — no hashing, no
            // allocation). ORDERING INVARIANT (mirrors the CPU): ink
            // substitutes for `cell.fg` FIRST, then the fg floors apply to the
            // FINAL ink colour — min-contrast on unselected cells, the
            // selection floor on selected cells (exclusive branches, as
            // pre-ink; selection legibility wins over shimmer). A row beyond
            // u16 can carry no ink (`InkCell.row` is u16) → empty slice.
            let mut ink_walk = aterm_render::InkWalk::new(
                u16::try_from(r)
                    .map_or(&[][..], |row| aterm_render::ink_row_slice(&input.ink, row)),
            );
            // EMBERFORGE charred glyph-ink overrides for THIS row: the same
            // once-per-row slice + lockstep merge-walk as ink, at the same fg
            // seam — INK WINS when a cell carries both (mirrors the CPU).
            let mut char_fg_walk =
                aterm_render::CharFgWalk::new(u16::try_from(r).map_or(&[][..], |row| {
                    aterm_render::char_fg_row_slice(&input.char_fg, row)
                }));
            // The DEDICATED fire_halo walk for the CONTRAST-HALO gate: answers
            // "how engulfed is this cell?" for the halo ring without touching
            // the ink/char_fg substitution above — the colour-free strength
            // stream (the ink never recolours). Same sorted-unique row-slice
            // regime, same ascending lockstep, shared with the CPU
            // (`fire_halo_row_slice`/`FireHaloWalk`).
            let mut halo_walk =
                aterm_render::FireHaloWalk::new(u16::try_from(r).map_or(&[][..], |row| {
                    aterm_render::fire_halo_row_slice(&input.fire_halo, row)
                }));
            // SCISSORED PATH ONLY: a FULL-ROW-WIDTH theme-bg quad FIRST, so the
            // band is fully re-established from background even if the per-cell
            // fills below leave any sliver (degenerate cols). bg is REPLACE and the
            // per-cell quads fully tile [0, w) (single: cols·cw == w; double-width:
            // cols·2cw ⊇ w), so this quad is entirely overwritten — byte-identical
            // to the FULL path's `LoadOp::Clear(theme.bg)` for this band, with no
            // seam and no stale contamination. (FULL path keeps the pass Clear, so
            // it does NOT emit this — its whole-target clear already covers it.)
            if matches!(scope, RepaintScope::Dirty(_)) {
                bg_inst.push(BgInstance {
                    rect: [0, y0u, w as u16, ch as u16],
                    color: theme_bg,
                });
            }
            for (c, cell) in cells.iter().take(cols).enumerate() {
                // Stop once a cell's origin is off the RIGHT of the clamped framebuffer
                // (`clip_cols` ⇒ the width was clamped): the rest of the row is
                // off-screen. `+ rcw` keeps a boundary cell's glyph bearing.
                if clip_cols && pad + c * rcw >= w as usize + rcw {
                    break;
                }
                let x0u = sat_pos_u16(pad + c * rcw);
                // A lead cell is wide iff the NEXT cell is its continuation.
                let is_wide_lead = cells.get(c + 1).is_some_and(|n| n.wide);
                let color = if selection.contains_cell(sel_row, c as u16, is_wide_lead, cell.wide) {
                    // The active/inactive selection band colour captured above.
                    rgb4_u32(theme_selection)
                } else {
                    // Background-opacity: ONLY a cell whose bg resolved to the
                    // frame DEFAULT bg carries the alpha; SGR-colored bg cells
                    // stay opaque (the CPU render_row pass-1 rule, verbatim).
                    let mut c = rgb4(cell.bg);
                    if bg_alpha != 255 && aterm_render::rgb_to_u32(cell.bg) == frame_bg {
                        c[3] = bg_alpha;
                    }
                    c
                };
                bg_inst.push(BgInstance {
                    rect: [x0u, y0u, rcw as u16, ch as u16],
                    color,
                });
            }
            for (c, cell) in cells.iter().take(cols).enumerate() {
                if clip_cols && pad + c * rcw >= w as usize + rcw {
                    break; // off the right edge of the clamped framebuffer — see vis_rows
                }
                // Image-covered cells skip their glyph (image-vs-glyph
                // precedence) — the CPU `image_covers` guard, mirrored. A z<0 image
                // (behind text) does NOT hide the glyph (shared `image_hides_glyph_at`).
                if !Self::drawable(cell) || input.image_hides_glyph_at(r, c) {
                    continue;
                }
                // Cached resolve (the atlas pass above already keyed this char).
                // Direct `self.cpu` field access (not the `cell_key` method) so
                // the `mono_res`/`color_res` borrows above stay disjoint. A
                // ligature-owned column draws the shaped `mono_gid` glyph at the
                // column origin, IDENTICAL to the CPU plan (parity).
                let key = match row_plans[r]
                    .get(c)
                    .copied()
                    .unwrap_or(aterm_render::ColumnGlyph::PerCell)
                {
                    aterm_render::ColumnGlyph::Ligated(gid) => {
                        self.cpu.ligature_key(gid, aterm_render::cell_style(cell))
                    }
                    // M4: collapsed cell -> the wide glyph's per-cell slice tile,
                    // emitted as an ordinary cell-local quad (xmin 0, cell_w wide) at
                    // this column — byte-identical to the CPU slice blit.
                    aterm_render::ColumnGlyph::LigatedSlice { gid, k, .. } => self
                        .cpu
                        .ligature_slice_key(gid, k, aterm_render::cell_style(cell)),
                    aterm_render::ColumnGlyph::PerCell => {
                        self.cpu.resolve_cell_key(input.cluster_at(r, c), cell)
                    }
                };
                // The same absolute shade-phase fold as the CPU blit + the
                // atlas pass above (a no-op for non-shade keys), so this quad
                // references the atlas slot holding the right dither variant.
                let key = aterm_render::shade_phase_key(key, pad + c * rcw, grid_top + r * ch);
                // W4: this column contributes a cut-out slice — the cursor cell
                // itself, or a member of the cursor's ligated run whose covering
                // glyph spans the cursor cell. The FULL quad stays in the normal
                // fg pass (the opaque cursor fill covers its in-rect part,
                // exactly like the CPU base pass + fill), so the complement of
                // the cursor rect is byte-identical to the no-cursor frame; only
                // the slice INSIDE the rect is re-drawn after the fill.
                // W4 cut-out gated by `cursor_opaque` (theirs' cursor-opacity): a
                // TRANSLUCENT cursor draws its glyph in the NORMAL fg pass under the
                // blended fill — no cut-out slice, mirroring the CPU's blend+skip.
                let in_cutout =
                    cut_out_on && cursor_opaque && r == cr && c >= cut_lo && c <= cut_hi;
                let cut_scale = aterm_render::Scale {
                    clip_x0: cut_x0,
                    clip_x1: cut_x1,
                    ..scale
                };
                // Colour emoji: blit straight RGBA from the colour atlas. The
                // emoji carries its own colour, so the instance `color` is unused
                // — exactly like the CPU's Rgba blit. Under the block cursor its
                // in-rect slice is re-drawn over the cursor fill (own colours),
                // as the CPU does (its Rgba blit ignores the cut-out colour).
                if let Some(slot) = color_atlas.map.get(&key) {
                    if slot.gw == 0 || slot.gh == 0 {
                        continue;
                    }
                    let Some((rect, uv)) = aterm_render::glyph_quad(
                        (pad + c * rcw) as f32,
                        anchor_y,
                        baseline,
                        scale,
                        slot.ax,
                        slot.ay,
                        slot.gw,
                        slot.gh,
                        slot.xmin,
                        slot.ymin,
                        caw,
                        cah,
                    ) else {
                        continue;
                    };
                    color_inst.push(GlyphInstance {
                        rect,
                        uv,
                        color: [0, 0, 0, 0],
                        // fs_glyph_color ignores bg (no remap for colour emoji).
                        bg: [0, 0, 0, 0],
                    });
                    if in_cutout
                        && let Some((rect, uv)) = aterm_render::glyph_quad(
                            (pad + c * rcw) as f32,
                            anchor_y,
                            baseline,
                            cut_scale,
                            slot.ax,
                            slot.ay,
                            slot.gw,
                            slot.gh,
                            slot.xmin,
                            slot.ymin,
                            caw,
                            cah,
                        )
                    {
                        cursor_color_inst.push(GlyphInstance {
                            rect,
                            uv,
                            color: [0, 0, 0, 0],
                            bg: [0, 0, 0, 0],
                        });
                    }
                    continue;
                }
                let Some(slot) = atlas.map.get(&key) else {
                    continue;
                };
                if slot.gw == 0 || slot.gh == 0 {
                    continue;
                }
                // Normal fg — the one shared ink policy (selectionForeground /
                // selection floor / per-cell minimum contrast) via the same
                // `effective_glyph_fg` the CPU blit calls (parity by construction).
                // Ink override BEFORE any floor (Sparkle Words v2), then the
                // EMBERFORGE char_fg override at the same seam — INK WINS when a
                // cell carries both: the substituted base_fg is the fg operand,
                // so a settled/animated override colour is floored for selection /
                // min-contrast legibility — the Rgba (emoji) branch above never
                // reaches here, so emoji cells are untouched, as on the CPU. The
                // block-cursor cut-out no longer recolours this quad (it stays in
                // the fg pass, covered by the fill); the in-rect slice is pushed
                // separately below.
                let base_fg = match ink_walk.at(c as u16) {
                    Some(ink) => aterm_render::rgb_to_u32(ink),
                    None => char_fg_walk
                        .at(c as u16)
                        .unwrap_or_else(|| aterm_render::rgb_to_u32(cell.fg)),
                };
                let cell_selected = selection.contains_cell(
                    sel_row,
                    c as u16,
                    cells.get(c + 1).is_some_and(|n| n.wide),
                    cell.wide,
                );
                let glyph_color = rgb4_u32(aterm_render::effective_glyph_fg(
                    selection_fg,
                    min_contrast,
                    base_fg,
                    aterm_render::rgb_to_u32(cell.bg),
                    cell_selected,
                    theme_selection,
                ));
                let Some((rect, uv)) = aterm_render::glyph_quad(
                    (pad + c * rcw) as f32,
                    anchor_y,
                    baseline,
                    scale,
                    slot.ax,
                    slot.ay,
                    slot.gw,
                    slot.gh,
                    slot.xmin,
                    slot.ymin,
                    aw,
                    ah,
                ) else {
                    continue;
                };
                // W2: the colour painted UNDER this glyph — the luminance
                // operand of the corrected-alpha remap. Matches the fill this
                // cell actually received: the cursor block for the cut-out
                // glyph, the selection band for selected cells, else the cell
                // bg (exactly the bg-quad loop's choice above).
                let bg_under = if cell_selected {
                    rgb4_u32(theme_selection)
                } else {
                    rgb4(cell.bg)
                };
                // CONTRAST-HALO: an engulfed cell (a `fire_halo` entry in this
                // fire frame) gets a dark warm dilation ring — the glyph's OWN
                // coverage quad stamped at each shared device-px offset, into the
                // `glyph_halo` stream drawn earlier (over the flame, under the
                // glyph ink) via the mono-bound deco-over pipeline, at the cell's
                // engulfment-scaled alpha (the shared `fire_halo_alpha` byte — a
                // lick barely rims, the wall rims firmly). Only the rect shifts
                // (`uv` unchanged), so the atlas sample stays inside this glyph's
                // tile — no neighbour bleed. Mirrors the CPU `blit_glyph_halo`
                // offset dilation byte-for-byte; the ink itself is untouched
                // (the no-recolor law).
                if fire_present && let Some(strength) = halo_walk.at(c as u16) {
                    let a = aterm_render::fire_halo_alpha(strength);
                    for &(dx, dy) in &aterm_render::HALO_DILATE_OFFSETS {
                        glyph_halo_inst.push(GlyphInstance {
                            rect: [rect[0] + dx as f32, rect[1] + dy as f32, rect[2], rect[3]],
                            uv,
                            color: [halo_rgb[0], halo_rgb[1], halo_rgb[2], a],
                            bg: [0, 0, 0, 0],
                        });
                    }
                }
                glyph_inst.push(GlyphInstance {
                    rect,
                    uv,
                    color: glyph_color,
                    bg: bg_under,
                });
                // W4: the cut-out slice — the quad clipped to the cursor rect,
                // "cut out" in the CURSOR cell's bg over the cursor fill (the
                // W2 remap operand), drawn AFTER the fill. The slice's UV shift
                // is integer under NEAREST, so its texels equal the fg quad's.
                if in_cutout
                    && let Some((rect, uv)) = aterm_render::glyph_quad(
                        (pad + c * rcw) as f32,
                        anchor_y,
                        baseline,
                        cut_scale,
                        slot.ax,
                        slot.ay,
                        slot.gw,
                        slot.gh,
                        slot.xmin,
                        slot.ymin,
                        aw,
                        ah,
                    )
                {
                    cursor_glyph_inst.push(GlyphInstance {
                        rect,
                        uv,
                        color: cutout_color,
                        bg: rgb4_u32(cursor_fill),
                    });
                }
            }
        }
        // Last reader of `row_plans` (the glyph loop) is done; restore the scratch
        // so its inner-Vec capacities persist into the next frame.
        self.row_plans = row_plans;
        // Combining diacritics: overlay each mark's glyph on its base cell, in
        // the cell foreground — appended AFTER the bases so they draw on top,
        // matching the CPU's mark-after-base blit order.
        for (r, cells) in rendered.iter().enumerate().take(vis_rows) {
            if !row_active(r) || input.combining[r].is_empty() {
                continue;
            }
            let line_size = input.line_sizes[r];
            let rcw = aterm_render::row_cell_w(line_size, cw);
            // Inset the row origin (`grid_top + r * ch`) to MATCH the base-glyph
            // loop (line ~2328) and the CPU path. Without the inset the GPU
            // rendered decomposed combining marks `grid_top` px too high — a
            // CPU/GPU divergence for NFD sequences (e.g. base + U+0301). The mark
            // x is already padded below, so this aligns the y onto the identical
            // pixel.
            let (scale, anchor_y) =
                aterm_render::row_scale(line_size, grid_top + r * ch, ch, r + 1 == input.rows);
            let sel_row = r as i32 - display_offset;
            // Fresh lockstep ink + char_fg walks for this loop's own column
            // scan: combining marks FOLLOW ink — and the EMBERFORGE char_fg
            // recolour (ink wins when both govern a cell) — their base_fg is
            // fed through the shared floors below, identical to the CPU
            // combining blit.
            let mut ink_walk = aterm_render::InkWalk::new(
                u16::try_from(r)
                    .map_or(&[][..], |row| aterm_render::ink_row_slice(&input.ink, row)),
            );
            let mut char_fg_walk =
                aterm_render::CharFgWalk::new(u16::try_from(r).map_or(&[][..], |row| {
                    aterm_render::char_fg_row_slice(&input.char_fg, row)
                }));
            for (c, cell) in cells.iter().take(cols).enumerate() {
                if clip_cols && pad + c * rcw >= w as usize + rcw {
                    break; // off the right edge of the clamped framebuffer — see vis_rows
                }
                // Image-covered cells skip their combining overlay too (the CPU
                // overlays marks only inside the non-image glyph path).
                if cell.wide || input.cluster_at(r, c).is_some() || input.image_at(r, c).is_some() {
                    continue;
                }
                let Some(marks) = input.combining_at(r, c) else {
                    continue;
                };
                // W2: the fill under this cell (selection band or cell bg) for
                // the remap — same choice as the base-glyph loop. (A mark can
                // overlap its base's ink; the CPU remaps against the true dst
                // there, which stays inside the AA parity tolerance.)
                let base_fg = match ink_walk.at(c as u16) {
                    Some(ink) => aterm_render::rgb_to_u32(ink),
                    None => char_fg_walk
                        .at(c as u16)
                        .unwrap_or_else(|| aterm_render::rgb_to_u32(cell.fg)),
                };
                let cell_selected = selection.contains_cell(
                    sel_row,
                    c as u16,
                    cells.get(c + 1).is_some_and(|n| n.wide),
                    cell.wide,
                );
                let bg_under = if cell_selected {
                    rgb4_u32(theme_selection)
                } else {
                    rgb4(cell.bg)
                };
                // W5b: marks paint in the SAME effective fg as their base glyph —
                // the selection/min-contrast floors applied to the INK/CHAR_FG-
                // substituted base_fg, identical to the CPU combining blit (which
                // paints marks in the base's floored effective fg).
                let mark_fg = rgb4_u32(aterm_render::effective_glyph_fg(
                    selection_fg,
                    min_contrast,
                    base_fg,
                    aterm_render::rgb_to_u32(cell.bg),
                    cell_selected,
                    theme_selection,
                ));
                for &m in marks {
                    let key = self.cpu.glyph_key(m);
                    let Some(slot) = atlas.map.get(&key) else {
                        continue;
                    };
                    if slot.gw == 0 || slot.gh == 0 {
                        continue;
                    }
                    // Centre the mark's ink in the cell (see CPU `mark_cell_x`):
                    // identical integer arithmetic → identical pixel position.
                    let cx = aterm_render::mark_cell_x(c, rcw, slot.gw as usize, slot.xmin, scale)
                        + pad as i32;
                    let Some((rect, uv)) = aterm_render::glyph_quad(
                        cx as f32, anchor_y, baseline, scale, slot.ax, slot.ay, slot.gw, slot.gh,
                        slot.xmin, slot.ymin, aw, ah,
                    ) else {
                        continue;
                    };
                    glyph_inst.push(GlyphInstance {
                        rect,
                        uv,
                        color: mark_fg,
                        bg: bg_under,
                    });
                }
            }
        }
        // Inline images (iTerm2 OSC 1337 `File=`): one quad per image-covered
        // cell, sampling that cell's tile of the per-frame image texture. Built
        // ONLY when `image_plane` was populated this frame (an image-free frame
        // leaves the stream empty → zero image draws, byte-identical text path).
        // Geometry MIRRORS the CPU `render_one_row` Pass 1b EXACTLY: the tile's
        // dest box is `cell_w × cell_h` (the image is NEVER DEC-scaled — the CPU
        // `blit_image_cell` blits a natural-size tile), but it is positioned at the
        // ROW cell advance `c * rcw` (so on a DECDWL/DECDHL row, tiles are spaced
        // by `2*cell_w` with bg gaps between them, just like the CPU). The UV is
        // the cell's `(cell_col, cell_row)` tile inside the image's footprint
        // region of the stacked texture — NEAREST-sampled, so each texel maps to
        // the CPU's 1:1 per-cell copy (the parity gate).
        if let Some(plane) = win.image_plane.as_ref() {
            let image_inst = &mut self.inst.image;
            let (pw, ph) = (plane.w as f32, plane.h as f32);
            for (r, row_images) in input.images.iter().enumerate().take(vis_rows) {
                if !row_active(r) || row_images.is_empty() {
                    continue;
                }
                // Row cell advance, doubled on a DEC double-size line — exactly the
                // `cw` the CPU Pass 1b passes as the per-cell x stride.
                let rcw = aterm_render::row_cell_w(input.line_sizes[r], cw);
                // Inset the image tile's dest origin by `(pad, grid_top)`, exactly
                // like the CPU `blit_image_cell` call site (which passes `pad_x +
                // c*cw`, `y0 = grid_top + r*cell_h`). Without this the inline image
                // would draw at the unpadded grid origin while glyphs/bg shifted —
                // breaking both the CPU/GPU parity and the image-vs-window parity.
                // `pad == head == 0` keeps the historical origin (byte-identical).
                let y0 = (grid_top + r * ch) as f32;
                for (c, image) in row_images {
                    if *c >= cols {
                        continue;
                    }
                    // Off the right edge of the clamped framebuffer → no visible pixels.
                    // (`row_images` isn't column-sorted, so `continue`, not `break`.)
                    if clip_cols && pad + *c * rcw >= w as usize + rcw {
                        continue;
                    }
                    let fp_w = image.image.cols as usize * cw;
                    let fp_h = image.image.rows as usize * ch;
                    let key = (std::sync::Arc::as_ptr(&image.image) as usize, fp_w, fp_h);
                    let Some(&(img_y0, dw, dh)) = plane.placements.get(&key) else {
                        // Failed decode (no texture rows): nothing to draw, the
                        // cell bg shows through — exactly the CPU negative-cache.
                        continue;
                    };
                    // Source tile origin within the footprint (CPU `blit_image_cell`
                    // uses `cell_col*cw`/`cell_row*ch`), offset by the image's row in
                    // the stacked texture. Clamp the tile to the footprint so a cell
                    // at the image edge never samples a neighbour's region.
                    let sx0 = (image.cell_col as usize * cw) as u32;
                    let sy0 = (image.cell_row as usize * ch) as u32;
                    if sx0 >= dw || sy0 >= dh {
                        continue;
                    }
                    let tile_w = (cw as u32).min(dw - sx0);
                    let tile_h = (ch as u32).min(dh - sy0);
                    let x0 = (pad + *c * rcw) as f32;
                    let rect = [x0, y0, tile_w as f32, tile_h as f32];
                    let uv = [
                        sx0 as f32 / pw,
                        (img_y0 + sy0) as f32 / ph,
                        tile_w as f32 / pw,
                        tile_h as f32 / ph,
                    ];
                    image_inst.push(GlyphInstance {
                        rect,
                        uv,
                        color: [0, 0, 0, 0],
                        // Image tiles carry their own colour; no remap, bg unused.
                        bg: [0, 0, 0, 0],
                    });
                }
            }
        }
        // Block-cursor fill — drawn AFTER the glyph/decoration passes (not in the
        // bg pass) so it covers any neighbour glyph overflow into the cursor
        // cell, exactly as the CPU paints the block cursor last. The in-rect
        // glyph slice is then re-drawn over it (cut-out) from
        // cursor_glyph/color_inst. W4: the fill is `cur_block_w` wide — BOTH
        // cells of a wide lead — matching the CPU's widened `cursor_rects`.
        // (Pushed into the cleared persistent `cursor_block` stream: an empty
        // stream == the old `Vec::new()`, a single push == the old one-elem vec.)
        // (`row_active(cr)` is always true here in the scissored path — the cursor
        // row is in the dirty set whenever the cursor is shown — but guard it
        // explicitly so no cursor instance can ever leak outside the dirty band.)
        if block_cursor && row_active(cr) {
            // The (possibly contrast-floored, W5b) cursor fill, carrying the
            // cursor-opacity coverage in alpha: at 255 the opaque fill + cut-out is
            // byte-identical; below 255 the stream draws through the ALPHA_BLENDING
            // `cursor_blend_pipeline` (see emit_cursor), blending over the cell.
            let mut color = rgb4_u32(cursor_fill);
            color[3] = cursor_cov;
            self.inst.cursor_block.push(BgInstance {
                rect: [
                    sat_pos_u16(pad + cc * cur_cw),
                    sat_pos_u16(grid_top + cr * ch),
                    cur_block_w as u16,
                    ch as u16,
                ],
                color,
            });
        }

        // Line decorations (underline / strikethrough / overline) OVER the
        // glyphs — same rects as the CPU Pass 3 (`aterm_render::underline_rects`
        // / `strike_overline_rects`), drawn as opaque quads in a pass after the
        // glyphs so CPU and GPU produce identical pixels. W7: bands come from
        // the shared resolved metrics; descender ink-skip subtracts the same
        // kept spans the CPU computes (ONE method on the wrapped CPU face);
        // curly cells emit AA undercurl quads into the `curl` stream, drawn
        // AFTER all solid deco quads — the CPU mirrors that order per row
        // (pass 3 then 3b), and in-cell band clamping makes the orders
        // equivalent across rows.
        let dm = self.cpu.deco_metrics();
        let curly_mask_ok = aterm_render::undercurl_supported(cw, ch) && self.deco_atlas.is_some();
        let curl_atlas_w = self.deco_atlas.as_ref().map_or(0, |d| d.atlas_w);
        // One reusable rect scratch fed to the `_into` variants instead of a fresh
        // Vec per decorated cell — same rects, no per-cell allocation. Reused across
        // both the underline and strike/overline calls (each `*_into` clears first).
        // Taken from the persistent renderer-owned scratch (mem::take, like
        // `row_plans`/`dirty_scratch`) so a decorated frame allocates no per-frame
        // Vec; restored right after the loop. Byte-identical rects and draw order.
        let mut deco_rects = std::mem::take(&mut self.deco_rects_scratch);
        let mut skip_rects = std::mem::take(&mut self.deco_skip_scratch);
        let mut curl_ink = std::mem::take(&mut self.curl_ink_scratch);
        let mut curl_spans = std::mem::take(&mut self.curl_spans_scratch);
        for (r, cells) in rendered.iter().enumerate().take(vis_rows) {
            if !row_active(r) {
                continue;
            }
            let y0 = grid_top + r * ch;
            let rcw = aterm_render::row_cell_w(input.line_sizes[r], cw);
            // Fresh lockstep ink + char_fg walks for this loop's own column
            // scan: line decorations follow cell semantics, so they follow ink
            // — and the EMBERFORGE char_fg recolour (ink wins when both govern
            // a cell) — except an explicit SGR 58 underline colour, which wins
            // (matches the CPU).
            let mut ink_walk = aterm_render::InkWalk::new(
                u16::try_from(r)
                    .map_or(&[][..], |row| aterm_render::ink_row_slice(&input.ink, row)),
            );
            let mut char_fg_walk =
                aterm_render::CharFgWalk::new(u16::try_from(r).map_or(&[][..], |row| {
                    aterm_render::char_fg_row_slice(&input.char_fg, row)
                }));
            for (c, cell) in cells.iter().take(cols).enumerate() {
                if clip_cols && pad + c * rcw >= w as usize + rcw {
                    break; // off the right edge of the clamped framebuffer — see vis_rows
                }
                if cell.wide
                    || (matches!(cell.underline, UnderlineStyle::None)
                        && !cell.strikethrough
                        && !cell.overline)
                {
                    continue;
                }
                let x = pad + c * rcw;
                let is_wide_lead = cells.get(c + 1).is_some_and(|n| n.wide);
                let dw = if is_wide_lead { 2 * rcw } else { rcw };
                // W5b: decorations route through the SAME floors as the glyph
                // fg via the shared `effective_deco_color`/`effective_glyph_fg`,
                // fed the INK/CHAR_FG-substituted base_fg (Sparkle Words v2 /
                // EMBERFORGE) — identical to the CPU Pass 3, so the quads stay
                // parity (SGR 58 still wins).
                let base_fg = match ink_walk.at(c as u16) {
                    Some(ink) => aterm_render::rgb_to_u32(ink),
                    None => char_fg_walk
                        .at(c as u16)
                        .unwrap_or_else(|| aterm_render::rgb_to_u32(cell.fg)),
                };
                let cell_selected = selection.contains_cell(
                    r as i32 - display_offset,
                    c as u16,
                    is_wide_lead,
                    cell.wide,
                );
                let ucolor = rgb4_u32(aterm_render::effective_deco_color(
                    selection_fg,
                    min_contrast,
                    cell.underline_color.map(aterm_render::rgb_to_u32),
                    base_fg,
                    aterm_render::rgb_to_u32(cell.bg),
                    cell_selected,
                    theme_selection,
                ));
                let is_curly = matches!(cell.underline, UnderlineStyle::Curly);
                aterm_render::underline_rects_into(
                    &mut deco_rects,
                    cell.underline,
                    x,
                    y0,
                    dw,
                    rcw,
                    ch,
                    dm,
                    curly_mask_ok,
                );
                // Descender ink-skip (W7): the SAME kept spans the CPU pass 3
                // computes — one shared method on the wrapped CPU face, so the
                // two renderers can never disagree on a skip. Needed when this
                // cell emitted underline rects OR draws the undercurl mask.
                let plan_entry = self
                    .row_plans
                    .get(r)
                    .and_then(|p| p.get(c))
                    .copied()
                    .unwrap_or(aterm_render::ColumnGlyph::PerCell);
                let want_spans = !deco_rects.is_empty() || (is_curly && curly_mask_ok);
                let skip = want_spans
                    && self.cpu.underline_keep_spans_into(
                        input,
                        r,
                        c,
                        plan_entry,
                        x,
                        dw,
                        &mut curl_ink,
                        &mut curl_spans,
                    );
                if skip && !deco_rects.is_empty() {
                    skip_rects.clear();
                    for &rect in &deco_rects {
                        aterm_render::deco::intersect_rect_spans(
                            &mut skip_rects,
                            rect,
                            &curl_spans,
                        );
                    }
                    std::mem::swap(&mut deco_rects, &mut skip_rects);
                }
                for &[rx, ry, rw, rh] in &deco_rects {
                    self.inst.deco.push(BgInstance {
                        rect: [sat_pos_u16(rx), sat_pos_u16(ry), rw as u16, rh as u16],
                        color: ucolor,
                    });
                }
                // AA undercurl quads: one per kept-span × cell tile (a wide
                // lead tiles the wave twice), sampling the shared atlas sprite
                // 1:1 on single-width rows and NEAREST-stretched on DEC rows —
                // exactly the sparkle-word sampling (proven byte-parity).
                if is_curly && curly_mask_ok {
                    if !skip {
                        curl_spans.clear();
                        curl_spans.push((x, dw));
                    }
                    let curl_x0 = aterm_render::UNDERCURL_SPRITE * cw;
                    for ti in 0..dw / rcw.max(1) {
                        let tx0 = x + ti * rcw;
                        for &(sx, sw) in &curl_spans {
                            let lo = sx.max(tx0);
                            let hi = (sx + sw).min(tx0 + rcw);
                            if lo >= hi {
                                continue;
                            }
                            let scale = cw as f32 / rcw as f32;
                            let u0 = curl_x0 as f32 + (lo - tx0) as f32 * scale;
                            let uw = (hi - lo) as f32 * scale;
                            self.inst.curl.push(GlyphInstance {
                                rect: [lo as f32, y0 as f32, (hi - lo) as f32, ch as f32],
                                uv: [u0 / curl_atlas_w as f32, 0.0, uw / curl_atlas_w as f32, 1.0],
                                color: ucolor,
                                // fs_deco_over ignores bg (no remap).
                                bg: [0, 0, 0, 0],
                            });
                        }
                    }
                }
                // Strike/overline follow the SAME shared floor as the glyph fg
                // (W5b), fed the ink/char_fg-substituted base_fg — parity with
                // the CPU.
                let fgc = rgb4_u32(aterm_render::effective_glyph_fg(
                    selection_fg,
                    min_contrast,
                    base_fg,
                    aterm_render::rgb_to_u32(cell.bg),
                    cell_selected,
                    theme_selection,
                ));
                aterm_render::strike_overline_rects_into(
                    &mut deco_rects,
                    cell.strikethrough,
                    cell.overline,
                    x,
                    y0,
                    dw,
                    ch,
                    dm,
                );
                for &[rx, ry, rw, rh] in &deco_rects {
                    self.inst.deco.push(BgInstance {
                        rect: [sat_pos_u16(rx), sat_pos_u16(ry), rw as u16, rh as u16],
                        color: fgc,
                    });
                }
            }
        }
        // Restore the decoration scratches (capacity retained across frames).
        self.deco_rects_scratch = deco_rects;
        self.deco_skip_scratch = skip_rects;
        self.curl_ink_scratch = curl_ink;
        self.curl_spans_scratch = curl_spans;
        // Cursor MOTION TRAIL (the comet EMBER BED): solid quads, one per swept
        // cell, pre-blended over that cell's own background with the SHARED
        // `aterm_render::blend_rgb` so the quad colour is byte-identical to the
        // CPU `draw_trail` fill; emitted through the bg pipeline (REPLACE) like
        // every other solid quad, drawn at the END of the base-bg half — UNDER
        // the Unorm interpose light and UNDER the glyph ink (== the CPU phase
        // B2b), so swept text stays readable. Row-gated by `row_active` to
        // match the CPU's dirty-row repaint + the scissor band
        // (`compute_dirty_rows` marks every prev/cur trail row dirty on a
        // change). Filled by the comet style natively (`trail_is_comet`) and by
        // the web pipeline; the CPU/GPU parity suite covers a populated trail.
        if !input.cursor_trail.is_empty() {
            let trail_color = input.cursor_trail_color;
            for t in &input.cursor_trail {
                if t.row >= rows || t.col >= cols || !row_active(t.row) {
                    continue;
                }
                let rcw = aterm_render::row_cell_w(input.line_sizes[t.row], cw);
                // Grid-interior bound (the CPU rule, verbatim): on a DEC
                // double-width row a stale mid-fade col past `cols/2` would
                // land the bed in the window pad band — skip it. Single-width
                // rows reduce to the `col < cols` check above (identity).
                if (t.col + 1) * rcw > cols * cw {
                    continue;
                }
                let cell_bg = rendered
                    .get(t.row)
                    .and_then(|r| r.get(t.col))
                    .map_or(self.theme.bg, |c| aterm_render::rgb_to_u32(c.bg));
                let color = aterm_render::blend_rgb(cell_bg, trail_color, t.alpha);
                let mut c4 = rgb4_u32(color);
                // Vibrancy: a bed cell over the frame's DEFAULT bg carries the
                // bg quad's alpha (the per-cell bg rule, verbatim), so a
                // translucent window's comet sweep never punches opaque holes
                // in the glass; SGR-coloured cells stay opaque. Byte-identical
                // at opacity 1.0 (`bg_alpha == 255`), matching the CPU's
                // transmittance carry in `draw_trail`.
                if bg_alpha != 255 && cell_bg == frame_bg {
                    c4[3] = bg_alpha;
                }
                self.inst.trail.push(BgInstance {
                    rect: [
                        // Saturate the whole `pad + col·rcw` offset in one step: the old
                        // inner `(t.col * rcw) as u16` WRAPPED before the saturating_add.
                        sat_pos_u16(pad + t.col * rcw),
                        sat_pos_u16(grid_top + t.row * ch),
                        rcw as u16,
                        ch as u16,
                    ],
                    color: c4,
                });
            }
        }
        // EMBERFORGE UNDER-GLYPH light (`glow_under`): the flame BODY — the
        // exact aurora emission (host premultiplied `q.color`; single-row
        // WINDOW-ABSOLUTE quads, converted at the producer; row-gated by
        // `row_active` so additive light is never re-added onto a
        // Load-preserved row) through the SAME `glow_add_pipeline`, but drawn
        // in its OWN Unorm pass BETWEEN the base pass's bg/sprite draws and
        // its glyph draws — matching the CPU phase-B3 `draw_glow_under` slot
        // (under the glyph ink). Deliberately NOT fed into the bloom pass —
        // bloom stays `glow_add`-only.
        if !input.glow_under.is_empty() {
            for q in &input.glow_under {
                if (q.row as usize) >= rows || !row_active(q.row as usize) {
                    continue;
                }
                self.inst.glow_under.push(BgInstance {
                    rect: [q.x, q.y, q.w, q.h],
                    color: rgb4_u32(q.color),
                });
            }
        }

        // EMBERFORGE per-pixel FIRE FIELD patches (`fire_patch`): the flame
        // BODY at full art scale — every covered fragment recomputes the
        // shared pure-integer field (`fs_fire_add`/`fs_fire_over`, the WGSL
        // twin of `aterm_render::fire_field`), so the instance carries the
        // EXACT field operands the CPU `draw_fire_patch` uses: the
        // WINDOW-ABSOLUTE rect and root (converted at the producer — flames
        // may rise into the head band), and the raw patch parameters. Same A2
        // slot as `glow_under` (under the glyph ink), right after it; split
        // per mode (Add then Over — the CPU sweep order); row-gated by
        // `row_active`.
        if !input.fire_patch.is_empty() {
            let pad16 = pad as u16;
            for q in &input.fire_patch {
                if (q.row as usize) >= rows || !row_active(q.row as usize) {
                    continue;
                }
                let inst = FireInstance {
                    rect: [q.x, q.y, q.w, q.h],
                    // geo.w STAYS pad16 — the top-edge fade stays anchored at
                    // the top pad strip, mirroring the CPU `top_fade_y` in
                    // draw_fire_patch (the byte-identity + parity anchor).
                    geo: [q.base_y, q.peak_h, q.cell_h, pad16],
                    phase: q.phase,
                    tsl: [q.temp, q.strength, q.cov_cap, q.lean as u8],
                };
                match q.mode {
                    aterm_render::FireMode::Add => self.inst.fire_add.push(inst),
                    aterm_render::FireMode::Over => self.inst.fire_over.push(inst),
                }
            }
        }

        // LUMEN aurora: PREMULTIPLIED ADDITIVE light quads (comet/bloom/ring/sparks).
        // The host already premultiplied `q.color`, so we emit it straight (rgb4_u32's
        // a=255 is irrelevant under One/One + COLOR write-mask) and the glow_add
        // pipeline adds it onto the dest — byte-identical to the CPU `add_sat`. Quads
        // are WINDOW-ABSOLUTE pixels (converted at the producer) tagged with a
        // single-row damage hint; row-gated by `row_active` so they stay inside
        // the scissor's dirty band.
        if !input.cursor_glow_add.is_empty() {
            for q in &input.cursor_glow_add {
                if (q.row as usize) >= rows || !row_active(q.row as usize) {
                    continue;
                }
                self.inst.glow_add.push(BgInstance {
                    rect: [q.x, q.y, q.w, q.h],
                    color: rgb4_u32(q.color),
                });
            }
        }

        // GLOW-HALO cursor-effect radial light (`glow_halo`): the aurora's
        // RADIAL sibling — the exact `rain_add` emission (RainGlowInstance
        // through the shared `rain_glow_pipeline`; host premultiplied peak
        // `q.color`; single-row WINDOW-ABSOLUTE quads, converted at the
        // producer; row-gated by `row_active` so additive light is never
        // re-added onto a Load-preserved row). Drawn right AFTER the aurora
        // and BEFORE the nova/rain, matching the CPU's
        // `draw_glow_halo`-after-`draw_glow` order. Deliberately NOT fed
        // into the bloom pass — the radial falloff is self-soft, so bloom
        // stays `glow_add`-only for now. The stream splits PER MODE: Add quads
        // keep the One/One instance vec; `HaloMode::Over` veils go to their
        // own vec drawn right after it (the CPU's Add-then-Over sweep).
        if !input.glow_halo.is_empty() {
            for q in &input.glow_halo {
                if (q.row as usize) >= rows || !row_active(q.row as usize) {
                    continue;
                }
                let inst = RainGlowInstance {
                    rect: [q.x, q.y, q.w, q.h],
                    color: rgb4_u32(q.color),
                    // Falloff basis in WINDOW pixels (window-absolute centre),
                    // the exact operands the CPU `draw_radial_add` uses.
                    falloff: [q.cx, q.cy, q.rx, q.ry],
                };
                match q.mode {
                    aterm_render::HaloMode::Add => self.inst.glow_halo.push(inst),
                    aterm_render::HaloMode::Over => {
                        self.inst.glow_halo_over.push(with_over_cap(inst, q.color));
                    }
                }
            }
        }

        // SUPERNOVA additive light (Sparkle Words v2): the second premultiplied
        // One/One stream — the EXACT same emission as the aurora above (host
        // premultiplied `q.color`; single-row GRID-INTERIOR quads, offset here;
        // row-gated by `row_active` so the scissored band covers them). Drawn
        // right after the glow through the same
        // `glow_add_pipeline`, matching the CPU's `draw_nova`-after-`draw_glow`
        // order (saturating add commutes, but keeping the order pinned keeps
        // the two backends trivially auditable).
        if !input.nova_add.is_empty() {
            // GRID stream (unlike the window-absolute cursor streams above): the
            // word_decorations nova producer emits grid-relative pixels, so the
            // grid offsets are added HERE — mirroring the CPU draw_nova.
            let pad16 = pad as u16;
            let grid_top16 = grid_top as u16;
            for q in &input.nova_add {
                if (q.row as usize) >= rows || !row_active(q.row as usize) {
                    continue;
                }
                self.inst.nova_add.push(BgInstance {
                    rect: [
                        pad16.saturating_add(q.x),
                        grid_top16.saturating_add(q.y),
                        q.w,
                        q.h,
                    ],
                    color: rgb4_u32(q.color),
                });
            }
        }

        // PHOSPHOR rain bright-head halos (`rain_add`): the third premultiplied
        // One/One stream — the exact nova emission (host premultiplied
        // `q.color`; single-row grid-interior quads; row-gated by `row_active`
        // so additive light is never re-added onto a Load-preserved row).
        // Drawn right after the nova through the same `glow_add_pipeline`,
        // matching the CPU's rain-after-nova additive order. Split PER MODE
        // exactly like `glow_halo` above (the two RainHalo-shaped streams
        // share the CPU rasterizer, so they must share the GPU split too).
        if !input.rain_add.is_empty() {
            // Rain is a GRID stream (it falls over the grid interior): x keeps
            // the `pad` inset, y moves with the grid to `grid_top`.
            let pad16 = pad as u16;
            let grid_top16 = grid_top as u16;
            for q in &input.rain_add {
                if (q.row as usize) >= rows || !row_active(q.row as usize) {
                    continue;
                }
                let inst = RainGlowInstance {
                    rect: [
                        pad16.saturating_add(q.x),
                        grid_top16.saturating_add(q.y),
                        q.w,
                        q.h,
                    ],
                    color: rgb4_u32(q.color),
                    // Falloff basis in WINDOW pixels (grid-interior-offset
                    // centre), the exact operands the CPU `draw_rain_add` uses.
                    falloff: [
                        pad16.saturating_add(q.cx),
                        grid_top16.saturating_add(q.cy),
                        q.rx,
                        q.ry,
                    ],
                };
                match q.mode {
                    aterm_render::HaloMode::Add => self.inst.rain_add.push(inst),
                    aterm_render::HaloMode::Over => {
                        self.inst.rain_add_over.push(with_over_cap(inst, q.color));
                    }
                }
            }
        }

        // Sparkle-word decorations: one textured quad per decoration, sampling its
        // sprite from the deco atlas, with the decoration's colour + alpha. Row-
        // gated by `row_active`, drawn AFTER the aurora and UNDER the cursor — the
        // GPU twin of the CPU `draw_decorations` (Over → paw, Add → sparkle). The
        // dest rect + uv match the CPU mask 1:1 on single-width rows (byte parity).
        if !input.word_decorations.is_empty()
            && let Some(datlas_w) = self.deco_atlas.as_ref().map(|d| d.atlas_w)
        {
            for d in &input.word_decorations {
                let (row, col) = (d.row as usize, d.col as usize);
                if row >= rows || col >= cols || !row_active(row) {
                    continue;
                }
                // Freeze the additive sparkle over selected cells — identical
                // per-cell predicate to the CPU `draw_decorations`, so parity holds.
                if matches!(d.blend, aterm_render::DecoBlend::Add)
                    && input.selection.has_selection()
                {
                    let row_cells = &input.cells[row];
                    let is_wide_lead = row_cells.get(col + 1).is_some_and(|n| n.wide);
                    let cell_wide = row_cells.get(col).is_some_and(|n| n.wide);
                    if input.selection.contains_cell(
                        row as i32 - input.display_offset,
                        col as u16,
                        is_wide_lead,
                        cell_wide,
                    ) {
                        continue;
                    }
                }
                let rcw = aterm_render::row_cell_w(input.line_sizes[row], cw);
                let idx = deco_sprite_index(d.glyph);
                let rect = [
                    pad as f32 + (col * rcw) as f32 + f32::from(d.dx),
                    grid_top as f32 + (row * ch) as f32 + f32::from(d.dy),
                    rcw as f32,
                    ch as f32,
                ];
                let uv = [
                    (idx * cw) as f32 / datlas_w as f32,
                    0.0,
                    cw as f32 / datlas_w as f32,
                    1.0,
                ];
                let color = [
                    (d.color >> 16) as u8,
                    (d.color >> 8) as u8,
                    d.color as u8,
                    d.alpha,
                ];
                // Sparkle decorations blend un-remapped (fs_deco_*): bg unused.
                let gi = GlyphInstance {
                    rect,
                    uv,
                    color,
                    bg: [0, 0, 0, 0],
                };
                match d.blend {
                    aterm_render::DecoBlend::Over => self.inst.wdeco_over.push(gi),
                    aterm_render::DecoBlend::Add => self.inst.wdeco_add.push(gi),
                }
            }
        }

        // Underline/bar/hollow cursors paint OVER the glyph (the CPU fills
        // them after its glyph blits), so their quads form a third pass that
        // runs after the glyph pass. Same rects as the CPU: `cursor_rects`.
        // (Extends the cleared persistent `cursor` stream — identical contents
        // in identical order to the old `.collect()`.)
        if cursor_drawn && !block_cursor && row_active(cr) {
            // Same (possibly floored, W5b) fill as the block path, carrying the
            // cursor-opacity coverage in alpha (translucent draws through the
            // ALPHA_BLENDING pipeline) — see above.
            let cursor_color = {
                let mut c = rgb4_u32(cursor_fill);
                c[3] = cursor_cov;
                c
            };
            self.inst.cursor.extend(
                aterm_render::cursor_rects(
                    style,
                    pad + cc * cur_cw,
                    grid_top + cr * ch,
                    cur_cw,
                    ch,
                )
                .into_iter()
                .map(|[x, y, rw, rh]| BgInstance {
                    rect: [sat_pos_u16(x), sat_pos_u16(y), rw as u16, rh as u16],
                    color: cursor_color,
                }),
            );
        }

        // Upload uniforms. `Uniforms` is a pure function of the frame size + the
        // W2 text-blend mode, so it is rewritten ONLY when it differs from what the
        // SHARED buffer currently holds — not every frame (the steady-state present
        // otherwise re-uploaded an unchanged 16-byte buffer each frame). The memo is
        // renderer-level (keyed to the buffer, NOT the window): differently sized
        // windows interleaving through the one buffer each rewrite it here before
        // their submit. W2: the key includes the mode (read off the CPU face, the
        // single source of truth), so a mode flip re-uploads and both backends remap
        // coverage identically. Byte-identical to the old write for the same key.
        let text_blend =
            u32::from(self.cpu.text_blending() == aterm_render::TextBlending::LinearCorrected);
        if self.uniform_written != Some((w, h, text_blend)) {
            self.ctx.queue.write_buffer(
                &self.uniform_buf,
                0,
                bytemuck::bytes_of(&Uniforms {
                    screen: [w as f32, h as f32],
                    text_blend: text_blend as f32,
                    _pad: 0.0,
                }),
            );
            self.uniform_written = Some((w, h, text_blend));
        }
        // Per-frame vertex streams now reuse persistent buffers (grow-only), so
        // there is no per-frame allocation in the common case — only a
        // `write_buffer` copy. `upload` returns `None` for an empty stream,
        // exactly like the old `Option<Buffer>` gating: an EMPTY `bg` stream (e.g.
        // a degenerate/zero-cell frame) draws nothing, and the bg pass still
        // CLEARS the target (LoadOp::Clear) — matching the CPU's all-background
        // frame. We also slice each buffer to EXACTLY this frame's byte length so
        // stale tail bytes from a larger previous frame are never bound or drawn.
        // SCENE ("Living Panels") + CAT: build this frame's sprite instances (SpriteQuad →
        // GlyphInstance) for the already-uploaded atlases. UVs normalize against each
        // atlas's texel dims; `flip_x` mirrors the source-u window. Empty when no scene/cat.
        fn build_scene(
            dst: &mut Vec<GlyphInstance>,
            quads: &[aterm_core::render::SpriteQuad],
            saw: f32,
            sah: f32,
            padf: f32,
            grid_topf: f32,
            keep: impl Fn(u16) -> bool,
        ) {
            for q in quads {
                if q.w == 0 || q.h == 0 || q.aw == 0 || q.ah == 0 || !keep(q.row) {
                    continue;
                }
                let (mut u0, mut du) = (q.ax as f32 / saw, q.aw as f32 / saw);
                if q.flip_x {
                    u0 = (q.ax as f32 + q.aw as f32) / saw;
                    du = -(q.aw as f32) / saw;
                }
                dst.push(GlyphInstance {
                    rect: [
                        padf + q.x as f32,
                        grid_topf + q.y as f32,
                        q.w as f32,
                        q.h as f32,
                    ],
                    uv: [u0, q.ay as f32 / sah, du, q.ah as f32 / sah],
                    color: [
                        ((q.tint >> 16) & 0xff) as u8,
                        ((q.tint >> 8) & 0xff) as u8,
                        (q.tint & 0xff) as u8,
                        q.alpha,
                    ],
                    // W2: scene sprites blend un-remapped (fs_scene_*): bg unused.
                    bg: [0, 0, 0, 0],
                });
            }
        }
        let padf = pad as f32;
        let grid_topf = grid_top as f32;
        if let Some((saw, sah)) = self.scene_atlas.as_ref().map(|s| (s.w as f32, s.h as f32)) {
            build_scene(
                &mut self.inst.scene_over,
                &input.scene_over,
                saw,
                sah,
                padf,
                grid_topf,
                |_| true,
            );
            build_scene(
                &mut self.inst.scene_add,
                &input.scene_add,
                saw,
                sah,
                padf,
                grid_topf,
                |_| true,
            );
        }
        // PHOSPHOR rain sprites (`rain_quads`): the same SpriteQuad → GlyphInstance
        // build against the RAIN atlas dims (NEAREST, 1:1 — the cat regime).
        // ROW-FILTERED by `row_active` (round-3 audit): every rain quad is
        // single-row-band, and `compute_dirty_rows`' scissor-band fill marks
        // every band row a rain quad overlaps as dirty — so a quad the scissor
        // admits ALWAYS has `row_active(q.row)` true (kept), and a quad on an
        // inactive row is entirely scissor-clipped (dropping it is pixel-
        // identical). Under `RepaintScope::Full` the filter passes everything.
        // A sparse-damage frame during a 2048-quad downpour thus builds and
        // uploads only the dirty rows' instances instead of the whole field.
        if let Some((raw, rah)) = self.rain_atlas.as_ref().map(|s| (s.w as f32, s.h as f32)) {
            build_scene(
                &mut self.inst.rain_under,
                &input.rain_quads,
                raw,
                rah,
                padf,
                grid_topf,
                |r| row_active(usize::from(r)),
            );
        }
        // Peeking-CAT sprites (Sparkle Words v2): the same SpriteQuad → GlyphInstance
        // build against the CAT atlas dims. The instances draw through the shared
        // src-over scene pipeline but bind the CAT atlas group, whose sampler is
        // NEAREST (bake == dest size, 1:1 — no filtering on either backend).
        if let Some((caw, cah)) = self.cat_atlas.as_ref().map(|s| (s.w as f32, s.h as f32)) {
            build_scene(
                &mut self.inst.cat_over,
                &input.cat_quads,
                caw,
                cah,
                padf,
                grid_topf,
                |_| true,
            );
        }
        // FREE-floating sprites (arbitrary pixel rects, SIGNED i32 origin): the same
        // GlyphInstance build against the FREE atlas dims, partitioned by `z` into the
        // under-text / over-text streams. Built UNCONDITIONALLY (not row_active-
        // filtered), like cats — the dirty-band scissor clips them, and
        // `compute_dirty_rows` guarantees every sprite band INSIDE that scissor is in
        // the dirty set (the row-union marks a CHANGED sprite's full prev∪cur
        // Y-extent; the scissor-band fill marks a SETTLED sprite's rows whenever
        // unrelated dirt puts them inside the bounding band), so every sprite pixel
        // the scissor admits lands on a fully rebuilt row — never re-blended over its
        // own Load-preserved pixels.
        if let Some((faw, fah)) = self.free_atlas.as_ref().map(|s| (s.w as f32, s.h as f32)) {
            for s in &input.free_sprites {
                // v1 is NEAREST-only (`FreeSampler::Linear` deferred): debug-assert
                // it off, ignore it in release.
                debug_assert!(
                    matches!(s.sampler, aterm_core::render::FreeSampler::Nearest),
                    "FreeSampler::Linear is deferred — v1 free sprites are NEAREST-only"
                );
                if !matches!(s.sampler, aterm_core::render::FreeSampler::Nearest) {
                    continue;
                }
                if s.w == 0 || s.h == 0 || s.aw == 0 || s.ah == 0 {
                    continue;
                }
                let (mut u0, mut du) = (s.ax as f32 / faw, s.aw as f32 / faw);
                if s.flip_x {
                    u0 = (s.ax as f32 + s.aw as f32) / faw;
                    du = -(s.aw as f32) / faw;
                }
                let dst = match s.z {
                    aterm_core::render::FreeZ::UnderText => &mut self.inst.free_under,
                    aterm_core::render::FreeZ::OverText => &mut self.inst.free_over,
                };
                dst.push(GlyphInstance {
                    rect: [
                        padf + s.x as f32,
                        grid_topf + s.y as f32,
                        s.w as f32,
                        s.h as f32,
                    ],
                    uv: [u0, s.ay as f32 / fah, du, s.ah as f32 / fah],
                    color: [
                        ((s.tint >> 16) & 0xff) as u8,
                        ((s.tint >> 8) & 0xff) as u8,
                        (s.tint & 0xff) as u8,
                        s.alpha,
                    ],
                    // W2: free sprites blend un-remapped (fs_scene_over): bg unused.
                    bg: [0, 0, 0, 0],
                });
            }
        }
        // Scene/cat/free atlas bind groups for the draws below (None when absent this
        // frame; the corresponding streams are then empty, so the draw_stream gate
        // skips them).
        let scene_bind = self.scene_atlas.as_ref().map(|s| &s.bind);
        let cat_bind = self.cat_atlas.as_ref().map(|s| &s.bind);
        let free_bind = self.free_atlas.as_ref().map(|s| &s.bind);
        let rain_bind = self.rain_atlas.as_ref().map(|s| &s.bind);

        let (device, queue) = (&self.ctx.device, &self.ctx.queue);
        let bg_buf = self
            .vbufs
            .bg
            .upload(device, queue, bytemuck::cast_slice(&self.inst.bg));
        let image_buf =
            self.vbufs
                .image
                .upload(device, queue, bytemuck::cast_slice(&self.inst.image));
        let glyph_buf =
            self.vbufs
                .glyph
                .upload(device, queue, bytemuck::cast_slice(&self.inst.glyph));
        let glyph_halo_buf = self.vbufs.glyph_halo.upload(
            device,
            queue,
            bytemuck::cast_slice(&self.inst.glyph_halo),
        );
        let color_buf =
            self.vbufs
                .color
                .upload(device, queue, bytemuck::cast_slice(&self.inst.color));
        let cursor_buf =
            self.vbufs
                .cursor
                .upload(device, queue, bytemuck::cast_slice(&self.inst.cursor));
        let deco_buf = self
            .vbufs
            .deco
            .upload(device, queue, bytemuck::cast_slice(&self.inst.deco));
        let curl_buf = self
            .vbufs
            .curl
            .upload(device, queue, bytemuck::cast_slice(&self.inst.curl));
        let trail_buf =
            self.vbufs
                .trail
                .upload(device, queue, bytemuck::cast_slice(&self.inst.trail));
        let glow_add_buf =
            self.vbufs
                .glow_add
                .upload(device, queue, bytemuck::cast_slice(&self.inst.glow_add));
        let glow_halo_buf =
            self.vbufs
                .glow_halo
                .upload(device, queue, bytemuck::cast_slice(&self.inst.glow_halo));
        let glow_halo_over_buf = self.vbufs.glow_halo_over.upload(
            device,
            queue,
            bytemuck::cast_slice(&self.inst.glow_halo_over),
        );
        let glow_under_buf = self.vbufs.glow_under.upload(
            device,
            queue,
            bytemuck::cast_slice(&self.inst.glow_under),
        );
        let fire_add_buf =
            self.vbufs
                .fire_add
                .upload(device, queue, bytemuck::cast_slice(&self.inst.fire_add));
        let fire_over_buf =
            self.vbufs
                .fire_over
                .upload(device, queue, bytemuck::cast_slice(&self.inst.fire_over));
        let nova_add_buf =
            self.vbufs
                .nova_add
                .upload(device, queue, bytemuck::cast_slice(&self.inst.nova_add));
        let rain_add_buf =
            self.vbufs
                .rain_add
                .upload(device, queue, bytemuck::cast_slice(&self.inst.rain_add));
        let rain_add_over_buf = self.vbufs.rain_add_over.upload(
            device,
            queue,
            bytemuck::cast_slice(&self.inst.rain_add_over),
        );
        let wdeco_over_buf = self.vbufs.wdeco_over.upload(
            device,
            queue,
            bytemuck::cast_slice(&self.inst.wdeco_over),
        );
        let wdeco_add_buf =
            self.vbufs
                .wdeco_add
                .upload(device, queue, bytemuck::cast_slice(&self.inst.wdeco_add));
        let scene_over_buf = self.vbufs.scene_over.upload(
            device,
            queue,
            bytemuck::cast_slice(&self.inst.scene_over),
        );
        let scene_add_buf =
            self.vbufs
                .scene_add
                .upload(device, queue, bytemuck::cast_slice(&self.inst.scene_add));
        let rain_under_buf = self.vbufs.rain_under.upload(
            device,
            queue,
            bytemuck::cast_slice(&self.inst.rain_under),
        );
        let cat_over_buf =
            self.vbufs
                .cat_over
                .upload(device, queue, bytemuck::cast_slice(&self.inst.cat_over));
        let free_under_buf = self.vbufs.free_under.upload(
            device,
            queue,
            bytemuck::cast_slice(&self.inst.free_under),
        );
        let free_over_buf =
            self.vbufs
                .free_over
                .upload(device, queue, bytemuck::cast_slice(&self.inst.free_over));
        let cursor_block_buf = self.vbufs.cursor_block.upload(
            device,
            queue,
            bytemuck::cast_slice(&self.inst.cursor_block),
        );
        let cursor_glyph_buf = self.vbufs.cursor_glyph.upload(
            device,
            queue,
            bytemuck::cast_slice(&self.inst.cursor_glyph),
        );
        let cursor_color_buf = self.vbufs.cursor_color.upload(
            device,
            queue,
            bytemuck::cast_slice(&self.inst.cursor_color),
        );

        // Resident offscreen target: reuse the texture + view when `(w, h)` is
        // unchanged; (re)create them only on the first frame or a resize. On a
        // (re)create the previous target (and the blit-source bind group built
        // from it in `present_input`) is replaced — including the per-format blit
        // PIPELINES staying valid (they key on swapchain format, not this view),
        // so no stale resource survives a dimension change. Usage is unchanged
        // (`RENDER_ATTACHMENT | COPY_SRC | TEXTURE_BINDING`).
        let recreate = match &win.offscreen {
            Some(o) => o.w != w || o.h != h,
            None => true,
        };
        if recreate {
            let tex = self.ctx.offscreen_texture(w, h);
            let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
            // The sRGB-typed view the base OVER/REPLACE passes attach (linear-light
            // blend). Its format is the single-source-of-truth offscreen_srgb_view_format:
            // on native it's an sRGB ALIAS of the Unorm texture (declared in the
            // texture's view_formats); on downlevel the texture is already sRGB, so the
            // explicit format equals the texture format (no alias needed). Either way it
            // equals what build_cell_pipelines targets — see crate::format_plan.
            let view_srgb = tex.create_view(&wgpu::TextureViewDescriptor {
                format: Some(self.ctx.offscreen_srgb_view_format()),
                ..Default::default()
            });
            // The blit-source bind group samples this exact `tex` into the
            // swapchain. Built ONCE here (and reused every present) instead of
            // per-present. `present_input` only writes the per-frame invert flag.
            let src_view = tex.create_view(&wgpu::TextureViewDescriptor::default());
            let blit_bind = self
                .ctx
                .device
                .create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("aterm-gpu blit bg"),
                    layout: &self.blit_bgl,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(&src_view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(&self.blit_sampler),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: self.blit_uniform_buf.as_entire_binding(),
                        },
                    ],
                });
            // GPU bloom target: a half-res texture the comet glow is re-rendered
            // into, blurred, and composited back over `view`. Rebuilt with the
            // offscreen; absent (a no-op) when bloom is disabled.
            let bloom = self.enable_bloom.then(|| {
                let bw = (w / BLOOM_DOWNSCALE).max(1);
                let bh = (h / BLOOM_DOWNSCALE).max(1);
                let btex = self.ctx.offscreen_texture(bw, bh);
                let bview = btex.create_view(&wgpu::TextureViewDescriptor::default());
                let sview = btex.create_view(&wgpu::TextureViewDescriptor::default());
                let bind = self
                    .ctx
                    .device
                    .create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("aterm-gpu bloom bind"),
                        layout: &self.bloom_bgl,
                        entries: &[
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: wgpu::BindingResource::TextureView(&sview),
                            },
                            wgpu::BindGroupEntry {
                                binding: 1,
                                resource: wgpu::BindingResource::Sampler(&self.bloom_sampler),
                            },
                            wgpu::BindGroupEntry {
                                binding: 2,
                                resource: self.bloom_uniform_buf.as_entire_binding(),
                            },
                        ],
                    });
                BloomTarget {
                    tex: btex,
                    view: bview,
                    bind,
                    bw,
                    bh,
                }
            });
            win.offscreen = Some(Offscreen {
                tex,
                view,
                view_srgb,
                blit_bind,
                w,
                h,
                bloom,
            });
        }

        // SCISSORED PATH: the load op + the scissor band. FULL clears the whole
        // target (== CPU `vec![theme.bg]`); SCISSORED loads the prior frame
        // (preserving every untouched row) and clips the pass to the dirty rows'
        // bounding band so only those pixels are written. The dirty-row instances
        // we built above are the ONLY draws, and rows are disjoint vertical bands,
        // so the band is bit-identical to a full render and the rest is verbatim.
        //
        // Load on a JUST-(re)created texture would read undefined tiles — but the
        // scissor path is only chosen by `encode_present_frame` when the offscreen
        // already held the prior frame at these dims (so `recreate` is false).
        // Assert that invariant, and fall back to Clear if it is ever violated.
        let (load_op, scissor) = match &scope {
            RepaintScope::Dirty(_) if !recreate => {
                match dirty_band {
                    Some((f, last)) => {
                        // Inset the dirty band by `grid_top` (the grid origin),
                        // exactly like the per-row instances above — EXCEPT at the
                        // grid's first/last row, where the band extends over the
                        // top strip (head band + pad) / bottom pad strip so the
                        // strip-reset quads above (and the effects that draw there
                        // — bloom bleed, fire in the head band) land inside the
                        // scissor. An interior band's strips are bg from the
                        // first full render and preserved by `LoadOp::Load`.
                        // BOTH edges are clamped to the CLAMPED framebuffer
                        // height: a dirty row below the device-limit clamp (an
                        // oversized control-socket grid) must not push the scissor
                        // origin past the attachment — wgpu validates that and
                        // aborts — so it saturates to a zero-height in-bounds rect
                        // (nothing visible changed) instead.
                        let y0 = if f == 0 {
                            0
                        } else {
                            ((grid_top + f * ch) as u32).min(h)
                        };
                        let y1 = if last + 1 >= rows {
                            h
                        } else {
                            ((grid_top + (last + 1) * ch) as u32).min(h)
                        };
                        (
                            wgpu::LoadOp::Load,
                            Some((0u32, y0, w, y1.saturating_sub(y0))),
                        )
                    }
                    // Reusable but zero dirty rows: nothing to draw. Load preserves
                    // the prior frame; a degenerate 0-height scissor draws nothing.
                    None => (wgpu::LoadOp::Load, Some((0, 0, 0, 0))),
                }
            }
            // FULL (or the can't-happen Dirty-after-recreate): clear everything.
            _ => {
                debug_assert!(
                    matches!(scope, RepaintScope::Full),
                    "scissored Load requires the prior frame resident (no recreate)"
                );
                (
                    wgpu::LoadOp::Clear(theme_color_alpha(frame_bg, f64::from(bg_opacity))),
                    None,
                )
            }
        };

        let off = win.offscreen.as_ref().expect("offscreen set above");
        // Base OVER/REPLACE passes attach the sRGB-typed `view_srgb` so fixed-function
        // blending composites in LINEAR light (matching the CPU `blend`). The ADDITIVE
        // streams (glow_add, wdeco_add — One/One) attach the offscreen's default `view`:
        // plain Unorm on native, so the raw 8-bit add stays byte-exact with the CPU
        // `add_sat`; sRGB on downlevel, so the add lands in linear (the accepted cosmetic
        // approximation — see the header DOWNLEVEL FALLBACK). Both views alias the SAME
        // texture (sRGB-encoded bytes), so the blit + readback (which use `view`) are
        // unchanged — screenshot==glass holds.
        let view = &off.view;
        let view_srgb = &off.view_srgb;
        let mut enc = self
            .ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aterm-gpu frame"),
            });

        // SINGLE render pass: the eight former passes (bg → glyph → color →
        // deco → cursor_block → cursor_glyph → cursor_color → cursor_strip) all
        // targeted the SAME offscreen view with LoadOp::Clear (first) then
        // LoadOp::Load (the rest). Collapsing them into one `begin_render_pass`
        // removes seven gratuitous pass setups and — on Apple-Silicon/TBDR —
        // seven full-target store+reload tile round-trips, while issuing the
        // EXACT same draws in the EXACT same order with the same pipelines and
        // blend states, so the output stays BYTE-IDENTICAL.
        //
        // Safe to fuse because (confirmed above):
        //   * the render target is RENDER_ATTACHMENT|COPY_SRC only (no
        //     TEXTURE_BINDING), and shaders sample only the atlas — no
        //     read-after-write hazard between streams;
        //   * no MSAA (sample_count 1), no depth/stencil, no resolve target;
        //   * no scissor/viewport/blend-constant/stencil state to re-establish.
        //
        // LoadOp::Clear stays the pass's single load op (clearing to the theme
        // bg, == CPU `vec![theme.bg]`); we do NOT switch to Load (which on
        // Metal/TBDR would force a tile load-from-undefined). Bind group 0
        // (`uniform_bg`) is identical for all three pipelines, so it is set ONCE.
        // `set_pipeline`/`set_bind_group(1, ..)` are emitted only when the
        // pipeline / atlas actually changes between consecutive *drawn* streams
        // — the gating (skip-when-empty) is preserved, so an empty stream sets
        // no state and draws nothing, exactly as before.
        // Pipeline / atlas trackers + the per-stream draw helper, hoisted to the
        // ENCODE scope so the (conditional) multiple passes below all share them.
        #[derive(PartialEq, Clone, Copy)]
        enum Pipe {
            Bg,
            CursorBlend,
            Glyph,
            Color,
            GlowAdd,
            RainGlow,
            RainGlowOver,
            FireAdd,
            FireOver,
            DecoOver,
            DecoAdd,
            SceneOver,
            SceneAdd,
            RainUnder,
            CatOver,
            FreeUnder,
            FreeOver,
        }
        #[derive(PartialEq, Clone, Copy)]
        enum Atlas {
            Mono,
            Color,
            Image,
            Deco,
            Scene,
            Rain,
            Cat,
            Free,
        }
        // Bind the pipeline (+ atlas group) only on a change, then draw. References
        // the `pass`/`cur_pipe`/`cur_atlas` in scope at the CALL SITE (each pass
        // block below declares its own), so an empty stream binds nothing.
        macro_rules! draw_stream {
            ($p:ident, $cp:ident, $ca:ident, $buf:expr, $insts:expr, $pipe:expr, $pipeline:expr, $atlas:expr) => {
                if let Some(buf) = $buf.as_ref() {
                    if $cp != Some($pipe) {
                        $p.set_pipeline($pipeline);
                        $cp = Some($pipe);
                    }
                    if let Some((atlas_kind, atlas_bg)) = $atlas {
                        if $ca != Some(atlas_kind) {
                            $p.set_bind_group(1, atlas_bg, &[]);
                            $ca = Some(atlas_kind);
                        }
                    }
                    $p.set_vertex_buffer(0, *buf);
                    $p.draw(0..6, 0..$insts.len() as u32);
                }
            };
        }
        // Open a render pass on `$view` with `$load`, set the scissor + shared bind
        // group 0, and yield the pass. `scissor`/`self` resolve at the call site.
        macro_rules! open_pass {
            ($view:expr, $load:expr) => {{
                let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("aterm-gpu frame pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: $view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: $load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                if let Some((sx, sy, sw, sh)) = scissor {
                    pass.set_scissor_rect(sx, sy, sw, sh);
                }
                pass.set_bind_group(0, &self.uniform_bg, &[]);
                pass
            }};
        }
        // The draw-stream sequences, factored so the no-additive single-pass path
        // and the additive split share them verbatim (preserving EXACT draw order).
        // `emit_base_pre` is further split into its bg half (everything UNDER the
        // glyph ink) and its fg half (the glyph ink and above) so a live
        // `glow_under` stream can interpose its own Unorm pass BETWEEN them — the
        // EMBERFORGE under-glyph slot; with `glow_under` empty the two halves run
        // back-to-back in ONE pass, byte-identical to the historical fused emit.
        macro_rules! emit_base_bg {
            ($p:ident, $cp:ident, $ca:ident) => {
                // bg / under-text sprites — all OVER or REPLACE, composited in
                // linear light on the sRGB view.
                draw_stream!(
                    $p,
                    $cp,
                    $ca,
                    bg_buf,
                    self.inst.bg,
                    Pipe::Bg,
                    &self.bg_pipeline,
                    None::<(Atlas, &wgpu::BindGroup)>
                );
                // SCENE world sprites, UNDER the text (after the cell bg, before glyphs).
                draw_stream!(
                    $p,
                    $cp,
                    $ca,
                    scene_over_buf,
                    self.inst.scene_over,
                    Pipe::SceneOver,
                    &self.scene_over_pipeline,
                    scene_bind.map(|b| (Atlas::Scene, b))
                );
                // PHOSPHOR rain sprites, right after scene_over and BEFORE the
                // cats — the CPU stamps rain in pass 1c between scene_over and
                // cat_quads, so cats walk ON rain by construction. Same src-over
                // pipeline; the RAIN atlas bind group carries the NEAREST
                // sampler (1:1 regime).
                draw_stream!(
                    $p,
                    $cp,
                    $ca,
                    rain_under_buf,
                    self.inst.rain_under,
                    Pipe::RainUnder,
                    &self.scene_over_pipeline,
                    rain_bind.map(|b| (Atlas::Rain, b))
                );
                // Peeking-CAT sprites, right after scene_over — the same under-text
                // slot (the CPU stamps both in pass 1c inside render_row, so the
                // z-order matches by construction). Same src-over pipeline; the CAT
                // atlas bind group carries the NEAREST sampler (1:1 regime).
                draw_stream!(
                    $p,
                    $cp,
                    $ca,
                    cat_over_buf,
                    self.inst.cat_over,
                    Pipe::CatOver,
                    &self.scene_over_pipeline,
                    cat_bind.map(|b| (Atlas::Cat, b))
                );
                // FREE-floating UNDER-TEXT sprites, right after cat_over and
                // before the glyphs: free under-sprites draw OVER legacy
                // scene/cat and UNDER text. Same src-over pipeline; the FREE
                // atlas bind group carries the NEAREST sampler (v1 regime).
                draw_stream!(
                    $p,
                    $cp,
                    $ca,
                    free_under_buf,
                    self.inst.free_under,
                    Pipe::FreeUnder,
                    &self.scene_over_pipeline,
                    free_bind.map(|b| (Atlas::Free, b))
                );
                // Comet EMBER BED (cursor motion trail): pre-blended solid
                // fills through the bg pipeline (REPLACE), at the END of the
                // base-bg half — UNDER the Unorm interpose light and UNDER the
                // glyph ink of the fg half (== the CPU phase B2b inside
                // `composite_free`), so swept text stays readable.
                draw_stream!(
                    $p,
                    $cp,
                    $ca,
                    trail_buf,
                    self.inst.trail,
                    Pipe::Bg,
                    &self.bg_pipeline,
                    None::<(Atlas, &wgpu::BindGroup)>
                );
            };
        }
        macro_rules! emit_glow_under {
            ($p:ident, $cp:ident, $ca:ident) => {
                // EMBERFORGE UNDER-GLYPH light: premultiplied One/One flame-body
                // quads through the SAME `glow_add_pipeline` as the aurora, on
                // the Unorm view (byte-exact add == CPU `add_sat`) — drawn OVER
                // the base-bg half and UNDER the base-fg half (== the CPU
                // phase-B3 `draw_glow_under` between phases B2 and C).
                draw_stream!(
                    $p,
                    $cp,
                    $ca,
                    glow_under_buf,
                    self.inst.glow_under,
                    Pipe::GlowAdd,
                    &self.glow_add_pipeline,
                    None::<(Atlas, &wgpu::BindGroup)>
                );
                // EMBERFORGE per-pixel FIRE FIELD: the full-art flame body in
                // the same under-glyph slot, right after the glow_under quads
                // (== the CPU phase-B3b `draw_fire_patch` after
                // `draw_glow_under`), Add patches then Over patches — the
                // CPU's per-mode sweep order, so overlapping mixed-mode
                // patches stay parity-exact (ink dims light, never vice
                // versa).
                draw_stream!(
                    $p,
                    $cp,
                    $ca,
                    fire_add_buf,
                    self.inst.fire_add,
                    Pipe::FireAdd,
                    &self.fire_add_pipeline,
                    None::<(Atlas, &wgpu::BindGroup)>
                );
                draw_stream!(
                    $p,
                    $cp,
                    $ca,
                    fire_over_buf,
                    self.inst.fire_over,
                    Pipe::FireOver,
                    &self.fire_over_pipeline,
                    None::<(Atlas, &wgpu::BindGroup)>
                );
            };
        }
        macro_rules! emit_base_fg {
            ($p:ident, $cp:ident, $ca:ident) => {
                // EMBERFORGE CONTRAST-HALO: the dark warm dilation ring around
                // every fire-engulfed glyph, drawn FIRST in the fg half — OVER
                // the flame body the glow_under/fire Unorm pass just laid down,
                // and UNDER the glyph ink below — so the heat-glow letterform
                // separates from the flame. Straight source-over (`fs_deco_over`
                // == CPU `blend`) through the deco pipeline, but binding the MONO
                // atlas so it samples the GLYPH coverage (the offset-shifted quads
                // dilate it). Empty (no draw) for every fire-free frame, so
                // ordinary text is byte-identical.
                draw_stream!(
                    $p,
                    $cp,
                    $ca,
                    glyph_halo_buf,
                    self.inst.glyph_halo,
                    Pipe::DecoOver,
                    &self.deco_over_pipeline,
                    Some((Atlas::Mono, atlas_bind))
                );
                // glyph / colour-emoji / inline-image / line-deco / undercurl
                // — all OVER or REPLACE, composited in linear light on the
                // sRGB view.
                draw_stream!(
                    $p,
                    $cp,
                    $ca,
                    glyph_buf,
                    self.inst.glyph,
                    Pipe::Glyph,
                    &self.glyph_pipeline,
                    Some((Atlas::Mono, atlas_bind))
                );
                draw_stream!(
                    $p,
                    $cp,
                    $ca,
                    color_buf,
                    self.inst.color,
                    Pipe::Color,
                    &self.color_glyph_pipeline,
                    Some((Atlas::Color, color_bind))
                );
                if let Some(plane) = win.image_plane.as_ref() {
                    draw_stream!(
                        $p,
                        $cp,
                        $ca,
                        image_buf,
                        self.inst.image,
                        Pipe::Color,
                        &self.color_glyph_pipeline,
                        Some((Atlas::Image, &plane.bind))
                    );
                }
                draw_stream!(
                    $p,
                    $cp,
                    $ca,
                    deco_buf,
                    self.inst.deco,
                    Pipe::Bg,
                    &self.bg_pipeline,
                    None::<(Atlas, &wgpu::BindGroup)>
                );
                // W7 AA undercurl quads: AFTER every solid deco quad (the CPU
                // pass 3 → 3b order), textured coverage over the deco atlas'
                // curl sprite, ALPHA_BLENDING (`fs_deco_over` == CPU `blend`).
                // The stream is empty whenever the atlas is absent (shared
                // `undercurl_supported` gate), so no bind can dangle.
                if let Some(db) = deco_bind {
                    draw_stream!(
                        $p,
                        $cp,
                        $ca,
                        curl_buf,
                        self.inst.curl,
                        Pipe::DecoOver,
                        &self.deco_over_pipeline,
                        Some((Atlas::Deco, db))
                    );
                }
                // (The cursor MOTION TRAIL — the comet ember bed — now draws at
                // the end of `emit_base_bg`, UNDER the glyph ink, matching the
                // CPU phase B2b.)
            };
        }
        // The historical fused base emit: bg half then fg half, back-to-back in
        // the SAME pass — used whenever `glow_under` is empty, so every
        // glow_under-free frame issues the EXACT pre-split draw sequence.
        macro_rules! emit_base_pre {
            ($p:ident, $cp:ident, $ca:ident) => {
                emit_base_bg!($p, $cp, $ca);
                emit_base_fg!($p, $cp, $ca);
            };
        }
        macro_rules! emit_glow {
            ($p:ident, $cp:ident, $ca:ident) => {
                // LUMEN aurora: PREMULTIPLIED ADDITIVE light, on the Unorm view.
                draw_stream!(
                    $p,
                    $cp,
                    $ca,
                    glow_add_buf,
                    self.inst.glow_add,
                    Pipe::GlowAdd,
                    &self.glow_add_pipeline,
                    None::<(Atlas, &wgpu::BindGroup)>
                );
                // GLOW-HALO cursor-effect radial light: right AFTER the aurora
                // and BEFORE the nova/rain (== the CPU `draw_glow_halo` after
                // `draw_glow`), through the SAME radial pipeline as `rain_add`.
                draw_stream!(
                    $p,
                    $cp,
                    $ca,
                    glow_halo_buf,
                    self.inst.glow_halo,
                    Pipe::RainGlow,
                    &self.rain_glow_pipeline,
                    None::<(Atlas, &wgpu::BindGroup)>
                );
                // GLOW-HALO `HaloMode::Over` veils: the Over half of the
                // stream's per-mode split, right AFTER its Add half (== the
                // CPU `draw_radial_add` Add-then-Over sweep — veils dim
                // light) through the deco source-over blend state on the
                // same Unorm view (byte-exact == CPU `over_rgb` on native).
                draw_stream!(
                    $p,
                    $cp,
                    $ca,
                    glow_halo_over_buf,
                    self.inst.glow_halo_over,
                    Pipe::RainGlowOver,
                    &self.rain_glow_over_pipeline,
                    None::<(Atlas, &wgpu::BindGroup)>
                );
                // SUPERNOVA additive light: the second One/One stream, right
                // after the aurora (== the CPU `draw_nova` after `draw_glow`).
                // Same pipeline + `Pipe` tag, so a frame with both streams sets
                // the pipeline once.
                draw_stream!(
                    $p,
                    $cp,
                    $ca,
                    nova_add_buf,
                    self.inst.nova_add,
                    Pipe::GlowAdd,
                    &self.glow_add_pipeline,
                    None::<(Atlas, &wgpu::BindGroup)>
                );
                // PHOSPHOR rain bright-head halos: the third One/One stream,
                // right after the nova (== the CPU's rain-after-nova additive
                // order). Same pipeline + `Pipe` tag, so a frame with several
                // additive streams sets the pipeline once.
                draw_stream!(
                    $p,
                    $cp,
                    $ca,
                    rain_add_buf,
                    self.inst.rain_add,
                    Pipe::RainGlow,
                    &self.rain_glow_pipeline,
                    None::<(Atlas, &wgpu::BindGroup)>
                );
                // PHOSPHOR rain `HaloMode::Over` veils: the Over half of the
                // `rain_add` split, right after its Add half — the exact
                // glow_halo_over contract (the two RainHalo streams share
                // one CPU rasterizer, so they share one GPU discipline).
                draw_stream!(
                    $p,
                    $cp,
                    $ca,
                    rain_add_over_buf,
                    self.inst.rain_add_over,
                    Pipe::RainGlowOver,
                    &self.rain_glow_over_pipeline,
                    None::<(Atlas, &wgpu::BindGroup)>
                );
            };
        }
        macro_rules! emit_scene_add {
            ($p:ident, $cp:ident, $ca:ident) => {
                // SCENE additive light (sun/fireflies/comet), on the Unorm view.
                draw_stream!(
                    $p,
                    $cp,
                    $ca,
                    scene_add_buf,
                    self.inst.scene_add,
                    Pipe::SceneAdd,
                    &self.scene_add_pipeline,
                    scene_bind.map(|b| (Atlas::Scene, b))
                );
            };
        }
        macro_rules! emit_wdeco_over {
            ($p:ident, $cp:ident, $ca:ident) => {
                // Sparkle-word "paw" decoration (ALPHA_BLENDING) — sRGB view.
                if let Some(db) = deco_bind {
                    draw_stream!(
                        $p,
                        $cp,
                        $ca,
                        wdeco_over_buf,
                        self.inst.wdeco_over,
                        Pipe::DecoOver,
                        &self.deco_over_pipeline,
                        Some((Atlas::Deco, db))
                    );
                }
            };
        }
        macro_rules! emit_free_over {
            ($p:ident, $cp:ident, $ca:ident) => {
                // FREE-floating OVER-TEXT sprites (FreeZ::OverText): after the
                // wdeco streams, immediately BEFORE the cursor — src-over on the
                // sRGB view through the shared scene pipeline, FREE atlas bind
                // (NEAREST).
                draw_stream!(
                    $p,
                    $cp,
                    $ca,
                    free_over_buf,
                    self.inst.free_over,
                    Pipe::FreeOver,
                    &self.scene_over_pipeline,
                    free_bind.map(|b| (Atlas::Free, b))
                );
            };
        }
        macro_rules! emit_wdeco_add {
            ($p:ident, $cp:ident, $ca:ident) => {
                // Sparkle-word "sparkle" decoration (One/One additive) — Unorm view.
                if let Some(db) = deco_bind {
                    draw_stream!(
                        $p,
                        $cp,
                        $ca,
                        wdeco_add_buf,
                        self.inst.wdeco_add,
                        Pipe::DecoAdd,
                        &self.deco_add_pipeline,
                        Some((Atlas::Deco, db))
                    );
                }
            };
        }
        // The cursor FILL pipeline: the historical REPLACE bg pipeline when
        // opaque (byte-identical default), the ALPHA_BLENDING twin when the
        // host set `cursor_opacity < 1` (fill blends over the rendered cell,
        // == CPU `blend_rect`; the cut-out streams are empty then).
        let (cursor_fill_pipe, cursor_fill_pipeline) = if cursor_opaque {
            (Pipe::Bg, &self.bg_pipeline)
        } else {
            (Pipe::CursorBlend, &self.cursor_blend_pipeline)
        };
        macro_rules! emit_cursor {
            ($p:ident, $cp:ident, $ca:ident) => {
                // cursor fill / cut-out glyph / colour glyph / strip — OVER/REPLACE.
                draw_stream!(
                    $p,
                    $cp,
                    $ca,
                    cursor_block_buf,
                    self.inst.cursor_block,
                    cursor_fill_pipe,
                    cursor_fill_pipeline,
                    None::<(Atlas, &wgpu::BindGroup)>
                );
                draw_stream!(
                    $p,
                    $cp,
                    $ca,
                    cursor_glyph_buf,
                    self.inst.cursor_glyph,
                    Pipe::Glyph,
                    &self.glyph_pipeline,
                    Some((Atlas::Mono, atlas_bind))
                );
                draw_stream!(
                    $p,
                    $cp,
                    $ca,
                    cursor_color_buf,
                    self.inst.cursor_color,
                    Pipe::Color,
                    &self.color_glyph_pipeline,
                    Some((Atlas::Color, color_bind))
                );
                draw_stream!(
                    $p,
                    $cp,
                    $ca,
                    cursor_buf,
                    self.inst.cursor,
                    cursor_fill_pipe,
                    cursor_fill_pipeline,
                    None::<(Atlas, &wgpu::BindGroup)>
                );
            };
        }

        // The ADDITIVE streams (glow aurora, sparkle-add) — and the HaloMode::Over
        // radial veils, which ride the same pass — must composite on the Unorm
        // view (byte-exact add / source-over). When ALL are empty — the common
        // case and every parity test — the whole frame is ONE sRGB pass (no extra
        // TBDR tile round-trips). Otherwise split, keeping the EXACT draw order:
        //   A(sRGB) bg..curl | B(Unorm) glow | C(sRGB) wdeco_over | D(Unorm)
        //   wdeco_add | E(sRGB) cursor.
        // A live `glow_under` (EMBERFORGE flame body) splits pass A itself —
        //   A1(sRGB) bg..trail | A2(Unorm) glow_under | A3(sRGB) glyph..curl
        // — so its light lands UNDER the glyph ink; a glow_under-free frame never
        // opens A2/A3 (the fused pass A is byte-identical to before).
        let has_additive = !self.inst.glow_add.is_empty()
            || !self.inst.glow_halo.is_empty()
            || !self.inst.glow_halo_over.is_empty()
            || !self.inst.glow_under.is_empty()
            || !self.inst.fire_add.is_empty()
            || !self.inst.fire_over.is_empty()
            || !self.inst.nova_add.is_empty()
            || !self.inst.rain_add.is_empty()
            || !self.inst.rain_add_over.is_empty()
            || !self.inst.wdeco_add.is_empty()
            || !self.inst.scene_add.is_empty();
        if !has_additive {
            let mut pass = open_pass!(view_srgb, load_op);
            let mut cur_pipe: Option<Pipe> = None;
            let mut cur_atlas: Option<Atlas> = None;
            emit_base_pre!(pass, cur_pipe, cur_atlas);
            emit_wdeco_over!(pass, cur_pipe, cur_atlas);
            emit_free_over!(pass, cur_pipe, cur_atlas);
            emit_cursor!(pass, cur_pipe, cur_atlas);
            let _ = (cur_pipe, cur_atlas);
        } else {
            // Open a pass ONLY when it has something to draw: on TBDR (Metal) a
            // Load+Store pass costs a full-attachment tile load + store even when
            // it draws NOTHING, so the typical aurora frame (glow live, sparkle
            // words idle) would pay two empty full-screen round-trips for C and D.
            // Each gate mirrors EXACTLY the skip conditions of the draws inside
            // (`draw_stream!`'s `buf.is_some()`, plus `deco_bind.is_some()` for the
            // wdeco streams), so an omitted pass had zero draws — the draw order
            // among OPENED passes is unchanged and the output is byte-identical.
            // Gating on BUFFER presence (not instance emptiness) also skips the
            // max-buffer-size degradation path where `upload` returned `None` for
            // a non-empty stream. Pass A (A1 on the glow_under split) stays
            // UNCONDITIONAL: it carries `load_op`, which may be a `Clear` that
            // must run even with all streams empty.
            if glow_under_buf.is_some() || fire_add_buf.is_some() || fire_over_buf.is_some() {
                // EMBERFORGE split: the base pass parts around the under-glyph
                // light. A1 draws everything UNDER the glyph ink (bg + sprite
                // streams) on the sRGB view; A2 One/One-adds the flame body on
                // the Unorm view (byte-exact == CPU `add_sat`, the pass-B
                // contract); A3 draws the glyph ink and the rest of the base on
                // the sRGB view again. Same draws, same order, same views per
                // stream as the fused pass — only the pass boundaries (Load-
                // preserving, same scissor) move, so a glow_under-free frame
                // and the split frame agree byte-for-byte outside the added
                // light. A3 is gated like the other split passes: each term
                // mirrors the exact skip conditions of the draws inside.
                {
                    let mut pass = open_pass!(view_srgb, load_op);
                    let mut cur_pipe: Option<Pipe> = None;
                    let mut cur_atlas: Option<Atlas> = None;
                    emit_base_bg!(pass, cur_pipe, cur_atlas);
                    let _ = (cur_pipe, cur_atlas);
                }
                {
                    let mut pass = open_pass!(view, wgpu::LoadOp::Load);
                    let mut cur_pipe: Option<Pipe> = None;
                    let mut cur_atlas: Option<Atlas> = None;
                    emit_glow_under!(pass, cur_pipe, cur_atlas);
                    let _ = (cur_pipe, cur_atlas);
                }
                if glyph_buf.is_some()
                    || glyph_halo_buf.is_some()
                    || color_buf.is_some()
                    || (win.image_plane.is_some() && image_buf.is_some())
                    || deco_buf.is_some()
                    || (deco_bind.is_some() && curl_buf.is_some())
                {
                    let mut pass = open_pass!(view_srgb, wgpu::LoadOp::Load);
                    let mut cur_pipe: Option<Pipe> = None;
                    let mut cur_atlas: Option<Atlas> = None;
                    emit_base_fg!(pass, cur_pipe, cur_atlas);
                    let _ = (cur_pipe, cur_atlas);
                }
            } else {
                let mut pass = open_pass!(view_srgb, load_op);
                let mut cur_pipe: Option<Pipe> = None;
                let mut cur_atlas: Option<Atlas> = None;
                emit_base_pre!(pass, cur_pipe, cur_atlas);
                let _ = (cur_pipe, cur_atlas);
            }
            if glow_add_buf.is_some()
                || glow_halo_buf.is_some()
                || glow_halo_over_buf.is_some()
                || nova_add_buf.is_some()
                || rain_add_buf.is_some()
                || rain_add_over_buf.is_some()
                || scene_add_buf.is_some()
            {
                let mut pass = open_pass!(view, wgpu::LoadOp::Load);
                let mut cur_pipe: Option<Pipe> = None;
                let mut cur_atlas: Option<Atlas> = None;
                emit_glow!(pass, cur_pipe, cur_atlas);
                emit_scene_add!(pass, cur_pipe, cur_atlas);
                let _ = (cur_pipe, cur_atlas);
            }
            if deco_bind.is_some() && wdeco_over_buf.is_some() {
                let mut pass = open_pass!(view_srgb, wgpu::LoadOp::Load);
                let mut cur_pipe: Option<Pipe> = None;
                let mut cur_atlas: Option<Atlas> = None;
                emit_wdeco_over!(pass, cur_pipe, cur_atlas);
                let _ = (cur_pipe, cur_atlas);
            }
            if deco_bind.is_some() && wdeco_add_buf.is_some() {
                let mut pass = open_pass!(view, wgpu::LoadOp::Load);
                let mut cur_pipe: Option<Pipe> = None;
                let mut cur_atlas: Option<Atlas> = None;
                emit_wdeco_add!(pass, cur_pipe, cur_atlas);
                let _ = (cur_pipe, cur_atlas);
            }
            // FREE over-text sprites keep their pre-cursor slot on the additive
            // split too (sRGB view, src-over). Gated like the wdeco passes so an
            // empty stream opens no pass (mirrors draw_stream!'s skip).
            if free_bind.is_some() && free_over_buf.is_some() {
                let mut pass = open_pass!(view_srgb, wgpu::LoadOp::Load);
                let mut cur_pipe: Option<Pipe> = None;
                let mut cur_atlas: Option<Atlas> = None;
                emit_free_over!(pass, cur_pipe, cur_atlas);
                let _ = (cur_pipe, cur_atlas);
            }
            if cursor_block_buf.is_some()
                || cursor_glyph_buf.is_some()
                || cursor_color_buf.is_some()
                || cursor_buf.is_some()
            {
                let mut pass = open_pass!(view_srgb, wgpu::LoadOp::Load);
                let mut cur_pipe: Option<Pipe> = None;
                let mut cur_atlas: Option<Atlas> = None;
                emit_cursor!(pass, cur_pipe, cur_atlas);
                let _ = (cur_pipe, cur_atlas);
            }
        }

        // GPU-only BLOOM (the "more amazing on GPU" layer) is NO LONGER composited
        // here. The comet halo is a soft ADDITIVE layer; compositing it into this
        // (reusable, scissor-preserved) offscreen would force every aurora tick to
        // REBUILD the whole halo band (the halo re-adds over any Load-preserved row —
        // accumulation), defeating the incremental present under a moving cursor.
        // Instead the offscreen stays the CLEAN base+aurora scissor base, and the
        // halo is composited at PRESENT time over a throwaway copy of it
        // (`compose_present_offscreen`) or, on a `present_prev`-invalidated readback /
        // tray / scroll frame, in place (`composite_bloom_in_place`). Either way the
        // presented pixels are byte-identical to the old in-offscreen bloom, while
        // the scissored dirty set stays proportional to real content change.
        self.ctx.queue.submit([enc.finish()]);
        // Record the total instances built this frame (diagnostic). In the
        // scissored path this is ~proportional to the dirty-row count, not the
        // screen — the headline win.
        self.last_instances = self.inst.bg.len()
            + self.inst.image.len()
            + self.inst.glyph.len()
            + self.inst.color.len()
            + self.inst.cursor.len()
            + self.inst.deco.len()
            + self.inst.curl.len()
            + self.inst.trail.len()
            + self.inst.glow_add.len()
            + self.inst.nova_add.len()
            + self.inst.cursor_block.len()
            + self.inst.cursor_glyph.len()
            + self.inst.cursor_color.len();
        // The rendered target lives on `win.offscreen` (resident across frames);
        // callers read it from there (`render_input` for readback, `present_input`
        // for the blit source).
        (w, h)
    }
}

/// Saturating narrow of a pixel POSITION offset to a packed-u16 instance-rect
/// coordinate. A grid far larger than the (clamped) framebuffer produces cell
/// origins beyond `u16::MAX`; saturating (vs. the wrapping `as u16`) lands such an
/// off-screen cell at `u16::MAX` — far outside the clamped framebuffer, so the
/// scissor / NDC clip discards it — instead of WRAPPING it back over the visible
/// left/top edge. Cell widths/heights stay small (bounded by the cell size) and are
/// still cast directly. For an in-bounds grid (`< u16::MAX` px) this is identical to
/// the old cast, so CPU/GPU parity is unchanged.
#[inline]
fn sat_pos_u16(v: usize) -> u16 {
    u16::try_from(v).unwrap_or(u16::MAX)
}

/// IEEE 754 binary16 → f32, exact (every half value is representable in f32).
/// Used only by the M3 EDR test readback (`present_hdr_for_test`) — the
/// workspace has no `half` crate, and 15 lines beat a dependency. Subnormals
/// scale the raw mantissa by 2^-24; Inf/NaN map to the f32 equivalents.
fn f16_bits_to_f32(bits: u16) -> f32 {
    let sign = if bits & 0x8000 != 0 { -1.0f32 } else { 1.0 };
    let exp = (bits >> 10) & 0x1f;
    let man = u32::from(bits & 0x3ff);
    let mag = match (exp, man) {
        (0, 0) => 0.0f32,
        (0, m) => (m as f32) * (-24f32).exp2(),
        (0x1f, 0) => f32::INFINITY,
        (0x1f, _) => f32::NAN,
        (e, m) => (1.0 + m as f32 / 1024.0) * f32::from(e as i16 - 15).exp2(),
    };
    sign * mag
}

/// `[r, g, b]` (0..=255) -> opaque RGBA bytes (a == 255). The `Unorm8x4` vertex
/// attribute decodes these as exactly `value/255.0` — the identical IEEE-754
/// floats the old `[f32;4]` form computed, so packing stays byte-identical.
fn rgb4([r, g, b]: [u8; 3]) -> [u8; 4] {
    [r, g, b, 255]
}

/// `0x00RRGGBB` -> opaque RGBA bytes (a == 255).
fn rgb4_u32(c: u32) -> [u8; 4] {
    rgb4([(c >> 16) as u8, (c >> 8) as u8, c as u8])
}

/// Stamp a [`HaloMode::Over`](aterm_render::HaloMode::Over) veil instance with
/// its per-pixel alpha CEILING: the straight `color`'s HIGH BYTE
/// ([`aterm_render::halo_over_cap`]) is carried in the instance ALPHA byte the
/// `fs_rain_glow_over` shader reads back (`0` == uncapped, so every historical
/// veil — packed `a == 0` here — stays byte-identical to the CPU rasterizer).
fn with_over_cap(mut inst: RainGlowInstance, color: u32) -> RainGlowInstance {
    inst.color[3] = (color >> 24) as u8;
    inst
}

/// Decode the inline image identified by `key` (`(arc_ptr, fp_w, fp_h)`) to its
/// footprint RGBA, finding the matching `ImageRef` in `input` by `Arc` identity.
/// Uses the SAME `aterm_render::decode_image_to_footprint` the CPU path caches, so
/// the bytes the GPU samples are byte-identical to the CPU `blit_image_cell` copy.
/// Returns a negative result (empty `rgba`) when the image is absent or fails to
/// decode — cached so a bad image draws nothing without re-decoding every frame.
fn decode_for_key(
    image: &std::sync::Arc<aterm_core::grid::extra::ImageData>,
    fp_w: usize,
    fp_h: usize,
) -> GpuDecodedImage {
    // The caller already holds the live `Arc<ImageData>` for this distinct image, so
    // decode straight from it. The previous signature took only the raw pointer key and
    // RE-SCANNED the whole grid (`for row in &input.images`) to re-find the matching
    // Arc before decoding — O(image-cells) per newly-seen image, i.e. O(new_images ×
    // image-cells) per frame, quadratic when a screenful of distinct images changes
    // every frame (round-9). The footprint (fp_w, fp_h) was computed from this image's
    // cols/rows at the call site, so it matches by construction.
    let rgba = aterm_render::decode_image_to_footprint(&image.bytes, image.format, fp_w, fp_h)
        .unwrap_or_default();
    GpuDecodedImage {
        w: fp_w as u32,
        h: fp_h as u32,
        rgba,
    }
}

/// `0x00RRGGBB` -> a `wgpu::Color` clear value for the sRGB-typed base attachment.
/// The base pass attaches an `Rgba8UnormSrgb` view that ENCODES linear->sRGB on
/// store, and a clear value is interpreted in that linear space — so we DECODE the
/// sRGB byte to linear here, so the stored byte round-trips back to `c` (== the
/// CPU's raw `vec![theme.bg]`). Keeps the empty-cell background byte-exact.
fn theme_color_alpha(c: u32, a: f64) -> wgpu::Color {
    fn s2l(b: u32) -> f64 {
        let c = b as f64 / 255.0;
        if c <= 0.040_45 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }
    // Alpha is LINEAR (never sRGB-encoded): the stored byte is round(a*255),
    // matching the CPU's default-bg transmittance (`255 - round(a*255)`) after
    // `read_back` re-folds it. `a == 1.0` stores 255 — the historical bytes.
    wgpu::Color {
        r: s2l((c >> 16) & 0xff),
        g: s2l((c >> 8) & 0xff),
        b: s2l(c & 0xff),
        a,
    }
}

#[cfg(test)]
mod tests {
    use aterm_render::GlowQuad;

    /// REGRESSION (inline-image plane panic, renderer.rs:2158): the decoded LRU
    /// is bounded to `GpuImageCache::MAX`, but the per-frame placement map that
    /// drives the pack loop is NOT. When more than `MAX` DISTINCT images are
    /// visible, the layout loop evicts an already-placed key, so the pack loop's
    /// lookup of that key MUST be able to miss. This proves `peek` returns `None`
    /// for an evicted-but-still-placed key — the exact condition that made the
    /// pack loop's old `.expect("placed image is cached")` abort `encode_frame`.
    /// `build_image_plane` now treats that `None` as a graceful skip.
    #[test]
    fn image_cache_evicts_past_cap_so_peek_can_miss() {
        let mut cache = GpuImageCache::default();
        let mk_decoded = |i: usize| GpuDecodedImage {
            w: 1,
            h: 1,
            rgba: vec![i as u8, 0, 0, 255],
        };
        // Insert one more DISTINCT image than the cache can hold (the >8 case). Each
        // `Arc` is retained so we can look it up by `Arc::ptr_eq` afterwards.
        let n = GpuImageCache::MAX + 1;
        let imgs: Vec<_> = (0..n).map(|_| test_image_data()).collect();
        for (i, img) in imgs.iter().enumerate() {
            cache.put(img.clone(), 1, 1, mk_decoded(i));
        }
        // The cache never exceeds its cap.
        assert!(cache.entries.len() <= GpuImageCache::MAX);
        // The FIRST-inserted image was evicted: a pack loop whose (unbounded)
        // placement map still references it gets `None` here — the fix skips it
        // instead of `.expect()`-panicking.
        assert!(cache.peek(&imgs[0], 1, 1).is_none());
        // The most-recently inserted image is still resident.
        assert!(cache.peek(&imgs[n - 1], 1, 1).is_some());
    }

    /// REGRESSION (inline-image ABA, idx5): two DISTINCT images with the IDENTICAL
    /// footprint must not alias in the decode cache. The old key was a bare
    /// `Arc::as_ptr` value, so a freed image's address reused by a new same-size
    /// image returned the FIRST image's decoded pixels (a stale-render + leak).
    /// Matching the held `Arc` by `Arc::ptr_eq` keeps them separate; holding the
    /// `Arc` also pins the address so it can't actually be reused while cached. We
    /// simulate the reuse here with two live, distinct Arcs of equal footprint.
    #[test]
    fn image_cache_distinguishes_distinct_arcs_of_equal_footprint() {
        let mut cache = GpuImageCache::default();
        let a = test_image_data();
        let b = test_image_data();
        // Cache A's decode under footprint (4, 4).
        cache.put(
            a.clone(),
            4,
            4,
            GpuDecodedImage {
                w: 4,
                h: 4,
                rgba: vec![0xAA; 4 * 4 * 4],
            },
        );
        // B is a DIFFERENT image with the SAME footprint: its lookup must MISS, never
        // alias onto A's pixels (the ABA bug returned A's decode here).
        assert!(
            cache.get(&b, 4, 4).is_none(),
            "a distinct Arc must not hit A's cached entry"
        );
        // A still hits, with its own pixels (copy out so the borrow ends before the
        // next `&mut self` call below).
        let a_first = cache.get(&a, 4, 4).expect("A is cached").rgba[0];
        assert_eq!(a_first, 0xAA, "A returns its OWN decode");
        // The SAME Arc at a DIFFERENT footprint also misses (footprint disambiguation
        // is preserved alongside the Arc-identity match).
        assert!(
            cache.get(&a, 8, 8).is_none(),
            "same Arc, different footprint must miss"
        );
    }

    use super::*;
    use aterm_core::terminal::Terminal;
    use aterm_render::Theme;

    #[test]
    fn present_crop_normalizes_only_after_logical_validation() {
        let crop = PresentCrop {
            source_y: 3,
            height: 92,
        };
        assert_eq!(
            normalize_present_crop(crop, 100, 100),
            Some(crop),
            "an entirely resident crop is unchanged"
        );
        assert_eq!(
            normalize_present_crop(crop, 100, 64),
            Some(PresentCrop {
                source_y: 3,
                height: 61,
            }),
            "a valid oversized-grid crop is trimmed to the resident top prefix"
        );

        for malformed in [
            PresentCrop {
                source_y: 0,
                height: 0,
            },
            PresentCrop {
                source_y: 100,
                height: 1,
            },
            PresentCrop {
                source_y: 99,
                height: 2,
            },
            PresentCrop {
                source_y: u32::MAX,
                height: u32::MAX,
            },
        ] {
            assert_eq!(
                normalize_present_crop(malformed, 100, 64),
                None,
                "malformed logical crop {malformed:?} must be rejected"
            );
        }
        assert_eq!(
            normalize_present_crop(
                PresentCrop {
                    source_y: 70,
                    height: 10,
                },
                100,
                64,
            ),
            None,
            "a logically valid interval wholly below the resident prefix is empty"
        );
    }

    #[test]
    fn visible_source_scissor_preserves_signed_odd_crop_intersections() {
        // 7x8 destination centred under a 10x11 raw source gives (-2,-2).
        // Exposing source rows [1,7) therefore starts at destination y=-1 and
        // intersects as y=[0,5); X intersects the entire narrow destination.
        let uniform = present_blit_uniform(
            false,
            None,
            Some(PresentCrop {
                source_y: 1,
                height: 6,
            }),
            10,
            11,
            7,
            8,
            0,
        );
        assert_eq!(uniform.content_off, [-2.0, -2.0]);
        assert_eq!(visible_source_scissor(&uniform, 7, 8), Some((0, 0, 7, 5)));

        let wholly_above = present_blit_uniform(
            false,
            None,
            Some(PresentCrop {
                source_y: 0,
                height: 1,
            }),
            10,
            11,
            1,
            1,
            0,
        );
        assert_eq!(
            visible_source_scissor(&wholly_above, 1, 1),
            None,
            "a fully clipped crown must not issue a zero/underflowed scissor"
        );
    }

    /// A fresh, distinct `Arc<ImageData>` for the decode-cache tests (each call
    /// allocates a NEW `ImageData`, so two calls never share a pointer).
    fn test_image_data() -> std::sync::Arc<aterm_core::grid::extra::ImageData> {
        std::sync::Arc::new(aterm_core::grid::extra::ImageData {
            bytes: Vec::new(),
            format: aterm_core::grid::extra::ImageFormat::Unknown,
            cols: 1,
            rows: 1,
            z_index: 0,
        })
    }

    /// `pack_image_plane`'s `dw == tw` single-memcpy fast path must produce
    /// BYTE-IDENTICAL output to a naive row-by-row pack — the invariant the GPU
    /// build path relies on (also guarded end-to-end by `inline_image_parity`).
    #[test]
    fn pack_image_plane_fast_path_matches_per_row() {
        // Reference: always copy row by row (the pre-optimization path).
        fn per_row(items: &[(u32, u32, u32, &[u8])], tw: u32, th: u32) -> Vec<u8> {
            let mut data = vec![0u8; (tw * th * 4) as usize];
            for &(y0, dw, dh, rgba) in items {
                for y in 0..dh {
                    let src = (y * dw * 4) as usize;
                    let dst = ((y0 + y) * tw) as usize * 4;
                    data[dst..dst + (dw * 4) as usize]
                        .copy_from_slice(&rgba[src..src + (dw * 4) as usize]);
                }
            }
            data
        }
        // Deterministic raster: distinct bytes per pixel so a mis-copy is visible.
        let raster = |w: u32, h: u32, seed: u8| -> Vec<u8> {
            (0..(w * h * 4))
                .map(|i| (i as u8).wrapping_add(seed))
                .collect()
        };
        let a = raster(7, 5, 1);
        // Case 1: single full-width image (dw == tw -> fast path).
        let items = [(0u32, 7u32, 5u32, a.as_slice())];
        assert_eq!(
            GpuRenderer::pack_image_plane(&items, 7, 5),
            per_row(&items, 7, 5)
        );
        // Case 2: two stacked same-width images (both hit the fast path).
        let b = raster(7, 3, 9);
        let items = [(0u32, 7, 5, a.as_slice()), (5u32, 7, 3, b.as_slice())];
        assert_eq!(
            GpuRenderer::pack_image_plane(&items, 7, 8),
            per_row(&items, 7, 8)
        );
        // Case 3: a narrow image under a wider plane (dw < tw -> else path).
        let narrow = raster(3, 4, 4);
        let wide = raster(7, 2, 7);
        let items = [
            (0u32, 7, 2, wide.as_slice()),
            (2u32, 3, 4, narrow.as_slice()),
        ];
        assert_eq!(
            GpuRenderer::pack_image_plane(&items, 7, 6),
            per_row(&items, 7, 6)
        );
    }

    /// SACRED CONSTRAINT (rendering architecture): the GPU consumes the CPU
    /// renderer's EXACT glyph bytes. For every glyph of a representative frame
    /// (the parity-suite demo grid: red RR, blue bg, CJK 日本 via fallback,
    /// inverse XX, plain ab, cursor), every texel the atlas holds for that
    /// glyph must equal the CPU cache byte — exact, not within tolerance.
    /// Pure CPU: `build_atlas` needs no GPU device, so this runs headless.
    #[test]
    fn atlas_texel_bytes_match_cpu_glyph_bytes_exactly() {
        let Some(mut cpu) = Renderer::from_system(18.0, Theme::default()) else {
            eprintln!("SKIP: no system monospace font");
            return;
        };

        let (rows, cols) = (6usize, 12usize);
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process(
            b"\x1b[31mRR\x1b[0m\r\n\
\x1b[44m  \x1b[0m\r\n\
\xe6\x97\xa5\xe6\x9c\xac\r\n\
\x1b[7mXX\x1b[0m\r\n\
ab\r\n",
        );

        // The same key set encode_frame builds for this frame.
        let input = term.cell_frame(rows, cols);
        let mut keys: BTreeSet<GlyphKey> = BTreeSet::new();
        for cells in &input.cells {
            for cell in cells.iter().take(cols) {
                if GpuRenderer::drawable(cell) {
                    keys.insert(cpu.glyph_key(cell.ch));
                }
            }
        }
        assert!(
            keys.len() >= 5,
            "demo frame should contribute several glyphs"
        );

        let atlas = build_atlas(&mut cpu, &keys, u32::MAX);
        for &key in &keys {
            let slot = atlas
                .map
                .get(&key)
                .expect("every requested glyph gets a slot");
            let img = cpu.glyph_image(key).clone();
            let GlyphImage::Mono {
                width,
                height,
                bytes,
                ..
            } = &img
            else {
                panic!("char keys rasterize as Mono");
            };
            if *width == 0 || *height == 0 {
                assert_eq!(
                    (slot.gw, slot.gh),
                    (0, 0),
                    "empty glyph must pack as an empty slot"
                );
                continue;
            }
            assert_eq!(
                (slot.gw as usize, slot.gh as usize),
                (*width, *height),
                "slot size differs from CPU bitmap for {:?}",
                key.chr()
            );
            for j in 0..slot.gh {
                for i in 0..slot.gw {
                    let atlas_byte =
                        atlas.data[((slot.ay + j) * atlas.width + slot.ax + i) as usize];
                    let cpu_byte = bytes[(j * slot.gw + i) as usize];
                    assert_eq!(
                        atlas_byte,
                        cpu_byte,
                        "atlas texel ({i},{j}) of {:?} differs from the CPU cache byte",
                        key.chr()
                    );
                }
            }
        }
    }

    /// REGRESSION (idx3): the shelf packer bounds the packed HEIGHT (rolled back /
    /// capped at `cap_h` in `build_kind`/`grow_atlas`) but historically NOT the
    /// WIDTH. A single glyph wider than the atlas (`gw + pad > width`) has no shelf
    /// to wrap onto: the old code reset `px=0` and recorded a slot with `gw > width`,
    /// so `blit`'s per-row `copy_from_slice` of `gw*bpp` bytes ran PAST the row
    /// stride — corrupting the next shelf (1024 < gw <= 2048) or panicking with
    /// `dst + row_bytes > data.len()` (gw > 2048). `place` now degrades an over-wide
    /// glyph exactly like the non-packable/empty branch: Mono records a zero slot
    /// (the lookup still resolves so the cell paints its background), Color skips,
    /// and the cursor is left untouched. Pure CPU — no GPU/font needed.
    #[test]
    fn atlas_place_degrades_glyph_wider_than_atlas() {
        let px_q = GlyphKey::quantize_px(18.0);
        // Over-wide by more than 2x the atlas width: the class that used to PANIC in
        // `blit` (`dst + row_bytes` past `data.len()`), and huge enough that a naive
        // `gw + pad` could wrap u32 — the guard uses `width.saturating_sub(pad)`.
        let ow = (ATLAS_WIDTH as usize) * 2 + 100;
        let overwide = GlyphImage::Mono {
            width: ow,
            height: 10,
            xmin: 3,
            ymin: 7,
            advance: 12.0,
            bytes: vec![0xAB; ow * 10],
        };

        // --- Mono atlas: over-wide glyph degrades to a zero slot, cursor unmoved. ---
        let mut mono = Atlas {
            kind: AtlasKind::Mono,
            width: ATLAS_WIDTH,
            height: 1,
            data: vec![0u8; ATLAS_WIDTH as usize],
            map: FxHashMap::default(),
            px: 0,
            py: 0,
            shelf_h: 0,
        };
        let key = GlyphKey::mono_char(
            aterm_render::FaceId::Primary,
            'W',
            aterm_render::StyleBits::REGULAR,
            px_q,
        );
        assert!(
            mono.place(key, &overwide).is_none(),
            "an over-wide glyph must not claim a packed band"
        );
        // Cursor untouched — no phantom shelf advance/wrap that would corrupt later
        // packing.
        assert_eq!(
            (mono.px, mono.py, mono.shelf_h),
            (0, 0, 0),
            "cursor unmoved"
        );
        // Mono records a ZERO slot so lookups still resolve, preserving the font's
        // placement offsets so the cell paints its background correctly.
        let slot = *mono.map.get(&key).expect("Mono records every key");
        assert_eq!((slot.gw, slot.gh), (0, 0), "degraded slot is zero-sized");
        assert_eq!(
            (slot.xmin, slot.ymin),
            (3, 7),
            "placement offsets preserved"
        );
        // Blitting the zero slot is a no-op and must not touch `data` (no overflow).
        let before = mono.data.clone();
        mono.blit(&overwide, &slot);
        assert_eq!(mono.data, before, "zero-slot blit writes nothing");
        // A subsequent NORMAL glyph still packs at the origin — proof the over-wide
        // attempt left the packer state intact.
        let normal = GlyphImage::Mono {
            width: 5,
            height: 8,
            xmin: 0,
            ymin: 0,
            advance: 6.0,
            bytes: vec![0xFF; 5 * 8],
        };
        let nkey = GlyphKey::mono_char(
            aterm_render::FaceId::Primary,
            'x',
            aterm_render::StyleBits::REGULAR,
            px_q,
        );
        assert_eq!(
            mono.place(nkey, &normal),
            Some((0, 8)),
            "a normal glyph still packs at the origin"
        );
        let nslot = *mono.map.get(&nkey).expect("normal glyph slot");
        assert_eq!((nslot.ax, nslot.ay, nslot.gw, nslot.gh), (0, 0, 5, 8));

        // --- Color atlas: over-wide glyph is skipped entirely (no slot recorded). ---
        let mut color = Atlas {
            kind: AtlasKind::Color,
            width: ATLAS_WIDTH,
            height: 1,
            data: vec![0u8; (ATLAS_WIDTH * 4) as usize],
            map: FxHashMap::default(),
            px: 0,
            py: 0,
            shelf_h: 0,
        };
        let overwide_rgba = GlyphImage::Rgba {
            width: ow,
            height: 10,
            xmin: 0,
            ymin: 0,
            advance: 12.0,
            bytes: vec![0u8; ow * 10 * 4],
        };
        let ckey = GlyphKey::rgba_char(aterm_render::FaceId::Primary, '\u{1F600}', px_q);
        assert!(
            color.place(ckey, &overwide_rgba).is_none(),
            "an over-wide colour glyph must not claim a packed band"
        );
        assert_eq!(
            (color.px, color.py, color.shelf_h),
            (0, 0, 0),
            "cursor unmoved"
        );
        assert!(
            color.map.is_empty(),
            "Color skips an over-wide glyph entirely (no slot)"
        );
    }

    /// G-1 fix gate: the glyph atlas is PERSISTED across frames. Two consecutive
    /// `render_input` calls with an UNCHANGED glyph set must NOT create a new
    /// atlas texture (the steady state — incl. idle cursor-blink ticks — reuses
    /// the resident textures + bind groups untouched). Asserted two ways: the
    /// texture-creation counter does not advance, and the resident texture dims
    /// are byte-identical between the frames (same textures, not recreated ones).
    /// Gated: no GPU/font -> skip cleanly.
    #[test]
    fn set_font_theme_keeps_configured_family() {
        // Regression (the multi-window/splits merge LOST this): the in-place GPU
        // rebuild (zoom / config hot-reload / Retina auto-scale) must re-resolve the
        // CONFIGURED font family, not the system monospace. Before the fix
        // `set_font_theme` called the family-LESS `from_system`, so a configured
        // family was silently dropped on the first Retina-forced rebuild. Construct
        // WITH a family, rebuild via set_font_theme, and confirm the family is wired
        // onto GpuRenderer and the rebuild leaves a valid renderer. (A face-name
        // assertion would need a Renderer resolved-family accessor — a follow-up.)
        let theme = Theme::default();
        let mut gpu = match GpuRenderer::new_with_family(Some("Menlo"), 16.0, theme) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("SKIP: no GPU/font available: {e}");
                return;
            }
        };
        assert_eq!(
            gpu.font_family.as_deref(),
            Some("Menlo"),
            "family wired at construction"
        );
        gpu.set_font_theme(24.0, theme)
            .expect("in-place rebuild succeeds with a configured family");
        assert_eq!(
            gpu.font_family.as_deref(),
            Some("Menlo"),
            "family retained across rebuild"
        );
        let (cw, ch) = gpu.cell_size();
        assert!(
            cw > 0 && ch > 0,
            "renderer valid after the family-aware rebuild"
        );
    }

    #[test]
    fn set_font_family_theme_changes_the_resolved_family_atomically() {
        // Fix for the GPU font-family no-op: a live `font_family` config change must
        // reach the GPU face. Before, `font_family` was frozen at construction with
        // no setter, so a reload re-resolved the OLD family and the text looked
        // unchanged. `set_font_family_theme` must now adopt the new family and
        // face as one commit. Switching to `None` (system monospace, always
        // resolvable) keeps this cross-platform.
        let theme = Theme::default();
        let mut gpu = match GpuRenderer::new_with_family(Some("Menlo"), 16.0, theme) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("SKIP: no GPU/font available: {e}");
                return;
            }
        };
        assert_eq!(gpu.font_family.as_deref(), Some("Menlo"));
        // The fix: resolve and commit the new family + face as one operation.
        gpu.set_font_family_theme(None, 16.0, theme)
            .expect("rebuild succeeds with the updated family");
        assert_eq!(
            gpu.font_family.as_deref(),
            None,
            "the changed family is adopted, not the frozen construction-time one"
        );
        let (cw, ch) = gpu.cell_size();
        assert!(cw > 0 && ch > 0, "renderer valid after the family change");
    }

    #[test]
    fn failed_font_family_theme_resolution_keeps_complete_gpu_authority() {
        let theme = Theme::default();
        let mut gpu = match GpuRenderer::new_with_family(Some("Menlo"), 16.0, theme) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("SKIP: no GPU/font available: {e}");
                return;
            }
        };
        let old_family = gpu.font_family.clone();
        let old_theme = gpu.theme;
        let old_cell = gpu.cell_size();
        let rejected_theme = Theme {
            fg: theme.fg ^ 0x00ff_ffff,
            bg: theme.bg ^ 0x00ff_ffff,
            cursor: theme.cursor ^ 0x00ff_ffff,
            selection: theme.selection ^ 0x00ff_ffff,
        };

        let result = gpu.set_font_family_theme_with(
            Some("candidate-family".to_string()),
            31.0,
            rejected_theme,
            |_, _, _| None,
        );
        assert!(
            result.is_err(),
            "injected font discovery failure reaches caller"
        );
        assert_eq!(gpu.font_family, old_family, "family was not staged early");
        assert_eq!(
            (
                gpu.theme.fg,
                gpu.theme.bg,
                gpu.theme.cursor,
                gpu.theme.selection
            ),
            (
                old_theme.fg,
                old_theme.bg,
                old_theme.cursor,
                old_theme.selection
            ),
            "theme was not staged early"
        );
        assert_eq!(gpu.cell_size(), old_cell, "the old face remains live");
    }

    #[test]
    fn set_face_preserves_text_shaping() {
        // A face rebuild (zoom / reload / Retina) must NOT silently revert the user's
        // ligature/feature choice. set_face carries the prior shaping onto the new
        // face. Build with ligatures OFF, rebuild, and confirm it stays OFF.
        let theme = Theme::default();
        let mut gpu = match GpuRenderer::new_with_family(Some("Menlo"), 16.0, theme) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("SKIP: no GPU/font available: {e}");
                return;
            }
        };
        gpu.set_text_shaping(aterm_render::TextShapingConfig {
            ligature_mode: aterm_types::text_shaping::LigatureMode::Disabled,
            ..Default::default()
        });
        gpu.set_font_theme(24.0, theme).expect("in-place rebuild");
        assert_eq!(
            gpu.cpu.text_shaping().ligature_mode,
            aterm_types::text_shaping::LigatureMode::Disabled,
            "shaping must survive the face rebuild, not reset to Enabled"
        );
    }

    /// REGRESSION (FIX B): `ensure_deco_atlas` capped only the atlas WIDTH
    /// (`atlas_w = DECO_GLYPHS.len()·cw > MAX_DECO_ATLAS_W`), NOT the HEIGHT (`ch`,
    /// the `Extent3d.height` it hands `create_texture`). A font whose cell height
    /// exceeds `max_texture_dimension_2d` — while `atlas_w` stays small, so the width
    /// guard doesn't fire — aborted the process (no uncaptured-error handler). The
    /// height guard now DROPS the atlas (`deco_atlas = None`), the same graceful
    /// fallback the width-overflow path uses. Gated (needs a GPU device).
    #[test]
    fn deco_atlas_drops_on_oversized_cell_height_no_abort() {
        let mut gpu = match GpuRenderer::new(18.0, Theme::default()) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("SKIP: no GPU/font available: {e}");
                return;
            }
        };
        let (cw, ch) = gpu.cell_size();
        // A realistic cell BUILDS the atlas — the guard must NOT over-trigger (parity).
        gpu.ensure_deco_atlas(cw, ch);
        assert!(
            gpu.deco_atlas.is_some(),
            "a realistic {cw}x{ch} cell must build the deco atlas (guard must not over-trigger)"
        );
        // Oversized HEIGHT with an unchanged (small) width: the WIDTH guard does NOT
        // fire, so pre-fix this reached `create_texture` with `height = 10_000`, which
        // exceeds both the downlevel (2048) AND native (`Limits::default()` == 8192)
        // `max_texture_dimension_2d` → abort. Post-fix the `ch` cap drops the atlas.
        gpu.ensure_deco_atlas(cw, 10_000);
        assert!(
            gpu.deco_atlas.is_none(),
            "an oversized cell height must DROP the deco atlas (graceful), not abort"
        );
    }

    #[test]
    fn atlas_persists_across_unchanged_frames() {
        let theme = Theme::default();
        let px = 18.0;
        let mut gpu = match GpuRenderer::new(px, theme) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("SKIP: no GPU/font available: {e}");
                return;
            }
        };

        // A small static grid (cursor hidden so a blink phase can't change the
        // glyph set), then the EXACT same input rendered again.
        let (rows, cols) = (3usize, 8usize);
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process(b"\x1b[?25lhello\r\nworld\r\nabcd");
        let input = term.cell_frame(rows, cols);
        let mut win = WindowGpu::new();

        // Frame 1: cold cache -> the atlases are built (both textures created).
        let _ = gpu.render_input(&mut win, &input, None);
        let after_first = gpu.atlas_tex_creations();
        let dims_first = gpu
            .atlas_tex_dims()
            .expect("atlases resident after first frame");
        assert!(
            after_first >= 1,
            "first frame should have created at least one atlas texture"
        );

        // Frame 2: identical glyph set -> reuse, NO new texture creation.
        let _ = gpu.render_input(&mut win, &input, None);
        let after_second = gpu.atlas_tex_creations();
        let dims_second = gpu.atlas_tex_dims().expect("atlases still resident");

        assert_eq!(
            after_first, after_second,
            "an unchanged-glyph frame must NOT create a new atlas texture \
             (creations {after_first} -> {after_second}) — the atlas is not persisting"
        );
        assert_eq!(
            dims_first, dims_second,
            "resident atlas texture dims changed across an unchanged frame — textures were recreated"
        );

        // And a THIRD identical frame is still a no-op (steady state holds).
        let _ = gpu.render_input(&mut win, &input, None);
        assert_eq!(
            after_second,
            gpu.atlas_tex_creations(),
            "repeated unchanged frames must keep reusing the resident atlas"
        );
    }

    /// Companion to the persistence test: introducing a NEW glyph must NOT force
    /// a full repack into a new texture in the common case — it APPENDS into the
    /// resident atlas (incremental growth) via a sub-region upload, so the
    /// texture identity (dims) is unchanged and no new texture is created. Only
    /// genuine overflow recreates. Gated: no GPU/font -> skip.
    #[test]
    fn new_glyph_grows_atlas_in_place_without_recreating_texture() {
        let theme = Theme::default();
        let px = 18.0;
        let mut gpu = match GpuRenderer::new(px, theme) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("SKIP: no GPU/font available: {e}");
                return;
            }
        };
        let (rows, cols) = (2usize, 8usize);

        let mut win = WindowGpu::new();
        let render = |gpu: &mut GpuRenderer, win: &mut WindowGpu, bytes: &[u8]| {
            let mut term = Terminal::new(rows as u16, cols as u16);
            term.process(bytes);
            let input = term.cell_frame(rows, cols);
            gpu.render_input(win, &input, None);
        };

        // Cold frame with a few glyphs.
        render(&mut gpu, &mut win, b"\x1b[?25labc");
        let creations_after_cold = gpu.atlas_tex_creations();
        let dims_after_cold = gpu.atlas_tex_dims().expect("atlases resident");

        // A frame adding NEW glyphs (xyz) on top of the resident set: the mono
        // atlas grows in place. The R8 atlas is 1024 wide with vast vertical
        // headroom, so three more small glyphs append without overflow — no new
        // texture, same dims.
        render(&mut gpu, &mut win, b"\x1b[?25labcxyz");
        assert_eq!(
            creations_after_cold,
            gpu.atlas_tex_creations(),
            "appending a few new glyphs must grow the atlas in place, not recreate the texture"
        );
        assert_eq!(
            dims_after_cold,
            gpu.atlas_tex_dims().expect("atlases still resident"),
            "incremental growth must keep the SAME atlas texture (dims unchanged)"
        );
    }

    /// FIX 3 gate: `present_prev`/`prev_input` are PER-WINDOW. Two `WindowGpu`
    /// driven INTERLEAVED through the scissored present-readback path against ONE
    /// shared `GpuRenderer` (at equal dims) must each read back byte-identical to a
    /// FRESH FULL render of THAT window's own input. If the prior-frame state were
    /// shared on the renderer, window B's present would diff against window A's last
    /// frame: the scissor would Load A's pixels and repaint only the rows that
    /// differ between A and B, leaking A's content into B's readback. Per-window
    /// state means each window diffs only against its OWN prior frame. Gated: no
    /// GPU/font -> skip cleanly.
    #[test]
    fn present_prev_is_per_window_no_cross_window_leak() {
        let theme = Theme::default();
        let px = 18.0;
        let mut gpu = match GpuRenderer::new(px, theme) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("SKIP: no GPU/font available: {e}");
                return;
            }
        };

        let (rows, cols) = (4usize, 10usize);
        // Two DISTINCT frame sequences per window. A prev-frame leak between the two
        // windows would only manifest when the two windows hold DIFFERENT content,
        // so make every interleaved frame differ from the other window's frame and
        // from this window's own prior frame (so the scissor path is exercised).
        let frame = |bytes: &[u8]| {
            let mut term = Terminal::new(rows as u16, cols as u16);
            term.process(b"\x1b[?25l");
            term.process(bytes);
            term.cell_frame(rows, cols)
        };
        let a_frames = [
            frame(b"AAAA"),
            frame(b"AAAA\r\nbbbb"),
            frame(b"AAAA\r\ncccc"),
        ];
        let b_frames = [
            frame(b"ZZZZ"),
            frame(b"ZZZZ\r\nyyyy"),
            frame(b"ZZZZ\r\nxxxx"),
        ];

        let mut win_a = WindowGpu::new();
        let mut win_b = WindowGpu::new();

        // Drive the two windows interleaved through the SCISSORED present path on
        // the ONE shared renderer: A0, B0, A1, B1, A2, B2.
        let mut last_a = None;
        let mut last_b = None;
        for i in 0..a_frames.len() {
            last_a = Some(gpu.present_input_readback(&mut win_a, &a_frames[i]));
            last_b = Some(gpu.present_input_readback(&mut win_b, &b_frames[i]));
        }
        let got_a = last_a.expect("at least one frame driven");
        let got_b = last_b.expect("at least one frame driven");

        // The SCISSOR path must actually have fired (else this proves nothing about
        // cross-window diffing): the second+ frame of each window is a stable-dims
        // change, which takes the scissor.
        assert!(
            gpu.scissor_taken() > 0,
            "scissor path must be exercised by the changed frames"
        );

        // Ground truth: a FRESH full render of each window's FINAL input on its own
        // clean window. `render_input` always clears + draws every row, so it is the
        // leak-free reference. Use fresh windows so no prior-frame state is consulted.
        let mut ref_win_a = WindowGpu::new();
        let mut ref_win_b = WindowGpu::new();
        let want_a = gpu.render_input(&mut ref_win_a, &a_frames[a_frames.len() - 1], None);
        let want_b = gpu.render_input(&mut ref_win_b, &b_frames[b_frames.len() - 1], None);

        assert_eq!(
            (got_a.width, got_a.height),
            (want_a.width, want_a.height),
            "window A readback dims must match the reference"
        );
        assert_eq!(
            got_a.pixels, want_a.pixels,
            "window A's interleaved scissored readback must be byte-identical to a fresh full \
             render — a mismatch means window B's prior frame leaked into window A"
        );
        assert_eq!(
            (got_b.width, got_b.height),
            (want_b.width, want_b.height),
            "window B readback dims must match the reference"
        );
        assert_eq!(
            got_b.pixels, want_b.pixels,
            "window B's interleaved scissored readback must be byte-identical to a fresh full \
             render — a mismatch means window A's prior frame leaked into window B"
        );
    }

    /// REGRESSION: the scissored dirty-row present path must fire at the GUI's REAL
    /// interior padding (`pad > 0`), not only at `pad == 0`. The scissor gate sizes
    /// `(w, h)` to compare against the offscreen, which `encode_frame` creates at
    /// PADDED dims (`cols*cw + 2*pad`); if the gate used the unpadded size the
    /// comparison would NEVER match when `pad > 0` and every present would silently
    /// fall back to a Full repaint, defeating the optimization in the windowed GUI.
    /// This drives the production present path at `pad = 14` and asserts (a) the
    /// scissor actually fires and (b) the scissored readback is byte-identical to a
    /// fresh full render — so the dead-scissor regression cannot return unnoticed.
    #[test]
    fn scissored_present_fires_and_is_correct_at_nonzero_pad() {
        let theme = Theme::default();
        let px = 18.0;
        let mut gpu = match GpuRenderer::new(px, theme) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("SKIP: no GPU/font available: {e}");
                return;
            }
        };
        // The GUI runs at pad = pad_for_scale(scale) > 0; exercise that regime.
        gpu.set_pad(14);
        assert!(gpu.pad() > 0, "precondition: this test exercises pad > 0");

        let (rows, cols) = (4usize, 10usize);
        let frame = |bytes: &[u8]| {
            let mut term = Terminal::new(rows as u16, cols as u16);
            term.process(b"\x1b[?25l");
            term.process(bytes);
            term.cell_frame(rows, cols)
        };
        // Same dims, changing content: frame 2+ is a stable-dims change → scissor.
        let frames = [
            frame(b"AAAA"),
            frame(b"AAAA\r\nbbbb"),
            frame(b"AAAA\r\ncccc"),
        ];

        let mut win = WindowGpu::new();
        let mut last = None;
        for f in &frames {
            last = Some(gpu.present_input_readback(&mut win, f));
        }
        let got = last.expect("at least one frame driven");

        // The whole point: with padded gate dims the scissor MUST fire at pad>0.
        // Before the fix this was 0 (offscreen_holds_prev always false) and the
        // present silently full-repainted every frame.
        assert!(
            gpu.scissor_taken() > 0,
            "scissor must fire at pad>0 (got scissor_taken={}, full_repaints={})",
            gpu.scissor_taken(),
            gpu.full_repaints(),
        );

        // Correctness: the scissored band must be byte-identical to a fresh full
        // render of the same final input on a clean window (the leak-free reference).
        let mut ref_win = WindowGpu::new();
        let want = gpu.render_input(&mut ref_win, &frames[frames.len() - 1], None);
        assert_eq!(
            (got.width, got.height),
            (want.width, want.height),
            "padded scissored readback dims must match the full-render reference"
        );
        assert_eq!(
            got.pixels, want.pixels,
            "padded scissored readback must be byte-identical to a fresh full render"
        );
    }

    /// A top-pad-only transition keeps the offscreen dimensions unchanged. Both
    /// the scissored-present state and the readback gate must stamp `grid_top`, or
    /// they will preserve/return pixels at the prior Y origin indefinitely.
    #[test]
    fn asymmetric_top_pad_change_invalidates_gpu_present_and_readback_caches() {
        let model = aterm_spec::derive::asymmetric_pad_layout_model();
        let picked = model
            .successors("PickLayout", &model.init_state())
            .into_iter()
            .find(|state| {
                state["pad"] == 3
                    && state["head"] == 1
                    && state["initial_request"] == 3
                    && state["changed_request"] == 0
            })
            .expect("bounded GPU layout fixture");
        let initial = model.successors("ApplyInitialTop", &picked)[0].clone();
        let primed = model.successors("PrimeLayoutCache", &initial)[0].clone();
        let changed = model.successors("ApplyChangedTop", &primed)[0].clone();
        let decided = model.successors("RenderWithLayoutCache", &changed)[0].clone();
        assert_eq!((decided["cache_hit"], decided["full_repaint"]), (0, 1));

        let mut gpu = match GpuRenderer::new(18.0, Theme::default()) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("SKIP: no GPU/font available: {e}");
                return;
            }
        };
        gpu.set_pad(picked["pad"] as usize);
        gpu.set_head(picked["head"] as usize);
        gpu.set_pad_top(picked["initial_request"] as usize);
        assert_eq!(gpu.pad_top(), initial["pad_top"] as usize);
        gpu.set_pad(picked["pad"] as usize);
        assert_eq!(
            gpu.pad_top(),
            initial["pad_top"] as usize,
            "the GPU delegate must preserve an asymmetric origin on same-value set_pad"
        );
        let (rows, cols) = (4usize, 12usize);
        let frame_size = gpu.frame_size(rows, cols);
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process(b"\x1b[?25lgrid-origin");
        let input = term.cell_frame(rows, cols);
        let mut win = WindowGpu::new();

        let first = gpu.present_input_readback(&mut win, &input);
        let initial_projection = gpu.project_asymmetric_pad_layout(&win);
        assert_eq!(
            initial_projection.present_cached_grid_top,
            Some(primed["cached_grid_top"] as usize)
        );
        assert_eq!(initial_projection.grid_top, initial["grid_top"] as usize);
        let full_before = gpu.full_repaints();
        gpu.set_pad_top(picked["changed_request"] as usize);
        assert_eq!(gpu.frame_size(rows, cols), frame_size);
        let stale_present = gpu.project_asymmetric_pad_layout(&win);
        assert_eq!(stale_present.grid_top, changed["grid_top"] as usize);
        assert_eq!(
            stale_present.present_cached_grid_top,
            Some(changed["cached_grid_top"] as usize)
        );
        assert_ne!(
            stale_present.present_cached_grid_top,
            Some(stale_present.grid_top)
        );

        // NEGATIVE CONTROL: same content + dimensions make the pre-fix
        // dimension-only key report a hit, which the model rejects because the
        // cached and current grid origins differ.
        let dimension_only_mutant_hit = gpu.frame_size(rows, cols) == frame_size;
        assert!(dimension_only_mutant_hit);
        assert_ne!(
            usize::from(dimension_only_mutant_hit),
            decided["cache_hit"] as usize
        );

        let moved = gpu.present_input_readback(&mut win, &input);
        assert!(
            gpu.full_repaints() > full_before,
            "grid-origin change must force a full GPU repaint"
        );
        let moved_projection = gpu.project_asymmetric_pad_layout(&win);
        assert_eq!(
            moved_projection.present_cached_grid_top,
            Some(changed["grid_top"] as usize)
        );
        let mut reference = WindowGpu::new();
        let fresh = gpu.render_input(&mut reference, &input, None);
        assert_eq!(moved.pixels, fresh.pixels);
        assert_ne!(first.pixels, moved.pixels, "the visible grid must move");

        // Exercise the separate readback gate as well: prime at one origin, move
        // without changing input/dimensions, and require the fresh-origin pixels.
        let mut gate_win = WindowGpu::new();
        gpu.set_pad_top(picked["initial_request"] as usize);
        let gated_first = gpu
            .render_input_cached(&mut gate_win, &input)
            .pixels()
            .to_vec();
        let gate_primed = gpu.project_asymmetric_pad_layout(&gate_win);
        assert_eq!(
            gate_primed.gate_cached_grid_top,
            Some(primed["cached_grid_top"] as usize)
        );
        let hits_before = gpu.gate_hits();
        let _ = gpu.render_input_cached(&mut gate_win, &input);
        assert_eq!(gpu.gate_hits(), hits_before + 1, "identical layout may hit");

        gpu.set_pad_top(picked["changed_request"] as usize);
        let stale_gate = gpu.project_asymmetric_pad_layout(&gate_win);
        assert_eq!(stale_gate.grid_top, changed["grid_top"] as usize);
        assert_eq!(
            stale_gate.gate_cached_grid_top,
            Some(changed["cached_grid_top"] as usize)
        );
        let misses_before = gpu.gate_misses();
        let gated_moved = gpu
            .render_input_cached(&mut gate_win, &input)
            .pixels()
            .to_vec();
        assert_eq!(
            gpu.gate_misses(),
            misses_before + 1,
            "origin change must miss the readback gate"
        );
        assert_eq!(gpu.gate_hits(), hits_before + 1);
        assert_eq!(
            gpu.project_asymmetric_pad_layout(&gate_win)
                .gate_cached_grid_top,
            Some(changed["grid_top"] as usize)
        );
        let mut gate_reference = WindowGpu::new();
        let gate_fresh = gpu.render_input(&mut gate_reference, &input, None);
        assert_eq!(gated_moved, gate_fresh.pixels);
        assert_ne!(gated_first, gated_moved);
    }

    /// REGRESSION (typing hot path): with bloom ENABLED (the shipped default) a
    /// glow-carrying frame used to force a FULL re-encode of the whole grid on
    /// every aurora tick. Now it takes the SCISSORED path with the dirty rows
    /// widened to the halo penumbra — and every frame of a spawn → animate →
    /// fade-out → post-fade glow sequence must stay byte-identical to a fresh
    /// FULL render of the same input (the no-ghosting / no-accumulation gate),
    /// including at the config-clamp-max bloom radius. Gated: no GPU/font → skip.
    #[test]
    fn bloom_glow_frames_scissor_and_match_full_render() {
        let mut gpu = match GpuRenderer::new(18.0, Theme::default()) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("SKIP: no GPU/font available: {e}");
                return;
            }
        };
        assert!(gpu.bloom_enabled(), "precondition: bloom defaults ON");
        // The heat shimmer (also default ON) rides the same present seam; PIN
        // its one wall-clock term so the scissored present and the fresh
        // reference compose the identical refraction — this extends the
        // byte-identity law over the whole parity class.
        gpu.set_shimmer_phase_for_test(Some(0.33));
        // The GUI regime: nonzero interior pad.
        gpu.set_pad(14);
        let pad = gpu.pad();
        let (cw, ch) = gpu.cell_size();
        let (rows, cols) = (10usize, 12usize);

        let frame = |bytes: &[u8]| {
            let mut term = Terminal::new(rows as u16, cols as u16);
            term.process(b"\x1b[?25l");
            term.process(bytes);
            term.cell_frame(rows, cols)
        };
        // A single-row WINDOW-ABSOLUTE glow quad (the GlowQuad producer
        // contract: coords carry the grid origin), well clear of the
        // top/bottom edges so the penumbra stays inside the grid.
        let quad = |row: usize, dx: usize| GlowQuad {
            row: row as u16,
            x: (pad + 2 * cw + dx) as u16,
            y: (pad + row * ch + 2) as u16,
            w: cw as u16,
            h: (ch - 4) as u16,
            color: 0x0030_50A0, // premultiplied light
        };

        let run = |gpu: &mut GpuRenderer, label: &str| {
            // spawn → animate (text stable, glow-only change) → fade-out (glow
            // empties) → post-fade (text change, no glow).
            let mut spawn = frame(b"AAAA\r\nbbbb");
            spawn.cursor_glow_add = vec![quad(4, 0)];
            let mut animate = frame(b"AAAA\r\nbbbb");
            animate.cursor_glow_add = vec![quad(4, 3), quad(5, 1)];
            let seq = [
                frame(b"AAAA"),
                spawn,
                animate,
                frame(b"AAAA\r\nbbbb"),
                frame(b"AAAA\r\ncccc"),
            ];
            let mut win = WindowGpu::new();
            let before = gpu.scissor_taken();
            for (i, input) in seq.iter().enumerate() {
                let got = gpu.present_input_readback(&mut win, input);
                // Fresh window ⇒ render_input is a clean FULL repaint reference
                // (bloom composited unscissored over a cleared offscreen).
                let mut ref_win = WindowGpu::new();
                let want = gpu.render_input(&mut ref_win, input, None);
                assert_eq!(
                    (got.width, got.height),
                    (want.width, want.height),
                    "[{label}] frame {i}: dims must match the full-render reference"
                );
                assert_eq!(
                    got.pixels, want.pixels,
                    "[{label}] frame {i}: scissored bloom present must be \
                     byte-identical to a fresh full render (halo ghost/accumulation?)"
                );
            }
            assert!(
                gpu.scissor_taken() > before,
                "[{label}] glow frames must take the scissored path, not force Full \
                 (scissor_taken={}, full_repaints={})",
                gpu.scissor_taken(),
                gpu.full_repaints(),
            );
        };

        run(&mut gpu, "default radius");
        // The config clamp max spreads the widest halo the margin must cover.
        gpu.set_bloom_params(BLOOM_STRENGTH, 8.0);
        run(&mut gpu, "radius 8.0");
    }

    /// A live bloom-radius change MID-SEQUENCE (a `set_bloom_params` between two
    /// glow presents of the SAME persistent window) stays on the SCISSOR path and
    /// is byte-identical to a fresh full render. Change #2 composites the halo at
    /// PRESENT time over a throwaway copy of the clean (bloom-free) offscreen, so
    /// the prior frame's halo never lands in the scissor base and the new radius is
    /// applied fresh every frame — there is no old-radius fringe to strand and no
    /// accumulation, so the pre-#2 conservative Full-repaint fallback on the
    /// transition frame is no longer needed (a strict win: the radius change no
    /// longer forces a full-grid rebuild). Byte-identity against a fresh full render
    /// is the correctness gate; the scissor count pins that the fast path held.
    /// Gated: no GPU/font → skip.
    #[test]
    fn bloom_radius_change_mid_sequence_scissors_byte_identical() {
        let mut gpu = match GpuRenderer::new(18.0, Theme::default()) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("SKIP: no GPU/font available: {e}");
                return;
            }
        };
        assert!(gpu.bloom_enabled(), "precondition: bloom defaults ON");
        // The heat shimmer (also default ON) rides the same present seam; PIN
        // its one wall-clock term so the scissored present and the fresh
        // reference compose the identical refraction — this extends the
        // byte-identity law over the whole parity class.
        gpu.set_shimmer_phase_for_test(Some(0.33));
        gpu.set_pad(14);
        let pad = gpu.pad();
        let (cw, ch) = gpu.cell_size();
        let (rows, cols) = (10usize, 12usize);
        let frame = |bytes: &[u8]| {
            let mut term = Terminal::new(rows as u16, cols as u16);
            term.process(b"\x1b[?25l");
            term.process(bytes);
            term.cell_frame(rows, cols)
        };
        // WINDOW-ABSOLUTE quads (the producer contract), as above.
        let quad = |row: usize, dx: usize| GlowQuad {
            row: row as u16,
            x: (pad + 2 * cw + dx) as u16,
            y: (pad + row * ch + 2) as u16,
            w: cw as u16,
            h: (ch - 4) as u16,
            color: 0x0030_50A0,
        };
        let mut glow_a = frame(b"AAAA\r\nbbbb");
        glow_a.cursor_glow_add = vec![quad(4, 0)];
        // Same TEXT, same glow rows — only the radius changes between the two
        // presents, so `compute_dirty_rows` alone would see a scissorable frame.
        let mut glow_b = frame(b"AAAA\r\nbbbb");
        glow_b.cursor_glow_add = vec![quad(4, 3), quad(5, 1)];

        let mut win = WindowGpu::new();
        // Prime the persistent window with the first glow frame at the default
        // radius, then flip the radius before the second present.
        let _ = gpu.present_input_readback(&mut win, &glow_a);
        gpu.set_bloom_params(BLOOM_STRENGTH, 8.0);
        let scissor_before = gpu.scissor_taken();

        let got = gpu.present_input_readback(&mut win, &glow_b);
        let mut ref_win = WindowGpu::new();
        let want = gpu.render_input(&mut ref_win, &glow_b, None);
        assert_eq!(
            (got.width, got.height),
            (want.width, want.height),
            "radius-change transition frame: dims must match the full-render reference"
        );
        assert_eq!(
            got.pixels, want.pixels,
            "a mid-sequence radius change must be byte-identical to a fresh full \
             render (present-time halo recomputed at the new radius over a clean base)"
        );
        assert!(
            gpu.scissor_taken() > scissor_before,
            "with present-time bloom (#2) the radius-change frame must stay on the \
             SCISSOR path — no forced full repaint from an old-radius fringe"
        );
    }

    /// The bloom scissor at the grid edge: a glow quad whose halo spills past
    /// the grid's top edge (cursor on row 0) keeps the SCISSORED repaint — a
    /// row-0 band extends the scissor over the whole top strip and the
    /// strip-reset quads re-establish it from bg, so Full and scissored frames
    /// compose the spilled light identically (no accumulation). Byte-identity
    /// against a fresh full render is the gate. Gated: no GPU/font → skip.
    #[test]
    fn bloom_glow_at_grid_edge_scissors_and_matches() {
        let mut gpu = match GpuRenderer::new(18.0, Theme::default()) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("SKIP: no GPU/font available: {e}");
                return;
            }
        };
        assert!(gpu.bloom_enabled(), "precondition: bloom defaults ON");
        // The heat shimmer (also default ON) rides the same present seam; PIN
        // its one wall-clock term so the scissored present and the fresh
        // reference compose the identical refraction — this extends the
        // byte-identity law over the whole parity class.
        gpu.set_shimmer_phase_for_test(Some(0.33));
        gpu.set_pad(14);
        let pad = gpu.pad();
        let (cw, ch) = gpu.cell_size();
        let (rows, cols) = (6usize, 10usize);
        let frame = |bytes: &[u8]| {
            let mut term = Terminal::new(rows as u16, cols as u16);
            term.process(b"\x1b[?25l");
            term.process(bytes);
            term.cell_frame(rows, cols)
        };
        let mut with_edge_glow = frame(b"AAAA\r\nbbbb");
        // WINDOW-ABSOLUTE quad hugging the grid's top edge: the penumbra
        // spills above the grid into the pad strip.
        with_edge_glow.cursor_glow_add = vec![GlowQuad {
            row: 0,
            x: (pad + cw) as u16,
            y: (pad + 1) as u16,
            w: cw as u16,
            h: (ch - 2) as u16,
            color: 0x0030_50A0,
        }];
        let seq = [frame(b"AAAA"), with_edge_glow, frame(b"AAAA\r\nbbbb")];
        let mut win = WindowGpu::new();
        let scissored_before = gpu.scissor_taken();
        for (i, input) in seq.iter().enumerate() {
            let got = gpu.present_input_readback(&mut win, input);
            let mut ref_win = WindowGpu::new();
            let want = gpu.render_input(&mut ref_win, input, None);
            assert_eq!(
                got.pixels, want.pixels,
                "frame {i}: edge-glow present must be byte-identical to a full render"
            );
        }
        // Both glow frames (edge spawn + fade-out against an edge prev) STAY on
        // the scissored path: the row-0 band opens the top strip and the
        // strip-reset quads rebuild it from bg, so the edge spill needs no Full
        // fallback — the byte-identity loop above is the contract; this pins
        // that the fast path is actually exercised.
        assert_eq!(
            gpu.scissor_taken(),
            scissored_before + 2,
            "edge-spilling halo frames must keep the scissored repaint"
        );
    }

    /// The settings-card upload skip: a present whose card bytes are identical to
    /// the resident texture's must NOT re-run the whole-card `write_texture`
    /// (hover-stable settings frames re-send the same raster every present), while
    /// changed bytes and size changes (texture recreate ⇒ undefined contents) must
    /// always upload. Gated: no GPU/font → skip.
    #[test]
    fn tray_upload_skips_unchanged_card_bytes() {
        let mut gpu = match GpuRenderer::new(18.0, Theme::default()) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("SKIP: no GPU/font available: {e}");
                return;
            }
        };
        let (rows, cols) = (4usize, 10usize);
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process(b"\x1b[?25lhello");
        let input = term.cell_frame(rows, cols);
        let mut win = WindowGpu::new();

        let (pw, ph) = (32u32, 16u32);
        let red: Vec<u8> = [200, 40, 40, 255].repeat((pw * ph) as usize);
        let green: Vec<u8> = [40, 200, 40, 255].repeat((pw * ph) as usize);
        fn tray(rgba: &[u8], pw: u32, ph: u32) -> TrayQuad<'_> {
            TrayQuad {
                rgba,
                pw,
                ph,
                dx: 4,
                dy: 4,
            }
        }

        let first = gpu.render_input(&mut win, &input, Some(tray(&red, pw, ph)));
        assert_eq!(gpu.tray_uploads(), 1, "first card present must upload");

        let second = gpu.render_input(&mut win, &input, Some(tray(&red, pw, ph)));
        assert_eq!(
            gpu.tray_uploads(),
            1,
            "identical card bytes must skip the write_texture re-upload"
        );
        assert_eq!(
            first.pixels, second.pixels,
            "the skipped upload must not change the composited output"
        );

        let third = gpu.render_input(&mut win, &input, Some(tray(&green, pw, ph)));
        assert_eq!(gpu.tray_uploads(), 2, "changed card bytes must upload");
        assert_ne!(
            second.pixels, third.pixels,
            "the new card must actually reach the frame"
        );

        // Same byte LENGTH, different (pw, ph): the texture is recreated, and a
        // fresh texture (undefined contents) must ALWAYS be uploaded into, even
        // though the bytes equal the mirror's.
        let swapped = TrayQuad {
            rgba: &green,
            pw: ph,
            ph: pw,
            dx: 4,
            dy: 4,
        };
        let _ = gpu.render_input(&mut win, &input, Some(swapped));
        assert_eq!(gpu.tray_uploads(), 3, "a recreate must always upload");
    }

    /// M5 true vibrancy: the swapchain composite-alpha choice is opacity- AND
    /// caps-aware. Solid (>= 1.0, or non-finite) is ALWAYS `Opaque` — the
    /// byte-identical default; translucent goes `PostMultiplied` ONLY where the
    /// surface offers it, otherwise it stays honestly solid (no stray alpha on a
    /// swapchain that would ignore it).
    #[test]
    fn present_alpha_mode_is_opacity_and_caps_aware() {
        use super::GpuRenderer;
        use wgpu::CompositeAlphaMode::{Opaque, PostMultiplied};
        // Solid opacity → Opaque even where PostMultiplied is offered.
        assert_eq!(GpuRenderer::present_alpha_mode(1.0, true), Opaque);
        assert_eq!(GpuRenderer::present_alpha_mode(1.5, true), Opaque);
        assert_eq!(GpuRenderer::present_alpha_mode(f32::NAN, true), Opaque);
        // Translucent + supported → PostMultiplied (blend over the vibrancy view).
        assert_eq!(GpuRenderer::present_alpha_mode(0.85, true), PostMultiplied);
        assert_eq!(GpuRenderer::present_alpha_mode(0.0, true), PostMultiplied);
        // Translucent but the surface has NO non-opaque composite → honestly solid.
        assert_eq!(GpuRenderer::present_alpha_mode(0.85, false), Opaque);
        assert_eq!(GpuRenderer::present_alpha_mode(0.0, false), Opaque);
    }

    /// M5: `with_translucency` sets the blit's translucent flag and threads the
    /// remainder-band alpha, while the solid default leaves the flag clear so the
    /// present forces on-glass alpha to 1.0 (byte-identical).
    #[test]
    fn blit_translucency_sets_flag_and_band_alpha() {
        use super::BlitUniform;
        let solid = BlitUniform::bell(false);
        assert_eq!(solid.translucent, 0.0, "solid default keeps the flag clear");
        let t = BlitUniform::bell(false).with_translucency(0.5);
        assert_eq!(t.translucent, 1.0, "translucent flag set");
        assert_eq!(t.band[3], 0.5, "band alpha threaded");
        // The std140 layout is 96 bytes: the M3 `sdr_white_scale` (Windows scRGB
        // reference-white scale) added a 16-byte block after `translucent`. The
        // uniform stays `Pod` + upload-compatible, and the WGSL `Blit` struct matches
        // byte-for-byte (the CPU/GPU parity test guards that).
        assert_eq!(std::mem::size_of::<BlitUniform>(), 96);
    }
}

// The GPU renderer as the injected `Rasterizer` (ATERM_DESIGN WS-F): the same
// trait `aterm_render::Renderer` implements, so a frontend can hold
// `Box<dyn Rasterizer>` and choose CPU vs GPU at runtime. Forwards to the inherent
// methods via UFCS to avoid the trait/inherent name clash. The trait is
// `&Terminal`-free (A-3): the renderer consumes only the engine-built `RenderInput`.
impl Rasterizer for GpuRenderer {
    fn cell_size(&self) -> (usize, usize) {
        GpuRenderer::cell_size(self)
    }
    fn render_input(&mut self, input: &RenderInput) -> Frame {
        // The inherent path threads per-window GPU state (offscreen / caches) on a
        // `WindowGpu`; the trait is `&Terminal`-/window-free, so the DI seam owns a
        // throwaway one for this call. A full-repaint readback (`render_input`
        // always clears + draws every row), so a fresh `WindowGpu` only forgoes
        // offscreen REUSE — the pixels are byte-identical. Frontends use the
        // inherent `GpuRenderer::render_input(win, ..)` (a persistent `WindowGpu`)
        // for the hot path; this object-safe forward exists for the `dyn Rasterizer`
        // seam only.
        let mut win = WindowGpu::new();
        GpuRenderer::render_input(self, &mut win, input, None)
    }
    // `render_input_cached` is intentionally NOT overridden: the inherent version
    // returns a `RenderView` borrowing the per-window `gate_cache`, which can't
    // outlive a local `WindowGpu`. The trait's default (`RenderView::Owned(self.
    // render_input(input))`) is byte-identical and object-safe — the GPU hot path
    // calls the inherent `render_input_cached(win, ..)` directly, not via the trait.
    fn set_cursor_blink_phase(&mut self, on: bool) {
        GpuRenderer::set_cursor_blink_phase(self, on)
    }
    fn set_cursor_style_override(&mut self, style: Option<CursorStyle>) {
        GpuRenderer::set_cursor_style_override(self, style)
    }
}

#[cfg(test)]
mod rasterizer_di_tests {
    use super::*;

    // Locks the WS-F injected-rasterizer abstraction: both the CPU and GPU
    // renderers must satisfy `Rasterizer`, so a frontend can hold either behind
    // one trait. Compile-time only — no GPU/font needed.
    #[test]
    fn both_renderers_implement_rasterizer() {
        fn assert_rasterizer<R: Rasterizer>() {}
        assert_rasterizer::<Renderer>();
        assert_rasterizer::<GpuRenderer>();
        // And the trait is object-safe (dyn dispatch = the DI the design wants).
        fn _takes_dyn(_: &mut dyn Rasterizer) {}
    }
}
