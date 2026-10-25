// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The GPU terminal renderer (`GpuRenderer`): backend orchestration and the glyph/quad pipelines.

#[cfg(wgpu_arm)]
use std::collections::HashMap;
use std::sync::{Arc, Weak};

use aterm_hash::{FxHashMap, FxHashSet};

use aterm_core::terminal::{CursorStyle, UnderlineStyle};
use aterm_render::{
    DirtyDecision, Frame, GlyphImage, GlyphKey, Rasterizer, RenderInput, RenderView, Renderer,
    SceneAtlas, Theme, compute_dirty_rows, is_unchanged_frame,
};

use crate::GpuContext;
#[cfg(any(wgpu_arm, target_os = "macos"))]
use crate::pipeline_table::ShaderLibrary;
#[cfg(wgpu_arm)]
use crate::pipeline_table::TargetFormats;
use crate::pipeline_table::{self, Pipeline};

#[cfg(wgpu_arm)]
#[inline]
const fn wgsl_source(library: ShaderLibrary) -> &'static str {
    match library {
        ShaderLibrary::Cell => SHADER,
        ShaderLibrary::Blit => BLIT_SHADER,
        ShaderLibrary::HdrGlow => HDR_GLOW_SHADER,
        ShaderLibrary::Tray => TRAY_SHADER,
        ShaderLibrary::Bloom => BLOOM_SHADER,
        ShaderLibrary::Shimmer => SHIMMER_SHADER,
    }
}

const ATLAS_WIDTH: u32 = 1024;

const ATLAS_GROW_HEADROOM: u32 = 256;

#[cfg(clean_islands)]
macro_rules! trust_island {
    ($($item:item)*) => {
        $($item)*
    };
}

#[cfg(not(clean_islands))]
macro_rules! trust_island {
    ($($tt:tt)*) => {};
}

trust_island! {
    clean {
        namespace GpuRowEncodeProofs

        -- When a row has no selection span (rowHasSel = false), no column on
        -- that row can be selected, so skipping selection_hit is exact.
        def cellSelectedOnRow (rowHasSel colInSpan : Bool) : Bool :=
          if rowHasSel then colInSpan else false

        theorem unselected_row_has_no_selected_cell (colInSpan : Bool) :
            cellSelectedOnRow false colInSpan = false := by
          rfl

        -- Consecutive cells on a row sharing (base_fg, cell.bg, hit) resolve to
        -- identical (glyph_color, bg_under) under the fixed frame
        -- selection_palette and min_contrast.
        def resolveRowFg (key memoKey memoVal freshVal : Nat) : Nat :=
          if Nat.beq key memoKey then memoVal else freshVal

        theorem row_fg_memo_hit_is_exact (val : Nat) :
            resolveRowFg 7 7 val val = val := by
          rfl

        theorem row_fg_memo_miss_recomputes (memoVal freshVal : Nat) :
            resolveRowFg 8 7 memoVal freshVal = freshVal := by
          rfl

        end GpuRowEncodeProofs
    }
}

macro_rules! impl_pod_zeroable {
    ($t:ident { $($f:ident : $ft:ty),+ $(,)? }) => {
        const _: () = assert!(
            ::core::mem::size_of::<$t>() == 0usize $(+ ::core::mem::size_of::<$ft>())+,
            concat!(
                stringify!($t),
                ": size_of is not the sum of its field sizes, so it has padding bytes. \
                 `Pod` forbids them (give the hole an explicit `_pad` field).",
            ),
        );
        const _: fn($t) = |v: $t| {
            const fn is_pod<T: ::aterm_bits::Pod>(_: &T) {}
            let $t { $($f: _),+ } = v;
            $({ let f: $ft = v.$f; is_pod(&f); })+
        };
        // SAFETY: `$t` is `#[repr(C)]`, `Copy + 'static`, and padding-free, so the all-zero
        // pattern is valid.
        unsafe impl ::aterm_bits::Zeroable for $t {}
        // SAFETY: `$t` has no padding and every field is `Pod`, so any `size_of::<$t>()`-byte
        // pattern is valid.
        unsafe impl ::aterm_bits::Pod for $t {}
    };
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Uniforms {
    screen: [f32; 2],
    text_blend: f32,
    _pad: f32,
}
impl_pod_zeroable!(Uniforms {
    screen: [f32; 2],
    text_blend: f32,
    _pad: f32
});

#[repr(C)]
#[derive(Clone, Copy)]
struct BgInstance {
    rect: [u16; 4],
    color: [u8; 4],
}
impl_pod_zeroable!(BgInstance {
    rect: [u16; 4],
    color: [u8; 4]
});

#[repr(C)]
#[derive(Clone, Copy)]
struct GlowInstance {
    rect: [u16; 4],
    color: [u8; 4],
    color2: [u8; 4],
}
impl_pod_zeroable!(GlowInstance {
    rect: [u16; 4],
    color: [u8; 4],
    color2: [u8; 4]
});

impl GlowInstance {
    #[inline]
    const fn of(rect: [u16; 4], q: &aterm_render::GlowQuad) -> Self {
        let [r, g, b, a, r2, g2, b2, a2] = glow8(q);
        Self {
            rect,
            color: [r, g, b, a],
            color2: [r2, g2, b2, a2],
        }
    }
}

type BgRun = (u16, u16, [u8; 4]);

#[inline]
fn push_bg_run(
    bg: &mut Vec<BgInstance>,
    run: &mut Option<BgRun>,
    y: u16,
    h: u16,
    x: u16,
    w: u16,
    color: [u8; 4],
) {
    if let Some((rx, rw, rc)) = run
        && *rc == color
        && u32::from(*rx) + u32::from(*rw) == u32::from(x)
        && u32::from(*rw) + u32::from(w) <= u32::from(u16::MAX)
    {
        *rw += w;
        return;
    }
    flush_bg_run(bg, run, y, h);
    *run = Some((x, w, color));
}

#[inline]
fn flush_bg_run(bg: &mut Vec<BgInstance>, run: &mut Option<BgRun>, y: u16, h: u16) {
    if let Some((x, w, color)) = run.take() {
        bg.push(BgInstance {
            rect: [x, y, w, h],
            color,
        });
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct GlyphInstance {
    rect: [f32; 4],
    uv: [f32; 4],
    color: [u8; 4],
    bg: [u8; 4],
}
impl_pod_zeroable!(GlyphInstance {
    rect: [f32; 4],
    uv: [f32; 4],
    color: [u8; 4],
    bg: [u8; 4]
});

const _: () = {
    assert!(std::mem::size_of::<BgInstance>() as u64 == pipeline_table::BG_LAYOUT.stride);
    assert!(std::mem::size_of::<GlowInstance>() as u64 == pipeline_table::GLOW_LAYOUT.stride);
    assert!(std::mem::size_of::<GlyphInstance>() as u64 == pipeline_table::GLYPH_LAYOUT.stride);
};

trait RasterEdit {
    fn dropped_rasters(&self) -> bool;
}

impl RasterEdit for bool {
    #[inline]
    fn dropped_rasters(&self) -> bool {
        *self
    }
}

impl RasterEdit for () {
    #[inline]
    fn dropped_rasters(&self) -> bool {
        true
    }
}

impl RasterEdit for Result<(), String> {
    #[inline]
    fn dropped_rasters(&self) -> bool {
        self.is_ok()
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RainGlowInstance {
    rect: [u16; 4],
    color: [u8; 4],
    falloff: [u16; 4],
}
impl_pod_zeroable!(RainGlowInstance {
    rect: [u16; 4],
    color: [u8; 4],
    falloff: [u16; 4]
});
const _: () = assert!(
    std::mem::size_of::<RainGlowInstance>() as u64 == pipeline_table::RAIN_GLOW_LAYOUT.stride
);

#[repr(C)]
#[derive(Clone, Copy)]
struct FireInstance {
    rect: [u16; 4],
    geo: [u16; 4],
    phase: u32,
    tsl: [u8; 4],
}
impl_pod_zeroable!(FireInstance {
    rect: [u16; 4],
    geo: [u16; 4],
    phase: u32,
    tsl: [u8; 4]
});
const _: () =
    assert!(std::mem::size_of::<FireInstance>() as u64 == pipeline_table::FIRE_LAYOUT.stride);

#[cfg(wgpu_arm)]
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

// GLOW (the LUMEN aurora and every other `GlowQuad` stream): a `GlowInstance` —
// the bg rect plus a colour PAIR, the quad's LEFT and RIGHT edges.
// The vertex stage hands the fragment the pair and the rect's x-span FLAT, and
// the fragment ramps between the two ends PER COLUMN with the SAME integer law
// the CPU rasterizer uses (`aterm_render::glow_lerp`), for alpha alike:
//
//     c(i) = (c0·(2w − m) + c1·m + w) / (2w),   m = 2i + 1
//
// A flat pair (c0 == c1) is the identity for every i, so a flat stream keeps
// its exact bytes; a gradient is continuous inside the quad. The rasterizer's
// float colour interpolant is deliberately NOT used —
// its rounding is not the CPU's, and byte parity is the contract. The fragment
// emits `c / 255.0` RAW (no sRGB decode; the same division the Unorm8x4 decode
// performed for the flat path) into the glow blend `src + dst·(1 − src_a)`,
// which rounds ONCE on store: over the offscreen's default view that is
// byte-exact == the CPU `add_sat` / `over_premul` on native, and lands in
// linear on downlevel (the accepted approximation — see the header DOWNLEVEL
// FALLBACK).
struct GlowVsOut {
    @builtin(position) pos: vec4<f32>,
    // The quad's left edge and width in device px, as whole numbers.
    @location(0) @interpolate(flat) span: vec2<f32>,
    // The colour pair as exact bytes (r, g, b, a): premultiplied, `a` the mode.
    @location(1) @interpolate(flat) c0: vec4<u32>,
    @location(2) @interpolate(flat) c1: vec4<u32>,
    // The fragment's x INSIDE THE QUAD, in the quad's own px (0 at its left
    // edge, its width at its right), interpolated linearly: the same pipeline
    // draws the bloom EXTRACT into the HALF-RES bloom target under the
    // full-res uniforms, where `pos.x - span.x` would mix a half-res pixel with
    // a full-res edge and paint the quad's left half in its LEFT colour.
    // cell.metal's `vs_glow` is the twin.
    @location(3) @interpolate(linear, center) lx: f32,
};

@vertex
fn vs_glow(@builtin(vertex_index) vi: u32,
           @location(0) rect_u: vec4<u32>,
           @location(1) c0: vec4<u32>,
           @location(2) c1: vec4<u32>) -> GlowVsOut {
    let rect = vec4<f32>(rect_u);
    let k = corner(vi);
    let px = rect.xy + k * rect.zw;
    var o: GlowVsOut;
    o.pos = vec4<f32>(to_ndc(px), 0.0, 1.0);
    o.span = rect.xz;
    o.lx = k.x * rect.z;
    o.c0 = c0;
    o.c1 = c1;
    return o;
}

// The fragment's 0-based column inside its quad, from its x in the quad's own
// px. At full res the pixel CENTRE interpolates to i + 0.5, so the floor is i
// with half a pixel of margin either side; at half res each fragment reads the
// quad's own column under its centre. The clamp only guards the edges.
fn glow_column(lx: f32, w: f32) -> u32 {
    return u32(clamp(floor(lx), 0.0, max(w - 1.0, 0.0)));
}

// `aterm_render::glow_lerp`, per channel and for alpha alike, at column `i` of
// a `w`-wide quad (`w >= 1`: a zero-width quad rasterizes no fragment).
fn glow_lerp4(c0: vec4<u32>, c1: vec4<u32>, i: u32, w: u32) -> vec4<u32> {
    let m = 2u * min(i, w - 1u) + 1u;
    return (c0 * (2u * w - m) + c1 * m + vec4<u32>(w)) / (2u * w);
}

@fragment
fn fs_glow(in: GlowVsOut) -> @location(0) vec4<f32> {
    let w = max(u32(in.span.y), 1u);
    let c = glow_lerp4(in.c0, in.c1, glow_column(in.lx, in.span.y), w);
    return vec4<f32>(c) / 255.0;
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
    // ROOT SKIRT (CPU twin: fire_field FireRow): below the root the
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

fn fire_pal_add(idx_in: i32) -> vec3<i32> {
    var pal = array<vec3<i32>, 5>(
        vec3<i32>(42, 0, 0), vec3<i32>(139, 26, 0), vec3<i32>(224, 74, 0),
        vec3<i32>(255, 176, 32), vec3<i32>(255, 240, 192));
    // == FIRE_ADD_IDX_MAX: additive fire stops a breath past amber.
    let idx = min(idx_in, 780);
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
// CPU `fire_shade_add` bytes, so the add is byte-exact (the fs_rain_glow
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
// `fire_shade_over`.
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
// gated EXACTLY like the CPU `blend_text`: interior coverage only
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
// QUANTIZED to the CPU's byte lattice — `round(cov·color.a·255)/255`, the
// float twin of the CPU `(cov * alpha + 127) / 255` — because the CPU stamp
// rounds that intermediate to a byte before it premultiplies/blends, and the
// un-quantized f32 product landed ±1 code value off it on ~12% of interior
// (cov, alpha) pairs (as in fs_sprite_over; ties are unreachable by parity,
// so round() reproduces the byte exactly).
//
// OVER (the feline cat-paw): output (rgb, a) into ALPHA_BLENDING ⇒
//   dst*(1-a) + rgb*a  == the CPU `blend(dst, color, a)`.
@fragment
fn fs_deco_over(in: GlyphVsOut) -> @location(0) vec4<f32> {
    let cov = textureSample(atlas_tex, atlas_samp, in.uv).r;
    let a = round(cov * in.color.a * 255.0) / 255.0;
    return vec4<f32>(s2l(in.color.rgb), a);
}

// ADD (the profanity sparkle): output PREMULTIPLIED (rgb*a, a) into the One/One
// additive pipeline ⇒ dst + rgb*a == the CPU `add_sat(dst, premul_rgb(color, a))`.
@fragment
fn fs_deco_add(in: GlyphVsOut) -> @location(0) vec4<f32> {
    let cov = textureSample(atlas_tex, atlas_samp, in.uv).r;
    let a = round(cov * in.color.a * 255.0) / 255.0;
    return vec4<f32>(in.color.rgb * a, a);
}

// RGBA8 sprites use a per-instance multiply tint and opacity. The output
// into ALPHA_BLENDING on the sRGB view ⇒ dst*(1-a)+rgb*a == the CPU pass-1c sprite stamp.
@fragment
// The tint multiply and the opacity multiply are QUANTIZED to 8 bits with
// round-half BEFORE the sRGB decode, exactly reproducing the CPU stamp's
// intermediate byte: the cat/free path's mul8(c,f) = (c*f+127)/255 and the
// scaled-sprite path's (t*tint*255 + 0.5) as u8. round(x255*y255/255) equals
// mul8 in the float domain (c = x255/255, f = y255/255), so the tinted colour
// lands on the SAME 8-bit lattice the CPU linearizes and the linear-light
// composite that follows is parity-exact rather than diverging by the ~1 LSB
// the un-quantized f32 product injected on mid-ramp tints (the design 9 tint
// divergence). At the cat's identity tint (0xFFFFFF, a=255) the quantization
// is a no-op (round(c*1*255)/255 == c), so cat parity is unchanged; the
// free-sprite paths already quantize CPU-side, so this matches them.
fn fs_sprite_over(in: GlyphVsOut) -> @location(0) vec4<f32> {
    let c = textureSample(atlas_tex, atlas_samp, in.uv);
    let tinted = round(c.rgb * in.color.rgb * 255.0) / 255.0;
    let a = round(c.a * in.color.a * 255.0) / 255.0;
    return vec4<f32>(s2l(tinted), a);
}

// FX LAYERS (design §4.2, §4.6): pixel law F over an `FxInstance`, one band
// slice of one item. Every operand reaches the fragment as a FLAT integer and
// the fragment addresses from its own pixel `p = floor(pos.xy)` and the
// item's UNCLAMPED origin, so a slice cut anywhere draws the bytes the CPU
// stamps. No sampler and no float UV: texels are read by integer address and
// their bytes recovered exactly (`round(v·255)`; a stored byte k reads k/255).
//
// flags: bits 0-1 the kind (0 Rect, 1 Ramps, 2 Sprite), bit 2 flip_x, bits
// 3-4 the texel kind (0 Index8, 1 Rgba8Premul, 2 Rgba8Straight), bits 8-15
// an Index8 sprite's reach (indices 1..=reach have a palette entry).
// The origin is stored mod 2^16 and every slice lies inside its item's box,
// whose sides are below 2^16, so `(p - O) mod 2^16` is the item-local pixel.
struct FxVsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) @interpolate(flat) origin: vec2<i32>,
    @location(1) @interpolate(flat) dst: vec2<u32>,
    @location(2) @interpolate(flat) src: vec4<u32>,
    @location(3) @interpolate(flat) pal: u32,
    @location(4) @interpolate(flat) flags: u32,
};

@vertex
fn vs_fx(@builtin(vertex_index) vi: u32,
         @location(0) rect_u: vec4<u32>,
         @location(1) origin: vec2<i32>,
         @location(2) dst: vec2<u32>,
         @location(3) src: vec4<u32>,
         @location(4) pal: u32,
         @location(5) flags: u32) -> FxVsOut {
    let rect = vec4<f32>(rect_u);
    let k = corner(vi);
    let px = rect.xy + k * rect.zw;
    var o: FxVsOut;
    o.pos = vec4<f32>(to_ndc(px), 0.0, 1.0);
    o.origin = origin;
    o.dst = dst;
    o.src = src;
    o.pal = pal;
    o.flags = flags;
    return o;
}

// The Index8 atlas (R8Unorm), the RGBA atlas (Rgba8Unorm) and the frame's
// palette (each entry's RGBA bytes read little-endian), at bindings no other
// entry point names, so one group-1 layout serves the fx rows.
@group(1) @binding(2) var fx_r8: texture_2d<f32>;
@group(1) @binding(3) var fx_rgba: texture_2d<f32>;
@group(1) @binding(4) var<storage, read> fx_pal: array<u32>;

// Palette entry `j` as bytes (r, g, b, a).
fn fx_ink(j: u32) -> vec4<u32> {
    let e = fx_pal[j];
    return vec4<u32>(e & 255u, (e >> 8u) & 255u, (e >> 16u) & 255u, e >> 24u);
}

// The stored byte of a Unorm8 texel channel.
fn fx_byte4(v: vec4<f32>) -> vec4<u32> {
    return vec4<u32>(round(v * 255.0));
}

// The item-local pixel `l = (p - O) mod 2^16`.
fn fx_local(pos: vec4<f32>, origin: vec2<i32>) -> vec2<u32> {
    let p = vec2<i32>(floor(pos.xy));
    return vec2<u32>(p - origin) & vec2<u32>(65535u);
}

// The sprite texel under `l`: `t = (l.x·w / dw, l.y·h / dh)`, mirrored under
// flip_x, offset to the art's atlas region.
fn fx_texel(l: vec2<u32>, dst: vec2<u32>, src: vec4<u32>, flags: u32) -> vec2<i32> {
    var t = vec2<u32>(l.x * src.z / dst.x, l.y * src.w / dst.y);
    if ((flags & 4u) != 0u) {
        t.x = src.z - 1u - t.x;
    }
    return vec2<i32>(src.xy + t);
}

// The Exact law: `(c, a) / 255` raw into `src + dst·(1 − a)` on the Unorm
// view, `fs_glow`'s state: `over_premul`, `add_sat` at a = 0.
@fragment
fn fs_fx(in: FxVsOut) -> @location(0) vec4<f32> {
    let l = fx_local(in.pos, in.origin);
    let kind = in.flags & 3u;
    var c: vec4<u32>;
    if (kind == 2u) {
        let t = fx_texel(l, in.dst, in.src, in.flags);
        if (((in.flags >> 3u) & 3u) == 0u) {
            // Index8: 0 writes nothing; k names entry ink + k − 1, and an
            // index past the palette writes nothing.
            let k = fx_byte4(textureLoad(fx_r8, t, 0)).x;
            let reach = (in.flags >> 8u) & 255u;
            if (k == 0u || k > reach) {
                discard;
            }
            // In range even for the discarded (helper) invocation.
            c = fx_ink(in.pal + clamp(k, 1u, reach) - 1u);
        } else {
            // Rgba8Premul: the texel is its own (c, a); a = 0 is light.
            c = fx_byte4(textureLoad(fx_rgba, t, 0));
        }
    } else {
        // Rect (row 0) and Ramps (row l.y): the GlowQuad ramp over entries
        // j, j + 1 at column l.x of the item's width.
        let j = in.pal + 2u * select(0u, l.y, kind == 1u);
        c = glow_lerp4(fx_ink(j), fx_ink(j + 1u), l.x, max(in.src.x, 1u));
    }
    return vec4<f32>(c) / 255.0;
}

// The Linear law (FreeSprite's): a straight texel under the tint entry,
// `(mul8(rgb, tint.rgb), mul8(A, tint.a))` in exact integers, decoded and
// blended in linear light on the sRGB view (ALPHA_BLENDING).
@fragment
fn fs_fx_linear(in: FxVsOut) -> @location(0) vec4<f32> {
    let l = fx_local(in.pos, in.origin);
    let v = fx_byte4(textureLoad(fx_rgba, fx_texel(l, in.dst, in.src, in.flags), 0));
    let tint = fx_ink(in.pal);
    let p = (v * tint + vec4<u32>(127u)) / 255u;
    if (p.w == 0u) {
        discard;
    }
    return vec4<f32>(s2l(vec3<f32>(p.xyz) / 255.0), f32(p.w) / 255.0);
}

// The Exact law where every pixel the layer draws is opaque (`FxContent`'s
// `draws_opaque`, route (1) of the pass economy): `over_premul` at a = 255 is
// the entry's colour, so the draw stores it through the sRGB view with no
// blend, as `fs_bg` stores a cell's background, and the Color mask keeps alpha.
@fragment
fn fs_fx_store(in: FxVsOut) -> @location(0) vec4<f32> {
    let l = fx_local(in.pos, in.origin);
    let kind = in.flags & 3u;
    var c: vec4<u32>;
    if (kind == 2u) {
        let t = fx_texel(l, in.dst, in.src, in.flags);
        if (((in.flags >> 3u) & 3u) == 0u) {
            let k = fx_byte4(textureLoad(fx_r8, t, 0)).x;
            let reach = (in.flags >> 8u) & 255u;
            if (k == 0u || k > reach) {
                discard;
            }
            c = fx_ink(in.pal + clamp(k, 1u, reach) - 1u);
        } else {
            c = fx_byte4(textureLoad(fx_rgba, t, 0));
        }
    } else {
        let j = in.pal + 2u * select(0u, l.y, kind == 1u);
        c = glow_lerp4(fx_ink(j), fx_ink(j + 1u), l.x, max(in.src.x, 1u));
    }
    return vec4<f32>(s2l(vec3<f32>(c.rgb) / 255.0), 1.0);
}

"#;

#[cfg(wgpu_arm)]
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
// std140 layout — must match the Rust `BlitUniform` byte-for-byte (112 bytes).
struct Blit {
    flag: u32,        // bell-flash invert
    overlay: u32,     // drop-target highlight enabled
    border_px: f32,   // inset border thickness, device px
    encode_srgb: f32, // !=0: re-encode linear->sRGB (downlevel WebGL2 blit)
    accent: vec3<f32>,// overlay accent rgb, normalized 0..1
    chrome_y1: f32,   // source rows [chrome_y0, chrome_y1) are host chrome: their bands continue the frame's edge pixels
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
    premult: f32,     // H1: !=0: multiply output rgb by the emitted alpha (DComp visual swapchain)
    chrome_y0: f32,   // first host-chrome source row (rows above it keep flat bands)
    chrome_pad0: f32, // std140: pad the struct to a 16-byte multiple (112)
    chrome_pad1: f32,
    chrome_pad2: f32,
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
    var p = in.pos.xy - b.content_off;
    let visible_y1 = b.visible_y + b.visible_h;
    // CHROME REACHES THE WINDOW EDGE: beside a host-chrome source row
    // (`chrome_y0 <= p.y < chrome_y1`, `aterm_render::Renderer::chrome_extent_px`)
    // the horizontal remainder bands CONTINUE the frame's own edge pixel instead
    // of the band colour — the CPU twin is `place_frame_bands`' `edge_rows`. The
    // fetched texel is still a BAND pixel: never inverted, never washed.
    // `chrome_y1 == 0` (every frame without chrome) never takes this arm, and
    // the rows above `chrome_y0` (a surfaceless strip that keeps the padding)
    // keep the band colour like any terminal row.
    var chrome_edge = false;
    if ((p.x < 0.0 || p.x >= b.dims.x) && p.y >= max(b.visible_y, b.chrome_y0) && p.y < min(visible_y1, b.chrome_y1)) {
        p.x = clamp(p.x, 0.0, b.dims.x - 1.0);
        chrome_edge = true;
    }
    if (p.x < 0.0 || p.y < b.visible_y || p.x >= b.dims.x || p.y >= visible_y1) {
        // M3: the bands are chrome in the SAME stream as the frame — on the EDR
        // swapchain they take the same linear decode (and the same <=1.0 clamp).
        // M5: on a translucent window the remainder bands take the bg-quad alpha
        // (`b.band.a`) too, so the whole window reads as one sheet of glass; the
        // opaque default forces 1.0 (byte-identical chrome).
        let band_a = select(1.0, b.band.a, b.translucent != 0.0);
        var band_rgb = b.band.rgb;
        if (b.hdr != 0.0) {
            // The SAME reference-white scale as the grid arm below. A band left
            // at scRGB 1.0 == 80 nits composes 1/scale as bright as the grid
            // beside it on a Windows HDR desktop (measured in the FP16
            // composition: a 3 px strip at linear 0.0056 next to a grid at
            // 0.0168 on a 240-nit SDR-white desktop — a dark frame line around
            // every non-grid-fit window). The bands are the same sheet as the
            // grid, so they take the same encode; 1.0 on macOS / SDR.
            band_rgb = hdr_grid_encode3(band_rgb) * b.sdr_white_scale;
        }
        // H1: a premultiplied destination (DComp visual swapchain) wants
        // rgb·a. Identity on every opaque present (band_a == 1.0 there).
        if (b.premult != 0.0) {
            band_rgb = band_rgb * band_a;
        }
        return vec4<f32>(band_rgb, band_a);
    }
    let c = textureLoad(src_tex, vec2<i32>(p), 0);
    var rgb = c.rgb;
    if (b.flag != 0u && !chrome_edge) {
        rgb = vec3<f32>(1.0) - rgb;
    }
    // Drag-and-drop drop-target highlight: faint accent wash + inset accent
    // border, relative to the CONTENT frame (`p` is the frame-local device
    // pixel). Gated on `overlay` so a normal present is byte-identical.
    if (b.overlay != 0u && !chrome_edge) {
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
    // H1 (Windows Mica/Acrylic): the DComp visual swapchain composites
    // PREMULTIPLIED (DXGI rejects straight alpha for composition), so scale rgb
    // by the emitted alpha. Opaque pixels (the whole grid body — cells are the
    // ratified opaque surface) have out_a == 1.0 and pass through byte-identical;
    // only the padding/chrome-bleed margins carry alpha < 1 and pick up the
    // multiply, which is exactly what stops the "bright fringe" a straight-alpha
    // frame would show when DWM treats it as premultiplied.
    if (b.premult != 0.0) {
        rgb = rgb * out_a;
    }
    return vec4<f32>(rgb, out_a);
}
"#;

#[cfg(wgpu_arm)]
const HDR_GLOW_SHADER: &str = r#"
struct HdrU {
    screen: vec2<f32>,      // SWAPCHAIN width,height in px (the NDC divisor)
    content_off: vec2<f32>, // W1 frame top-left inside the swapchain, device px
    boost: f32,             // linear emission boost (aterm_render::hdr::HDR_GLOW_BOOST)
    headroom: f32,          // sanitize(edr_max) - 1.0, >= 0 (proven CPU-side)
    _pad: vec2<f32>,
};
@group(0) @binding(0) var<uniform> hu: HdrU;

// The crown reads the SAME `GlowInstance` stream the bloom extract does — a
// rect plus a colour PAIR (`aterm_render::GlowQuad::color2`) — and ramps it
// per column with the identical integer law `fs_glow` uses, BEFORE its boost,
// so a gradient quad is the same gradient in the crown as on the glass and a
// flat pair is the identity.
struct HdrVsOut {
    @builtin(position) pos: vec4<f32>,
    // The quad's left edge in SWAPCHAIN px (the rect's x plus the W1 band
    // offset, both whole pixels) and its width.
    @location(0) @interpolate(flat) span: vec2<f32>,
    @location(1) @interpolate(flat) c0: vec4<u32>,
    @location(2) @interpolate(flat) c1: vec4<u32>,
};

fn glow_column(pos_x: f32, span: vec2<f32>) -> u32 {
    return u32(clamp(floor(pos_x - span.x), 0.0, max(span.y - 1.0, 0.0)));
}

fn glow_lerp4(c0: vec4<u32>, c1: vec4<u32>, i: u32, w: u32) -> vec4<u32> {
    let m = 2u * min(i, w - 1u) + 1u;
    return (c0 * (2u * w - m) + c1 * m + vec4<u32>(w)) / (2u * w);
}

// The fragment's premultiplied sRGB-space colour, ramped, as 0..1 floats.
fn crown_color(in: HdrVsOut) -> vec3<f32> {
    let w = max(u32(in.span.y), 1u);
    let c = glow_lerp4(in.c0, in.c1, glow_column(in.pos.x, in.span), w);
    return vec3<f32>(c.rgb) / 255.0;
}

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
               @location(1) c0: vec4<u32>,
               @location(2) c1: vec4<u32>) -> HdrVsOut {
    let rect = vec4<f32>(rect_u);
    let k = hdr_corner(vi);
    // Offscreen px -> swapchain px (the W1 band placement) -> NDC.
    let px = rect.xy + k * rect.zw + hu.content_off;
    var o: HdrVsOut;
    o.pos = vec4<f32>(2.0 * px.x / hu.screen.x - 1.0, 1.0 - 2.0 * px.y / hu.screen.y, 0.0, 1.0);
    o.span = vec2<f32>(rect.x + hu.content_off.x, rect.z);
    o.c0 = c0;
    o.c1 = c1;
    return o;
}

// WHITE IS HOT (design §L5). The WHITE PART of a premultiplied sRGB colour is
// its smallest channel: a spectrum stop (`#FF0000`, `#FFFF00`, ...) has a zero
// channel and therefore NO white part, however bright it is, while `#FFFFFF`
// at coverage `k` is white all the way to `k`. So `min(r, g, b)` reads the
// luminance hierarchy off the colour itself, with no side channel and no
// per-quad tagging.
fn white_part(c: vec3<f32>) -> f32 {
    return min(min(c.r, c.g), c.b);
}

// Decode the premultiplied sRGB-space aurora colour to linear (same piecewise
// s2l as everywhere), boost, clamp to the headroom (never negative), and emit
// into the One/One add. COLOR write-mask: the blit's alpha stays 1.0.
//
// THE BOOST IS PER-FRAGMENT (§L5, "in EDR, `out` transients <= 460 ms may
// exceed reference white"): `1 + 2*smoothstep(0.35, 0.60, white_part)`, capped
// at 3x linear and then clamped to the panel's real headroom. Below the 0.35
// knee the factor is EXACTLY 1.0 and a multiply by 1.0 is exact in IEEE-754,
// so every coloured mark — star bars, halos, the ring, the fan grains, the
// ribbon's 0.15 hot edge — emits unboosted bits. Only the whitest cores (the meteor
// nucleus and its white shoulder, the smallest star cores, the landing pin,
// the caret's flare frame) climb, and they are the smallest marks on the
// glass — which is what makes the hierarchy read as light instead of as gain.
@fragment
fn fs_hdr_glow(in: HdrVsOut) -> @location(0) vec4<f32> {
    let c = clamp(crown_color(in), vec3<f32>(0.0), vec3<f32>(1.0));
    let lo = c / 12.92;
    let hi = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    let lin = select(lo, hi, c > vec3<f32>(0.04045));
    let hot = min(1.0 + 2.0 * smoothstep(0.35, 0.60, white_part(c)), 3.0);
    let bound = max(hu.headroom, 0.0);
    let add = max(min(lin * hu.boost * hot, vec3<f32>(bound)), vec3<f32>(0.0));
    return vec4<f32>(add, 0.0);
}

// SDR twin of the boost (the swapchain-side crown on a NON-f16 present): scale
// the aurora colour by the BUDGET (hu.headroom carries it, <= 0.35 by the proven
// `sdr_glow_budget` bound; hu.boost is 1.0) and emit it RAW — the SDR swapchain
// is a non-sRGB Unorm view, so blending works in code values (no s2l decode).
// The pipeline composites this through the BOUNDED screen operator
// (`pipeline_table::Blend::SCREEN`), NOT One/One: the crown is the last light drawn on
// the frame and an add here lands outside every budget upstream of it. Scaling
// (not clamping) preserves the glow's gradient shape: peak = budget at full
// colour, no plateau. COLOR write-mask: the blit's alpha stays 1.0.
@fragment
fn fs_sdr_glow(in: HdrVsOut) -> @location(0) vec4<f32> {
    let c = clamp(crown_color(in), vec3<f32>(0.0), vec3<f32>(1.0));
    let bound = max(hu.headroom, 0.0);
    return vec4<f32>(c * bound * max(hu.boost, 0.0), 0.0);
}
"#;

#[repr(C)]
#[derive(Clone, Copy, PartialEq)]
struct HdrGlowUniform {
    screen: [f32; 2],
    content_off: [f32; 2],
    boost: f32,
    headroom: f32,
    _pad: [f32; 2],
}
impl_pod_zeroable!(HdrGlowUniform {
    screen: [f32; 2],
    content_off: [f32; 2],
    boost: f32,
    headroom: f32,
    _pad: [f32; 2],
});

#[cfg(wgpu_arm)]
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

#[cfg(wgpu_arm)]
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
struct BloomU {
    texel: vec2<f32>,
    strength: f32,
    radius: f32,        // the WHITE part's radius, half-res texels
    chroma_radius: f32, // the COLOURED part's, always the shorter of the two
    _pad0: f32,
    _pad1: f32,
};
@group(0) @binding(2) var<uniform> bu: BloomU;

// The bloom's twin of `HDR_GLOW_SHADER::white_part` — the smallest channel is
// the white light in a premultiplied colour, the rest is its chroma.
fn white_part(c: vec3<f32>) -> f32 {
    return min(min(c.r, c.g), c.b);
}

// WHITE IS HOT (design §L5), the halo's half: ONE 25-tap kernel, two radii.
// The white part of the source spreads at `radius`, the chroma at the shorter
// `chroma_radius`, and the two are summed back into one halo — so a meteor
// reads as an over-white streak dying into a coloured train, and less coloured
// light smears sideways into the glyphs beside it. The taps, their gaussian
// weights and the normalization are the kernel that was here before; only the
// offset each half samples at differs, and the shorter chroma offset stays
// well inside the composite scissor the longer white radius already dilates.
@fragment
fn fs_bloom(in: VsOut) -> @location(0) vec4<f32> {
    var white = 0.0;
    var chroma = vec3<f32>(0.0, 0.0, 0.0);
    var wsum = 0.0;
    for (var j: i32 = -2; j <= 2; j = j + 1) {
        for (var i: i32 = -2; i <= 2; i = i + 1) {
            let tap = vec2<f32>(f32(i), f32(j)) * bu.texel;
            let d2 = f32(i * i + j * j);
            let w = exp(-d2 / 4.0);
            let sw = textureSample(bloom_src, bloom_samp, in.uv + tap * bu.radius).rgb;
            white = white + white_part(sw) * w;
            let sc = textureSample(bloom_src, bloom_samp, in.uv + tap * bu.chroma_radius).rgb;
            chroma = chroma + (sc - vec3<f32>(white_part(sc))) * w;
            wsum = wsum + w;
        }
    }
    let sum = vec3<f32>(white) + chroma;
    return vec4<f32>(sum / wsum * bu.strength, 1.0);
}
"#;

#[repr(C)]
#[derive(Clone, Copy, PartialEq)]
struct BloomUniform {
    texel: [f32; 2],
    strength: f32,
    radius: f32,
    chroma_radius: f32,
    /// The share of the half texture's storage that its texels in use fill, per axis: 1 for an
    /// exact texture. Only the GL arm's present keeps roomy storage, and only `bloom.glsl` reads
    /// it; the other arms' shaders take it as padding.
    extent: [f32; 2],
    _pad: f32,
}
impl_pod_zeroable!(BloomUniform {
    texel: [f32; 2],
    strength: f32,
    radius: f32,
    chroma_radius: f32,
    extent: [f32; 2],
    _pad: f32
});

#[cfg(wgpu_arm)]
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
    // The WHOLE displaced sample, alpha included: rgb and alpha must describe
    // the same source pixel, because the translucent (M5 glass) present
    // publishes that alpha (a stale alpha shows glyph-edge flecks as opaque bg
    // and ink at bg opacity inside the haze band). On an
    // opaque present the offscreen alpha is uniform inside the grid, so
    // carrying it is byte-identical there.
    return c;
}
"#;

#[repr(C)]
#[derive(Clone, Copy, PartialEq)]
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
impl_pod_zeroable!(ShimmerUniform {
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
});

/// Blit uniform.
#[repr(C)]
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct BlitUniform {
    flag: u32,
    overlay: u32,
    border_px: f32,
    encode_srgb: f32,
    accent: [f32; 3],
    chrome_y1: f32,
    dims: [f32; 2],
    wash_a: f32,
    border_a: f32,
    band: [f32; 4],
    content_off: [f32; 2],
    hdr: f32,
    translucent: f32,
    sdr_white_scale: f32,
    visible_y: f32,
    visible_h: f32,
    premult: f32,
    chrome_y0: f32,
    chrome_pad: [f32; 3],
}
impl_pod_zeroable!(BlitUniform {
    flag: u32,
    overlay: u32,
    border_px: f32,
    encode_srgb: f32,
    accent: [f32; 3],
    chrome_y1: f32,
    dims: [f32; 2],
    wash_a: f32,
    border_a: f32,
    band: [f32; 4],
    content_off: [f32; 2],
    hdr: f32,
    translucent: f32,
    sdr_white_scale: f32,
    visible_y: f32,
    visible_h: f32,
    premult: f32,
    chrome_y0: f32,
    chrome_pad: [f32; 3],
});

impl BlitUniform {
    #[inline]
    const fn bell(invert: bool) -> Self {
        Self {
            flag: invert as u32,
            overlay: 0,
            border_px: 0.0,
            encode_srgb: 0.0, // set by the caller from ctx.srgb_offscreen
            accent: [0.0; 3],
            chrome_y1: 0.0, // set by the present path from the chrome extent
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
            premult: 0.0,   // set by the present path on the DComp visual swapchain
            chrome_y0: 0.0, // set by the present path from the chrome extent
            chrome_pad: [0.0; 3],
        }
    }

    #[inline]
    const fn with_translucency(mut self, band_a: f32) -> Self {
        self.translucent = 1.0;
        self.band[3] = band_a;
        self
    }

    #[inline]
    const fn with_premultiplied_output(mut self) -> Self {
        self.premult = 1.0;
        self
    }

    #[inline]
    fn with_bands(mut self, fw: u32, fh: u32, dst_w: u32, dst_h: u32, band_rgb: u32) -> Self {
        let chan = |s: u32| ((band_rgb >> s) & 0xff) as f32 / 255.0;
        self.dims = [fw as f32, fh as f32];
        self.visible_y = 0.0;
        self.visible_h = fh as f32;
        let place =
            aterm_render::frame_placement(dst_w as usize, dst_h as usize, fw as usize, fh as usize);
        self.content_off = [place.x.origin as f32, place.y.origin as f32];
        self.band = [chan(16), chan(8), chan(0), 1.0];
        self
    }

    #[inline]
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

/// Tray-overlay uniform.
#[repr(C)]
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct TrayUniform {
    rect: [f32; 4],
    fb: [f32; 2],
    _pad: [f32; 2],
}
impl_pod_zeroable!(TrayUniform {
    rect: [f32; 4],
    fb: [f32; 2],
    _pad: [f32; 2]
});

/// Drop-target highlight parameters, passed to [`GpuRenderer::present_input`].
#[derive(Clone, Copy)]
pub struct DropOverlay {
    pub accent: u32,
    pub wash_a: u8,
    pub border_a: u8,
    pub border_scale_q4: u8,
}

/// Vertical interval of the raw offscreen that a frontend presents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PresentCrop {
    pub source_y: u32,
    pub height: u32,
}

/// A CPU-rasterized settings card composited over the swapchain at present time.
#[derive(Clone, Copy)]
pub struct TrayQuad<'a> {
    pub rgba: &'a [u8],
    pub pw: u32,
    pub ph: u32,
    pub dx: u32,
    pub dy: u32,
    pub scrim: Option<TrayScrim>,
}

/// A modal's smoke layer under its tray card ([`TrayQuad::scrim`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrayScrim {
    pub rgba: [u8; 4],
    pub top: u32,
}

/// Why an on-screen present did not acquire a drawable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurfacePresentFailure {
    AcquirePending,
    Reconfigured,
    Timeout,
    Unavailable,
    Occluded,
    Validation,
}

/// The per-window settings-card texture and its bind group.
#[cfg(wgpu_arm)]
pub(crate) struct TrayOverlay {
    texture: crate::device_layer::LayerTexture,
    pixels: Vec<u8>,
    #[cfg(wgpu_arm)]
    bind: wgpu::BindGroup,
    w: u32,
    h: u32,
}

/// The tray scrim's fill ([`TrayScrim`]) on the wgpu arm.
#[cfg(wgpu_arm)]
pub(crate) struct TrayScrimLayer {
    texture: crate::device_layer::LayerTexture,
    bind: wgpu::BindGroup,
    uniform: wgpu::Buffer,
    rgba: [u8; 4],
}

#[inline]
fn drop_border_px_gpu(w: u32, h: u32) -> u32 {
    (w.min(h) / 200).clamp(2, 6)
}

#[cfg(trust_verify)]
macro_rules! trust_contract_fn {
    (
        $(#[$meta:meta])*
        $vis:vis fn $name:ident($($arg:ident : $arg_ty:ty),* $(,)?) -> $ret:ident
        clauses [$($clauses:tt)+]
        { $($body:tt)* }
    ) => {
        $(#[$meta])*
        $vis fn $name($($arg: $arg_ty),*) -> $ret
            $($clauses)+
        { $($body)* }
    };
}

#[cfg(not(trust_verify))]
macro_rules! trust_contract_fn {
    (
        $(#[$meta:meta])*
        $vis:vis fn $name:ident($($arg:ident : $arg_ty:ty),* $(,)?) -> $ret:ident
        clauses [$($clauses:tt)+]
        { $($body:tt)* }
    ) => {
        $(#[$meta])*
        $vis fn $name($($arg: $arg_ty),*) -> $ret { $($body)* }
    };
}

#[cfg(trust_verify)]
macro_rules! verify_island {
    ($($tt:tt)*) => {
        $($tt)*
    };
}

#[cfg(not(trust_verify))]
macro_rules! verify_island {
    ($($tt:tt)*) => {};
}

/// `base` scaled by `DropOverlay::border_scale_q4` (1/16 units, 0 keeps `base`), at least one
/// pixel once scaled.
#[inline]
pub(crate) fn scaled_border_px(base: u32, scale_q4: u8) -> u32 {
    if scale_q4 == 0 {
        return base;
    }
    ((base * u32::from(scale_q4)) / 16).max(1)
}

#[cfg(test)]
mod scaled_border_px_tests {
    use super::scaled_border_px;

    #[test]
    fn scaled_border_px_is_sixteenths_never_thinner_than_a_pixel() {
        for (base, q4, want) in [
            (6, 0, 6),
            (6, 16, 6),
            (6, 24, 9),
            (6, 40, 15),
            (6, 44, 16),
            (1, 1, 1),
            (0, 16, 1),
            (0, 0, 0),
        ] {
            assert_eq!(scaled_border_px(base, q4), want, "base {base} × {q4}/16");
        }
    }
}

#[inline]
const fn valid_present_crop(crop: PresentCrop, raw_height: u32) -> bool {
    crop.height > 0
        && crop.source_y < raw_height
        && crop.height <= raw_height.saturating_sub(crop.source_y)
}

#[inline]
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

#[inline]
const fn rect_is_empty(r: [u32; 4]) -> bool {
    r[0] >= r[2] || r[1] >= r[3]
}

#[inline]
fn union_rect_opt(
    a: Option<[u32; 4]>,
    b: Option<[u32; 4]>,
    (w, h): (u32, u32),
) -> Option<[u32; 4]> {
    let (a, b) = (a?, b?);
    let clamp = |r: [u32; 4]| [r[0].min(w), r[1].min(h), r[2].min(w), r[3].min(h)];
    let (a, b) = (clamp(a), clamp(b));
    Some(match (rect_is_empty(a), rect_is_empty(b)) {
        (true, true) => [0, 0, 0, 0],
        (true, false) => b,
        (false, true) => a,
        (false, false) => [
            a[0].min(b[0]),
            a[1].min(b[1]),
            a[2].max(b[2]),
            a[3].max(b[3]),
        ],
    })
}

#[inline]
fn note_offscreen_written(win: &mut WindowGpu, rect: Option<[u32; 4]>, dims: (u32, u32)) {
    win.offscreen_dirty_since_sync = union_rect_opt(win.offscreen_dirty_since_sync, rect, dims);
    #[cfg(gl_arm)]
    {
        win.offscreen_dirty_since_post = union_rect_opt(win.offscreen_dirty_since_post, rect, dims);
    }
}

#[inline]
fn present_copy_rect(
    dirty: Option<[u32; 4]>,
    fx: Option<[u32; 4]>,
    (w, h): (u32, u32),
) -> [u32; 4] {
    union_rect_opt(dirty, fx, (w, h)).unwrap_or([0, 0, w, h])
}

#[inline]
fn elapsed_ns(started: aterm_time::Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

const G_BASE_BG: usize = 0;
const G_GLOW_UNDER: usize = 1;
const G_BASE_FG: usize = 2;
const G_GLOW: usize = 3;
const G_WDECO_OVER: usize = 4;
const G_WDECO_ADD: usize = 5;
const G_FREE_OVER: usize = 6;
const G_CURSOR: usize = 7;
const FRAME_GROUPS: usize = 8;

const GROUP_SRGB: [bool; FRAME_GROUPS] = {
    let mut srgb = [true; FRAME_GROUPS];
    srgb[G_GLOW_UNDER] = false; // the glow, Exact fx and fire under the text
    srgb[G_GLOW] = false; // aurora, nova and rain halos, and the glow and over fx
    srgb[G_WDECO_ADD] = false; // sparkle-word additive layer (One/One)
    srgb
};

#[inline]
fn coalesce_frame_passes<const N: usize>(
    enabled: &[bool; N],
    srgb: &[bool; N],
) -> ([usize; N], [bool; N], usize) {
    let mut pass_of = [usize::MAX; N];
    let mut pass_srgb = [true; N];
    let mut passes = 0usize;
    for g in 0..N {
        if !enabled[g] {
            continue;
        }
        if passes == 0 || pass_srgb[passes - 1] != srgb[g] {
            pass_srgb[passes] = srgb[g];
            passes += 1;
        }
        pass_of[g] = passes - 1;
    }
    (pass_of, pass_srgb, passes)
}

/// The staged band move's copy plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BandShift {
    pub(crate) src_y: u32,
    pub(crate) dst_y: u32,
    pub(crate) moved: u32,
}

/// Incoming-row apron strip, resolved once per frame by `GpuRenderer::apron_strip_plan`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ApronStrip {
    pub(crate) dst_y: u32,
    pub(crate) rows: u32,
    pub(crate) w: u32,
    pub(crate) cell_h: u32,
}

/// The copy plan under `shift_offscreen_band_px`, shared by the wgpu path and the Metal replay.
#[inline]
pub(crate) const fn band_shift_ops(y0: usize, y1: usize, delta: i64) -> Option<BandShift> {
    if delta == 0 {
        return None;
    }
    let mag = delta.unsigned_abs() as usize;
    let band_h = y1.saturating_sub(y0);
    let moved = band_h.saturating_sub(mag); // destination rows that pull in-band
    if moved == 0 {
        return None;
    }
    let (src_y, dst_y) = if delta > 0 {
        (y0 + mag, y0)
    } else {
        (y0, y0 + mag)
    };
    Some(BandShift {
        src_y: src_y as u32,
        dst_y: dst_y as u32,
        moved: moved as u32,
    })
}

#[cfg(target_os = "macos")]
fn record_band_shift(
    enc: &mut crate::device_layer::FrameEncoder<'_>,
    off: &crate::metal::resources::SealedTexture,
    scratch: &crate::metal::resources::SealedTexture,
    shift: BandShift,
    w: u32,
) -> Result<(), String> {
    use crate::device_layer::FrameCopyTexture;
    enc.copy_texture_rect(
        FrameCopyTexture::Metal(off),
        (0, shift.src_y),
        FrameCopyTexture::Metal(scratch),
        (0, 0),
        w,
        shift.moved,
    )?;
    enc.copy_texture_rect(
        FrameCopyTexture::Metal(scratch),
        (0, 0),
        FrameCopyTexture::Metal(off),
        (0, shift.dst_y),
        w,
        shift.moved,
    )
}

/// The pipeline a [`DrawItem`] binds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DrawPipe {
    Wallpaper,
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
    Fx,
    FxLinear,
    FxStore,
}

impl DrawPipe {
    /// The table row this pipe draws through.
    #[cfg(any(target_os = "macos", gl_arm))]
    #[inline]
    pub(crate) const fn row(self) -> Pipeline {
        match self {
            Self::Bg => Pipeline::Bg,
            Self::Glyph => Pipeline::Glyph,
            Self::Color => Pipeline::ColorGlyph,
            Self::CursorBlend => Pipeline::CursorBlend,
            Self::GlowAdd => Pipeline::GlowAdd,
            Self::RainGlow => Pipeline::RainGlow,
            Self::RainGlowOver => Pipeline::RainGlowOver,
            Self::FireAdd => Pipeline::FireAdd,
            Self::FireOver => Pipeline::FireOver,
            Self::DecoOver => Pipeline::DecoOver,
            Self::DecoAdd => Pipeline::DecoAdd,
            Self::Wallpaper => Pipeline::SpriteOver,
            Self::Fx => Pipeline::Fx,
            Self::FxLinear => Pipeline::FxLinear,
            Self::FxStore => Pipeline::FxStore,
        }
    }
}

/// The atlas a [`DrawItem`] samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DrawAtlas {
    Mono = 0,
    Color,
    Image,
    Deco,
    Wallpaper,
    Fx,
}

/// How many [`DrawAtlas`] slots exist.
#[cfg(any(target_os = "macos", gl_arm))]
pub(crate) const DRAW_ATLAS_COUNT: usize = DrawAtlas::Fx as usize + 1;

/// Every per-frame instance stream, in draw order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StreamId {
    Wallpaper = 0,
    Bg,
    ImageBelowBg,
    ImageBgCover,
    FxLinearUnder,
    Trail,
    GlowUnder,
    FxUnder,
    FireAdd,
    FireOver,
    FxKnockUnder,
    ImageUnder,
    GlyphHalo,
    Glyph,
    Color,
    Image,
    Deco,
    Curl,
    GlowAdd,
    FxGlow,
    GlowHalo,
    GlowHaloOver,
    NovaAdd,
    FxOver,
    RainAdd,
    RainAddOver,
    WdecoOver,
    WdecoAdd,
    FxLinearOver,
    CursorBlock,
    CursorGlyph,
    CursorColor,
    Cursor,
}

/// How many [`StreamId`]s exist (the per-arm stream array length).
pub(crate) const STREAM_COUNT: usize = StreamId::Cursor as usize + 1;

impl StreamId {
    /// Every stream in declaration (draw) order: `ALL[i] as usize == i`.
    pub(crate) const ALL: [Self; STREAM_COUNT] = [
        Self::Wallpaper,
        Self::Bg,
        Self::ImageBelowBg,
        Self::ImageBgCover,
        Self::FxLinearUnder,
        Self::Trail,
        Self::GlowUnder,
        Self::FxUnder,
        Self::FireAdd,
        Self::FireOver,
        Self::FxKnockUnder,
        Self::ImageUnder,
        Self::GlyphHalo,
        Self::Glyph,
        Self::Color,
        Self::Image,
        Self::Deco,
        Self::Curl,
        Self::GlowAdd,
        Self::FxGlow,
        Self::GlowHalo,
        Self::GlowHaloOver,
        Self::NovaAdd,
        Self::FxOver,
        Self::RainAdd,
        Self::RainAddOver,
        Self::WdecoOver,
        Self::WdecoAdd,
        Self::FxLinearOver,
        Self::CursorBlock,
        Self::CursorGlyph,
        Self::CursorColor,
        Self::Cursor,
    ];

    /// Whether this is one of the fx streams.
    #[inline]
    pub(crate) const fn is_fx(self) -> bool {
        matches!(
            self,
            Self::FxLinearUnder
                | Self::FxUnder
                | Self::FxKnockUnder
                | Self::FxGlow
                | Self::FxOver
                | Self::FxLinearOver
        )
    }

    /// The stable wire label `metrics` prints for this stream (`instances_by_stream=`).
    #[inline]
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Wallpaper => "wallpaper",
            Self::Bg => "bg",
            Self::ImageBelowBg => "image_below_bg",
            Self::ImageBgCover => "image_bg_cover",
            Self::FxLinearUnder => "fx_linear_under",
            Self::FxKnockUnder => "fx_knock_under",
            Self::Trail => "trail",
            Self::GlowUnder => "glow_under",
            Self::FxUnder => "fx_under",
            Self::FireAdd => "fire_add",
            Self::FireOver => "fire_over",
            Self::ImageUnder => "image_under",
            Self::GlyphHalo => "glyph_halo",
            Self::Glyph => "glyph",
            Self::Color => "color",
            Self::Image => "image",
            Self::Deco => "deco",
            Self::Curl => "curl",
            Self::GlowAdd => "glow_add",
            Self::FxGlow => "fx_glow",
            Self::GlowHalo => "glow_halo",
            Self::GlowHaloOver => "glow_halo_over",
            Self::NovaAdd => "nova_add",
            Self::FxOver => "fx_over",
            Self::RainAdd => "rain_add",
            Self::RainAddOver => "rain_add_over",
            Self::WdecoOver => "wdeco_over",
            Self::WdecoAdd => "wdeco_add",
            Self::FxLinearOver => "fx_linear_over",
            Self::CursorBlock => "cursor_block",
            Self::CursorGlyph => "cursor_glyph",
            Self::CursorColor => "cursor_color",
            Self::Cursor => "cursor",
        }
    }
}

/// The latest encoded frame's shape ([`GpuRenderer::frame_shape`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameShape {
    pub passes: u32,
    pub instances: [u32; STREAM_COUNT],
    pub instance_bytes: u64,
    pub build_ns: u64,
    pub stage_ns: u64,
    pub ablation: UnormAblation,
    pub unorm_draws: u32,
    pub unorm_instances: u64,
    /// Where the Unorm-law draws went: their own passes, or the sRGB passes by framebuffer fetch.
    pub route: UnormRoute,
    /// Whether the Exact Under fx stored at the end of the base pass through the sRGB view
    /// (route (1): every Exact Under layer drew opaque and no cursor glow drew under the text).
    pub under_store: bool,
    /// The framebuffer-fetch verdict as of the frame ([`GpuRenderer::fetch_probe`]): `None` until
    /// a frame that drew Unorm-law light settled it.
    pub fetch_probe: Option<FetchProbe>,
    pub fx_instances: u32,
    /// The fx bytes the frame uploaded: its instances, its palette and [`Self::fx_atlas_bytes`].
    pub fx_upload_bytes: u64,
    /// Texel bytes of the fx atlas regions the frame wrote: 0 once every art it draws is resident.
    pub fx_atlas_bytes: u64,
    pub fx_refused: u64,
    /// Metal: the frames in flight the fx atlas sync waited for before rewriting a region they may
    /// read, and the nanoseconds it parked; 0 on the other arms, whose queues order the writes.
    pub fx_reader_waits: u64,
    pub fx_reader_wait_ns: u64,
}

/// The pass-economy ablation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UnormAblation {
    #[default]
    Off,
    Drop,
    Thin,
}

impl UnormAblation {
    /// The seam's spelling, and `metrics`' `unorm_ablation=` word.
    #[must_use]
    #[inline]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Drop => "drop",
            Self::Thin => "thin",
        }
    }

    #[inline]
    fn from_seam(value: &str) -> Self {
        match value {
            "drop" | "1" => Self::Drop,
            "thin" => Self::Thin,
            _ => Self::Off,
        }
    }
}

/// How a frame's Unorm-law draws (the additive and glow groups) reached the offscreen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UnormRoute {
    /// Own passes on the Unorm view: every arm, and Metal until its device passed the fetch probe.
    #[default]
    Split,
    /// Inside the sRGB passes, by framebuffer fetch (Metal on an Apple GPU whose probe passed).
    Fetch,
}

impl UnormRoute {
    /// The seam's spelling, and `metrics`' `gpu_unorm_route=` word.
    #[must_use]
    #[inline]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Split => "split",
            Self::Fetch => "fetch",
        }
    }

    #[inline]
    fn from_seam(value: &str) -> Self {
        match value {
            "split" => Self::Split,
            _ => Self::Fetch,
        }
    }

    /// The route a renderer starts on: fetch wherever the device allows it, unless the
    /// `ATERM_GPU_UNORM_ROUTE` dev seam (`seam`, `None` without dev seams) names the split.
    #[inline]
    fn initial(seam: Option<&std::ffi::OsStr>) -> Self {
        seam.and_then(std::ffi::OsStr::to_str)
            .map_or(Self::Fetch, Self::from_seam)
    }
}

/// Which stage of the framebuffer-fetch probe a byte failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FetchStage {
    /// A byte's linear value did not store as that byte.
    Store,
    /// A stored byte, fetched and stored again, came back another byte.
    RoundTrip,
    /// A fetch after a fixed-function sRGB blend in the pass read another byte than it stores.
    InPass,
}

/// What a device's framebuffer-fetch probe found: whether `encode(decode(b)) == b` for all 256
/// bytes of every channel through its sRGB store and fetch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FetchProbe {
    /// No framebuffer fetch on the device (not an Apple GPU) or on the arm: the split stays.
    Unsupported,
    /// Every byte came back: Unorm-law draws blend inside the sRGB passes.
    Passed,
    /// `stage` returned `got` for `want` in `channel` (0 red … 3 alpha): the split stays.
    Failed {
        stage: FetchStage,
        channel: u8,
        want: u8,
        got: u8,
    },
    /// The probe could not run (a build, an encode or a readback failed): the split stays.
    Refused,
}

impl FetchProbe {
    /// `metrics`' `gpu_fetch_probe=` word.
    #[must_use]
    #[inline]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Unsupported => "unsupported",
            Self::Passed => "passed",
            Self::Failed { .. } => "failed",
            Self::Refused => "refused",
        }
    }
}

impl Default for FrameShape {
    fn default() -> Self {
        Self {
            passes: 0,
            instances: [0; STREAM_COUNT],
            instance_bytes: 0,
            build_ns: 0,
            stage_ns: 0,
            ablation: UnormAblation::Off,
            unorm_draws: 0,
            unorm_instances: 0,
            route: UnormRoute::Split,
            under_store: false,
            fetch_probe: None,
            fx_instances: 0,
            fx_upload_bytes: 0,
            fx_atlas_bytes: 0,
            fx_refused: 0,
            fx_reader_waits: 0,
            fx_reader_wait_ns: 0,
        }
    }
}

impl FrameShape {
    /// Every stream's wire label, in [`Self::instances`] order.
    pub const STREAMS: [&'static str; STREAM_COUNT] = {
        let mut names = [""; STREAM_COUNT];
        let mut i = 0;
        while i < STREAM_COUNT {
            names[i] = StreamId::ALL[i].name();
            i += 1;
        }
        names
    };

    /// The non-empty streams as `(label, instances)`, in draw order.
    #[inline]
    pub fn streams(&self) -> impl Iterator<Item = (&'static str, u32)> + '_ {
        Self::STREAMS
            .iter()
            .zip(self.instances.iter())
            .filter(|&(_, &n)| n > 0)
            .map(|(&name, &n)| (name, n))
    }

    /// Instances across every stream.
    #[must_use]
    #[inline]
    pub fn total_instances(&self) -> u64 {
        self.instances.iter().map(|&n| u64::from(n)).sum()
    }
}

/// A built frame plan's Unorm-law draws and instances, its ablation, the route those draws took
/// ([`route_frame_plan`]), and whether its Exact Under fx stores in the base pass (route (1)).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PlanUnorm {
    pub(crate) ablation: UnormAblation,
    pub(crate) draws: u32,
    pub(crate) instances: u64,
    pub(crate) route: UnormRoute,
    pub(crate) under_store: bool,
}

/// One draw: bind `pipe` (its fetch twin when `fetch`) and `atlas` on change, bind `stream`, and
/// draw `count` instanced quads in the pass `group` coalesces into.
#[derive(Clone, Copy, Debug)]
pub(crate) struct DrawItem {
    pub(crate) group: u8,
    pub(crate) pipe: DrawPipe,
    pub(crate) atlas: Option<DrawAtlas>,
    pub(crate) stream: StreamId,
    pub(crate) count: u32,
    /// A Unorm-law draw routed through its fetch twin in the sRGB view ([`UnormRoute::Fetch`]).
    pub(crate) fetch: bool,
}

impl DrawItem {
    /// Whether this draw renders through the sRGB view: an sRGB group's or a fetch twin's.
    #[inline]
    pub(crate) const fn srgb(&self) -> bool {
        GROUP_SRGB[self.group as usize] || self.fetch
    }
}

/// The presence flags [`build_frame_plan`] gates on.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FramePlanCtx {
    pub(crate) has_image_plane: bool,
    pub(crate) has_deco: bool,
    pub(crate) has_wallpaper: bool,
    pub(crate) cursor_opaque: bool,
    pub(crate) ablation: UnormAblation,
    /// Every Exact layer at Under draws only opaque pixels ([`fx_gpu::under_draws_opaque`]), so
    /// route (1) of the pass economy may store its fx at the end of the base pass.
    pub(crate) under_opaque: bool,
}

/// Route `plan`'s Unorm-law draws (built on the split) by `route`: under [`UnormRoute::Fetch`]
/// each takes its row's twin in the sRGB view, so the frame opens no Unorm-view pass.
#[cfg(target_os = "macos")]
pub(crate) fn route_frame_plan(plan: &mut [DrawItem], unorm: &mut PlanUnorm, route: UnormRoute) {
    unorm.route = route;
    if route == UnormRoute::Fetch {
        for item in plan
            .iter_mut()
            .filter(|item| !GROUP_SRGB[usize::from(item.group)])
        {
            item.fetch = true;
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one line per draw of the ladder; splitting would scatter the order law"
)]
pub(crate) fn build_frame_plan(
    plan: &mut Vec<DrawItem>,
    counts: &[Option<u32>; STREAM_COUNT],
    ctx: FramePlanCtx,
) -> PlanUnorm {
    plan.clear();
    let mut push = |group: usize, stream: StreamId, pipe: DrawPipe, atlas: Option<DrawAtlas>| {
        let mut count = counts[stream as usize];
        if !GROUP_SRGB[group] {
            match ctx.ablation {
                UnormAblation::Off => {}
                UnormAblation::Drop => return,
                UnormAblation::Thin => count = count.map(|n| n.min(1)),
            }
        }
        if let Some(count) = count {
            plan.push(DrawItem {
                group: group as u8,
                pipe,
                atlas,
                stream,
                count,
                fetch: false,
            });
        }
    };
    // Route (1) of the pass economy: an opaque Exact Under layer's fx is a store, so it may
    // draw at the end of the base pass through the sRGB view, where it lands in the same
    // order against every other draw unless a cursor glow under the text sits between.
    let under_store = ctx.under_opaque && counts[StreamId::GlowUnder as usize].is_none();

    push(
        G_BASE_BG,
        StreamId::Wallpaper,
        DrawPipe::Wallpaper,
        ctx.has_wallpaper.then_some(DrawAtlas::Wallpaper),
    );
    push(G_BASE_BG, StreamId::Bg, DrawPipe::Bg, None);
    if ctx.has_image_plane {
        push(
            G_BASE_BG,
            StreamId::ImageBelowBg,
            DrawPipe::Color,
            Some(DrawAtlas::Image),
        );
    }
    push(G_BASE_BG, StreamId::ImageBgCover, DrawPipe::Bg, None);
    // The Linear layers under the text but the companions: the rain's rows (lane B7 of
    // `docs/DESIGN-card-engine-2026-10-19.md`), under the trail and its light.
    push(
        G_BASE_BG,
        StreamId::FxLinearUnder,
        DrawPipe::FxLinear,
        Some(DrawAtlas::Fx),
    );
    push(G_BASE_BG, StreamId::Trail, DrawPipe::Bg, None);
    if under_store {
        push(
            G_BASE_BG,
            StreamId::FxUnder,
            DrawPipe::FxStore,
            Some(DrawAtlas::Fx),
        );
    }

    push(G_GLOW_UNDER, StreamId::GlowUnder, DrawPipe::GlowAdd, None);
    if !under_store {
        push(
            G_GLOW_UNDER,
            StreamId::FxUnder,
            DrawPipe::Fx,
            Some(DrawAtlas::Fx),
        );
    }
    push(G_GLOW_UNDER, StreamId::FireAdd, DrawPipe::FireAdd, None);
    push(G_GLOW_UNDER, StreamId::FireOver, DrawPipe::FireOver, None);

    // The companions' bodies (the knock layers) open the base-fg group: they stand on the
    // trail's light, under the ink, each cut where its letters lie
    // (`aterm_render::Renderer::cut_knock_layers`). Drawn before the light, a cat whose head
    // reached into the typed row wore the rainbow ribbon's colours, and the trail's bed cut a
    // band out of a cat on the typed row. An sRGB group, so the pass count is unchanged.
    push(
        G_BASE_FG,
        StreamId::FxKnockUnder,
        DrawPipe::FxLinear,
        Some(DrawAtlas::Fx),
    );

    if ctx.has_image_plane {
        push(
            G_BASE_FG,
            StreamId::ImageUnder,
            DrawPipe::Color,
            Some(DrawAtlas::Image),
        );
    }
    push(
        G_BASE_FG,
        StreamId::GlyphHalo,
        DrawPipe::DecoOver,
        Some(DrawAtlas::Mono),
    );
    push(
        G_BASE_FG,
        StreamId::Glyph,
        DrawPipe::Glyph,
        Some(DrawAtlas::Mono),
    );
    push(
        G_BASE_FG,
        StreamId::Color,
        DrawPipe::Color,
        Some(DrawAtlas::Color),
    );
    if ctx.has_image_plane {
        push(
            G_BASE_FG,
            StreamId::Image,
            DrawPipe::Color,
            Some(DrawAtlas::Image),
        );
    }
    push(G_BASE_FG, StreamId::Deco, DrawPipe::Bg, None);
    if ctx.has_deco {
        push(
            G_BASE_FG,
            StreamId::Curl,
            DrawPipe::DecoOver,
            Some(DrawAtlas::Deco),
        );
    }

    push(G_GLOW, StreamId::GlowAdd, DrawPipe::GlowAdd, None);
    push(G_GLOW, StreamId::FxGlow, DrawPipe::Fx, Some(DrawAtlas::Fx));
    push(G_GLOW, StreamId::GlowHalo, DrawPipe::RainGlow, None);
    push(G_GLOW, StreamId::GlowHaloOver, DrawPipe::RainGlowOver, None);
    push(G_GLOW, StreamId::NovaAdd, DrawPipe::GlowAdd, None);
    push(G_GLOW, StreamId::FxOver, DrawPipe::Fx, Some(DrawAtlas::Fx));
    push(G_GLOW, StreamId::RainAdd, DrawPipe::RainGlow, None);
    push(G_GLOW, StreamId::RainAddOver, DrawPipe::RainGlowOver, None);

    if ctx.has_deco {
        push(
            G_WDECO_OVER,
            StreamId::WdecoOver,
            DrawPipe::DecoOver,
            Some(DrawAtlas::Deco),
        );
        push(
            G_WDECO_ADD,
            StreamId::WdecoAdd,
            DrawPipe::DecoAdd,
            Some(DrawAtlas::Deco),
        );
    }

    push(
        G_FREE_OVER,
        StreamId::FxLinearOver,
        DrawPipe::FxLinear,
        Some(DrawAtlas::Fx),
    );

    let cursor_fill = if ctx.cursor_opaque {
        DrawPipe::Bg
    } else {
        DrawPipe::CursorBlend
    };
    push(G_CURSOR, StreamId::CursorBlock, cursor_fill, None);
    push(
        G_CURSOR,
        StreamId::CursorGlyph,
        DrawPipe::Glyph,
        Some(DrawAtlas::Mono),
    );
    push(
        G_CURSOR,
        StreamId::CursorColor,
        DrawPipe::Color,
        Some(DrawAtlas::Color),
    );
    push(G_CURSOR, StreamId::Cursor, cursor_fill, None);

    let unorm = plan
        .iter()
        .filter(|item| !GROUP_SRGB[usize::from(item.group)]);
    PlanUnorm {
        ablation: ctx.ablation,
        draws: u32::try_from(unorm.clone().count()).unwrap_or(u32::MAX),
        instances: unorm.map(|item| u64::from(item.count)).sum(),
        route: UnormRoute::Split,
        under_store: under_store && counts[StreamId::FxUnder as usize].is_some(),
    }
}

/// The wgpu arm's resolution set: the live objects each plan key maps to.
#[cfg(wgpu_arm)]
pub(crate) struct WgpuFrameRes<'a> {
    pub(crate) bg_pipeline: &'a wgpu::RenderPipeline,
    pub(crate) glyph_pipeline: &'a wgpu::RenderPipeline,
    pub(crate) color_glyph_pipeline: &'a wgpu::RenderPipeline,
    pub(crate) effects: &'a EffectPipelines,
    pub(crate) uniform_bg: &'a wgpu::BindGroup,
    pub(crate) view: &'a wgpu::TextureView,
    pub(crate) view_srgb: &'a wgpu::TextureView,
    pub(crate) atlas_mono: &'a wgpu::BindGroup,
    pub(crate) atlas_color: &'a wgpu::BindGroup,
    pub(crate) atlas_deco: Option<&'a wgpu::BindGroup>,
    pub(crate) atlas_image: Option<&'a wgpu::BindGroup>,
    pub(crate) atlas_wallpaper: Option<&'a wgpu::BindGroup>,
    pub(crate) atlas_fx: Option<&'a wgpu::BindGroup>,
    pub(crate) streams: [Option<wgpu::BufferSlice<'a>>; STREAM_COUNT],
}

#[allow(
    clippy::large_enum_variant,
    reason = "one FrameRes lives on the stack per encoded frame (the 864-byte \
              Wgpu arm is 27 stream slices + the bind refs); boxing it would put \
              a heap allocation on the keystroke-echo path to save nothing"
)]
pub(crate) enum FrameRes<'a> {
    #[cfg(wgpu_arm)]
    Wgpu(WgpuFrameRes<'a>),
    #[cfg(target_os = "macos")]
    Metal(&'a MetalFrameRig),
    #[cfg(gl_arm)]
    Gl(&'a GlFrameRig<'a>),
}

impl FrameRes<'_> {
    #[inline]
    const fn view(&self, srgb: bool) -> crate::device_layer::FrameView<'_> {
        match self {
            #[cfg(wgpu_arm)]
            Self::Wgpu(r) => {
                crate::device_layer::FrameView::Wgpu(if srgb { r.view_srgb } else { r.view })
            }
            #[cfg(target_os = "macos")]
            Self::Metal(rig) => crate::device_layer::FrameView::Metal(if srgb {
                &rig.offscreen_srgb
            } else {
                &rig.offscreen
            }),
            #[cfg(gl_arm)]
            Self::Gl(rig) => crate::device_layer::FrameView::Gl {
                tex: rig.offscreen,
                srgb,
            },
        }
    }

    #[inline]
    fn uniforms(&self) -> crate::device_layer::FrameUniforms<'_> {
        match self {
            #[cfg(wgpu_arm)]
            Self::Wgpu(r) => crate::device_layer::FrameUniforms::Wgpu(r.uniform_bg),
            #[cfg(target_os = "macos")]
            Self::Metal(rig) => rig.uniforms(),
            #[cfg(gl_arm)]
            Self::Gl(rig) => crate::device_layer::FrameUniforms::Gl {
                buf: rig.uniform,
                binding: rig.uniform_binding,
            },
        }
    }

    #[inline]
    fn pipeline(&self, pipe: DrawPipe, fetch: bool) -> crate::device_layer::FramePipeline<'_> {
        #[cfg(any(wgpu_arm, gl_arm))]
        let split_only = || {
            assert!(
                !fetch,
                "only the Metal arm routes a draw through framebuffer fetch"
            );
        };
        match self {
            #[cfg(wgpu_arm)]
            Self::Wgpu(r) => crate::device_layer::FramePipeline::Wgpu({
                split_only();
                match pipe {
                    DrawPipe::Bg => r.bg_pipeline,
                    DrawPipe::Glyph => r.glyph_pipeline,
                    DrawPipe::Color => r.color_glyph_pipeline,
                    DrawPipe::CursorBlend => r.effects.get(EffectPipeline::CursorBlend),
                    DrawPipe::GlowAdd => r.effects.get(EffectPipeline::GlowAdd),
                    DrawPipe::RainGlow => r.effects.get(EffectPipeline::RainGlow),
                    DrawPipe::RainGlowOver => r.effects.get(EffectPipeline::RainGlowOver),
                    DrawPipe::FireAdd => r.effects.get(EffectPipeline::FireAdd),
                    DrawPipe::FireOver => r.effects.get(EffectPipeline::FireOver),
                    DrawPipe::DecoOver => r.effects.get(EffectPipeline::DecoOver),
                    DrawPipe::DecoAdd => r.effects.get(EffectPipeline::DecoAdd),
                    DrawPipe::Wallpaper => r.effects.get(EffectPipeline::SpriteOver),
                    DrawPipe::Fx => r.effects.get(EffectPipeline::Fx),
                    DrawPipe::FxLinear => r.effects.get(EffectPipeline::FxLinear),
                    DrawPipe::FxStore => r.effects.get(EffectPipeline::FxStore),
                }
            }),
            #[cfg(target_os = "macos")]
            Self::Metal(rig) => rig.pipeline(pipe, fetch),
            #[cfg(gl_arm)]
            Self::Gl(rig) => {
                split_only();
                rig.pipeline(pipe)
            }
        }
    }

    #[inline]
    fn atlas(&self, atlas: DrawAtlas, pipe: DrawPipe) -> crate::device_layer::FrameAtlas<'_> {
        #[cfg(not(any(target_os = "macos", gl_arm)))]
        let _ = pipe;
        match self {
            #[cfg(wgpu_arm)]
            Self::Wgpu(r) => {
                let bind = match atlas {
                    DrawAtlas::Mono => Some(r.atlas_mono),
                    DrawAtlas::Color => Some(r.atlas_color),
                    DrawAtlas::Deco => r.atlas_deco,
                    DrawAtlas::Image => r.atlas_image,
                    DrawAtlas::Wallpaper => r.atlas_wallpaper,
                    DrawAtlas::Fx => r.atlas_fx,
                };
                crate::device_layer::FrameAtlas::Wgpu(bind.unwrap_or_else(|| {
                    panic!(
                        "frame plan: item tagged atlas {atlas:?} but its bind group is \
                         absent — build_frame_plan's coupling broke"
                    )
                }))
            }
            #[cfg(target_os = "macos")]
            Self::Metal(rig) => rig.atlas(atlas, pipe),
            #[cfg(gl_arm)]
            Self::Gl(rig) => rig.atlas(atlas, pipe),
        }
    }

    #[inline]
    fn stream(&self, id: StreamId) -> crate::device_layer::FrameStream<'_> {
        match self {
            #[cfg(wgpu_arm)]
            Self::Wgpu(r) => crate::device_layer::FrameStream::Wgpu(
                r.streams[id as usize].unwrap_or_else(|| {
                    panic!(
                        "frame plan: item for stream {id:?} but its buffer was not \
                         uploaded — build_frame_plan's gate broke"
                    )
                }),
            ),
            #[cfg(target_os = "macos")]
            Self::Metal(rig) => rig.stream(id),
            #[cfg(gl_arm)]
            Self::Gl(rig) => rig.stream(id),
        }
    }
}

/// The GL arm's resolution set.
#[cfg(gl_arm)]
pub(crate) struct GlFrameRig<'a> {
    pub(crate) offscreen: &'a crate::gl::resources::GlTexture,
    pub(crate) pipelines:
        [Option<&'a crate::gl::pipelines::GlPipeline>; pipeline_table::PIPELINE_COUNT],
    pub(crate) uniform: &'a crate::gl::resources::GlBuffer,
    pub(crate) uniform_binding: u32,
    pub(crate) streams: [Option<&'a crate::gl::resources::GlBuffer>; STREAM_COUNT],
    pub(crate) atlases: [Option<(
        &'a crate::gl::resources::GlTexture,
        &'a crate::gl::resources::GlSampler,
    )>; DRAW_ATLAS_COUNT],
    pub(crate) fx: Option<GlFxBind<'a>>,
}

/// The GL rig's [`DrawAtlas::Fx`] resources.
#[cfg(gl_arm)]
#[derive(Clone, Copy)]
pub(crate) struct GlFxBind<'a> {
    pub(crate) r8: &'a crate::gl::resources::GlTexture,
    pub(crate) rgba: &'a crate::gl::resources::GlTexture,
    pub(crate) palette: &'a crate::gl::resources::GlTexture,
    pub(crate) sampler: &'a crate::gl::resources::GlSampler,
}

#[cfg(any(target_os = "macos", gl_arm))]
#[inline]
fn fx_bind_slots(pipe: DrawPipe) -> ([u32; 2], u32) {
    let binds = &pipe.row().spec().binds;
    let [r8, rgba] = binds.fragment_textures else {
        panic!(
            "row {:?} binds the fx atlases at two fragment textures",
            pipe.row()
        )
    };
    let palette = binds
        .fragment_palette
        .unwrap_or_else(|| panic!("row {:?} binds no fx palette", pipe.row()));
    ([*r8, *rgba], palette)
}

#[cfg(gl_arm)]
impl GlFrameRig<'_> {
    #[inline]
    fn pipeline(&self, pipe: DrawPipe) -> crate::device_layer::FramePipeline<'_> {
        let row = pipe.row();
        crate::device_layer::FramePipeline::Gl(
            self.pipelines[row as usize].unwrap_or_else(|| {
                panic!("gl frame rig: no program for row {row:?} (pipe {pipe:?})")
            }),
        )
    }

    #[inline]
    fn atlas(&self, atlas: DrawAtlas, pipe: DrawPipe) -> crate::device_layer::FrameAtlas<'_> {
        if atlas == DrawAtlas::Fx {
            let fx = self
                .fx
                .unwrap_or_else(|| panic!("gl frame rig: no fx bind (pipe {pipe:?})"));
            let ([u0, u1], u2) = fx_bind_slots(pipe);
            return crate::device_layer::FrameAtlas::GlFx {
                r8: fx.r8,
                rgba: fx.rgba,
                palette: fx.palette,
                sampler: fx.sampler,
                units: [u0, u1, u2],
            };
        }
        let unit = *pipe
            .row()
            .spec()
            .binds
            .fragment_textures
            .first()
            .unwrap_or_else(|| panic!("row {:?} samples no fragment texture", pipe.row()));
        let (tex, sampler) = self.atlases[atlas as usize]
            .unwrap_or_else(|| panic!("gl frame rig: no atlas for {atlas:?} (pipe {pipe:?})"));
        crate::device_layer::FrameAtlas::Gl { tex, sampler, unit }
    }

    #[inline]
    fn stream(&self, id: StreamId) -> crate::device_layer::FrameStream<'_> {
        crate::device_layer::FrameStream::Gl(
            self.streams[id as usize]
                .unwrap_or_else(|| panic!("gl frame rig: no stream buffer for {id:?}")),
        )
    }
}

/// The Metal arm's resolution set, minted on one loss domain.
#[cfg(target_os = "macos")]
pub(crate) struct MetalFrameRig {
    pub(crate) offscreen: crate::metal::resources::SealedTexture,
    pub(crate) offscreen_srgb: crate::metal::resources::SealedTexture,
    pub(crate) psos: [Option<crate::metal::ffi::Obj>; pipeline_table::PIPELINE_COUNT],
    /// The framebuffer-fetch twins the plan's fetch draws bind, by [`pipeline_table::FetchTwin`].
    pub(crate) fetch_psos: [Option<crate::metal::ffi::Obj>; pipeline_table::FETCH_TWIN_COUNT],
    pub(crate) uniform: crate::metal::ffi::Obj,
    pub(crate) uniform_vertex_slot: u32,
    pub(crate) uniform_fragment_slot: Option<u32>,
    pub(crate) streams: [Option<crate::metal::resources::SharedBuffer>; STREAM_COUNT],
    pub(crate) atlases: [Option<(
        crate::metal::resources::SealedTexture,
        crate::metal::ffi::Obj,
    )>; DRAW_ATLAS_COUNT],
    pub(crate) fx: Option<MetalFxBind>,
}

/// The Metal rig's [`DrawAtlas::Fx`] resources, owned +1 for the frame.
#[cfg(target_os = "macos")]
pub(crate) struct MetalFxBind {
    pub(crate) r8: crate::metal::resources::SealedTexture,
    pub(crate) rgba: crate::metal::resources::SealedTexture,
    pub(crate) palette: crate::metal::ffi::Obj,
}

#[cfg(target_os = "macos")]
impl MetalFrameRig {
    #[inline]
    const fn uniforms(&self) -> crate::device_layer::FrameUniforms<'_> {
        crate::device_layer::FrameUniforms::Metal {
            buf: &self.uniform,
            vertex_slot: self.uniform_vertex_slot,
            fragment_slot: self.uniform_fragment_slot,
        }
    }

    #[inline]
    fn pipeline(&self, pipe: DrawPipe, fetch: bool) -> crate::device_layer::FramePipeline<'_> {
        let row = pipe.row();
        let pso = if fetch {
            let twin = pipeline_table::FetchTwin::of(row)
                .unwrap_or_else(|| panic!("metal frame rig: row {row:?} has no fetch twin"));
            self.fetch_psos[twin as usize].as_ref()
        } else {
            self.psos[row as usize].as_ref()
        };
        crate::device_layer::FramePipeline::Metal(pso.unwrap_or_else(|| {
            panic!("metal frame rig: no PSO for row {row:?} (pipe {pipe:?}, fetch {fetch})")
        }))
    }

    #[inline]
    fn atlas(&self, atlas: DrawAtlas, pipe: DrawPipe) -> crate::device_layer::FrameAtlas<'_> {
        if atlas == DrawAtlas::Fx {
            let fx = self
                .fx
                .as_ref()
                .unwrap_or_else(|| panic!("metal frame rig: no fx bind (pipe {pipe:?})"));
            let (tex_slots, palette_slot) = fx_bind_slots(pipe);
            return crate::device_layer::FrameAtlas::MetalFx {
                r8: fx.r8.obj(),
                rgba: fx.rgba.obj(),
                palette: &fx.palette,
                tex_slots,
                palette_slot,
            };
        }
        let binds = &pipe.row().spec().binds;
        let (&tex_slot, &sampler_slot) = (
            binds
                .fragment_textures
                .first()
                .unwrap_or_else(|| panic!("row {:?} samples no fragment texture", pipe.row())),
            binds
                .fragment_samplers
                .first()
                .unwrap_or_else(|| panic!("row {:?} binds no fragment sampler", pipe.row())),
        );
        let (tex, sampler) = self.atlases[atlas as usize].as_ref().unwrap_or_else(|| {
            panic!("metal frame rig: no atlas minted for {atlas:?} (pipe {pipe:?})")
        });
        crate::device_layer::FrameAtlas::Metal {
            tex: tex.obj(),
            sampler,
            tex_slot,
            sampler_slot,
        }
    }

    #[inline]
    fn stream(&self, id: StreamId) -> crate::device_layer::FrameStream<'_> {
        crate::device_layer::FrameStream::Metal(
            self.streams[id as usize]
                .as_ref()
                .unwrap_or_else(|| panic!("metal frame rig: no stream buffer for {id:?}"))
                .obj(),
        )
    }
}

#[cfg(not(wgpu_arm))]
fn first_party_note(what: &str) {
    #[cfg(target_os = "macos")]
    metal_arm_note(what);
    #[cfg(gl_arm)]
    gl_arm::gl_arm_note(what);
    #[cfg(not(any(target_os = "macos", gl_arm)))]
    crate::stderr_line!("aterm-gpu: {what}");
}

#[cfg(target_os = "macos")]
fn metal_arm_note(what: &str) {
    use std::sync::{Mutex, OnceLock};
    static SEEN: OnceLock<Mutex<std::collections::BTreeSet<String>>> = OnceLock::new();
    let seen = SEEN.get_or_init(|| Mutex::new(std::collections::BTreeSet::new()));
    if seen
        .lock()
        .expect("the note set is never poisoned (no panics under the lock)")
        .insert(what.to_owned())
    {
        crate::stderr_line!("aterm-gpu (metal arm): {what}");
    }
}

#[cfg(target_os = "macos")]
struct ArmAtlas {
    tex: crate::device_layer::LayerTexture,
    key: (u32, u32, u64),
    write_gen: u64,
    uploaded_py: u32,
}

#[cfg(target_os = "macos")]
struct ArmStream {
    buf: crate::device_layer::LayerBuffer,
    cap: usize,
}

#[cfg(target_os = "macos")]
const ARM_FRAME_SLOTS: usize = 3;

#[cfg(target_os = "macos")]
const ARM_SUBMIT_B_SLOTS: usize = 5;

#[cfg(target_os = "macos")]
struct ArmFrameSlot {
    uniform: crate::device_layer::LayerBuffer,
    uniform_key: Option<(u32, u32, u32)>,
    streams: [Option<ArmStream>; STREAM_COUNT],
    fx_palette: Option<ArmStream>,
    inflight: Option<crate::metal::encoder::Submitted>,
}

#[cfg(target_os = "macos")]
#[derive(Default)]
struct ArmSubmitBSlot {
    glow_stream: Option<ArmStream>,
    bloom_cell_u: Option<ArmStream>,
    bloom_u: Option<ArmStream>,
    shimmer_u: Option<ArmStream>,
    tray_u: Option<ArmStream>,
    crown_u: Option<ArmStream>,
    blit_u: Option<(BlitUniform, crate::device_layer::LayerBuffer)>,
}

#[cfg(target_os = "macos")]
struct MetalArmLive {
    /// The first-frame warm this arm waits out and takes the fetch verdict from
    /// ([`crate::metal::warm::FIRST_FRAME`], or a test's own latch).
    warm: &'static crate::metal::warm::WarmLatch,
    mint: crate::metal::resources::MetalResourceDevice,
    session: crate::metal::encoder::EncodeSession,
    lib: crate::metal::ffi::Library,
    psos: [Option<crate::metal::ffi::Obj>; pipeline_table::PIPELINE_COUNT],
    /// The device's framebuffer-fetch verdict, `None` until the first frame that draws Unorm-law
    /// light settles it ([`Self::fetch_verdict`]); [`FetchProbe::Passed`] routes those draws
    /// through [`Self::fetch_psos`].
    fetch: Option<FetchProbe>,
    fetch_psos: [Option<crate::metal::ffi::Obj>; pipeline_table::FETCH_TWIN_COUNT],
    nearest: crate::metal::ffi::Obj,
    linear: crate::metal::ffi::Obj,
    frame_slots: [ArmFrameSlot; ARM_FRAME_SLOTS],
    cur_slot: usize,
    submit_b_slots: [ArmSubmitBSlot; ARM_SUBMIT_B_SLOTS],
    submit_b_idx: usize,
    atlases: [Option<ArmAtlas>; DRAW_ATLAS_COUNT],
    libs: Vec<(ShaderLibrary, crate::metal::ffi::Library)>,
    present_psos: Vec<(
        (Pipeline, crate::metal::ffi::PixelFormat),
        crate::metal::ffi::Obj,
    )>,
    pending: Vec<PendingPresent>,
    present_seq: u64,
    awaited_frames: u64,
    frame_slot_wait_ns: u64,
    pending_ring_wait_ns: u64,
    encode_inline_waits: std::cell::Cell<u64>,
    encode_band_shifts: std::cell::Cell<u64>,
    fx_tex: [Option<fx_gpu::FxArmTex>; 2],
    fx_hazard: FxHazard,
    /// The last fx sync's reader settle ([`Self::sync_fx`]): the frames in flight it waited for
    /// before rewriting a region they may read, and the nanoseconds it parked.
    fx_reader_wait: (u64, u64),
    #[cfg(any(test, debug_assertions))]
    fx_shared_palette: Option<(crate::device_layer::LayerTexture, u32)>,
    /// Pipeline states the warms ([`crate::metal::warm::spawn`]) built off this thread that this
    /// arm has not adopted yet ([`Self::adopt_warmed`]), and the sender each warm's worker takes.
    warmed: (
        std::sync::mpsc::Sender<crate::metal::warm::Warmed>,
        std::sync::mpsc::Receiver<crate::metal::warm::Warmed>,
    ),
    /// What a pipeline warm is building for this arm and has not handed over.
    warm_owed: Vec<crate::metal::warm::WarmItem>,
    /// The pipeline states this arm built on its own thread, in order.
    #[cfg(test)]
    built_here: Vec<crate::metal::warm::WarmItem>,
}

#[cfg(target_os = "macos")]
struct PendingPresent {
    cb: crate::metal::encoder::Submitted,
    ticket: crate::metal::swapchain::PresentTicket,
    seq: u64,
}

#[cfg(all(target_os = "macos", test))]
thread_local! {
    static INJECTED_MINT_FAILURE: std::cell::Cell<Option<&'static str>> =
        const { std::cell::Cell::new(None) };
}

/// The fx hazard drill's record of a Metal arm (zero-sized outside test and debug builds).
#[cfg(target_os = "macos")]
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct FxHazard {
    #[cfg(any(test, debug_assertions))]
    pub(crate) settles: u64,
    #[cfg(any(test, debug_assertions))]
    pub(crate) settle_ns: u64,
    #[cfg(any(test, debug_assertions))]
    pub(crate) park_ns: u64,
    #[cfg(any(test, debug_assertions))]
    pub(crate) palette: usize,
}

#[cfg(target_os = "macos")]
impl FxHazard {
    #[inline]
    const fn parked(&mut self, ns: u64) {
        #[cfg(any(test, debug_assertions))]
        {
            self.park_ns = self.park_ns.saturating_add(ns);
        }
        #[cfg(not(any(test, debug_assertions)))]
        let _ = ns;
    }

    #[inline]
    fn settled(&mut self, ns: u64) {
        #[cfg(any(test, debug_assertions))]
        {
            self.settles += 1;
            self.settle_ns = self.settle_ns.saturating_add(ns);
        }
        self.parked(ns);
    }

    #[inline]
    fn bound(&mut self, palette: &crate::metal::ffi::Obj) {
        #[cfg(any(test, debug_assertions))]
        {
            self.palette = palette.id().as_ptr().addr();
        }
        #[cfg(not(any(test, debug_assertions)))]
        let _ = palette;
    }
}

#[cfg(target_os = "macos")]
impl MetalArmLive {
    fn new(
        latch: Arc<crate::metal::loss::LossLatch>,
        warm: &'static crate::metal::warm::WarmLatch,
    ) -> Result<Self, String> {
        #[cfg(test)]
        if let Some(injected) = INJECTED_MINT_FAILURE.with(std::cell::Cell::take) {
            return Err(injected.to_owned());
        }
        let Some(dev) = crate::metal::ffi::Device::preferred() else {
            return Err("no Metal device on this machine".to_owned());
        };
        let session = crate::metal::encoder::EncodeSession::new(&dev, Arc::clone(&latch))?;
        let mint = crate::metal::resources::MetalResourceDevice::new(&dev, latch);
        let lib = crate::metal::pipelines::compile_library(&dev, ShaderLibrary::Cell)?;
        let fetch = crate::metal::fetch::known(&dev, &lib);
        let layer = crate::device_layer::DeviceHandle::Metal(&mint);
        let nearest = layer
            .try_create_sampler(
                "aterm-gpu metal nearest sampler",
                crate::device_layer::SamplerKind::NearestClamp,
            )?
            .into_metal();
        let linear = layer
            .try_create_sampler(
                "aterm-gpu metal linear sampler",
                crate::device_layer::SamplerKind::LinearClamp,
            )?
            .into_metal();
        let mint_uniform = || {
            layer.try_create_uniform_buffer(
                "aterm-gpu metal frame uniforms",
                std::mem::size_of::<Uniforms>() as u64,
            )
        };
        let (u0, u1, u2) = (mint_uniform()?, mint_uniform()?, mint_uniform()?);
        let frame_slots = [u0, u1, u2].map(|uniform| ArmFrameSlot {
            uniform,
            uniform_key: None,
            streams: std::array::from_fn(|_| None),
            fx_palette: None,
            inflight: None,
        });
        Ok(Self {
            warm,
            mint,
            session,
            lib,
            psos: std::array::from_fn(|_| None),
            fetch,
            fetch_psos: std::array::from_fn(|_| None),
            nearest,
            linear,
            frame_slots,
            cur_slot: 0,
            submit_b_slots: std::array::from_fn(|_| ArmSubmitBSlot::default()),
            submit_b_idx: 0,
            atlases: std::array::from_fn(|_| None),
            libs: Vec::new(),
            present_psos: Vec::new(),
            pending: Vec::new(),
            present_seq: 0,
            awaited_frames: 0,
            frame_slot_wait_ns: 0,
            pending_ring_wait_ns: 0,
            encode_inline_waits: std::cell::Cell::new(0),
            encode_band_shifts: std::cell::Cell::new(0),
            fx_tex: [None, None],
            fx_hazard: FxHazard::default(),
            fx_reader_wait: (0, 0),
            #[cfg(any(test, debug_assertions))]
            fx_shared_palette: None,
            warmed: std::sync::mpsc::channel(),
            warm_owed: Vec::new(),
            #[cfg(test)]
            built_here: Vec::new(),
        })
    }

    #[inline]
    const fn layer(&self) -> crate::device_layer::DeviceHandle<'_> {
        crate::device_layer::DeviceHandle::Metal(&self.mint)
    }

    #[inline]
    const fn current_slot(&self) -> &ArmFrameSlot {
        &self.frame_slots[self.cur_slot]
    }

    #[inline]
    const fn current_slot_mut(&mut self) -> &mut ArmFrameSlot {
        &mut self.frame_slots[self.cur_slot]
    }

    #[cfg(all(test, wgpu_arm))]
    #[inline]
    fn has_inflight_frame(&self) -> bool {
        self.frame_slots.iter().any(|s| s.inflight.is_some())
    }

    fn library(&mut self, lib: ShaderLibrary) -> Result<&crate::metal::ffi::Library, String> {
        if !self.libs.iter().any(|(l, _)| *l == lib) {
            self.warm
                .await_item(crate::metal::warm::WarmItem::Library(lib));
            let compiled = crate::metal::pipelines::compile_library(self.mint.device(), lib)?;
            self.libs.push((lib, compiled));
        }
        Ok(self
            .libs
            .iter()
            .find(|(l, _)| *l == lib)
            .map(|(_, c)| c)
            .expect("inserted above"))
    }

    /// `row`'s state against a `present`-format destination: a `cell.metal` row draws into the
    /// offscreen whatever the destination (the bloom's glow extract), so it is the frame's own
    /// ([`Self::pso`]); any other is keyed by the attachment format it resolves to.
    fn present_pso(
        &mut self,
        row: Pipeline,
        present: crate::metal::ffi::PixelFormat,
    ) -> Result<crate::metal::ffi::Obj, String> {
        let spec = row.spec();
        if matches!(spec.library, ShaderLibrary::Cell) {
            return self.pso(row).map(crate::metal::ffi::Obj::clone_retained);
        }
        let key = (
            row,
            crate::metal::pipelines::metal_format(spec.target, present),
        );
        if !self.present_psos.iter().any(|(k, _)| *k == key) {
            self.adopt_warmed();
        }
        if let Some((_, pso)) = self.present_psos.iter().find(|(k, _)| *k == key) {
            return Ok(pso.clone_retained());
        }
        let _ = self.library(spec.library)?;
        let lib = self
            .libs
            .iter()
            .find(|(l, _)| *l == spec.library)
            .map(|(_, c)| c)
            .expect("compiled above");
        let item = crate::metal::warm::WarmItem::pipeline(row, present);
        self.warm.await_item(item);
        let pso = crate::metal::pipelines::build(self.mint.device(), lib, spec, present)?;
        #[cfg(test)]
        self.built_here.push(item);
        self.present_psos.push((key, pso.clone_retained()));
        Ok(pso)
    }

    fn await_frame_slot(&mut self) {
        self.cur_slot = (self.cur_slot + 1) % ARM_FRAME_SLOTS;
        for s in 0..ARM_FRAME_SLOTS {
            if s != self.cur_slot
                && self.frame_slots[s]
                    .inflight
                    .as_ref()
                    .is_some_and(|sub| sub.try_outcome().is_some())
            {
                let _ = self.frame_slots[s].inflight.take();
                self.awaited_frames += 1;
            }
        }
        if let Some(prev) = self.frame_slots[self.cur_slot].inflight.take() {
            // `try_outcome` feeds the loss latch like `wait_outcome`.
            if prev.try_outcome().is_none() {
                let parked = aterm_time::Instant::now();
                let _ = prev.wait_outcome();
                let ns = u64::try_from(parked.elapsed().as_nanos()).unwrap_or(u64::MAX);
                self.frame_slot_wait_ns = self.frame_slot_wait_ns.saturating_add(ns);
                self.fx_hazard.parked(ns);
            }
            self.awaited_frames += 1;
        }
    }

    fn await_atlas_readers(frame_slots: &mut [ArmFrameSlot]) -> (u64, u64) {
        let mut consumed = 0;
        let mut parked_ns = 0u64;
        for slot in frame_slots {
            if let Some(submitted) = slot.inflight.take() {
                if submitted.try_outcome().is_none() {
                    let parked = aterm_time::Instant::now();
                    let _ = submitted.wait_outcome();
                    parked_ns = parked_ns.saturating_add(
                        u64::try_from(parked.elapsed().as_nanos()).unwrap_or(u64::MAX),
                    );
                }
                consumed += 1;
            }
        }
        (consumed, parked_ns)
    }

    /// Wait for every frame in flight before an fx atlas region rewrite:
    /// how many it waited for and the nanoseconds it parked.
    fn settle_for_fx(
        frame_slots: &mut [ArmFrameSlot],
        awaited_frames: &mut u64,
        frame_slot_wait_ns: &mut u64,
        fx_hazard: &mut FxHazard,
    ) -> (u64, u64) {
        let (consumed, ns) = Self::await_atlas_readers(frame_slots);
        *awaited_frames += consumed;
        *frame_slot_wait_ns = frame_slot_wait_ns.saturating_add(ns);
        fx_hazard.settled(ns);
        (consumed, ns)
    }

    #[inline]
    const fn advance_submit_b_slot(&mut self) {
        self.submit_b_idx = (self.submit_b_idx + 1) % ARM_SUBMIT_B_SLOTS;
    }

    fn upload_scratch_stream(
        layer: crate::device_layer::DeviceHandle<'_>,
        slot: &mut Option<ArmStream>,
        bytes: &[u8],
    ) -> Result<crate::metal::ffi::Obj, String> {
        let need = bytes.len().max(16);
        if slot.as_ref().is_none_or(|s| s.cap < need) {
            let cap = need
                .next_power_of_two()
                .max(slot.as_ref().map_or(256, |s| s.cap));
            *slot = Some(ArmStream {
                buf: layer
                    .try_create_instance_buffer("aterm-gpu metal submit-b stream", cap as u64)?,
                cap,
            });
        }
        let stream = slot.as_ref().expect("sized above");
        if !bytes.is_empty() {
            // SAFETY: shared storage of at least `bytes.len()`; `drain_pending` guarantees this
            // ring slot is idle.
            unsafe { layer.write_buffer_from_zero(&stream.buf, bytes) };
        }
        Ok(stream.buf.metal().clone_retained())
    }

    fn ensure_submit_b_blit_u(
        &mut self,
        want: BlitUniform,
    ) -> Result<crate::metal::ffi::Obj, String> {
        let layer = crate::device_layer::DeviceHandle::Metal(&self.mint);
        let slot = &mut self.submit_b_slots[self.submit_b_idx].blit_u;
        if let Some((memo, buf)) = slot.as_mut() {
            if aterm_bits::bytes_of(memo) != aterm_bits::bytes_of(&want) {
                // SAFETY: `buf` is a shared-storage buffer of `size_of::<BlitUniform>()` owned by
                // this idle ring slot.
                unsafe { layer.write_buffer_from_zero(buf, aterm_bits::bytes_of(&want)) };
                *memo = want;
            }
            return Ok(buf.metal().clone_retained());
        }
        let buf = layer.try_create_uniform_buffer(
            "aterm-gpu metal blit uniform",
            std::mem::size_of::<BlitUniform>() as u64,
        )?;
        // SAFETY: freshly minted shared buffer of `size_of::<BlitUniform>()`.
        unsafe { layer.write_buffer_from_zero(&buf, aterm_bits::bytes_of(&want)) };
        let handle = buf.metal().clone_retained();
        *slot = Some((want, buf));
        Ok(handle)
    }

    #[inline]
    const fn take_present_seq(&mut self) -> u64 {
        let seq = self.present_seq;
        self.present_seq = seq.wrapping_add(1);
        seq
    }

    fn await_presents_through(&mut self, seq: u64) {
        let parked = aterm_time::Instant::now();
        let mut waited = false;
        for present in self
            .pending
            .iter()
            .filter(|present| crate::tray_card_ring::present_may_read(present.seq, seq))
        {
            if present.cb.try_outcome().is_none() {
                let _ = present.cb.wait_outcome();
                waited = true;
            }
        }
        if waited {
            self.pending_ring_wait_ns = self
                .pending_ring_wait_ns
                .saturating_add(u64::try_from(parked.elapsed().as_nanos()).unwrap_or(u64::MAX));
        }
    }

    fn drain_pending(&mut self) {
        self.pending
            .retain(|p| p.cb.try_outcome().is_none() || p.ticket.try_outcome().is_none());
        while self.pending.len() > 3 {
            let oldest = self.pending.remove(0);
            let parked = aterm_time::Instant::now();
            let _ = oldest.cb.wait_outcome();
            let _ = oldest.ticket.wait_outcome();
            self.pending_ring_wait_ns = self
                .pending_ring_wait_ns
                .saturating_add(u64::try_from(parked.elapsed().as_nanos()).unwrap_or(u64::MAX));
        }
    }

    #[inline]
    fn take_park_ns(&mut self) -> (u64, u64) {
        (
            std::mem::take(&mut self.frame_slot_wait_ns),
            std::mem::take(&mut self.pending_ring_wait_ns),
        )
    }

    #[inline]
    fn wait_inline(&self, sub: &crate::metal::encoder::Submitted) -> crate::metal::loss::CbOutcome {
        self.encode_inline_waits
            .set(self.encode_inline_waits.get().saturating_add(1));
        sub.wait_outcome()
    }

    fn pso(&mut self, row: Pipeline) -> Result<&crate::metal::ffi::Obj, String> {
        let spec = row.spec();
        if !matches!(spec.library, ShaderLibrary::Cell) {
            return Err(format!(
                "metal arm: frame row {row:?} claims library {:?} (want Cell)",
                spec.library
            ));
        }
        if self.psos[row as usize].is_none() {
            self.adopt_warmed();
        }
        let slot = &mut self.psos[row as usize];
        if slot.is_none() {
            let item = crate::metal::warm::WarmItem::pipeline(
                row,
                crate::metal::ffi::PixelFormat::Bgra8Unorm,
            );
            self.warm.await_item(item);
            *slot = Some(crate::metal::pipelines::build(
                self.mint.device(),
                &self.lib,
                spec,
                crate::metal::ffi::PixelFormat::Bgra8Unorm,
            )?);
            #[cfg(test)]
            self.built_here.push(item);
        }
        Ok(slot.as_ref().expect("filled above"))
    }

    /// The device's framebuffer-fetch verdict for a frame that draws Unorm-law light: a warm's
    /// once it landed; `None` (the split, for this frame) while the first-frame warm or this arm's
    /// warm still owes it, so no frame waits on the probe; else the process's known verdict or
    /// this arm's own probe ([`crate::metal::fetch::verdict`]).
    fn fetch_verdict(&mut self) -> Option<FetchProbe> {
        if self.fetch.is_none() {
            self.adopt_warmed();
        }
        if self.fetch.is_none()
            && !self.warm.probe_pending()
            && !self
                .warm_owed
                .contains(&crate::metal::warm::WarmItem::FetchProbe)
        {
            let known = crate::metal::fetch::known(self.mint.device(), &self.lib);
            #[cfg(test)]
            if known.is_none() {
                self.built_here
                    .push(crate::metal::warm::WarmItem::FetchProbe);
            }
            self.fetch = Some(known.unwrap_or_else(|| {
                crate::metal::fetch::verdict(&self.mint, &self.session, &self.lib)
            }));
        }
        self.fetch
    }

    /// The framebuffer-fetch twin's PSO, a pipeline warm's or built on first use.
    fn fetch_pso(
        &mut self,
        twin: pipeline_table::FetchTwin,
    ) -> Result<&crate::metal::ffi::Obj, String> {
        if self.fetch_psos[twin as usize].is_none() {
            self.adopt_warmed();
        }
        let slot = &mut self.fetch_psos[twin as usize];
        if slot.is_none() {
            let item = crate::metal::warm::WarmItem::Twin(twin);
            self.warm.await_item(item);
            *slot = Some(crate::metal::pipelines::build(
                self.mint.device(),
                &self.lib,
                &twin.spec(),
                crate::metal::ffi::PixelFormat::Bgra8Unorm,
            )?);
            #[cfg(test)]
            self.built_here.push(item);
        }
        Ok(slot.as_ref().expect("filled above"))
    }

    /// Whether this arm holds `item`'s pipeline state.
    fn holds(&self, item: crate::metal::warm::WarmItem) -> bool {
        use crate::metal::warm::WarmItem;
        match item {
            WarmItem::Pipeline(row, _) if matches!(row.spec().library, ShaderLibrary::Cell) => {
                self.psos[row as usize].is_some()
            }
            WarmItem::Pipeline(row, present) => {
                self.present_psos.iter().any(|(k, _)| *k == (row, present))
            }
            WarmItem::Twin(twin) => self.fetch_psos[twin as usize].is_some(),
            WarmItem::FetchProbe => self.fetch.is_some(),
            WarmItem::Library(_) => false,
        }
    }

    /// Take one warm's build: its state unless this arm built one meanwhile, or the verdict its
    /// probe settled for the process (none where the probe could not run, so a frame probes again).
    fn adopt(&mut self, warmed: crate::metal::warm::Warmed) {
        use crate::metal::warm::{WarmItem, Warmed};
        let Warmed { item, pso } = warmed;
        self.warm_owed.retain(|owed| *owed != item);
        if item == WarmItem::FetchProbe && self.fetch.is_none() {
            self.fetch = crate::metal::fetch::known(self.mint.device(), &self.lib);
        }
        let Some(pso) = pso else {
            return;
        };
        match item {
            WarmItem::Pipeline(row, _) if matches!(row.spec().library, ShaderLibrary::Cell) => {
                let _ = self.psos[row as usize].get_or_insert(pso);
            }
            WarmItem::Pipeline(row, present) => {
                if !self.holds(item) {
                    self.present_psos.push(((row, present), pso));
                }
            }
            WarmItem::Twin(twin) => {
                let _ = self.fetch_psos[twin as usize].get_or_insert(pso);
            }
            WarmItem::Library(_) | WarmItem::FetchProbe => {}
        }
    }

    /// Take what the pipeline warms built since the last look, without waiting.
    fn adopt_warmed(&mut self) {
        while let Ok(warmed) = self.warmed.1.try_recv() {
            self.adopt(warmed);
        }
    }

    /// Start a pipeline warm for what this arm's frames and presents can bind under `knobs`
    /// ([`crate::metal::warm::bindable`]) that it neither holds nor is owed: the twins unless the
    /// verdict is known and not a pass, led by the fetch probe where the verdict is unknown and no
    /// first-frame warm owes it, so no frame probes on this thread.
    fn warm_pipelines(&mut self, knobs: crate::metal::warm::PresentKnobs) {
        use crate::metal::warm::WarmItem;
        self.adopt_warmed();
        let verdict = self
            .fetch
            .or_else(|| crate::metal::fetch::known(self.mint.device(), &self.lib));
        let twins = verdict.is_none_or(|v| v == FetchProbe::Passed);
        let probe =
            (verdict.is_none() && !self.warm.probe_pending()).then_some(WarmItem::FetchProbe);
        let items: Vec<WarmItem> = probe
            .into_iter()
            .chain(crate::metal::warm::bindable(knobs, twins))
            .filter(|item| !self.holds(*item) && !self.warm_owed.contains(item))
            .collect();
        if items.is_empty() {
            return;
        }
        match crate::metal::warm::spawn(
            self.mint.device(),
            self.warm,
            items.clone(),
            self.warmed.0.clone(),
        ) {
            Ok(_detached) => self.warm_owed.extend(items),
            Err(e) => metal_arm_note(&format!(
                "the pipeline warm could not start ({e}); frames build their own"
            )),
        }
    }

    /// Wait (a hang detector of `hang`) until the warms built all they owe, put each build back
    /// for the frames to take, and return the items whose build failed (never the probe, which
    /// hands over no state).
    #[cfg(test)]
    fn await_warmed(&mut self, hang: std::time::Duration) -> Vec<crate::metal::warm::WarmItem> {
        let began = aterm_time::Instant::now();
        let mut built = Vec::new();
        while built.len() < self.warm_owed.len() {
            let left = hang
                .checked_sub(began.elapsed())
                .expect("the pipeline warm never built what it owed");
            built.push(
                self.warmed
                    .1
                    .recv_timeout(left)
                    .expect("the pipeline warm never built what it owed"),
            );
        }
        let failed = built
            .iter()
            .filter(|warmed| {
                warmed.pso.is_none() && warmed.item != crate::metal::warm::WarmItem::FetchProbe
            })
            .map(|warmed| warmed.item)
            .collect();
        for warmed in built {
            self.warmed
                .0
                .send(warmed)
                .expect("the arm holds its own receiver");
        }
        failed
    }

    #[inline]
    fn ensure_uniform(&mut self, w: u32, h: u32, text_blend: u32) {
        let layer = crate::device_layer::DeviceHandle::Metal(&self.mint);
        let slot = &mut self.frame_slots[self.cur_slot];
        if slot.uniform_key == Some((w, h, text_blend)) {
            return;
        }
        // SAFETY: `slot.uniform` is a shared-storage `Uniforms` buffer and `cur_slot` was awaited
        // at staging start.
        unsafe {
            layer.write_buffer_from_zero(
                &slot.uniform,
                aterm_bits::bytes_of(&Uniforms {
                    screen: [w as f32, h as f32],
                    text_blend: text_blend as f32,
                    _pad: 0.0,
                }),
            );
        }
        slot.uniform_key = Some((w, h, text_blend));
    }

    fn upload_stream(&mut self, id: StreamId, bytes: &[u8]) -> Result<bool, String> {
        if !should_slice(bytes.len()) {
            return Ok(false);
        }
        let layer = crate::device_layer::DeviceHandle::Metal(&self.mint);
        let slot = &mut self.frame_slots[self.cur_slot].streams[id as usize];
        let need = bytes.len();
        if slot.as_ref().is_none_or(|s| s.cap < need) {
            let cap = need
                .next_power_of_two()
                .max(slot.as_ref().map_or(0, |s| s.cap));
            *slot = Some(ArmStream {
                buf: layer
                    .try_create_instance_buffer("aterm-gpu metal frame stream", cap as u64)?,
                cap,
            });
        }
        let stream = slot.as_ref().expect("sized above");
        // SAFETY: shared storage of at least `bytes.len()` and `cur_slot` was awaited at staging
        // start.
        unsafe { layer.write_buffer_from_zero(&stream.buf, bytes) };
        Ok(true)
    }

    fn upload_fx_palette(&mut self, words: &[u32]) -> Result<crate::metal::ffi::Obj, String> {
        let layer = crate::device_layer::DeviceHandle::Metal(&self.mint);
        let slot = &mut self.frame_slots[self.cur_slot].fx_palette;
        let bytes: &[u8] = aterm_bits::cast_slice(words);
        let need = bytes.len().max(16);
        if slot.as_ref().is_none_or(|s| s.cap < need) {
            let cap = need
                .next_power_of_two()
                .max(slot.as_ref().map_or(256, |s| s.cap));
            *slot = Some(ArmStream {
                buf: layer.try_create_instance_buffer("aterm-gpu metal fx palette", cap as u64)?,
                cap,
            });
        }
        let stream = slot.as_ref().expect("sized above");
        if !bytes.is_empty() {
            // SAFETY: shared storage of at least `bytes.len()`, owned by
            // `cur_slot`, which staging awaited (`await_frame_slot`): no frame
            // in flight reads it. No other slot's palette is touched.
            unsafe { layer.write_buffer_from_zero(&stream.buf, bytes) };
        }
        Ok(stream.buf.metal().clone_retained())
    }

    fn sync_fx(&mut self, atlas: &fx_gpu::FxAtlas, drawing: bool) -> Result<u64, String> {
        let mut bytes = 0;
        self.fx_reader_wait = (0, 0);
        for kind in [fx_gpu::FxAtlasKind::R8, fx_gpu::FxAtlasKind::Rgba] {
            let side = atlas.side(kind);
            if !fx_gpu::fx_tex_due(&self.fx_tex[kind as usize], side, drawing) {
                continue;
            }
            let Self {
                mint,
                fx_tex,
                frame_slots,
                awaited_frames,
                frame_slot_wait_ns,
                fx_hazard,
                fx_reader_wait,
                ..
            } = self;
            let synced = fx_gpu::sync_fx_tex(
                crate::device_layer::DeviceHandle::Metal(mint),
                &mut fx_tex[kind as usize],
                side,
                |_| true,
                || {
                    let (frames, ns) = Self::settle_for_fx(
                        frame_slots,
                        awaited_frames,
                        frame_slot_wait_ns,
                        fx_hazard,
                    );
                    fx_reader_wait.0 += frames;
                    fx_reader_wait.1 = fx_reader_wait.1.saturating_add(ns);
                },
            )
            .map_err(|e| format!("fx {kind:?} atlas: {e}"))?;
            bytes += synced.bytes;
        }
        Ok(bytes)
    }

    #[cfg(any(test, debug_assertions))]
    fn write_shared_palette(&mut self, words: &[u32]) -> Result<(), String> {
        let rows = words.len().div_ceil(1024).max(1) as u32;
        let fits = self
            .fx_shared_palette
            .as_ref()
            .is_some_and(|(_, h)| *h >= rows);
        if !fits {
            let h = rows.next_power_of_two();
            let tex = self.layer().try_create_texture_2d(
                "aterm-gpu fx shared palette (hazard control)",
                crate::device_layer::TexelFormat::Rgba8Unorm,
                1024,
                h,
                crate::device_layer::TexUsage::UPLOADED_SAMPLED,
                None,
            )?;
            self.fx_shared_palette = Some((tex, h));
        } else {
            let _ = Self::settle_for_fx(
                &mut self.frame_slots,
                &mut self.awaited_frames,
                &mut self.frame_slot_wait_ns,
                &mut self.fx_hazard,
            );
        }
        let mut padded = words.to_vec();
        padded.resize(rows as usize * 1024, 0);
        let (tex, _) = self.fx_shared_palette.as_ref().expect("minted above");
        self.layer().upload_texture_region(
            tex,
            0,
            0,
            1024,
            rows,
            aterm_bits::cast_slice(&padded),
            1024 * 4,
        );
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn ensure_atlas(
        layer: crate::device_layer::DeviceHandle<'_>,
        slot: &mut Option<ArmAtlas>,
        atlas: DrawAtlas,
        key: (u32, u32, u64),
        write_gen: u64,
        cur_py: u32,
        format: crate::device_layer::TexelFormat,
        bytes: &[u8],
        frame_slots: &mut [ArmFrameSlot],
    ) -> Result<(u64, u64), String> {
        let (tw, th, _) = key;
        let bpp = format.bytes_per_texel();
        let row_bytes = (tw * bpp) as usize;
        let full = (tw * th * bpp) as usize;
        if bytes.len() > full || (row_bytes > 0 && !bytes.len().is_multiple_of(row_bytes)) {
            return Err(format!(
                "metal arm: {atlas:?} atlas bytes ({}) invalid for {tw}x{th}x{bpp}",
                bytes.len()
            ));
        }
        let occupied_rows = bytes.len().checked_div(row_bytes).unwrap_or(0) as u32;
        if let Some(a) = slot.as_mut()
            && a.key == key
        {
            if a.write_gen == write_gen {
                return Ok((0, 0));
            }
            let y0 = a.uploaded_py.min(occupied_rows);
            let rows = occupied_rows.saturating_sub(y0);
            let mut waited = (0, 0);
            if rows > 0 {
                waited = Self::await_atlas_readers(frame_slots);
                debug_assert!(frame_slots.iter().all(|slot| slot.inflight.is_none()));
                let start = y0 as usize * row_bytes;
                let end = occupied_rows as usize * row_bytes;
                layer.upload_texture_rows(
                    &a.tex,
                    y0,
                    rows,
                    &bytes[start..end],
                    row_bytes as u32,
                    tw,
                );
            }
            a.write_gen = write_gen;
            a.uploaded_py = cur_py;
            return Ok(waited);
        }
        let tex = layer
            .try_create_texture_2d(
                "aterm-gpu metal atlas",
                format,
                tw,
                th,
                crate::device_layer::TexUsage::UPLOADED_SAMPLED,
                None,
            )
            .map_err(|e| format!("metal arm: {atlas:?} atlas: {e}"))?;
        layer.upload_texture_rows(&tex, 0, occupied_rows, bytes, row_bytes as u32, tw);
        *slot = Some(ArmAtlas {
            tex,
            key,
            write_gen,
            uploaded_py: cur_py,
        });
        Ok((0, 0))
    }
}

#[cfg(clean_islands)]
macro_rules! trust_island {
    ($($item:item)*) => {
        $($item)*
    };
}

#[cfg(not(clean_islands))]
macro_rules! trust_island {
    ($($tt:tt)*) => {};
}

trust_island! {
    clean {
        namespace MetalAtlasAndFrameRing

        -- Formal state machine for the Metal glyph-atlas incremental row-band
        -- upload and the 3-slot Submit-A frame ring.
        structure World where
            cpuEpoch : Nat
            cpuWriteGen : Nat
            cpuPy : Nat
            occupiedRows : Nat
            gpuEpoch : Nat
            gpuWriteGen : Nat
            uploadedPy : Nat
            gpuRows : Nat
            remintCount : Nat
            curSlot : Nat
            slot0Busy : Bool
            slot1Busy : Bool
            slot2Busy : Bool
            stalls : Nat

        inductive Event where
            | AppendShelf (newPy : Nat) (newOccupied : Nat)
            | RepackAtlas (newOccupied : Nat)
            | BeginFrame (prevCompleted : Bool)

        def initWorld : World :=
            { cpuEpoch := 1
            , cpuWriteGen := 0
            , cpuPy := 0
            , occupiedRows := 16
            , gpuEpoch := 1
            , gpuWriteGen := 0
            , uploadedPy := 0
            , gpuRows := 16
            , remintCount := 1
            , curSlot := 0
            , slot0Busy := true
            , slot1Busy := false
            , slot2Busy := false
            , stalls := 0 }

        def Synced (w : World) : Bool :=
            Nat.beq w.cpuEpoch w.gpuEpoch
                && Nat.beq w.cpuWriteGen w.gpuWriteGen
                && Nat.beq w.cpuPy w.uploadedPy
                && Nat.beq w.occupiedRows w.gpuRows

        -- Fixed ensure_atlas + 3-slot ring transition.
        def step (w : World) (ev : Event) : World :=
            match ev with
            | Event.AppendShelf newPy newOccupied =>
                let nextGen := Nat.succ w.cpuWriteGen
                if Nat.beq w.cpuEpoch w.gpuEpoch then
                    { w with
                        cpuWriteGen := nextGen
                        cpuPy := newPy
                        occupiedRows := newOccupied
                        gpuWriteGen := nextGen
                        uploadedPy := newPy
                        gpuRows := newOccupied }
                else
                    { w with
                        cpuWriteGen := nextGen
                        cpuPy := newPy
                        occupiedRows := newOccupied
                        gpuEpoch := w.cpuEpoch
                        gpuWriteGen := nextGen
                        uploadedPy := newPy
                        gpuRows := newOccupied
                        remintCount := Nat.succ w.remintCount }
            | Event.RepackAtlas newOccupied =>
                let nextEpoch := Nat.succ w.cpuEpoch
                { w with
                    cpuEpoch := nextEpoch
                    cpuWriteGen := 0
                    cpuPy := 0
                    occupiedRows := newOccupied
                    gpuEpoch := nextEpoch
                    gpuWriteGen := 0
                    uploadedPy := 0
                    gpuRows := newOccupied
                    remintCount := Nat.succ w.remintCount }
            | Event.BeginFrame prevCompleted =>
                let nextSlot := (Nat.succ w.curSlot) % 3
                let s0 := if prevCompleted then false else w.slot0Busy
                let s1 := if prevCompleted then false else w.slot1Busy
                let s2 := if prevCompleted then false else w.slot2Busy
                let targetBusy :=
                    if Nat.beq nextSlot 0 then s0
                    else if Nat.beq nextSlot 1 then s1
                    else s2
                let nextStalls := if targetBusy then Nat.succ w.stalls else w.stalls
                { w with
                    curSlot := nextSlot
                    slot0Busy := if Nat.beq nextSlot 0 then true else s0
                    slot1Busy := if Nat.beq nextSlot 1 then true else s1
                    slot2Busy := if Nat.beq nextSlot 2 then true else s2
                    stalls := nextStalls }

        -- Buggy pre-fix variant 1: folding map.len() into key forces a texture
        -- re-mint on every glyph append.
        def stepBuggyRemintOnAppend (w : World) (newPy newOccupied : Nat) : World :=
            let nextGen := Nat.succ w.cpuWriteGen
            { w with
                cpuWriteGen := nextGen
                cpuPy := newPy
                occupiedRows := newOccupied
                gpuWriteGen := nextGen
                uploadedPy := newPy
                gpuRows := newOccupied
                remintCount := Nat.succ w.remintCount }

        -- Buggy pre-fix variant 2: a single-slot frame buffer stalls whenever
        -- the immediately preceding frame is still executing on the GPU.
        def stepBuggySingleSlot (w : World) (prevCompleted : Bool) : World :=
            let nextStalls := if prevCompleted then w.stalls else Nat.succ w.stalls
            { w with slot0Busy := true, stalls := nextStalls }

        theorem initWorld_synced : Synced initWorld = true := rfl

        theorem append_shelf_preserves_sync :
            let w1 := step initWorld (Event.AppendShelf 16 32)
            let w2 := step w1 (Event.AppendShelf 32 48)
            Synced w2 = true := rfl

        theorem append_shelf_avoids_remint :
            let w1 := step initWorld (Event.AppendShelf 16 32)
            let w2 := step w1 (Event.AppendShelf 32 48)
            w2.remintCount = 1 := rfl

        theorem repack_atlas_preserves_sync :
            let w1 := step initWorld (Event.RepackAtlas 24)
            Synced w1 = true := rfl

        theorem repack_atlas_bumps_remint :
            let w1 := step initWorld (Event.RepackAtlas 24)
            w1.remintCount = 2 := rfl

        theorem counterexample_buggy_append_remints_every_time :
            let w1 := stepBuggyRemintOnAppend initWorld 16 32
            let w2 := stepBuggyRemintOnAppend w1 32 48
            w2.remintCount = 3 := rfl

        theorem three_slot_ring_avoids_stall_when_prev_frame_busy :
            let w1 := step initWorld (Event.BeginFrame false)
            let w2 := step w1 (Event.BeginFrame false)
            w2.stalls = 0 := rfl

        theorem counterexample_single_slot_stalls_when_prev_frame_busy :
            let w1 := stepBuggySingleSlot initWorld false
            let w2 := stepBuggySingleSlot w1 false
            w2.stalls = 2 := rfl

        end MetalAtlasAndFrameRing
    }
}

#[cfg(target_os = "macos")]
struct MetalArmCell {
    live: MetalArmLive,
    pinned: std::thread::ThreadId,
}

#[cfg(target_os = "macos")]
impl MetalArmCell {
    #[inline]
    fn new(live: MetalArmLive) -> Self {
        Self {
            live,
            pinned: std::thread::current().id(),
        }
    }

    #[inline]
    fn live_mut(&mut self) -> &mut MetalArmLive {
        assert!(
            std::thread::current().id() == self.pinned,
            "metal arm: thread-pinned state touched off the render thread that \
             minted it (the armed session/queue is single-thread by the metal \
             module convention)"
        );
        &mut self.live
    }

    #[inline]
    fn live_ref(&self) -> &MetalArmLive {
        assert!(
            std::thread::current().id() == self.pinned,
            "metal arm: thread-pinned state touched off the render thread that \
             minted it (the armed session/queue is single-thread by the metal \
             module convention)"
        );
        &self.live
    }
}

// SAFETY: moving the holder is sound because `live_mut` checks the minting thread and
// `objc_release` is thread-safe.
#[cfg(target_os = "macos")]
unsafe impl Send for MetalArmCell {}

/// `plan`'s passes ([`coalesce_frame_passes`]): the base group always and each group a draw names,
/// each on the view its draws take (a fetch draw takes the sRGB view in a Unorm-law group).
fn plan_passes(plan: &[DrawItem]) -> ([usize; FRAME_GROUPS], [bool; FRAME_GROUPS], usize) {
    let mut enabled = [false; FRAME_GROUPS];
    let mut srgb = GROUP_SRGB;
    enabled[G_BASE_BG] = true;
    for item in plan {
        enabled[item.group as usize] = true;
        srgb[item.group as usize] = item.srgb();
    }
    coalesce_frame_passes(&enabled, &srgb)
}

pub(crate) fn run_frame_plan(
    enc: &mut crate::device_layer::FrameEncoder<'_>,
    res: &FrameRes<'_>,
    plan: &[DrawItem],
    load0: crate::device_layer::FrameLoad,
    scissor: Option<(u32, u32, u32, u32)>,
) -> Result<usize, String> {
    let (pass_of, pass_srgb, passes) = plan_passes(plan);
    for (p, &srgb) in pass_srgb.iter().enumerate().take(passes) {
        let load = if p == 0 {
            load0
        } else {
            crate::device_layer::FrameLoad::Load
        };
        let mut pass = enc.begin_pass(
            "aterm-gpu frame pass",
            res.view(srgb),
            load,
            scissor,
            res.uniforms(),
        )?;
        let mut cur_pipe: Option<(DrawPipe, bool)> = None;
        let mut cur_atlas: Option<DrawAtlas> = None;
        for item in plan.iter().filter(|it| pass_of[it.group as usize] == p) {
            if cur_pipe != Some((item.pipe, item.fetch)) {
                pass.set_pipeline(res.pipeline(item.pipe, item.fetch));
                cur_pipe = Some((item.pipe, item.fetch));
            }
            if let Some(atlas) = item.atlas
                && cur_atlas != Some(atlas)
            {
                pass.bind_atlas(res.atlas(atlas, item.pipe));
                cur_atlas = Some(atlas);
            }
            pass.set_stream(res.stream(item.stream));
            pass.draw_quads(item.count)?;
        }
    }
    Ok(passes)
}

/// Which destination a `fs_blit` test case blits into.
#[cfg(wgpu_arm)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BlitTestTarget {
    Rgba8Unorm,
    #[cfg(all(test, target_os = "macos"))]
    Bgra8Unorm,
    #[cfg(all(test, target_os = "macos"))]
    Rgba16Float,
}

#[cfg(wgpu_arm)]
impl BlitTestTarget {
    const fn wgpu(self) -> wgpu::TextureFormat {
        match self {
            Self::Rgba8Unorm => wgpu::TextureFormat::Rgba8Unorm,
            #[cfg(all(test, target_os = "macos"))]
            Self::Bgra8Unorm => wgpu::TextureFormat::Bgra8Unorm,
            #[cfg(all(test, target_os = "macos"))]
            Self::Rgba16Float => wgpu::TextureFormat::Rgba16Float,
        }
    }

    #[cfg(all(target_os = "macos", test))]
    fn bytes_per_texel(self) -> usize {
        match self {
            Self::Rgba8Unorm | Self::Bgra8Unorm => 4,
            Self::Rgba16Float => 8,
        }
    }
}

/// Which of `fs_blit`'s uniform-driven arms a test blit turns on.
#[cfg(wgpu_arm)]
#[derive(Clone, Copy)]
pub(crate) struct BlitTestEffects {
    pub(crate) invert: bool,
    pub(crate) overlay: Option<DropOverlay>,
    pub(crate) crop: Option<PresentCrop>,
    pub(crate) encode_srgb: bool,
    pub(crate) hdr: bool,
    pub(crate) sdr_white_scale: f32,
    pub(crate) translucent: Option<f32>,
    pub(crate) premult: bool,
}

#[cfg(wgpu_arm)]
impl BlitTestEffects {
    /// Every effect off: the plain present.
    pub(crate) const PLAIN: Self = Self {
        invert: false,
        overlay: None,
        crop: None,
        encode_srgb: false,
        hdr: false,
        sdr_white_scale: 1.0,
        translucent: None,
        premult: false,
    };
}

#[inline]
const fn frame_live_bg(input: &RenderInput, theme_bg: u32) -> u32 {
    if input.default_bg == aterm_core::render::COLOR_UNSET {
        theme_bg
    } else {
        input.default_bg
    }
}

#[inline]
fn page_bloom_strength(strength: f32, input: &RenderInput, theme_bg: u32) -> f32 {
    let bg = frame_live_bg(input, theme_bg);
    if aterm_render::theme_is_dark(bg) {
        return strength;
    }
    strength * aterm_render::hdr::light_page_bloom_share(bg)
}

struct PresentUniforms {
    live_bg: u32,
    want: BlitUniform,
    run_hdr_glow: bool,
    run_sdr_glow: bool,
    crown_scissor: Option<(u32, u32, u32, u32)>,
    crown: Option<HdrGlowUniform>,
}

#[inline]
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

#[inline]
fn crown_band_scissor(
    uniform: &BlitUniform,
    band_shift: Option<BandShift>,
    destination_width: u32,
    destination_height: u32,
) -> Option<(u32, u32, u32, u32)> {
    let Some(shift) = band_shift else {
        return visible_source_scissor(uniform, destination_width, destination_height);
    };
    let mut shifted = *uniform;
    let vis_y0 = uniform.visible_y.max(shift.dst_y as f32);
    let vis_y1 =
        (uniform.visible_y + uniform.visible_h).min(shift.dst_y.saturating_add(shift.moved) as f32);
    shifted.visible_y = vis_y0;
    shifted.visible_h = (vis_y1 - vis_y0).max(0.0);
    visible_source_scissor(&shifted, destination_width, destination_height)
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
                border_px: scaled_border_px(
                    drop_border_px_gpu(source_width, visible_height),
                    overlay.border_scale_q4,
                )
                .min(source_width / 2)
                .min(visible_height / 2)
                .max(1) as f32,
                accent: [chan(16), chan(8), chan(0)],
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

#[cfg(not(wgpu_arm))]
#[derive(Clone, Copy)]
struct NeutralSurfaceConfig {
    width: u32,
    height: u32,
    format: crate::device_layer::TexelFormat,
}

#[cfg(all(not(wgpu_arm), target_os = "macos"))]
fn metal_attach_format<W: WindowTarget>(
    hdr_glow: bool,
    target: &W,
) -> crate::device_layer::TexelFormat {
    let potential = crate::metal::present::screen_edr_potential_of(target);
    if crate::format_plan::hdr_swapchain_wants_f16_on_screen(hdr_glow, true, potential) {
        crate::device_layer::TexelFormat::Rgba16Float
    } else {
        crate::device_layer::TexelFormat::Bgra8Unorm
    }
}

#[cfg(wgpu_arm)]
mod live_surface {
    use super::{GpuRenderer, configure_scoped};
    use crate::format_plan::HdrReconfigurePlan;

    pub(super) struct LiveSurface(wgpu::Surface<'static>);

    impl LiveSurface {
        /// Wrap a surface its attach has configured.
        #[inline]
        pub(super) const fn attached(surface: wgpu::Surface<'static>) -> Self {
            Self(surface)
        }

        /// Reconfigure, then re-tag scRGB when the surface was HDR.
        #[inline]
        pub(super) fn configure_retagging(
            &self,
            device: &wgpu::Device,
            config: &wgpu::SurfaceConfiguration,
            was_hdr: bool,
        ) -> Result<bool, String> {
            configure_scoped(device, &self.0, config)?;
            Ok(was_hdr && GpuRenderer::tag_swapchain_scrgb(&self.0))
        }

        /// The SDR escape.
        #[inline]
        pub(super) fn configure_sdr_escape(
            &self,
            device: &wgpu::Device,
            config: &wgpu::SurfaceConfiguration,
            plan: HdrReconfigurePlan,
        ) -> Result<(), String> {
            assert_eq!(
                plan,
                HdrReconfigurePlan::FallbackToSdr,
                "the untagged reconfigure is the SDR escape and nothing else"
            );
            configure_scoped(device, &self.0, config)
        }

        /// Re-tag the current swapchain scRGB without reconfiguring it.
        #[inline]
        pub(super) fn retag(&self) -> bool {
            GpuRenderer::tag_swapchain_scrgb(&self.0)
        }

        /// Whether the surface's output advertises the HDR10 desktop colour space.
        #[inline]
        pub(super) fn output_hdr_enabled(&self) -> bool {
            GpuRenderer::surface_output_hdr_enabled(&self.0)
        }

        #[inline]
        pub(super) fn get_current_texture(&self) -> wgpu::CurrentSurfaceTexture {
            self.0.get_current_texture()
        }
    }
}

#[cfg(wgpu_arm)]
fn configure_scoped(
    device: &wgpu::Device,
    surface: &wgpu::Surface<'_>,
    config: &wgpu::SurfaceConfiguration,
) -> Result<(), String> {
    wgpu_scoped(device, || surface.configure(device, config))
}

#[cfg(wgpu_arm)]
fn wgpu_scoped<T>(device: &wgpu::Device, f: impl FnOnce() -> T) -> Result<T, String> {
    let oom = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
    let internal = device.push_error_scope(wgpu::ErrorFilter::Internal);
    let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let out = f();
    let validation = crate::block_on::block_on(validation.pop());
    let internal = crate::block_on::block_on(internal.pop());
    let oom = crate::block_on::block_on(oom.pop());
    match validation.or(internal).or(oom) {
        None => Ok(out),
        Some(err) => Err(err
            .to_string()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")),
    }
}

/// An on-screen presentation target.
pub struct GpuSurface {
    acquire_id: u64,
    #[cfg(not(wgpu_arm))]
    neutral: NeutralSurfaceConfig,
    #[cfg(wgpu_arm)]
    surface: live_surface::LiveSurface,
    #[cfg(wgpu_arm)]
    config: wgpu::SurfaceConfiguration,
    #[cfg(wgpu_arm)]
    sdr_format: wgpu::TextureFormat,
    #[cfg_attr(
        target_arch = "wasm32",
        expect(dead_code, reason = "read by the window HDR paths; a canvas has none")
    )]
    supports_f16: bool,
    #[cfg(wgpu_arm)]
    last_hdr_probe: Option<aterm_time::Instant>,
    post_mult: bool,
    #[cfg(wgpu_arm)]
    pre_mult: bool,
    copyable: bool,
    #[cfg(target_os = "macos")]
    metal: Option<crate::metal::present::MetalWindowSurface>,
    #[cfg(all(gl_arm, not(wgpu_arm)))]
    gl: Option<crate::gl::WindowSurface>,
}

fn next_surface_acquire_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let mut id = NEXT.load(Ordering::Relaxed);
    loop {
        let next = id.checked_add(1).expect("surface identity exhausted");
        match NEXT.compare_exchange_weak(id, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return id,
            Err(current) => id = current,
        }
    }
}

impl GpuSurface {
    /// Stable identity used to reject late wakes after surface replacement.
    #[must_use]
    #[inline]
    pub const fn acquire_id(&self) -> u64 {
        self.acquire_id
    }

    /// Install the main-loop wake before the first attached native present.
    #[inline]
    pub fn set_acquire_ready_callback(
        &mut self,
        notify: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<(), String> {
        #[cfg(target_os = "macos")]
        if let Some(metal) = self.metal.as_mut() {
            return metal.set_acquire_ready_callback(notify);
        }
        let _ = notify;
        Ok(())
    }

    /// Reject a pending drawable after resize, occlusion, or surface retirement.
    #[inline]
    pub fn discard_pending_acquire(&mut self) {
        #[cfg(target_os = "macos")]
        if let Some(metal) = self.metal.as_mut() {
            metal.discard_pending_acquire();
        }
    }

    /// How this swapchain's presents queue.
    #[must_use]
    #[inline]
    pub fn fifo_queue_depth(&self) -> Option<u32> {
        #[cfg(target_os = "macos")]
        if self.metal.is_some() {
            return None;
        }
        #[cfg(all(gl_arm, not(wgpu_arm)))]
        if let Some(gl) = self.gl.as_ref() {
            return gl.synced().then_some(1);
        }
        #[cfg(wgpu_arm)]
        {
            matches!(
                self.config.present_mode,
                wgpu::PresentMode::Fifo | wgpu::PresentMode::FifoRelaxed
            )
            .then_some(self.config.desired_maximum_frame_latency.max(1))
        }
        #[cfg(not(wgpu_arm))]
        {
            None
        }
    }

    /// Whether this swapchain is the HDR (`Rgba16Float` extended-linear) target.
    #[must_use]
    #[inline]
    pub fn is_hdr(&self) -> bool {
        #[cfg(wgpu_arm)]
        {
            self.config.format == wgpu::TextureFormat::Rgba16Float
        }
        #[cfg(not(wgpu_arm))]
        {
            self.neutral.format == crate::device_layer::TexelFormat::Rgba16Float
        }
    }

    /// Whether a presented snapshot of this surface can be armed.
    #[must_use]
    #[inline]
    pub const fn snapshot_is_copyable(&self) -> bool {
        self.copyable
    }

    /// The bytes a presented snapshot holds ([`crate::video_tap::RawCapture::allocation`]).
    #[must_use]
    #[inline]
    pub fn snapshot_bytes(&self) -> Option<usize> {
        if !self.copyable {
            return None;
        }
        #[cfg(wgpu_arm)]
        let (width, height, format) = (
            self.config.width,
            self.config.height,
            crate::device_layer::TexelFormat::from_wgpu(self.config.format)?,
        );
        #[cfg(not(wgpu_arm))]
        let (width, height, format) =
            (self.neutral.width, self.neutral.height, self.neutral.format);
        let len = crate::video_tap::presented_snapshot_len(width, height, format).ok()?;
        usize::try_from(len).ok()
    }

    #[cfg(target_os = "macos")]
    #[inline]
    const fn neutral_config(&self) -> (u32, u32, crate::device_layer::TexelFormat) {
        #[cfg(wgpu_arm)]
        {
            let f = crate::device_layer::TexelFormat::from_wgpu(self.config.format)
                .expect("the swapchain format is inside the map's closed set of six");
            (self.config.width, self.config.height, f)
        }
        #[cfg(not(wgpu_arm))]
        {
            (self.neutral.width, self.neutral.height, self.neutral.format)
        }
    }
}

#[cfg(wgpu_arm)]
const HDR_COLOR_SPACE_PROBE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

#[must_use]
#[cfg(wgpu_arm)]
#[inline]
fn hdr_color_space_probe_due(last: Option<aterm_time::Instant>, now: aterm_time::Instant) -> bool {
    match last {
        None => true,
        Some(last) => now.saturating_duration_since(last) >= HDR_COLOR_SPACE_PROBE_INTERVAL,
    }
}

#[derive(Clone, Copy)]
struct GlyphSlot {
    ax: u32,
    ay: u32,
    gw: u32,
    gh: u32,
    xmin: i32,
    ymin: i32,
    ink_rise: Option<i32>,
}

impl GlyphSlot {
    #[inline]
    fn quad(
        &self,
        x: f32,
        anchor_y: i32,
        baseline: i32,
        scale: aterm_render::Scale,
        (aw, ah): (f32, f32),
    ) -> Option<([f32; 4], [f32; 4])> {
        aterm_render::glyph_quad(
            x, anchor_y, baseline, scale, self.ax, self.ay, self.gw, self.gh, self.xmin, self.ymin,
            aw, ah,
        )
    }

    #[inline]
    fn ink_reach(&self, anchor_y: i32, baseline: i32, scale: aterm_render::Scale) -> i32 {
        self.ink_rise.map_or(i32::MAX, |rise| {
            (anchor_y + scale.ys.max(1) as i32 * (baseline - rise)).max(scale.clip_y0)
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AtlasKind {
    Mono,
    Color,
}

impl AtlasKind {
    #[inline]
    const fn bpp(self) -> u32 {
        match self {
            AtlasKind::Mono => 1,
            AtlasKind::Color => 4,
        }
    }
}

struct Atlas {
    kind: AtlasKind,
    width: u32,
    height: u32,
    data: Vec<u8>,
    map: FxHashMap<GlyphKey, GlyphSlot>,
    px: u32,
    py: u32,
    shelf_h: u32,
    write_gen: u64,
}

impl Atlas {
    #[inline]
    fn packs(kind: AtlasKind, img: &GlyphImage) -> bool {
        let real = img.width() > 0 && img.height() > 0;
        match kind {
            AtlasKind::Mono => real && matches!(img, GlyphImage::Mono { .. }),
            AtlasKind::Color => real && matches!(img, GlyphImage::Rgba { .. }),
        }
    }

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
                        ink_rise: None,
                    },
                );
            }
            return None;
        }
        let (gw, gh) = (img.width() as u32, img.height() as u32);
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
                        ink_rise: None,
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
                ink_rise: aterm_render::glyph_ink_box(img)
                    .map(|(ink_h, ink_ymin)| ink_ymin + ink_h as i32),
            },
        );
        self.px += gw + pad;
        self.shelf_h = self.shelf_h.max(gh);
        Some((ay, gh))
    }

    fn blit(&mut self, img: &GlyphImage, slot: &GlyphSlot) {
        if slot.gw == 0 || slot.gh == 0 {
            return;
        }
        let bpp = self.kind.bpp();
        let bytes = img.bytes();
        let row_bytes = (slot.gw * bpp) as usize;
        for j in 0..slot.gh {
            let src = ((j * slot.gw) * bpp) as usize;
            let dst = (((slot.ay + j) * self.width + slot.ax) * bpp) as usize;
            self.data[dst..dst + row_bytes].copy_from_slice(&bytes[src..src + row_bytes]);
        }
    }

    #[inline]
    fn occupied_height(&self) -> u32 {
        (self.py + self.shelf_h + 1).max(1)
    }
}

fn build_atlas(cpu: &mut Renderer, keys: &[GlyphKey], cap_h: u32) -> Atlas {
    build_kind(cpu, keys, AtlasKind::Mono, cap_h)
}

fn build_color_atlas(cpu: &mut Renderer, keys: &[GlyphKey], cap_h: u32) -> Atlas {
    build_kind(cpu, keys, AtlasKind::Color, cap_h)
}

fn build_kind(cpu: &mut Renderer, keys: &[GlyphKey], kind: AtlasKind, cap_h: u32) -> Atlas {
    let mut atlas = Atlas {
        kind,
        width: ATLAS_WIDTH,
        height: 1,
        data: Vec::new(),
        map: FxHashMap::default(),
        px: 0,
        py: 0,
        shelf_h: 0,
        write_gen: 0,
    };
    let mut placed: Vec<GlyphKey> = Vec::new();
    for &key in keys {
        let img = cpu.glyph_image(key);
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

fn grow_atlas(
    cpu: &mut Renderer,
    atlas: &mut Atlas,
    new_keys: &[GlyphKey],
    cap_h: u32,
) -> Option<(u32, u32)> {
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
        write_gen: atlas.write_gen,
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
        for &key in new_keys {
            let img = cpu.glyph_image(key);
            atlas.place(key, img);
        }
        return Some((0, 0));
    }

    let new_total = (atlas.width * need_h * atlas.kind.bpp()) as usize;
    if atlas.data.len() < new_total {
        atlas.data.resize(new_total, 0);
    }
    atlas.height = need_h.max(atlas.height);
    for &key in new_keys {
        let img = cpu.glyph_image(key);
        if atlas.place(key, img).is_some() {
            let slot = atlas.map[&key];
            atlas.blit(img, &slot);
        }
    }
    atlas.write_gen = atlas.write_gen.wrapping_add(1);
    Some((dirty_lo, dirty_hi))
}

const DECO_GLYPHS: [aterm_render::DecoGlyph; 7] = [
    aterm_render::DecoGlyph::Star4,
    aterm_render::DecoGlyph::Star5,
    aterm_render::DecoGlyph::Dot,
    aterm_render::DecoGlyph::Plus,
    aterm_render::DecoGlyph::Paw,
    aterm_render::DecoGlyph::RingArc,
    aterm_render::DecoGlyph::Shade,
];

const _: () = assert!(DECO_GLYPHS.len() == aterm_render::UNDERCURL_SPRITE);
const _: () = assert!(DECO_GLYPHS.len() + 1 == aterm_render::DECO_ATLAS_SPRITES);

#[inline]
const fn deco_sprite_index(g: aterm_render::DecoGlyph) -> usize {
    match g {
        aterm_render::DecoGlyph::Star4 => 0,
        aterm_render::DecoGlyph::Star5 => 1,
        aterm_render::DecoGlyph::Dot => 2,
        aterm_render::DecoGlyph::Plus => 3,
        aterm_render::DecoGlyph::Paw => 4,
        aterm_render::DecoGlyph::RingArc => 5,
        aterm_render::DecoGlyph::Shade => 6,
    }
}

struct DecoAtlas {
    #[cfg(wgpu_arm)]
    bind: wgpu::BindGroup,
    #[cfg(any(target_os = "macos", gl_arm))]
    data: Vec<u8>,
    cw: usize,
    ch: usize,
    curl_band: (usize, usize),
    atlas_w: usize,
}

struct SpriteTex {
    #[cfg(target_os = "macos")]
    metal_atlas: std::sync::OnceLock<ArmAtlas>,
    #[cfg(gl_arm)]
    gl_atlas: std::sync::Mutex<Option<Arc<crate::gl::resources::GlTexture>>>,
    #[cfg(wgpu_arm)]
    bind: wgpu::BindGroup,
    w: u32,
    h: u32,
    src: Arc<SceneAtlas>,
}

impl SpriteTex {
    #[inline]
    fn matches(&self, source: &Arc<SceneAtlas>) -> bool {
        self.w == source.width
            && self.h == source.height
            && self.src.version == source.version
            && Arc::ptr_eq(&self.src, source)
    }
}

const SPRITE_TEXTURE_LOOKUP_LIMIT: usize = 64;

#[derive(Default)]
struct SpriteTextureLookup {
    recent: Vec<Weak<SpriteTex>>,
}

impl SpriteTextureLookup {
    fn find(&mut self, source: &Arc<SceneAtlas>) -> Option<Arc<SpriteTex>> {
        self.recent.retain(|entry| entry.strong_count() != 0);
        let mut found = None;
        let index = self.recent.iter().position(|entry| {
            if let Some(texture) = entry.upgrade()
                && texture.matches(source)
            {
                found = Some(texture);
                true
            } else {
                false
            }
        })?;
        let entry = self.recent.remove(index);
        self.recent.push(entry);
        found
    }

    fn insert(&mut self, texture: &Arc<SpriteTex>) {
        if self.recent.len() == SPRITE_TEXTURE_LOOKUP_LIMIT {
            self.recent.remove(0);
        }
        self.recent.push(Arc::downgrade(texture));
    }
}

#[cfg(target_os = "macos")]
impl SpriteTex {
    fn metal_atlas(
        &self,
        layer: crate::device_layer::DeviceHandle<'_>,
        atlas: DrawAtlas,
    ) -> Result<&ArmAtlas, String> {
        if let Some(resident) = self.metal_atlas.get() {
            return Ok(resident);
        }
        let mut slot = None;
        MetalArmLive::ensure_atlas(
            layer,
            &mut slot,
            atlas,
            (self.w, self.h, Arc::as_ptr(&self.src) as u64),
            0,
            0,
            crate::device_layer::TexelFormat::Rgba8Unorm,
            &self.src.rgba[..(self.w as usize * self.h as usize * 4)],
            &mut [],
        )?;
        let _ = self
            .metal_atlas
            .set(slot.expect("ensure_atlas filled the slot"));
        Ok(self.metal_atlas.get().expect("shared sprite initialized"))
    }
}

struct ResidentAtlas {
    atlas: Atlas,
    #[cfg(wgpu_arm)]
    tex: crate::device_layer::LayerTexture,
    #[cfg(wgpu_arm)]
    bind: wgpu::BindGroup,
    tex_h: u32,
}

/// Per-window GPU state.
#[derive(Default)]
pub struct WindowGpu {
    wallpaper_tex: Option<Arc<SpriteTex>>,
    last_present_work_ns: u64,
    last_present_encode_ns: u64,
    last_acquire_wait_ns: u64,
    acquire_wait_sample_ns: Option<u64>,
    last_acquire_queue_ns: u64,
    acquire_queue_sample_ns: Option<u64>,
    last_gpu_park_ns: u64,
    gpu_park_sample_ns: Option<u64>,
    #[cfg(wgpu_arm)]
    pub(crate) offscreen: Option<Offscreen>,
    pub(crate) gate_cache: Option<GpuGateCache>,
    pub(crate) image_cache: aterm_render::ImageCache,
    pub(crate) video: Option<crate::video_tap::VideoTap>,
    pub(crate) presented_snapshot: Option<crate::video_tap::PresentedFrameTap>,
    #[cfg(any(wgpu_arm, target_os = "macos"))]
    pub(crate) copy_src_hold: CopySrcHold,
    pub(crate) image_plane: Option<ImagePlane>,
    #[cfg(wgpu_arm)]
    pub(crate) tray_overlay: Option<TrayOverlay>,
    pub(crate) present_prev: Option<PresentPrev>,
    sdr_glow_level: f32,
    sdr_glow_level_at: Option<aterm_time::Instant>,
    pub(crate) prev_input: RenderInput,
    resident_input_epoch: u64,
    resident_input_effects_transport_shift_y: i32,
    virtual_presented_input_epoch: u64,
    pub(crate) edr_max: f32,
    pub(crate) sdr_white_scale: f32,
    pub(crate) chrome_y0: u32,
    pub(crate) chrome_y1: u32,
    pub(crate) capture_color_space: crate::video_tap::CaptureColorSpace,
    #[cfg(wgpu_arm)]
    pub(crate) shift_scratch: Option<crate::device_layer::LayerTexture>,
    pub(crate) apron_scratch: aterm_render::ApronScratch,
    pub(crate) apron_rgba: Vec<u8>,
    pub(crate) apron_rgba_gen: u64,
    #[cfg(wgpu_arm)]
    pub(crate) apron_tex: Option<crate::device_layer::LayerTexture>,
    #[cfg(wgpu_arm)]
    pub(crate) apron_tex_bytes: Vec<u8>,
    #[cfg(wgpu_arm)]
    pub(crate) apron_tex_gen: u64,
    #[cfg(target_os = "macos")]
    pub(crate) metal_apron_tex: Option<crate::metal::resources::SealedTexture>,
    #[cfg(target_os = "macos")]
    pub(crate) metal_apron_bytes: Vec<u8>,
    #[cfg(target_os = "macos")]
    pub(crate) metal_apron_gen: u64,
    #[cfg(wgpu_arm)]
    pub(crate) present_offscreen: Option<PresentOffscreen>,
    pub(crate) offscreen_dirty_since_sync: Option<[u32; 4]>,
    pub(crate) present_offscreen_fx: Option<[u32; 4]>,
    #[cfg(wgpu_arm)]
    pub(crate) shimmer_scratch: Option<ShimmerScratch>,
    #[cfg(wgpu_arm)]
    pub(crate) virtual_target: Option<VirtualTarget>,
    #[cfg(target_os = "macos")]
    pub(crate) metal_offscreen: Option<MetalOffscreen>,
    #[cfg(gl_arm)]
    gl_offscreen: Option<gl_arm::GlOffscreen>,
    #[cfg(gl_arm)]
    gl_shift_scratch: Option<crate::gl::resources::GlTexture>,
    #[cfg(gl_arm)]
    gl_tray_card: Option<gl_arm::GlTrayCard>,
    #[cfg(gl_arm)]
    gl_present_off: Option<crate::gl::resources::GlTexture>,
    #[cfg(gl_arm)]
    gl_bloom_half: Option<crate::gl::resources::GlTexture>,
    #[cfg(gl_arm)]
    gl_shimmer_scratch: Option<crate::gl::resources::GlTexture>,
    #[cfg(gl_arm)]
    gl_apron_tex: Option<crate::gl::resources::GlTexture>,
    #[cfg(gl_arm)]
    gl_apron_gen: u64,
    #[cfg(gl_arm)]
    gl_virtual_off: Option<crate::gl::resources::GlTexture>,
    #[cfg(gl_arm)]
    offscreen_dirty_since_post: Option<[u32; 4]>,
    #[cfg(gl_arm)]
    gl_posted: Option<gl_present::GlPosted>,
    #[cfg(gl_arm)]
    gl_bloom_inputs: Vec<u8>,
    #[cfg(gl_arm)]
    #[cfg_attr(
        wgpu_arm,
        expect(
            dead_code,
            reason = "read by the GL window post: a build with wgpu posts on wgpu"
        )
    )]
    gl_posted_bloom: Vec<u8>,
    #[cfg(target_os = "macos")]
    pub(crate) metal_present_off: Option<crate::metal::resources::SealedTexture>,
    #[cfg(all(target_os = "macos", test))]
    pub(crate) metal_last_compose_rect: Option<[u32; 4]>,
    #[cfg(target_os = "macos")]
    pub(crate) metal_present_off_pending: Vec<crate::metal::encoder::CbProbe>,
    #[cfg(target_os = "macos")]
    pub(crate) metal_shift_scratch: Option<crate::metal::resources::SealedTexture>,
    #[cfg(target_os = "macos")]
    pub(crate) metal_bloom_half: Option<crate::metal::resources::SealedTexture>,
    #[cfg(target_os = "macos")]
    pub(crate) metal_shimmer_scratch: Option<crate::metal::resources::SealedTexture>,
    #[cfg(target_os = "macos")]
    pub(crate) metal_tray_card: Option<MetalTrayCard>,
    #[cfg(target_os = "macos")]
    pub(crate) metal_virtual_off: Option<crate::metal::resources::SealedTexture>,
    occluded_hint: bool,
    capture_present: bool,
}

#[cfg(target_os = "macos")]
#[inline]
const fn encode_work_ns_excluding_park(work_elapsed_ns: u64, frame_slot_park_ns: u64) -> u64 {
    work_elapsed_ns.saturating_sub(frame_slot_park_ns)
}

/// How long a presented snapshot keeps its window's swapchain copyable after it is armed.
#[cfg(any(wgpu_arm, target_os = "macos"))]
pub(crate) const SNAPSHOT_COPY_SRC_HOLD: std::time::Duration = std::time::Duration::from_secs(2);

/// The copyable-swapchain hold a presented snapshot leaves behind.
#[cfg(any(wgpu_arm, target_os = "macos"))]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CopySrcHold {
    until: Option<aterm_time::Instant>,
}

#[cfg(any(wgpu_arm, target_os = "macos"))]
impl CopySrcHold {
    /// A snapshot was armed at `now`: hold the bit until the hold elapses.
    #[inline]
    pub(crate) fn arm(&mut self, now: aterm_time::Instant) {
        self.until = Some(now + SNAPSHOT_COPY_SRC_HOLD);
    }

    /// Whether the hold still stands at `now`.
    #[inline]
    pub(crate) fn holds(&self, now: aterm_time::Instant) -> bool {
        self.until.is_some_and(|until| now < until)
    }
}

impl WindowGpu {
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    #[cfg(any(wgpu_arm, target_os = "macos"))]
    #[inline]
    fn wants_copy_src(&self, now: aterm_time::Instant) -> bool {
        GpuRenderer::tap_wants_copy_src(
            self.video.is_some(),
            self.presented_snapshot.is_some(),
            self.copy_src_hold.holds(now),
        )
    }

    /// Record winit's `WindowEvent::Occluded` state (see `occluded_hint`).
    #[inline]
    pub const fn set_occluded_hint(&mut self, occluded: bool) {
        self.occluded_hint = occluded;
    }

    /// The last frontend-reported occlusion state (see [`Self::set_occluded_hint`]).
    #[must_use]
    #[inline]
    pub const fn occluded_hint(&self) -> bool {
        self.occluded_hint
    }

    /// Mark the frontend's next presents as a capture's (`on`), or no longer (`!on`).
    #[inline]
    pub const fn set_capture_present(&mut self, on: bool) {
        self.capture_present = on;
    }

    /// Whether the wgpu arm refuses this window's present as `Occluded` on Windows.
    #[must_use]
    #[inline]
    pub const fn parks_while_minimized(&self) -> bool {
        self.occluded_hint
            && !self.capture_present
            && self.presented_snapshot.is_none()
            && self.video.is_none()
    }

    /// Test/diagnostic.
    #[doc(hidden)]
    #[must_use]
    #[inline]
    pub fn bloom_target_resident(&self) -> bool {
        #[cfg(wgpu_arm)]
        {
            self.offscreen.as_ref().is_some_and(|o| o.bloom.is_some())
        }
        #[cfg(not(wgpu_arm))]
        {
            false
        }
    }

    /// CPU wall time encoding and calling `queue.submit` for the latest successful present.
    #[must_use]
    #[inline]
    pub const fn last_present_work_ns(&self) -> u64 {
        self.last_present_work_ns
    }

    /// CPU wall time encoding the latest successful present up to its last commit, excluding
    /// recorded frame-slot and ring parks.
    #[must_use]
    #[inline]
    pub const fn last_present_encode_ns(&self) -> u64 {
        self.last_present_encode_ns
    }

    /// Wall time inside the latest completed drawable acquisition, including refusals.
    #[must_use]
    #[inline]
    pub const fn last_acquire_wait_ns(&self) -> u64 {
        self.last_acquire_wait_ns
    }

    /// Consume this surface-present attempt's acquire measurement exactly once.
    #[inline]
    pub const fn take_acquire_wait_sample_ns(&mut self) -> Option<u64> {
        self.acquire_wait_sample_ns.take()
    }

    /// Wall time the latest completed acquisition waited in the drawable worker's queue.
    #[must_use]
    #[inline]
    pub const fn last_acquire_queue_ns(&self) -> u64 {
        self.last_acquire_queue_ns
    }

    /// Consume this surface-present attempt's queue measurement exactly once.
    #[inline]
    pub const fn take_acquire_queue_sample_ns(&mut self) -> Option<u64> {
        self.acquire_queue_sample_ns.take()
    }

    /// Wall time the latest armed present blocked on the GPU (frame-slot wait plus ring drain).
    #[must_use]
    #[inline]
    pub const fn last_gpu_park_ns(&self) -> u64 {
        self.last_gpu_park_ns
    }

    /// Consume this present attempt's GPU-park measurement once.
    #[inline]
    pub const fn take_gpu_park_sample_ns(&mut self) -> Option<u64> {
        self.gpu_park_sample_ns.take()
    }

    #[cfg(target_os = "macos")]
    #[inline]
    fn add_gpu_park(&mut self, parked_ns: u64) {
        let total = self
            .gpu_park_sample_ns
            .unwrap_or(0)
            .saturating_add(parked_ns);
        self.gpu_park_sample_ns = Some(total);
        self.last_gpu_park_ns = total;
    }

    #[inline]
    const fn begin_surface_present(&mut self) {
        self.acquire_wait_sample_ns = None;
        self.acquire_queue_sample_ns = None;
        self.gpu_park_sample_ns = None;
    }

    #[inline]
    fn measure_surface_acquire<T>(&mut self, acquire: impl FnOnce() -> T) -> T {
        let started = aterm_time::Instant::now();
        let result = acquire();
        self.record_surface_acquire(
            u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
        );
        result
    }

    #[inline]
    const fn record_surface_acquire(&mut self, waited_ns: u64) {
        self.last_acquire_wait_ns = waited_ns;
        self.acquire_wait_sample_ns = Some(waited_ns);
    }

    #[cfg(target_os = "macos")]
    #[inline]
    const fn record_surface_acquire_queue(&mut self, queued_ns: u64) {
        self.last_acquire_queue_ns = queued_ns;
        self.acquire_queue_sample_ns = Some(queued_ns);
    }

    /// Record the screen's raw EDR maximum for this window.
    #[inline]
    pub const fn set_edr_max(&mut self, v: f32) {
        self.edr_max = v;
    }

    /// Record the Windows scRGB reference-white scale (`SDRwhite_nits / 80`), clamped to `>= 1.0`.
    #[inline]
    pub const fn set_sdr_white_scale(&mut self, v: f32) {
        self.sdr_white_scale = if v.is_finite() { v.max(1.0) } else { 1.0 };
    }

    /// Record the colour-space tag the compositor uses for this window's presented texture.
    #[inline]
    pub const fn set_capture_color_space(
        &mut self,
        color_space: crate::video_tap::CaptureColorSpace,
    ) {
        self.capture_color_space = color_space;
    }

    /// The captured presentation colour space.
    #[must_use]
    #[inline]
    pub const fn capture_color_space(&self) -> crate::video_tap::CaptureColorSpace {
        self.capture_color_space
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "HdrReconfigureRetag",
            action = "RetagSucceeds",
            project = "aterm_gpu::WindowGpu::project_hdr_reconfigure_state"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "HdrReconfigureRetag",
            action = "RetagFails",
            project = "aterm_gpu::WindowGpu::project_hdr_reconfigure_state"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "HdrReconfigureRetag",
            action = "EnterSdrFallback",
            project = "aterm_gpu::WindowGpu::project_hdr_reconfigure_state"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "HdrReconfigureRetag",
            action = "UpgradeFails",
            project = "aterm_gpu::WindowGpu::project_hdr_reconfigure_state"
        )
    )]
    #[cfg(wgpu_arm)]
    fn apply_hdr_reconfigure_plan(&mut self, plan: crate::format_plan::HdrReconfigurePlan) {
        if plan == crate::format_plan::HdrReconfigurePlan::FallbackToSdr {
            self.capture_color_space = crate::video_tap::CaptureColorSpace::Srgb;
            self.sdr_white_scale = 1.0;
            self.edr_max = 0.0;
        }
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "HdrReconfigureRetag",
            action = "UpgradeSucceeds",
            project = "aterm_gpu::WindowGpu::project_hdr_reconfigure_state"
        )
    )]
    #[cfg(any(target_os = "macos", wgpu_arm, test))]
    fn apply_hdr_surface_upgrade(&mut self) {
        self.capture_color_space = crate::video_tap::CaptureColorSpace::ExtendedLinearSrgb;
        self.sdr_white_scale = 1.0;
        self.edr_max = 0.0;
    }

    /// The applied window metadata and the planner's resolved format, projected onto the four
    /// `HdrReconfigureRetag` model variables.
    #[cfg(all(test, wgpu_arm))]
    #[must_use]
    pub(crate) fn project_hdr_reconfigure_state(
        &self,
        stage: u8,
        retagged: bool,
        plan: crate::format_plan::HdrReconfigurePlan,
    ) -> crate::format_plan::HdrReconfigureProjection {
        crate::format_plan::HdrReconfigureProjection {
            stage,
            retagged,
            is_f16: plan == crate::format_plan::HdrReconfigurePlan::KeepHdr,
            capture_linear: self.capture_color_space
                == crate::video_tap::CaptureColorSpace::ExtendedLinearSrgb,
        }
    }

    /// The sanitized scRGB reference-white multiplier used by presentation and capture.
    #[must_use]
    #[inline]
    pub const fn sdr_white_scale(&self) -> f32 {
        if self.sdr_white_scale.is_finite() {
            self.sdr_white_scale.max(1.0)
        } else {
            1.0
        }
    }

    /// The recorded raw EDR maximum, which the frontend carries across `WindowGpu` rebuilds.
    #[must_use]
    #[inline]
    pub const fn edr_max(&self) -> f32 {
        self.edr_max
    }

    /// Force the next `present_input` to be a full repaint rather than a dirty-row diff.
    #[inline]
    pub fn invalidate_present(&mut self) {
        self.present_prev = None;
        #[cfg(gl_arm)]
        {
            self.gl_posted = None;
        }
        self.apron_scratch.invalidate();
        self.advance_resident_input_epoch();
    }

    #[inline]
    const fn advance_resident_input_epoch(&mut self) {
        self.resident_input_epoch = self.resident_input_epoch.saturating_add(1);
    }

    /// Test probe.
    #[cfg(all(target_os = "macos", test, wgpu_arm))]
    pub(crate) fn image_plane_probe_for_test(&self) -> Option<(u64, bool)> {
        self.image_plane
            .as_ref()
            .map(|p| (p.epoch, p.metal_texels.is_some()))
    }

    /// Identity of the input most recently encoded for presentation.
    #[must_use]
    #[inline]
    pub const fn resident_input_epoch(&self) -> u64 {
        self.resident_input_epoch
    }

    /// The byte length of [`GpuRenderer::virtual_presented_snapshot_current`], when available.
    #[must_use]
    pub fn virtual_snapshot_bytes(&self) -> Option<usize> {
        #[cfg(gl_arm)]
        if let Some(tex) = self.gl_virtual_off.as_ref() {
            return usize::try_from(u64::from(tex.width) * u64::from(tex.height) * 4).ok();
        }
        self.tapped_virtual_bytes()
    }

    #[cfg(any(wgpu_arm, target_os = "macos"))]
    fn tapped_virtual_bytes(&self) -> Option<usize> {
        #[cfg(wgpu_arm)]
        let (width, height) = self
            .virtual_target
            .as_ref()
            .map(|target| (target.w, target.h))?;
        #[cfg(not(wgpu_arm))]
        let (width, height) = {
            let target = self.metal_virtual_off.as_ref()?;
            (
                u32::try_from(target.width()).ok()?,
                u32::try_from(target.height()).ok()?,
            )
        };
        let len =
            crate::video_tap::presented_snapshot_len(width, height, VIRTUAL_PRESENT_TEXEL).ok()?;
        usize::try_from(len).ok()
    }

    #[cfg(not(any(wgpu_arm, target_os = "macos")))]
    #[expect(clippy::unused_self, reason = "the twin of the tapping arms' reader")]
    fn tapped_virtual_bytes(&self) -> Option<usize> {
        None
    }

    /// Clone the resident semantic model with the transport translation applied to its effects,
    /// in one read so input and translation match.
    #[must_use]
    pub fn clone_resident_input_with_transport_at(&self, epoch: u64) -> Option<(RenderInput, i32)> {
        (epoch != 0 && epoch != u64::MAX && self.resident_input_epoch == epoch).then(|| {
            (
                self.prev_input.clone(),
                self.resident_input_effects_transport_shift_y,
            )
        })
    }

    fn advance_sdr_glow_budget(
        &mut self,
        glow_present: bool,
        target: f32,
        now: aterm_time::Instant,
    ) -> f32 {
        if !glow_present {
            self.sdr_glow_level = 0.0;
            self.sdr_glow_level_at = None;
            return 0.0;
        }

        let dt = self
            .sdr_glow_level_at
            .map_or(0.016, |t| now.saturating_duration_since(t).as_secs_f32());
        self.sdr_glow_level_at = Some(now);
        if target > self.sdr_glow_level {
            let attack = 1.0 - (-dt / 0.045).exp();
            self.sdr_glow_level += (target - self.sdr_glow_level) * attack;
        } else {
            self.sdr_glow_level = target;
        }
        self.sdr_glow_level
    }
}

/// The per-frame inline-image texture.
pub(crate) struct ImagePlane {
    #[cfg(target_os = "macos")]
    metal_atlas: Option<ArmAtlas>,
    #[cfg(wgpu_arm)]
    bind: wgpu::BindGroup,
    w: u32,
    h: u32,
    placements: FxHashMap<(usize, usize, usize), (u32, u32, u32)>,
    _pinned_images: Vec<std::sync::Arc<aterm_core::grid::extra::ImageData>>,
    #[cfg(target_os = "macos")]
    metal_texels: Option<Vec<u8>>,
    #[cfg(target_os = "macos")]
    epoch: u64,
    #[cfg(gl_arm)]
    gl_texels: Option<Vec<u8>>,
    #[cfg(gl_arm)]
    gl_epoch: u64,
    #[cfg(gl_arm)]
    gl_atlas: Option<gl_arm::GlArmAtlas>,
}

/// GPU terminal renderer.
pub struct GpuRenderer {
    ctx: GpuContext,
    cpu: Renderer,
    #[cfg(not(target_arch = "wasm32"))]
    font_family: Option<String>,
    theme: Theme,
    #[cfg(wgpu_arm)]
    uniform_buf: wgpu::Buffer,
    #[cfg(wgpu_arm)]
    uniform_bg: wgpu::BindGroup,
    #[cfg(wgpu_arm)]
    uniform_written: Option<(u32, u32, u32)>,
    #[cfg(wgpu_arm)]
    atlas_bgl: wgpu::BindGroupLayout,
    #[cfg(wgpu_arm)]
    sampler: wgpu::Sampler,
    #[cfg(wgpu_arm)]
    bg_pipeline: wgpu::RenderPipeline,
    #[cfg(wgpu_arm)]
    glyph_pipeline: wgpu::RenderPipeline,
    #[cfg(wgpu_arm)]
    color_glyph_pipeline: wgpu::RenderPipeline,
    #[cfg(wgpu_arm)]
    effects: EffectPipelines,
    #[cfg(wgpu_arm)]
    bloom_bgl: wgpu::BindGroupLayout,
    #[cfg(wgpu_arm)]
    bloom_sampler: wgpu::Sampler,
    #[cfg(wgpu_arm)]
    bloom_pipeline: wgpu::RenderPipeline,
    #[cfg(wgpu_arm)]
    bloom_uniform_buf: wgpu::Buffer,
    enable_bloom: bool,
    hdr_glow: bool,
    backdrop_margins: bool,
    #[cfg(wgpu_arm)]
    hdr_glow_pipeline: Option<wgpu::RenderPipeline>,
    #[cfg(wgpu_arm)]
    hdr_glow_uniform_buf: Option<wgpu::Buffer>,
    #[cfg(wgpu_arm)]
    hdr_glow_bg: Option<wgpu::BindGroup>,
    #[cfg(wgpu_arm)]
    glow_boost_bgl: Option<wgpu::BindGroupLayout>,
    #[cfg(wgpu_arm)]
    sdr_glow_pipeline: Option<(wgpu::TextureFormat, wgpu::RenderPipeline)>,
    sdr_glow_boost: f32,
    deco_atlas: Option<DecoAtlas>,
    sprite_textures: SpriteTextureLookup,
    bloom_strength: f32,
    bloom_radius: f32,
    #[cfg(wgpu_arm)]
    shimmer: Option<ShimmerResources>,
    enable_shimmer: bool,
    shimmer_epoch: aterm_time::Instant,
    shimmer_phase_pin: Option<f32>,
    #[cfg(wgpu_arm)]
    blit_shader: wgpu::ShaderModule,
    #[cfg(wgpu_arm)]
    blit_bgl: wgpu::BindGroupLayout,
    #[cfg(wgpu_arm)]
    blit_layout: wgpu::PipelineLayout,
    #[cfg(wgpu_arm)]
    blit_sampler: wgpu::Sampler,
    #[cfg(wgpu_arm)]
    blit_uniform_buf: wgpu::Buffer,
    #[cfg(wgpu_arm)]
    blit_uniform_written: Option<BlitUniform>,
    #[cfg(wgpu_arm)]
    blit_pipelines: HashMap<wgpu::TextureFormat, wgpu::RenderPipeline>,
    #[cfg(wgpu_arm)]
    tray_shader: wgpu::ShaderModule,
    #[cfg(wgpu_arm)]
    tray_bgl: wgpu::BindGroupLayout,
    #[cfg(wgpu_arm)]
    tray_layout: wgpu::PipelineLayout,
    #[cfg(wgpu_arm)]
    tray_sampler: wgpu::Sampler,
    #[cfg(wgpu_arm)]
    tray_uniform_buf: wgpu::Buffer,
    #[cfg(wgpu_arm)]
    tray_pipelines: HashMap<wgpu::TextureFormat, wgpu::RenderPipeline>,
    #[cfg(wgpu_arm)]
    tray_scrim: Option<TrayScrimLayer>,
    #[cfg(wgpu_arm)]
    cached_surface_format: Option<wgpu::TextureFormat>,
    mono_res: Option<ResidentAtlas>,
    color_res: Option<ResidentAtlas>,
    resident_keys: FxHashSet<GlyphKey>,
    atlas_tex_creations: u64,
    #[cfg(wgpu_arm)]
    vbufs: VertexBuffers,
    #[cfg(target_os = "macos")]
    metal_armed: bool,
    #[cfg(target_os = "macos")]
    metal_mint_error: Option<String>,
    #[cfg(target_os = "macos")]
    metal_arm: Option<MetalArmCell>,
    #[cfg(target_os = "macos")]
    last_frame_arm_metal: bool,
    #[cfg(target_os = "macos")]
    metal_pending_band_shift: Option<BandShift>,
    #[cfg(target_os = "macos")]
    metal_inject_loss_after: Option<u64>,
    #[cfg(any(target_os = "macos", gl_arm))]
    image_plane_epoch: u64,
    #[cfg(any(target_os = "macos", gl_arm))]
    atlas_epoch: u64,
    #[cfg(gl_arm)]
    gl_armed: bool,
    #[cfg(gl_arm)]
    gl_arm: Option<gl_arm::GlArmLive>,
    #[cfg(gl_arm)]
    gl_mint_error: Option<String>,
    #[cfg(all(gl_arm, not(target_arch = "wasm32")))]
    gl_window_system: Option<crate::gl::device::WindowSystem>,
    #[cfg(gl_arm)]
    last_frame_arm_gl: bool,
    #[cfg(gl_arm)]
    gl_inject_loss_after: Option<u64>,
    inst: Instances,
    fx: fx_gpu::FxGpu,
    #[cfg(wgpu_arm)]
    fx_wgpu: FxWgpu,
    #[cfg(all(target_os = "macos", any(test, debug_assertions)))]
    fx_palette_shared_seam: bool,
    #[cfg(all(target_os = "macos", any(test, debug_assertions)))]
    fx_fail_seam: bool,
    gate_hits: u64,
    gate_misses: u64,
    dirty_scratch: Vec<bool>,
    row_plans: Vec<Vec<aterm_render::ColumnGlyph>>,
    deco_rects_scratch: Vec<[usize; 4]>,
    deco_skip_scratch: Vec<[usize; 4]>,
    curl_ink_scratch: Vec<bool>,
    curl_spans_scratch: Vec<(usize, usize)>,
    ligature_break_scratch: Vec<usize>,
    bloom_glow_scratch: Vec<GlowInstance>,
    bloom_quad_scratch: Vec<aterm_render::GlowQuad>,
    key_scratch: Vec<Option<GlyphKey>>,
    glyph_win_scratch: Vec<Option<(i32, i32)>>,
    overshoot_surprise: bool,
    image_order_scratch: Vec<(
        std::sync::Arc<aterm_core::grid::extra::ImageData>,
        usize,
        usize,
        usize,
    )>,
    image_seen_scratch: FxHashMap<(usize, usize, usize), usize>,
    image_placements_scratch: FxHashMap<(usize, usize, usize), (u32, u32, u32)>,
    scissor_taken: u64,
    scroll_rescues: u64,
    full_repaints: u64,
    tray_uploads: u64,
    tray_upload_bytes: u64,
    last_instances: usize,
    last_bg_instances: usize,
    last_frame_passes: u32,
    frame_shape: FrameShape,
    ablation: UnormAblation,
    /// The route the Metal arm's frame plans take where the device passed the fetch probe:
    /// [`UnormRoute::Fetch`] unless the `ATERM_GPU_UNORM_ROUTE` seam or [`Self::set_unorm_route`]
    /// chose the split.
    unorm_route: UnormRoute,
    /// A verdict standing in for the Metal arm's ([`Self::set_fetch_probe_seam`]).
    #[cfg(all(target_os = "macos", any(test, debug_assertions)))]
    fetch_probe_seam: Option<FetchProbe>,
    /// The first-frame warm the Metal arm's mint waits out ([`crate::metal::warm::FIRST_FRAME`],
    /// or a test's own, `Self::set_first_frame_latch_for_test`).
    #[cfg(target_os = "macos")]
    first_frame: &'static crate::metal::warm::WarmLatch,
    /// [`Self::warm_effect_pipelines`] was asked for: the Metal arm's mint starts the warm that a
    /// request before it could not.
    #[cfg(target_os = "macos")]
    metal_warm_wanted: bool,
    /// A window surface attached on this renderer, whose swapchain may be f16 (an EDR screen,
    /// `hdr_glow`); a headless renderer presents to a Bgra8Unorm target only.
    #[cfg(target_os = "macos")]
    metal_on_screen: bool,
    last_gpu_time_serial: Option<u64>,
    #[cfg(wgpu_arm)]
    wgpu_timer: Option<crate::wgpu_timer::WgpuGpuTimer>,
    frame_plan: Vec<DrawItem>,
    last_frame_load: crate::device_layer::FrameLoad,
    last_frame_scissor: Option<(u32, u32, u32, u32)>,
}

/// The previous presented frame's overlay state, for the scissored dirty-row repaint.
pub(crate) struct PresentPrev {
    blink_phase: bool,
    cursor_style_override: Option<CursorStyle>,
    grid_top: usize,
    #[cfg(target_os = "macos")]
    on_metal: bool,
    #[cfg(gl_arm)]
    on_gl: bool,
}

enum RepaintScope<'a> {
    Full,
    Dirty(&'a [bool]),
}

/// The GPU dirty-gate cache.
pub(crate) struct GpuGateCache {
    input: RenderInput,
    blink_phase: bool,
    cursor_style_override: Option<CursorStyle>,
    grid_top: usize,
    frame: Frame,
}

/// Projection of both GPU layout caches onto the `AsymmetricPadLayout` cache-origin variables.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AsymmetricPadGpuCacheProjection {
    pub grid_top: usize,
    pub present_cached_grid_top: Option<usize>,
    pub gate_cached_grid_top: Option<usize>,
}

/// The armed arm's per-window offscreen pair.
#[cfg(target_os = "macos")]
pub(crate) struct MetalOffscreen {
    pub(crate) tex: crate::metal::resources::SealedTexture,
    pub(crate) srgb: crate::metal::resources::SealedTexture,
    pub(crate) w: u32,
    pub(crate) h: u32,
}

/// The armed arm's resident settings card (`TrayOverlay` twin).
#[cfg(target_os = "macos")]
pub(crate) struct MetalTrayCard {
    texes: Vec<crate::device_layer::LayerTexture>,
    ring: crate::tray_card_ring::TrayCardRing,
    pub(crate) w: u32,
    pub(crate) h: u32,
    pixels: Vec<u8>,
}

#[cfg(target_os = "macos")]
impl MetalTrayCard {
    /// The texture the card's draw samples: the shown slot's.
    #[inline]
    pub(crate) fn tex(&self) -> &crate::metal::resources::SealedTexture {
        self.texes[self.ring.shown()].metal()
    }
}

/// Return the dirty row band (`y0..y1`) where `old` and `new` differ.
#[cfg(any(target_os = "macos", wgpu_arm))]
pub(crate) fn changed_row_band(old: &[u8], new: &[u8], stride: usize) -> Option<(u32, u32)> {
    if stride == 0 || old.len() != new.len() {
        return None;
    }
    let rows = new.len() / stride;
    let differs =
        |row: usize| old[row * stride..(row + 1) * stride] != new[row * stride..(row + 1) * stride];
    let first = (0..rows).find(|row| differs(*row))?;
    let last = (first..rows)
        .rev()
        .find(|row| differs(*row))
        .unwrap_or(first);
    Some((first as u32, last as u32 + 1))
}

#[cfg(wgpu_arm)]
pub(crate) struct Offscreen {
    #[cfg(wgpu_arm)]
    tex: wgpu::Texture,
    #[cfg(wgpu_arm)]
    view: wgpu::TextureView,
    #[cfg(wgpu_arm)]
    view_srgb: wgpu::TextureView,
    #[cfg(wgpu_arm)]
    blit_bind: wgpu::BindGroup,
    w: u32,
    h: u32,
    bloom: Option<BloomTarget>,
}

/// The present copy (see [`WindowGpu::present_offscreen`]).
#[cfg(wgpu_arm)]
pub(crate) struct PresentOffscreen {
    #[cfg(wgpu_arm)]
    tex: wgpu::Texture,
    #[cfg(wgpu_arm)]
    view: wgpu::TextureView,
    #[cfg(wgpu_arm)]
    blit_bind: wgpu::BindGroup,
    w: u32,
    h: u32,
}

/// Shimmer staging texture and sample bind group (see [`WindowGpu::shimmer_scratch`]).
#[cfg(wgpu_arm)]
pub(crate) struct ShimmerScratch {
    #[cfg(wgpu_arm)]
    tex: wgpu::Texture,
    #[cfg(wgpu_arm)]
    bind: wgpu::BindGroup,
    w: u32,
    h: u32,
}

/// Headless virtual swapchain (see [`WindowGpu::virtual_target`]) that
/// [`GpuRenderer::present_virtual`] renders into.
#[cfg(wgpu_arm)]
pub(crate) struct VirtualTarget {
    #[cfg(wgpu_arm)]
    tex: wgpu::Texture,
    w: u32,
    h: u32,
}

#[cfg(wgpu_arm)]
type OffscreenTexHandle = wgpu::Texture;
#[cfg(not(wgpu_arm))]
type OffscreenTexHandle = std::convert::Infallible;

#[cfg(any(wgpu_arm, target_os = "macos"))]
const VIRTUAL_PRESENT_TEXEL: crate::device_layer::TexelFormat =
    crate::device_layer::TexelFormat::Bgra8Unorm;
#[cfg(wgpu_arm)]
const VIRTUAL_PRESENT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8Unorm;

#[cfg(wgpu_arm)]
struct PresentDest<'a> {
    view: &'a wgpu::TextureView,
    tex: &'a wgpu::Texture,
    w: u32,
    h: u32,
    #[cfg(wgpu_arm)]
    format: wgpu::TextureFormat,
    translucent: bool,
    premult: bool,
}

/// The half-res bloom target and its composite bind group.
#[cfg(wgpu_arm)]
pub(crate) struct BloomTarget {
    #[cfg(wgpu_arm)]
    #[expect(
        dead_code,
        reason = "owned GPU texture kept alive for its view and bind group; never read"
    )]
    tex: wgpu::Texture,
    #[cfg(wgpu_arm)]
    view: wgpu::TextureView,
    #[cfg(wgpu_arm)]
    bind: wgpu::BindGroup,
    bw: u32,
    bh: u32,
}

#[derive(Default)]
struct Instances {
    keys: FxHashSet<GlyphKey>,
    key_order: Vec<GlyphKey>,
    bg: Vec<BgInstance>,
    image_below_bg: Vec<GlyphInstance>,
    image_bg_cover: Vec<BgInstance>,
    image_under: Vec<GlyphInstance>,
    image: Vec<GlyphInstance>,
    glyph: Vec<GlyphInstance>,
    glyph_halo: Vec<GlyphInstance>,
    color: Vec<GlyphInstance>,
    cursor: Vec<BgInstance>,
    deco: Vec<BgInstance>,
    curl: Vec<GlyphInstance>,
    trail: Vec<BgInstance>,
    glow_add: Vec<GlowInstance>,
    glow_halo: Vec<RainGlowInstance>,
    glow_halo_over: Vec<RainGlowInstance>,
    glow_under: Vec<GlowInstance>,
    fire_add: Vec<FireInstance>,
    fire_over: Vec<FireInstance>,
    nova_add: Vec<GlowInstance>,
    rain_add: Vec<RainGlowInstance>,
    rain_add_over: Vec<RainGlowInstance>,
    wdeco_over: Vec<GlyphInstance>,
    wdeco_add: Vec<GlyphInstance>,
    wallpaper: Vec<GlyphInstance>,
    cursor_block: Vec<BgInstance>,
    cursor_glyph: Vec<GlyphInstance>,
    cursor_color: Vec<GlyphInstance>,
    fx: [Vec<fx_gpu::FxInstance>; fx_gpu::FX_STREAM_COUNT],
}

impl Instances {
    /// One stream's CPU-side bytes and instance count, as both tails upload them.
    #[inline]
    pub(crate) fn stream_bytes(&self, id: StreamId) -> (&[u8], u32) {
        macro_rules! stream {
            ($field:ident) => {
                (
                    aterm_bits::cast_slice(&self.$field),
                    self.$field.len() as u32,
                )
            };
        }
        match id {
            StreamId::Wallpaper => stream!(wallpaper),
            StreamId::Bg => stream!(bg),
            StreamId::ImageBelowBg => stream!(image_below_bg),
            StreamId::ImageBgCover => stream!(image_bg_cover),
            StreamId::FxLinearUnder => self.fx_stream(fx_gpu::FxStream::LinearUnder),
            StreamId::FxKnockUnder => self.fx_stream(fx_gpu::FxStream::KnockUnder),
            StreamId::Trail => stream!(trail),
            StreamId::GlowUnder => stream!(glow_under),
            StreamId::FxUnder => self.fx_stream(fx_gpu::FxStream::Under),
            StreamId::FireAdd => stream!(fire_add),
            StreamId::FireOver => stream!(fire_over),
            StreamId::ImageUnder => stream!(image_under),
            StreamId::GlyphHalo => stream!(glyph_halo),
            StreamId::Glyph => stream!(glyph),
            StreamId::Color => stream!(color),
            StreamId::Image => stream!(image),
            StreamId::Deco => stream!(deco),
            StreamId::Curl => stream!(curl),
            StreamId::GlowAdd => stream!(glow_add),
            StreamId::FxGlow => self.fx_stream(fx_gpu::FxStream::Glow),
            StreamId::GlowHalo => stream!(glow_halo),
            StreamId::GlowHaloOver => stream!(glow_halo_over),
            StreamId::NovaAdd => stream!(nova_add),
            StreamId::FxOver => self.fx_stream(fx_gpu::FxStream::Over),
            StreamId::RainAdd => stream!(rain_add),
            StreamId::RainAddOver => stream!(rain_add_over),
            StreamId::WdecoOver => stream!(wdeco_over),
            StreamId::WdecoAdd => stream!(wdeco_add),
            StreamId::FxLinearOver => self.fx_stream(fx_gpu::FxStream::LinearOver),
            StreamId::CursorBlock => stream!(cursor_block),
            StreamId::CursorGlyph => stream!(cursor_glyph),
            StreamId::CursorColor => stream!(cursor_color),
            StreamId::Cursor => stream!(cursor),
        }
    }

    #[inline]
    fn fx_stream(&self, s: fx_gpu::FxStream) -> (&[u8], u32) {
        let v = &self.fx[s as usize];
        (aterm_bits::cast_slice(v), v.len() as u32)
    }

    #[inline]
    fn fx_instances(&self) -> usize {
        self.fx.iter().map(Vec::len).sum()
    }

    fn clear(&mut self) {
        self.keys.clear();
        self.key_order.clear();
        self.bg.clear();
        self.image_below_bg.clear();
        self.image_bg_cover.clear();
        self.image_under.clear();
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
        self.wallpaper.clear();
        self.cursor_block.clear();
        self.cursor_glyph.clear();
        self.cursor_color.clear();
        for v in &mut self.fx {
            v.clear();
        }
    }
}

#[cfg(wgpu_arm)]
#[derive(Default)]
struct FxWgpu {
    tex: [Option<fx_gpu::FxArmTex>; 2],
    palette: Option<(wgpu::Buffer, u64)>,
    mints: u64,
    bind: Option<(u64, wgpu::BindGroup)>,
}

#[cfg(wgpu_arm)]
struct VertexBuffers {
    streams: [VertexBuffer; STREAM_COUNT],
    bloom_glow: VertexBuffer,
}

#[cfg(wgpu_arm)]
impl VertexBuffers {
    fn new(handle: crate::device_layer::DeviceHandle<'_>) -> Self {
        const LABELS: [&str; STREAM_COUNT] = [
            "aterm-gpu wallpaper base instances",
            "aterm-gpu bg instances",
            "aterm-gpu below-cell-background image instances",
            "aterm-gpu image background-cover instances",
            "aterm-gpu fx linear under instances",
            "aterm-gpu cursor trail instances",
            "aterm-gpu emberforge glow-under instances",
            "aterm-gpu fx under instances",
            "aterm-gpu emberforge fire add instances",
            "aterm-gpu emberforge fire over instances",
            "aterm-gpu fx knock under instances",
            "aterm-gpu behind-text image instances",
            "aterm-gpu glyph contrast-halo instances",
            "aterm-gpu glyph instances",
            "aterm-gpu colour glyph instances",
            "aterm-gpu image instances",
            "aterm-gpu decoration instances",
            "aterm-gpu undercurl instances",
            "aterm-gpu lumen glow-add instances",
            "aterm-gpu fx glow instances",
            "aterm-gpu glow halo instances",
            "aterm-gpu glow halo over instances",
            "aterm-gpu supernova nova-add instances",
            "aterm-gpu fx over instances",
            "aterm-gpu rain add instances",
            "aterm-gpu rain add over instances",
            "aterm-gpu sparkle-word paw instances",
            "aterm-gpu sparkle-word sparkle instances",
            "aterm-gpu fx linear over instances",
            "aterm-gpu cursor block fill",
            "aterm-gpu cursor cut-out glyph",
            "aterm-gpu cursor colour glyph",
            "aterm-gpu cursor instances",
        ];
        Self {
            streams: LABELS.map(|label| VertexBuffer::new(handle, label)),
            bloom_glow: VertexBuffer::new(handle, "aterm-gpu bloom halo source (ungated glow)"),
        }
    }
}

#[cfg(wgpu_arm)]
struct VertexBuffer {
    buf: crate::device_layer::LayerBuffer,
    capacity: u64,
    label: &'static str,
}

#[cfg(wgpu_arm)]
impl VertexBuffer {
    fn new(handle: crate::device_layer::DeviceHandle<'_>, label: &'static str) -> Self {
        Self {
            buf: handle.create_instance_buffer(label, 0),
            capacity: 0,
            label,
        }
    }

    #[cfg(wgpu_arm)]
    fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bytes: &[u8],
    ) -> Option<wgpu::BufferSlice<'_>> {
        let handle = crate::device_layer::DeviceHandle::Wgpu { device, queue };
        if !self.upload_via(handle, bytes) {
            return None;
        }
        Some(self.buf.slice(..bytes.len() as u64))
    }

    #[cfg(wgpu_arm)]
    fn upload_via(&mut self, handle: crate::device_layer::DeviceHandle<'_>, bytes: &[u8]) -> bool {
        if !should_slice(bytes.len()) {
            return false;
        }
        let needed = align_up(bytes.len() as u64, wgpu::COPY_BUFFER_ALIGNMENT);
        if needed > self.capacity {
            let max_buf = handle.max_buffer_size();
            if needed > max_buf {
                return false;
            }
            let new_cap =
                align_up(needed.next_power_of_two(), wgpu::COPY_BUFFER_ALIGNMENT).min(max_buf);
            self.buf = handle.create_instance_buffer(self.label, new_cap);
            self.capacity = new_cap;
        }
        // SAFETY: wgpu has no precondition and Metal callers rewrite streams only after command
        // buffers settle.
        unsafe { handle.write_buffer_from_zero(&self.buf, bytes) };
        true
    }
}

#[cfg(wgpu_arm)]
fn fx_wgpu_sync(
    fxw: &mut FxWgpu,
    fx: &mut fx_gpu::FxGpu,
    effects: &mut EffectPipelines,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    inst: &Instances,
    under_opaque: bool,
) -> bool {
    let drawing = inst.fx_instances() > 0;
    let handle = crate::device_layer::DeviceHandle::Wgpu { device, queue };
    let mut atlas_bytes = 0;
    for kind in [fx_gpu::FxAtlasKind::R8, fx_gpu::FxAtlasKind::Rgba] {
        let side = fx.atlas.side(kind);
        if !fx_gpu::fx_tex_due(&fxw.tex[kind as usize], side, drawing) {
            continue;
        }
        match fx_gpu::sync_fx_tex(handle, &mut fxw.tex[kind as usize], side, |_| true, || {}) {
            Ok(synced) => {
                atlas_bytes += synced.bytes;
                if synced.minted {
                    fxw.mints += 1;
                }
            }
            Err(e) => {
                crate::stderr_line!("aterm-gpu: fx {kind:?} atlas: {e}; fx draws nothing");
                return false;
            }
        }
    }
    fx.atlas_bytes = atlas_bytes;
    if !drawing {
        return false;
    }
    let resident = effects.resident();
    let (exact, linear, store) = fx_effect_demand(inst, under_opaque);
    if (exact && !resident[EffectPipeline::Fx as usize])
        || (linear && !resident[EffectPipeline::FxLinear as usize])
        || (store && !resident[EffectPipeline::FxStore as usize])
    {
        return false;
    }
    let Ok(layouts) = effects.fx_layouts(device) else {
        return false;
    };
    let bytes: &[u8] = aterm_bits::cast_slice(&fx.palette);
    let need = align_up(bytes.len().max(16) as u64, wgpu::COPY_BUFFER_ALIGNMENT);
    if fxw.palette.as_ref().is_none_or(|(_, cap)| *cap < need) {
        let cap = need.next_power_of_two();
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aterm-gpu fx palette"),
            size: cap,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        fxw.palette = Some((buf, cap));
        fxw.mints += 1;
    }
    let (buf, _) = fxw.palette.as_ref().expect("minted above");
    if !bytes.is_empty() {
        queue.write_buffer(buf, 0, bytes);
    }
    if fxw.bind.as_ref().is_none_or(|(k, _)| *k != fxw.mints) {
        let view = |k: fx_gpu::FxAtlasKind| {
            fxw.tex[k as usize]
                .as_ref()
                .expect("an fx frame syncs both atlases")
                .tex
                .wgpu()
                .create_view(&wgpu::TextureViewDescriptor::default())
        };
        let (r8, rgba) = (
            view(fx_gpu::FxAtlasKind::R8),
            view(fx_gpu::FxAtlasKind::Rgba),
        );
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("aterm-gpu fx bg"),
            layout: &layouts.bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&r8),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&rgba),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: buf.as_entire_binding(),
                },
            ],
        });
        fxw.bind = Some((fxw.mints, bind));
    }
    true
}

trust_contract_fn! {
    /// Whether a vertex stream may be sliced and bound.
    #[must_use]
    pub fn should_slice(byte_len: usize) -> bool
    clauses [ensures result == (byte_len != 0) by should_slice_is_nonempty]
    {
        byte_len != 0
    }
}

verify_island! {
    clean {
        theorem should_slice_is_nonempty (byte_len : UInt64) :
            trust_import_aterm_gpu__renderer__should_slice byte_len
              = !(decide (byte_len = 0)) := by
          simp only [trust_import_aterm_gpu__renderer__should_slice]
    }
}

#[cfg(wgpu_arm)]
#[inline]
const fn align_up(n: u64, align: u64) -> u64 {
    (n + align - 1) & !(align - 1)
}

#[cfg(wgpu_arm)]
fn build_uniform_resources(
    handle: crate::device_layer::DeviceHandle<'_>,
) -> (wgpu::Buffer, wgpu::BindGroupLayout, wgpu::BindGroup) {
    let device = handle.wgpu_device();
    let uniform_buf = handle
        .create_uniform_buffer("aterm-gpu uniforms", std::mem::size_of::<Uniforms>() as u64)
        .into_wgpu();

    let uniform_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("aterm-gpu uniform bgl"),
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

#[cfg(wgpu_arm)]
fn build_atlas_resources(
    handle: crate::device_layer::DeviceHandle<'_>,
) -> (wgpu::BindGroupLayout, wgpu::Sampler) {
    let device = handle.wgpu_device();
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

    let sampler = handle
        .create_sampler(
            "aterm-gpu nearest",
            crate::device_layer::SamplerKind::NearestClamp,
        )
        .into_wgpu();

    (atlas_bgl, sampler)
}

use aterm_render::bloom::{BLOOM_DOWNSCALE, BLOOM_RADIUS, BLOOM_STRENGTH};

/// The chroma radius paired with a white blur `radius`, used by every uniform write site.
#[inline]
pub(crate) const fn bloom_chroma_radius(radius: f32) -> f32 {
    aterm_render::bloom::chroma_radius(radius)
}

#[cfg(wgpu_arm)]
fn build_bloom_resources(
    handle: crate::device_layer::DeviceHandle<'_>,
    targets: TargetFormats,
) -> (
    wgpu::BindGroupLayout,
    wgpu::Sampler,
    wgpu::RenderPipeline,
    wgpu::Buffer,
) {
    let device = handle.wgpu_device();
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("aterm-gpu bloom shader"),
        source: wgpu::ShaderSource::Wgsl(wgsl_source(Pipeline::Bloom.spec().library).into()),
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
    let sampler = handle
        .create_sampler(
            "aterm-gpu bloom linear",
            crate::device_layer::SamplerKind::LinearClamp,
        )
        .into_wgpu();
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("aterm-gpu bloom layout"),
        bind_group_layouts: &[Some(&bgl)],
        immediate_size: 0,
    });
    let spec = Pipeline::Bloom.spec();
    let pipeline = crate::pipeline_table::build_wgpu(spec, device, &shader, &layout, targets);
    let uniform = handle
        .create_uniform_buffer(
            "aterm-gpu bloom uniform",
            std::mem::size_of::<BloomUniform>() as u64,
        )
        .into_wgpu();
    (bgl, sampler, pipeline, uniform)
}

const SHIMMER_BANDS: usize = 64;
const SHIMMER_AMP_PX: f32 = 1.5;
const SHIMMER_RISE_CELLS: f32 = 3.5;
const SHIMMER_HEAT_GAIN: f32 = 1.35;
const SHIMMER_COPY_MARGIN: u32 = 4;
const SHIMMER_PHASE_WRAP_S: f32 = 100.0;

#[cfg(wgpu_arm)]
struct ShimmerResources {
    #[cfg(wgpu_arm)]
    bgl: wgpu::BindGroupLayout,
    #[cfg(wgpu_arm)]
    pipeline: wgpu::RenderPipeline,
    #[cfg(wgpu_arm)]
    uniform_buf: wgpu::Buffer,
}

#[cfg(wgpu_arm)]
fn build_shimmer_resources(
    handle: crate::device_layer::DeviceHandle<'_>,
    targets: TargetFormats,
) -> ShimmerResources {
    let device = handle.wgpu_device();
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("aterm-gpu shimmer shader"),
        source: wgpu::ShaderSource::Wgsl(wgsl_source(Pipeline::Shimmer.spec().library).into()),
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
    let spec = Pipeline::Shimmer.spec();
    let pipeline = crate::pipeline_table::build_wgpu(spec, device, &shader, &layout, targets);
    let uniform_buf = handle
        .create_uniform_buffer(
            "aterm-gpu shimmer uniform",
            std::mem::size_of::<ShimmerUniform>() as u64,
        )
        .into_wgpu();
    ShimmerResources {
        bgl,
        pipeline,
        uniform_buf,
    }
}

struct ShimmerRegion {
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
    #[cfg(wgpu_arm)]
    fw: u32,
    #[cfg(wgpu_arm)]
    fh: u32,
    hot_top: f32,
    rise: f32,
    band_x0: f32,
    band_w: f32,
    heat: [f32; SHIMMER_BANDS],
}

#[cfg(wgpu_arm)]
struct CellLayouts {
    #[cfg(wgpu_arm)]
    bg: wgpu::PipelineLayout,
    #[cfg(wgpu_arm)]
    glyph: wgpu::PipelineLayout,
    uniform_bgl: wgpu::BindGroupLayout,
}

/// The fx rows' group-1 layout and their pipeline layout.
#[cfg(wgpu_arm)]
pub(crate) struct FxWgpuLayouts {
    pub(crate) bgl: wgpu::BindGroupLayout,
    layout: wgpu::PipelineLayout,
}

#[cfg(wgpu_arm)]
fn build_fx_layouts(
    device: &wgpu::Device,
    uniform_bgl: &wgpu::BindGroupLayout,
) -> Result<FxWgpuLayouts, String> {
    if device.limits().max_storage_buffers_per_shader_stage == 0 {
        return Err("its fragment stage reads no storage buffer".into());
    }
    let texture = |binding| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    };
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("aterm-gpu fx bgl"),
        entries: &[
            texture(2),
            texture(3),
            wgpu::BindGroupLayoutEntry {
                binding: 4,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("aterm-gpu fx layout"),
        bind_group_layouts: &[Some(uniform_bgl), Some(&bgl)],
        immediate_size: 0,
    });
    Ok(FxWgpuLayouts { bgl, layout })
}

#[cfg(wgpu_arm)]
fn note_fx_closed(why: &str) {
    static NOTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !NOTED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        crate::stderr_line!("aterm-gpu: fx layers draw nothing on this device: {why}");
    }
}

#[cfg(wgpu_arm)]
fn build_cell_layouts(
    device: &wgpu::Device,
    uniform_bgl: &wgpu::BindGroupLayout,
    atlas_bgl: &wgpu::BindGroupLayout,
) -> CellLayouts {
    CellLayouts {
        bg: device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("aterm-gpu bg layout"),
            bind_group_layouts: &[Some(uniform_bgl)],
            immediate_size: 0,
        }),
        glyph: device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("aterm-gpu glyph layout"),
            bind_group_layouts: &[Some(uniform_bgl), Some(atlas_bgl)],
            immediate_size: 0,
        }),
        uniform_bgl: uniform_bgl.clone(),
    }
}

#[cfg(wgpu_arm)]
type CellPipelines = (
    wgpu::RenderPipeline,
    wgpu::RenderPipeline,
    wgpu::RenderPipeline,
);

#[cfg(wgpu_arm)]
fn build_cell_pipelines(
    device: &wgpu::Device,
    shader: &wgpu::ShaderModule,
    layouts: &CellLayouts,
    targets: TargetFormats,
) -> CellPipelines {
    let bg_layout = &layouts.bg;
    let glyph_layout = &layouts.glyph;

    let mut pipeline_mark = aterm_time::Instant::now();
    let bg_pipeline =
        crate::pipeline_table::build_wgpu(Pipeline::Bg.spec(), device, shader, bg_layout, targets);
    pipeline_mark = crate::startup_probe::split_cell_pipeline(0, pipeline_mark);

    let glyph_pipeline = crate::pipeline_table::build_wgpu(
        Pipeline::Glyph.spec(),
        device,
        shader,
        glyph_layout,
        targets,
    );
    pipeline_mark = crate::startup_probe::split_cell_pipeline(1, pipeline_mark);

    let color_glyph_pipeline = crate::pipeline_table::build_wgpu(
        Pipeline::ColorGlyph.spec(),
        device,
        shader,
        glyph_layout,
        targets,
    );
    let _ = crate::startup_probe::split_cell_pipeline(2, pipeline_mark);
    (bg_pipeline, glyph_pipeline, color_glyph_pipeline)
}

/// One of the effect-only cell pipelines, ordered by [`EFFECT_PIPELINE_NAMES`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(usize)]
pub enum EffectPipeline {
    CursorBlend = 0,
    GlowAdd = 1,
    RainGlow = 2,
    RainGlowOver = 3,
    FireAdd = 4,
    FireOver = 5,
    DecoOver = 6,
    DecoAdd = 7,
    SpriteOver = 8,
    Fx = 9,
    FxLinear = 10,
    FxStore = 11,
}

/// How many effect pipelines exist.
pub const EFFECT_PIPELINE_COUNT: usize = 12;

/// Names of the [`EFFECT_PIPELINE_COUNT`] slots, in [`EffectPipeline`] order.
pub const EFFECT_PIPELINE_NAMES: [&str; EFFECT_PIPELINE_COUNT] = [
    "cursor_blend",
    "glow_add",
    "rain_glow",
    "rain_glow_over",
    "fire_add",
    "fire_over",
    "deco_over",
    "deco_add",
    "sprite_over",
    "fx",
    "fx_linear",
    "fx_store",
];

/// Every [`EffectPipeline`], in slot order.
pub const EFFECT_PIPELINES: [EffectPipeline; EFFECT_PIPELINE_COUNT] = [
    EffectPipeline::CursorBlend,
    EffectPipeline::GlowAdd,
    EffectPipeline::RainGlow,
    EffectPipeline::RainGlowOver,
    EffectPipeline::FireAdd,
    EffectPipeline::FireOver,
    EffectPipeline::DecoOver,
    EffectPipeline::DecoAdd,
    EffectPipeline::SpriteOver,
    EffectPipeline::Fx,
    EffectPipeline::FxLinear,
    EffectPipeline::FxStore,
];

/// Demand-driven cache of the effect-only cell pipelines.
#[cfg(wgpu_arm)]
pub(crate) struct EffectPipelines {
    #[cfg(wgpu_arm)]
    shader: wgpu::ShaderModule,
    layouts: CellLayouts,
    fx: Option<Result<FxWgpuLayouts, String>>,
    targets: TargetFormats,
    built: [Option<wgpu::RenderPipeline>; EFFECT_PIPELINE_COUNT],
    builds: u32,
    build_ns: u64,
}

#[cfg(wgpu_arm)]
impl EffectPipelines {
    const fn new(shader: wgpu::ShaderModule, layouts: CellLayouts, targets: TargetFormats) -> Self {
        Self {
            shader,
            layouts,
            fx: None,
            targets,
            built: [const { None }; EFFECT_PIPELINE_COUNT],
            builds: 0,
            build_ns: 0,
        }
    }

    /// The fx rows' layouts, built on first call.
    pub(crate) fn fx_layouts(&mut self, device: &wgpu::Device) -> Result<&FxWgpuLayouts, &str> {
        let uniform_bgl = &self.layouts.uniform_bgl;
        self.fx
            .get_or_insert_with(|| build_fx_layouts(device, uniform_bgl))
            .as_ref()
            .map_err(String::as_str)
    }

    fn ensure(&mut self, device: &wgpu::Device, which: EffectPipeline) {
        if self.built[which as usize].is_some() {
            return;
        }
        let fx_layout = match which {
            EffectPipeline::Fx | EffectPipeline::FxLinear | EffectPipeline::FxStore => {
                if self.fx_layouts(device).is_err() {
                    return;
                }
                self.fx
                    .as_ref()
                    .and_then(|l| l.as_ref().ok())
                    .map(|l| &l.layout)
            }
            _ => None,
        };
        let started = aterm_time::Instant::now();
        let build = || {
            build_effect_pipeline(
                device,
                &self.shader,
                &self.layouts,
                fx_layout,
                self.targets,
                which,
            )
        };
        let pipeline = if fx_layout.is_some() {
            match wgpu_scoped(device, build) {
                Ok(pipeline) => pipeline,
                Err(cause) => {
                    let why = format!(
                        "it refused the {} pipeline: {cause}",
                        EFFECT_PIPELINE_NAMES[which as usize]
                    );
                    note_fx_closed(&why);
                    self.fx = Some(Err(why));
                    return;
                }
            }
        } else {
            build()
        };
        let elapsed = started.elapsed();
        self.build_ns = self
            .build_ns
            .saturating_add(u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX));
        self.builds = self.builds.saturating_add(1);
        crate::startup_probe::record_effect_build(which as usize, elapsed);
        self.built[which as usize] = Some(pipeline);
    }

    #[inline]
    fn get(&self, which: EffectPipeline) -> &wgpu::RenderPipeline {
        self.built[which as usize].as_ref().unwrap_or_else(|| {
            panic!(
                "effect pipeline `{}` was bound without being ensured this frame \
                 — add its stream to GpuRenderer::ensure_effect_pipelines",
                EFFECT_PIPELINE_NAMES[which as usize]
            )
        })
    }

    #[inline]
    fn resident(&self) -> [bool; EFFECT_PIPELINE_COUNT] {
        std::array::from_fn(|i| self.built[i].is_some())
    }
}

#[cfg(wgpu_arm)]
fn ensure_effect_pipelines(
    effects: &mut EffectPipelines,
    device: &wgpu::Device,
    inst: &Instances,
    input: &RenderInput,
    cursor_opaque: bool,
) {
    let want = frame_effect_pipelines(inst, input, cursor_opaque);
    for (slot, wanted) in want.iter().enumerate() {
        if *wanted {
            effects.ensure(device, EFFECT_PIPELINES[slot]);
        }
    }
}

#[cfg(wgpu_arm)]
fn frame_effect_pipelines(
    inst: &Instances,
    input: &RenderInput,
    cursor_opaque: bool,
) -> [bool; EFFECT_PIPELINE_COUNT] {
    let mut want = [false; EFFECT_PIPELINE_COUNT];
    want[EffectPipeline::CursorBlend as usize] = !cursor_opaque;
    want[EffectPipeline::GlowAdd as usize] = !inst.glow_under.is_empty()
        || !inst.glow_add.is_empty()
        || !inst.nova_add.is_empty()
        || fx_gpu::bloom_source_live(input);
    want[EffectPipeline::RainGlow as usize] =
        !inst.glow_halo.is_empty() || !inst.rain_add.is_empty();
    want[EffectPipeline::RainGlowOver as usize] =
        !inst.glow_halo_over.is_empty() || !inst.rain_add_over.is_empty();
    want[EffectPipeline::FireAdd as usize] = !inst.fire_add.is_empty();
    want[EffectPipeline::FireOver as usize] = !inst.fire_over.is_empty();
    want[EffectPipeline::DecoOver as usize] =
        !inst.glyph_halo.is_empty() || !inst.curl.is_empty() || !inst.wdeco_over.is_empty();
    want[EffectPipeline::DecoAdd as usize] = !inst.wdeco_add.is_empty();
    want[EffectPipeline::SpriteOver as usize] = !inst.wallpaper.is_empty();
    let (fx, linear, store) = fx_effect_demand(inst, fx_gpu::under_draws_opaque(input));
    want[EffectPipeline::Fx as usize] = fx;
    want[EffectPipeline::FxLinear as usize] = linear;
    want[EffectPipeline::FxStore as usize] = store;
    want
}

/// Which fx pipelines a frame's fx streams need, as `(fx, fx_linear, fx_store)`: an opaque Exact
/// Under stream always demands `fx_store` (route (1), [`build_frame_plan`]), and `fx` too while a
/// GlowUnder stream keeps it in its own group.
#[cfg(wgpu_arm)]
fn fx_effect_demand(inst: &Instances, under_opaque: bool) -> (bool, bool, bool) {
    let fx = |s: fx_gpu::FxStream| !inst.fx[s as usize].is_empty();
    let under = fx(fx_gpu::FxStream::Under);
    let stored = under_opaque && inst.glow_under.is_empty();
    (
        (under && !stored) || fx(fx_gpu::FxStream::Glow) || fx(fx_gpu::FxStream::Over),
        fx(fx_gpu::FxStream::LinearUnder)
            || fx(fx_gpu::FxStream::KnockUnder)
            || fx(fx_gpu::FxStream::LinearOver),
        under && under_opaque,
    )
}

#[cfg(wgpu_arm)]
fn build_effect_pipeline(
    device: &wgpu::Device,
    shader: &wgpu::ShaderModule,
    layouts: &CellLayouts,
    fx_layout: Option<&wgpu::PipelineLayout>,
    targets: TargetFormats,
    which: EffectPipeline,
) -> wgpu::RenderPipeline {
    let fx = || fx_layout.expect("the fx rows build only once their layout exists");
    let (row, layout) = match which {
        EffectPipeline::CursorBlend => (Pipeline::CursorBlend, &layouts.bg),
        EffectPipeline::GlowAdd => (Pipeline::GlowAdd, &layouts.bg),
        EffectPipeline::RainGlow => (Pipeline::RainGlow, &layouts.bg),
        EffectPipeline::RainGlowOver => (Pipeline::RainGlowOver, &layouts.bg),
        EffectPipeline::FireAdd => (Pipeline::FireAdd, &layouts.bg),
        EffectPipeline::FireOver => (Pipeline::FireOver, &layouts.bg),
        EffectPipeline::DecoOver => (Pipeline::DecoOver, &layouts.glyph),
        EffectPipeline::DecoAdd => (Pipeline::DecoAdd, &layouts.glyph),
        EffectPipeline::SpriteOver => (Pipeline::SpriteOver, &layouts.glyph),
        EffectPipeline::Fx => (Pipeline::Fx, fx()),
        EffectPipeline::FxLinear => (Pipeline::FxLinear, fx()),
        EffectPipeline::FxStore => (Pipeline::FxStore, fx()),
    };
    crate::pipeline_table::build_wgpu(row.spec(), device, shader, layout, targets)
}

#[cfg(wgpu_arm)]
fn build_blit_resources(
    handle: crate::device_layer::DeviceHandle<'_>,
) -> (
    wgpu::ShaderModule,
    wgpu::BindGroupLayout,
    wgpu::PipelineLayout,
    wgpu::Sampler,
    wgpu::Buffer,
) {
    let device = handle.wgpu_device();
    let blit_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("aterm-gpu blit shader"),
        source: wgpu::ShaderSource::Wgsl(wgsl_source(Pipeline::Blit.spec().library).into()),
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
    let blit_sampler = handle
        .create_sampler(
            "aterm-gpu blit nearest",
            crate::device_layer::SamplerKind::NearestClamp,
        )
        .into_wgpu();
    let blit_uniform_buf = handle
        .create_uniform_buffer(
            "aterm-gpu blit invert uniform",
            std::mem::size_of::<BlitUniform>() as u64,
        )
        .into_wgpu();

    (
        blit_shader,
        blit_bgl,
        blit_layout,
        blit_sampler,
        blit_uniform_buf,
    )
}

#[cfg(wgpu_arm)]
fn build_tray_resources(
    handle: crate::device_layer::DeviceHandle<'_>,
) -> (
    wgpu::ShaderModule,
    wgpu::BindGroupLayout,
    wgpu::PipelineLayout,
    wgpu::Sampler,
    wgpu::Buffer,
) {
    let device = handle.wgpu_device();
    let tray_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("aterm-gpu tray shader"),
        source: wgpu::ShaderSource::Wgsl(wgsl_source(Pipeline::Tray.spec().library).into()),
    });
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
    let tray_sampler = handle
        .create_sampler(
            "aterm-gpu tray linear",
            crate::device_layer::SamplerKind::LinearClamp,
        )
        .into_wgpu();
    let tray_uniform_buf = handle
        .create_uniform_buffer(
            "aterm-gpu tray placement uniform",
            std::mem::size_of::<TrayUniform>() as u64,
        )
        .into_wgpu();

    (
        tray_shader,
        tray_bgl,
        tray_layout,
        tray_sampler,
        tray_uniform_buf,
    )
}

impl GpuRenderer {
    /// Acquire a GPU and a CPU font face.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn new(px: f32, theme: Theme) -> Result<Self, String> {
        Self::new_with_family(None, px, theme)
    }

    /// Like [`GpuRenderer::new`], but resolves a configured font family (or font file path) first,
    /// mirroring [`Renderer::from_system_with_family`].
    #[cfg(not(target_arch = "wasm32"))]
    pub fn new_with_family(family: Option<&str>, px: f32, theme: Theme) -> Result<Self, String> {
        let family_owned = family.map(String::from);
        Self::new_loading(family.map(String::from), theme, move || {
            Renderer::from_system_with_family(family_owned.as_deref(), px, theme)
        })
    }

    /// Like [`GpuRenderer::new_with_family`].
    #[cfg(not(target_arch = "wasm32"))]
    pub fn new_with_face(
        face: Option<aterm_render::FaceRef>,
        family: Option<&str>,
        px: f32,
        theme: Theme,
    ) -> Result<Self, String> {
        Self::new_loading(family.map(String::from), theme, move || {
            Renderer::from_system_with_face(face.as_ref(), px, theme)
        })
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn new_loading(
        family: Option<String>,
        theme: Theme,
        load: impl FnOnce() -> Option<Renderer> + Send + 'static,
    ) -> Result<Self, String> {
        let font_handle = std::thread::spawn(move || {
            let font_started = aterm_time::Instant::now();
            let cpu = load();
            crate::startup_probe::record(
                crate::startup_probe::Leg::FontThread,
                font_started.elapsed(),
            );
            cpu
        });
        let ctx = GpuContext::new()?;
        let font_join_started = aterm_time::Instant::now();
        let cpu = font_handle
            .join()
            .map_err(|_| "font-load thread panicked".to_string())?
            .ok_or("no system monospace font")?;
        crate::startup_probe::record(
            crate::startup_probe::Leg::FontJoin,
            font_join_started.elapsed(),
        );
        Self::from_parts(ctx, cpu, family, theme)
    }

    /// Assemble a `GpuRenderer` from an acquired [`GpuContext`] and a CPU [`Renderer`].
    pub fn from_parts(
        ctx: GpuContext,
        cpu: Renderer,
        font_family: Option<String>,
        theme: Theme,
    ) -> Result<Self, String> {
        #[cfg(target_arch = "wasm32")]
        let _ = font_family; // no system font discovery in the browser
        #[cfg(wgpu_arm)]
        let device = &ctx.device;
        let from_parts_started = aterm_time::Instant::now();

        #[cfg(wgpu_arm)]
        let shader = crate::startup_probe::timed(crate::startup_probe::Leg::PipeShader, || {
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("aterm-gpu shader"),
                source: wgpu::ShaderSource::Wgsl(wgsl_source(Pipeline::Bg.spec().library).into()),
            })
        });

        let uniform_atlas_started = aterm_time::Instant::now();
        #[cfg(wgpu_arm)]
        let (uniform_buf, uniform_bgl, uniform_bg) = build_uniform_resources(ctx.device_layer());
        #[cfg(wgpu_arm)]
        let (atlas_bgl, sampler) = build_atlas_resources(ctx.device_layer());
        crate::startup_probe::record(
            crate::startup_probe::Leg::PipeUniformAtlas,
            uniform_atlas_started.elapsed(),
        );
        #[cfg(wgpu_arm)]
        let pipeline_targets = ctx.pipeline_targets();
        #[cfg(wgpu_arm)]
        let cell_layouts = build_cell_layouts(device, &uniform_bgl, &atlas_bgl);
        #[cfg(wgpu_arm)]
        let (bg_pipeline, glyph_pipeline, color_glyph_pipeline) =
            crate::startup_probe::timed(crate::startup_probe::Leg::PipeCell, || {
                build_cell_pipelines(device, &shader, &cell_layouts, pipeline_targets)
            });
        #[cfg(wgpu_arm)]
        let effects = EffectPipelines::new(shader, cell_layouts, pipeline_targets);

        #[cfg(wgpu_arm)]
        let (blit_shader, blit_bgl, blit_layout, blit_sampler, blit_uniform_buf) =
            crate::startup_probe::timed(crate::startup_probe::Leg::PipeBlit, || {
                build_blit_resources(ctx.device_layer())
            });

        #[cfg(wgpu_arm)]
        let (tray_shader, tray_bgl, tray_layout, tray_sampler, tray_uniform_buf) =
            crate::startup_probe::timed(crate::startup_probe::Leg::PipeTray, || {
                build_tray_resources(ctx.device_layer())
            });

        #[cfg(wgpu_arm)]
        let (bloom_bgl, bloom_sampler, bloom_pipeline, bloom_uniform_buf) =
            crate::startup_probe::timed(crate::startup_probe::Leg::PipeBloom, || {
                build_bloom_resources(ctx.device_layer(), ctx.pipeline_targets())
            });

        #[cfg(wgpu_arm)]
        let vbufs =
            crate::startup_probe::timed(crate::startup_probe::Leg::PipeVertexBuffers, || {
                VertexBuffers::new(ctx.device_layer())
            });

        let pipe_tail_started = aterm_time::Instant::now();
        let renderer = Self {
            ctx,
            cpu,
            #[cfg(not(target_arch = "wasm32"))]
            font_family,
            theme,
            #[cfg(wgpu_arm)]
            uniform_buf,
            #[cfg(wgpu_arm)]
            uniform_bg,
            #[cfg(wgpu_arm)]
            uniform_written: None,
            #[cfg(wgpu_arm)]
            atlas_bgl,
            #[cfg(wgpu_arm)]
            sampler,
            #[cfg(wgpu_arm)]
            bg_pipeline,
            #[cfg(wgpu_arm)]
            glyph_pipeline,
            #[cfg(wgpu_arm)]
            color_glyph_pipeline,
            #[cfg(wgpu_arm)]
            effects,
            #[cfg(wgpu_arm)]
            bloom_bgl,
            #[cfg(wgpu_arm)]
            bloom_sampler,
            #[cfg(wgpu_arm)]
            bloom_pipeline,
            #[cfg(wgpu_arm)]
            bloom_uniform_buf,
            enable_bloom: true,
            hdr_glow: false,
            backdrop_margins: false,
            #[cfg(wgpu_arm)]
            hdr_glow_pipeline: None,
            #[cfg(wgpu_arm)]
            hdr_glow_uniform_buf: None,
            #[cfg(wgpu_arm)]
            hdr_glow_bg: None,
            #[cfg(wgpu_arm)]
            glow_boost_bgl: None,
            #[cfg(wgpu_arm)]
            sdr_glow_pipeline: None,
            sdr_glow_boost: 0.0,
            deco_atlas: None,
            sprite_textures: SpriteTextureLookup::default(),
            bloom_strength: BLOOM_STRENGTH,
            bloom_radius: BLOOM_RADIUS,
            #[cfg(wgpu_arm)]
            shimmer: None,
            enable_shimmer: true,
            shimmer_epoch: aterm_time::Instant::now(),
            shimmer_phase_pin: None,
            #[cfg(wgpu_arm)]
            blit_shader,
            #[cfg(wgpu_arm)]
            blit_bgl,
            #[cfg(wgpu_arm)]
            blit_layout,
            #[cfg(wgpu_arm)]
            blit_sampler,
            #[cfg(wgpu_arm)]
            blit_uniform_buf,
            #[cfg(wgpu_arm)]
            blit_uniform_written: None,
            #[cfg(wgpu_arm)]
            blit_pipelines: HashMap::new(),
            #[cfg(wgpu_arm)]
            tray_shader,
            #[cfg(wgpu_arm)]
            tray_bgl,
            #[cfg(wgpu_arm)]
            tray_layout,
            #[cfg(wgpu_arm)]
            tray_sampler,
            #[cfg(wgpu_arm)]
            tray_uniform_buf,
            #[cfg(wgpu_arm)]
            tray_pipelines: HashMap::new(),
            #[cfg(wgpu_arm)]
            tray_scrim: None,
            #[cfg(wgpu_arm)]
            cached_surface_format: None,
            mono_res: None,
            color_res: None,
            resident_keys: FxHashSet::default(),
            atlas_tex_creations: 0,
            #[cfg(wgpu_arm)]
            vbufs,
            #[cfg(target_os = "macos")]
            metal_armed: crate::metal_backend_selected(),
            #[cfg(target_os = "macos")]
            metal_mint_error: None,
            #[cfg(target_os = "macos")]
            metal_arm: None,
            #[cfg(target_os = "macos")]
            last_frame_arm_metal: false,
            #[cfg(target_os = "macos")]
            metal_pending_band_shift: None,
            #[cfg(any(target_os = "macos", gl_arm))]
            image_plane_epoch: 0,
            #[cfg(any(target_os = "macos", gl_arm))]
            atlas_epoch: 0,
            #[cfg(gl_arm)]
            gl_armed: true,
            #[cfg(gl_arm)]
            gl_arm: None,
            #[cfg(gl_arm)]
            gl_mint_error: None,
            #[cfg(all(gl_arm, not(target_arch = "wasm32")))]
            gl_window_system: None,
            #[cfg(gl_arm)]
            last_frame_arm_gl: false,
            #[cfg(gl_arm)]
            gl_inject_loss_after: aterm_types::dev_seam!("ATERM_GL_INJECT_LOSS")
                .and_then(|v| v.to_str()?.parse::<u64>().ok())
                .filter(|n| *n > 0),
            #[cfg(target_os = "macos")]
            metal_inject_loss_after: aterm_types::dev_seam!("ATERM_METAL_INJECT_LOSS")
                .and_then(|v| v.to_str()?.parse::<u64>().ok())
                .filter(|n| *n > 0),
            inst: Instances::default(),
            fx: fx_gpu::FxGpu::default(),
            #[cfg(wgpu_arm)]
            fx_wgpu: FxWgpu::default(),
            #[cfg(all(target_os = "macos", any(test, debug_assertions)))]
            fx_palette_shared_seam: false,
            #[cfg(all(target_os = "macos", any(test, debug_assertions)))]
            fx_fail_seam: false,
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
            bloom_quad_scratch: Vec::new(),
            key_scratch: Vec::new(),
            glyph_win_scratch: Vec::new(),
            overshoot_surprise: false,
            image_order_scratch: Vec::new(),
            image_seen_scratch: FxHashMap::default(),
            image_placements_scratch: FxHashMap::default(),
            scissor_taken: 0,
            scroll_rescues: 0,
            full_repaints: 0,
            tray_uploads: 0,
            tray_upload_bytes: 0,
            last_instances: 0,
            last_bg_instances: 0,
            last_frame_passes: 0,
            frame_shape: FrameShape::default(),
            ablation: aterm_types::dev_seam!("ATERM_GPU_ABLATE_UNORM")
                .and_then(|v| v.to_str().map(UnormAblation::from_seam))
                .unwrap_or_default(),
            unorm_route: UnormRoute::initial(
                aterm_types::dev_seam!("ATERM_GPU_UNORM_ROUTE").as_deref(),
            ),
            #[cfg(all(target_os = "macos", any(test, debug_assertions)))]
            fetch_probe_seam: None,
            #[cfg(target_os = "macos")]
            first_frame: &crate::metal::warm::FIRST_FRAME,
            #[cfg(target_os = "macos")]
            metal_warm_wanted: false,
            #[cfg(target_os = "macos")]
            metal_on_screen: false,
            last_gpu_time_serial: None,
            #[cfg(wgpu_arm)]
            wgpu_timer: None,
            frame_plan: Vec::new(),
            last_frame_load: crate::device_layer::FrameLoad::Load,
            last_frame_scissor: None,
        };
        crate::startup_probe::record(
            crate::startup_probe::Leg::PipeTail,
            pipe_tail_started.elapsed(),
        );
        crate::startup_probe::record(
            crate::startup_probe::Leg::PipeTotal,
            from_parts_started.elapsed(),
        );
        Ok(renderer)
    }

    /// Install an already-admitted face and publish its configured family as one transaction.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn install_prepared_font(
        &mut self,
        family: Option<String>,
        renderer: Renderer,
        theme: Theme,
    ) {
        self.font_family = family;
        self.set_face(renderer, theme);
    }

    /// Forwards [`fallback_parse_pending`](Renderer::fallback_parse_pending).
    #[inline]
    pub fn fallback_parse_pending(&mut self) -> bool {
        self.cpu.fallback_parse_pending()
    }

    /// Forwards [`take_missing_font_classes`](Renderer::take_missing_font_classes).
    #[inline]
    pub fn take_missing_font_classes(&mut self) -> u8 {
        self.cpu.take_missing_font_classes()
    }

    /// Forward of the inner CPU rasterizer's
    /// [`set_runtime_font_discovery`](Renderer::set_runtime_font_discovery).
    #[inline]
    pub fn set_runtime_font_discovery(&mut self, enabled: bool) {
        self.raster_changed(|cpu| cpu.set_runtime_font_discovery(enabled));
    }

    /// Test/debug forward of [`Renderer::debug_block_on_lazy_fallbacks`], so CPU-vs-GPU
    /// comparisons are deterministic.
    #[doc(hidden)]
    #[inline]
    pub fn debug_block_on_lazy_fallbacks(&mut self) {
        self.cpu.debug_block_on_lazy_fallbacks();
    }

    /// A test's negative control: draw companions under the text without the knock-out
    /// (`aterm_render::Renderer::set_knock_out`), on the glyph-knock stream as on the CPU twin.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub fn set_knock_out(&mut self, on: bool) {
        self.cpu.set_knock_out(on);
    }

    /// Swap in an already-built CPU face and theme in place. The face keeps its own
    /// variation requests (`font_weight`, `font_variation`): a prepared face was built
    /// with the config it installs, and a rebuild carries the live ones
    /// ([`Renderer::rebuild_from_admitted`]).
    pub fn set_face(&mut self, cpu: Renderer, theme: Theme) {
        let prior_shaping = self.cpu.text_shaping().clone();
        let prior_blending = self.cpu.text_blending();
        let prior_thicken = self.cpu.font_thicken();
        let prior_stem_gamma = self.cpu.stem_gamma();
        self.cpu = cpu;
        self.cpu.set_text_shaping(prior_shaping);
        self.cpu.set_text_blending(prior_blending);
        self.cpu.set_font_thicken(prior_thicken);
        self.cpu.set_stem_gamma(prior_stem_gamma);
        self.theme = theme;
        self.resident_keys.clear();
        self.mono_res = None;
        self.color_res = None;
    }

    /// Install the text-shaping config (ligatures, OpenType features) and drop the atlas.
    #[inline]
    pub fn set_text_shaping(&mut self, shaping: aterm_render::TextShapingConfig) {
        self.raster_changed(|cpu| cpu.set_text_shaping(shaping));
    }

    /// Configured primary-face `font_features` tags the active font cannot apply.
    #[must_use]
    #[inline]
    pub fn unsupported_user_feature_tags(&self) -> Vec<[u8; 4]> {
        self.cpu.unsupported_user_feature_tags()
    }

    /// Install the coverage blend mode (config `text_blending`) on the wrapped CPU face, which
    /// `encode_frame` reads for the shader uniform.
    #[inline]
    pub fn set_text_blending(&mut self, mode: aterm_render::TextBlending) {
        self.cpu.set_text_blending(mode);
    }

    /// The active coverage blend mode (off the wrapped CPU face).
    #[must_use]
    #[inline]
    pub const fn text_blending(&self) -> aterm_render::TextBlending {
        self.cpu.text_blending()
    }

    /// Set macOS `font_thicken` on the wrapped CPU face and drop the atlas when it changed.
    pub fn set_font_thicken(&mut self, on: bool) {
        self.raster_changed(|cpu| {
            let changed = cpu.font_thicken() != on;
            if changed {
                cpu.set_font_thicken(on);
            }
            changed
        });
    }

    /// Install the variable-font config (`font_variation`, `font_weight`) and rebuild the atlas.
    pub fn set_font_variations(&mut self, requests: &[(u32, f32)], dark_nudge: f32) {
        self.raster_changed(|cpu| cpu.set_font_variations(requests, dark_nudge));
    }

    /// Set `stem_gamma` on the wrapped CPU face and drop the atlas on a change.
    pub fn set_stem_gamma(&mut self, gamma: f32) {
        self.raster_changed(|cpu| {
            let before = cpu.stem_gamma();
            cpu.set_stem_gamma(gamma);
            (cpu.stem_gamma() - before).abs() > f32::EPSILON
        });
    }

    /// Set the Linux/Windows grid-fitting mode (`font_hinting`) and drop the atlas on a change.
    pub fn set_font_hinting(&mut self, mode: &str) {
        self.raster_changed(|cpu| cpu.set_font_hinting(mode));
    }

    /// Record the Linux subpixel-RGB mode (`font_subpixel`) on the wrapped CPU face.
    pub fn set_font_subpixel(&mut self, mode: &str) {
        let _ = self.cpu.set_font_subpixel(mode);
    }

    /// Replace the theme live on the GPU state and the CPU face without a rebuild.
    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
        self.raster_changed(|cpu| {
            let coords_before: Option<Vec<(u32, f32)>> = cpu.variation_coords().map(<[_]>::to_vec);
            cpu.set_theme(theme);
            cpu.variation_coords() != coords_before.as_deref()
        });
    }

    /// Selected-text foreground (theme `selectionForeground`), or `None` for the contrast floor.
    #[inline]
    pub fn set_selection_fg(&mut self, fg: Option<u32>) {
        self.cpu.set_selection_fg(fg);
    }

    /// Per-cell minimum contrast ratio (xterm's `minimumContrastRatio`; `<= 1.0` is off).
    #[inline]
    pub fn set_minimum_contrast(&mut self, ratio: f32) {
        self.cpu.set_minimum_contrast(ratio);
    }

    /// Default-background opacity (0..=1; `1.0` = opaque, the default).
    #[inline]
    pub fn set_background_opacity(&mut self, opacity: f32) {
        self.cpu.set_background_opacity(opacity);
    }

    /// The configured default-background opacity (off the wrapped CPU face).
    #[must_use]
    #[inline]
    pub const fn background_opacity(&self) -> f32 {
        self.cpu.background_opacity()
    }

    /// Cursor fill opacity (0..=1; `1.0` = opaque fill plus block cut-out, the default).
    #[inline]
    pub fn set_cursor_opacity(&mut self, opacity: f32) {
        self.cpu.set_cursor_opacity(opacity);
    }

    /// The configured cursor-fill opacity (off the wrapped CPU face).
    #[must_use]
    #[inline]
    pub const fn cursor_opacity(&self) -> f32 {
        self.cpu.cursor_opacity()
    }

    /// Mark the pane unfocused (`true`) or focused for selection theming.
    #[inline]
    pub fn set_selection_inactive(&mut self, inactive: bool) {
        self.cpu.set_selection_inactive(inactive);
    }

    /// Inactive (unfocused) selection bg (0x00RRGGBB), or `None` to derive it from the theme.
    #[inline]
    pub fn set_selection_inactive_bg(&mut self, bg: Option<u32>) {
        self.cpu.set_selection_inactive_bg(bg);
    }

    /// Re-rasterize at a new pixel size (DPI change).
    #[inline]
    pub fn set_px(&mut self, px: f32) {
        self.raster_changed(|cpu| cpu.set_px(px));
    }

    /// Per-window size switch.
    #[inline]
    pub fn activate_px(&mut self, px: f32) {
        self.cpu.activate_px(px);
    }

    /// Cell geometry `(cell_w, cell_h, baseline)` at `px`, from the CPU face.
    #[inline]
    pub fn cell_geometry(&self, px: f32) -> (usize, usize, i32) {
        self.cpu.cell_geometry(px)
    }

    /// The band's `(head, foot)` room at the active size, from the wrapped CPU face's one memo.
    #[inline]
    pub fn band_room(&mut self) -> (usize, usize) {
        self.cpu.band_room()
    }

    /// Pure per-window decoration bands at `px`, delegated like [`Self::cell_geometry`].
    #[inline]
    pub fn deco_metrics_for(&self, px: f32) -> aterm_render::deco::DecoMetrics {
        self.cpu.deco_metrics_for(px)
    }

    /// Set a broad-coverage fallback face from font bytes and invalidate the atlas.
    pub fn set_fallback_font_bytes(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.raster_changed(|cpu| cpu.set_fallback_bytes(bytes))
    }

    /// Append a fallback face, so several scripts can find coverage, and invalidate the atlas.
    pub fn add_fallback_font_bytes(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.raster_changed(|cpu| cpu.add_fallback_bytes(bytes))
    }

    /// Set a colour-emoji (sbix) face from font bytes and invalidate the atlas.
    pub fn set_emoji_font_bytes(&mut self, bytes: Vec<u8>) -> Result<(), String> {
        self.raster_changed(|cpu| cpu.set_color_font_bytes(bytes))
    }

    /// Set a real bold face (`FaceId::BoldPrimary`) so SGR bold is not synthetic.
    pub fn set_bold_font_bytes(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.set_bold_face(bytes, 0)
    }

    /// [`Self::set_bold_font_bytes`] for face `index` of `bytes`.
    pub fn set_bold_face(&mut self, bytes: &[u8], index: u32) -> Result<(), String> {
        self.raster_changed(|cpu| cpu.set_bold_face(bytes, index))
    }

    /// Set a symbol fallback face (config `symbol_font`) from font bytes and invalidate the atlas.
    pub fn set_symbol_font_bytes(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.raster_changed(|cpu| cpu.set_symbol_fallback_bytes(bytes))
    }

    /// Set a real styled face and invalidate the atlas.
    pub fn set_styled_font_bytes(&mut self, slot: usize, bytes: &[u8]) -> Result<(), String> {
        self.set_styled_face(slot, bytes, 0)
    }

    /// [`Self::set_styled_font_bytes`] for face `index` of `bytes`.
    pub fn set_styled_face(&mut self, slot: usize, bytes: &[u8], index: u32) -> Result<(), String> {
        self.raster_changed(|cpu| cpu.set_styled_face(slot, bytes, index))
    }

    /// Enable/disable synthetic bold/italic (`font_synthetic_style`).
    pub fn set_synthetic_styles(&mut self, on: bool) {
        self.raster_changed(|cpu| cpu.set_synthetic_styles(on));
    }

    /// Install the config fallback chain (`fallback_fonts`), ordered config before built-ins.
    pub fn set_config_fallback_fonts(&mut self, paths: &[String]) {
        self.raster_changed(|cpu| cpu.set_config_fallback_fonts(paths));
    }

    /// Install the config symbol-fallback font (`symbol_font`).
    pub fn set_config_symbol_font(&mut self, path: Option<&str>) {
        self.raster_changed(|cpu| cpu.set_config_symbol_font(path));
    }

    /// Install the config colour-emoji font (`emoji_font`).
    pub fn set_config_emoji_font(&mut self, path: Option<&str>) {
        self.raster_changed(|cpu| cpu.set_config_emoji_font(path));
    }

    /// Install the display layer (`display_font`), invalidating the atlas when it changed.
    pub fn set_display_faces(&mut self, ids: &[&str]) -> Result<(), String> {
        let before = self.cpu.display_faces();
        self.cpu.set_display_faces(ids)?;
        if self.cpu.display_faces() != before {
            self.invalidate_atlas();
        }
        Ok(())
    }

    /// Swap the primary face and invalidate the atlas.
    pub fn set_primary_font_bytes(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.raster_changed(|cpu| cpu.set_primary_font(bytes))
    }

    /// Scale the cell box height without changing glyph px.
    pub fn set_line_height(&mut self, scale: f32) {
        self.raster_changed(|cpu| cpu.set_line_height(scale));
    }

    /// Shift the baseline by a signed px delta (`adjust_baseline`).
    pub fn set_adjust_baseline(&mut self, px: i32) {
        self.raster_changed(|cpu| {
            let before = cpu.adjust_baseline();
            cpu.set_adjust_baseline(px);
            cpu.adjust_baseline() != before
        });
    }

    /// Underline position/thickness adjustments (`adjust_underline_*`).
    #[inline]
    pub fn set_adjust_underline(&mut self, pos_px: i32, thick_px: i32) {
        self.cpu.set_adjust_underline(pos_px, thick_px);
    }

    /// Descender ink-skip toggle, read by both paths from the CPU face.
    #[inline]
    pub fn set_underline_skip_descenders(&mut self, on: bool) {
        self.cpu.set_underline_skip_descenders(on);
    }

    /// The CPU face's resolved primary font `(bytes, collection index)` for GUI chrome text.
    #[must_use]
    #[inline]
    pub fn chrome_primary_face(&self) -> Option<(std::sync::Arc<[u8]>, u32)> {
        self.cpu.chrome_primary_face()
    }

    /// Immutable source-path metadata for the wrapped primary generation.
    #[must_use]
    #[inline]
    pub fn primary_source_path(&self) -> Option<&str> {
        self.cpu.primary_source_path()
    }

    /// Arc-backed source snapshot of the wrapped font generation.
    #[must_use]
    #[inline]
    pub fn admitted_font_sources(&self) -> aterm_render::AdmittedFontSources {
        self.cpu.admitted_font_sources()
    }

    /// Whether the wrapped CPU generation is sealed against later font I/O.
    #[must_use]
    #[inline]
    pub const fn admitted_font_sources_sealed(&self) -> bool {
        self.cpu.admitted_font_sources_sealed()
    }

    /// Worker-only settlement of every path-backed face in the wrapped generation, for hosts that
    /// did not seal before installing.
    #[inline]
    pub fn seal_admitted_font_sources(&mut self) -> aterm_render::AdmittedFontSources {
        self.cpu.seal_admitted_font_sources()
    }

    /// Pre-rasterize regular and bold printable ASCII so the first atlas build is a copy.
    #[inline]
    pub fn prewarm_ascii(&mut self) {
        self.cpu.prewarm_ascii();
    }

    /// Diagnostic.
    #[doc(hidden)]
    #[must_use]
    #[inline]
    pub fn glyph_cache_len(&self) -> usize {
        self.cpu.glyph_cache_len()
    }

    /// Rebuild the font at `px`/`theme` from the sealed sources, keeping the device, pipelines and
    /// surfaces, and invalidating the atlases once.
    pub fn rebuild_font_from_admitted(&mut self, px: f32, theme: Theme) -> Result<(), String> {
        let cpu = self.cpu.rebuild_from_admitted(px, theme)?;
        self.set_face(cpu, theme);
        Ok(())
    }

    /// Rebuild this renderer from the sealed font sources on a fresh GPU context.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn rebuild_on_fresh_context(&self, px: f32) -> Result<Self, String> {
        let cpu = self.cpu.rebuild_from_admitted(px, self.theme)?;
        let ctx = GpuContext::new()?;
        Self::from_parts(ctx, cpu, self.font_family.clone(), self.theme)
    }

    /// What the wrapped sealed generation draws each style with.
    #[must_use]
    #[inline]
    pub fn drawn_faces(&self) -> Option<&aterm_render::DrawnFaces> {
        self.cpu.drawn_faces()
    }

    /// The wrapped CPU face's real bold sibling of the primary family for the GUI chrome.
    #[must_use]
    #[inline]
    pub fn chrome_bold_face(&mut self) -> Option<(std::sync::Arc<[u8]>, u32)> {
        self.cpu.chrome_bold_face()
    }

    /// Fork the wrapped CPU font engine for a renderer-native semantic surface.
    #[must_use]
    #[inline]
    pub fn fork_semantic_surface(&self, px: f32, theme: Theme) -> Option<aterm_render::Renderer> {
        self.cpu.fork_semantic_surface(px, theme)
    }

    #[inline]
    fn raster_changed<R: RasterEdit>(&mut self, edit: impl FnOnce(&mut Renderer) -> R) -> R {
        let out = edit(&mut self.cpu);
        if out.dropped_rasters() {
            self.invalidate_atlas();
        }
        out
    }

    #[inline]
    fn invalidate_atlas(&mut self) {
        self.resident_keys.clear();
        self.mono_res = None;
        self.color_res = None;
    }

    /// Test/diagnostic: `render_input_cached` calls that hit the dirty gate.
    #[doc(hidden)]
    #[must_use]
    #[inline]
    pub const fn gate_hits(&self) -> u64 {
        self.gate_hits
    }

    /// Test/diagnostic: `render_input_cached` calls that missed the dirty gate.
    #[doc(hidden)]
    #[must_use]
    #[inline]
    pub const fn gate_misses(&self) -> u64 {
        self.gate_misses
    }

    /// Test/diagnostic.
    #[doc(hidden)]
    #[must_use]
    pub fn allocator_reserved(&self) -> Option<(u64, u64, Vec<u64>)> {
        #[cfg(not(wgpu_arm))]
        {
            None
        }
        #[cfg(wgpu_arm)]
        {
            let report = self.ctx.device.generate_allocator_report()?;
            Some((
                report.total_allocated_bytes,
                report.total_reserved_bytes,
                report.blocks.iter().map(|b| b.size).collect(),
            ))
        }
    }

    /// Test/diagnostic.
    #[doc(hidden)]
    #[must_use]
    #[inline]
    pub const fn scissor_taken(&self) -> u64 {
        self.scissor_taken
    }

    /// Test/diagnostic.
    #[doc(hidden)]
    #[must_use]
    #[inline]
    pub const fn scroll_rescues(&self) -> u64 {
        self.scroll_rescues
    }

    /// Test/diagnostic.
    #[doc(hidden)]
    #[must_use]
    #[cfg(target_os = "macos")]
    #[inline]
    pub fn metal_encode_inline_waits(&self) -> u64 {
        self.metal_arm
            .as_ref()
            .map_or(0, |c| c.live_ref().encode_inline_waits.get())
    }

    /// Test/diagnostic.
    #[doc(hidden)]
    #[must_use]
    #[cfg(target_os = "macos")]
    #[inline]
    pub fn metal_encode_band_shifts(&self) -> u64 {
        self.metal_arm
            .as_ref()
            .map_or(0, |c| c.live_ref().encode_band_shifts.get())
    }

    /// Test seam.
    #[cfg(all(target_os = "macos", test))]
    pub(crate) fn metal_fx_hazard(&self) -> FxHazard {
        self.metal_arm
            .as_ref()
            .map_or_else(FxHazard::default, |c| c.live_ref().fx_hazard)
    }

    /// Test seam, the fx hazard drill's control.
    #[doc(hidden)]
    #[cfg(all(target_os = "macos", any(test, debug_assertions)))]
    pub const fn set_fx_palette_shared_for_test(&mut self, on: bool) {
        self.fx_palette_shared_seam = on;
    }

    /// Test seam.
    #[doc(hidden)]
    #[cfg(all(target_os = "macos", any(test, debug_assertions)))]
    pub const fn fail_fx_staging_for_test(&mut self, on: bool) {
        self.fx_fail_seam = on;
    }

    /// Test seam.
    #[doc(hidden)]
    #[cfg(any(test, debug_assertions))]
    pub fn drop_fx_arm_residency_for_test(&mut self) {
        #[cfg(target_os = "macos")]
        if let Some(c) = self.metal_arm.as_mut() {
            c.live_mut().fx_tex = [None, None];
        }
        #[cfg(wgpu_arm)]
        {
            self.fx_wgpu = FxWgpu::default();
        }
        #[cfg(gl_arm)]
        if let Some(live) = self.gl_arm.as_mut() {
            live.drop_fx_residency();
        }
    }

    /// Test/diagnostic: `present_input` frames that did a full repaint.
    #[doc(hidden)]
    #[must_use]
    #[inline]
    pub const fn full_repaints(&self) -> u64 {
        self.full_repaints
    }

    /// Test/diagnostic.
    #[doc(hidden)]
    #[must_use]
    #[inline]
    pub fn overshoot_rule_widened(&self) -> bool {
        self.cpu.overshoot_rule_widened()
    }

    /// The `PresentPrev` and `GpuGateCache` origin stamps the `AsymmetricPadLayout` anchors read.
    #[doc(hidden)]
    #[must_use]
    #[inline]
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

    /// Test/diagnostic: settings-card uploads that ran; an unchanged card skips.
    #[doc(hidden)]
    #[must_use]
    #[inline]
    pub const fn tray_uploads(&self) -> u64 {
        self.tray_uploads
    }

    /// Test/diagnostic.
    #[doc(hidden)]
    #[must_use]
    #[inline]
    pub const fn tray_upload_bytes(&self) -> u64 {
        self.tray_upload_bytes
    }

    /// Test/diagnostic: total instances built in the last encoded frame.
    #[doc(hidden)]
    #[must_use]
    #[inline]
    pub const fn last_instances(&self) -> usize {
        self.last_instances
    }

    /// Test/diagnostic.
    #[doc(hidden)]
    #[must_use]
    #[inline]
    pub const fn last_bg_instances(&self) -> usize {
        self.last_bg_instances
    }

    /// Test/diagnostic.
    #[doc(hidden)]
    #[must_use]
    #[inline]
    pub const fn last_frame_passes(&self) -> u32 {
        self.last_frame_passes
    }

    /// The latest drawn frame's shape.
    #[must_use]
    #[inline]
    pub const fn frame_shape(&self) -> FrameShape {
        self.frame_shape
    }

    /// The pass-economy ablation frame plans run under ([`UnormAblation`]).
    #[must_use]
    #[inline]
    pub const fn unorm_ablation(&self) -> UnormAblation {
        self.ablation
    }

    /// Set the pass-economy ablation (the `ATERM_GPU_ABLATE_UNORM` seam's programmatic twin).
    #[cfg(any(test, debug_assertions, feature = "dev-seams"))]
    #[inline]
    pub const fn set_unorm_ablation(&mut self, ablation: UnormAblation) {
        self.ablation = ablation;
    }

    /// The route frame plans take where the device allows it ([`UnormRoute::Fetch`] by default);
    /// [`FrameShape::route`] is the route the latest frame took.
    #[must_use]
    #[inline]
    pub const fn unorm_route(&self) -> UnormRoute {
        self.unorm_route
    }

    /// Choose the route frame plans take where the device allows it (the `ATERM_GPU_UNORM_ROUTE`
    /// seam's programmatic twin): [`UnormRoute::Split`] keeps a fetch-capable device on the split.
    #[cfg(any(test, debug_assertions, feature = "dev-seams"))]
    #[inline]
    pub const fn set_unorm_route(&mut self, route: UnormRoute) {
        self.unorm_route = route;
    }

    /// The framebuffer-fetch verdict: the Metal arm's device probe, [`FetchProbe::Unsupported`] on
    /// any other arm, and `None` until the Metal arm has minted and a frame that draws Unorm-law
    /// light settled it.
    #[must_use]
    pub fn fetch_probe(&self) -> Option<FetchProbe> {
        #[cfg(target_os = "macos")]
        if self.metal_armed {
            #[cfg(any(test, debug_assertions))]
            if self.fetch_probe_seam.is_some() {
                return self.fetch_probe_seam;
            }
            return self.metal_arm.as_ref().and_then(|c| c.live_ref().fetch);
        }
        Some(FetchProbe::Unsupported)
    }

    /// Stand `verdict` in for the Metal arm's fetch verdict (`None`: the device's own), so a test
    /// can drive a device that failed, refused or lacks the probe.
    #[cfg(all(target_os = "macos", any(test, debug_assertions)))]
    #[inline]
    pub const fn set_fetch_probe_seam(&mut self, verdict: Option<FetchProbe>) {
        self.fetch_probe_seam = verdict;
    }

    /// The verdict [`Self::set_fetch_probe_seam`] stands in, `None` in a release build.
    #[cfg(target_os = "macos")]
    #[inline]
    const fn fetch_seam(&self) -> Option<FetchProbe> {
        #[cfg(any(test, debug_assertions))]
        return self.fetch_probe_seam;
        #[cfg(not(any(test, debug_assertions)))]
        None
    }

    /// Mint the Metal arm against `latch` rather than the process's first-frame warm, so a test
    /// that runs its own warm never holds another test's frames on it.
    #[cfg(all(target_os = "macos", test))]
    pub(crate) const fn set_first_frame_latch_for_test(
        &mut self,
        latch: &'static crate::metal::warm::WarmLatch,
    ) {
        self.first_frame = latch;
    }

    /// The route the Metal arm gives a frame (`unorm`: it draws Unorm-law light):
    /// [`UnormRoute::Fetch`] only where `want` asks for it and the verdict (`seam`'s, else the
    /// device's) is [`FetchProbe::Passed`]. Only a frame with that light settles a verdict
    /// ([`MetalArmLive::fetch_verdict`]), so a session that draws none never probes.
    #[cfg(target_os = "macos")]
    fn metal_unorm_route(
        arm: Option<&mut MetalArmCell>,
        want: UnormRoute,
        seam: Option<FetchProbe>,
        unorm: bool,
    ) -> UnormRoute {
        if want != UnormRoute::Fetch {
            return UnormRoute::Split;
        }
        let verdict = seam.or_else(|| {
            let live = arm?.live_mut();
            if unorm {
                live.fetch_verdict()
            } else {
                live.fetch
            }
        });
        if verdict == Some(FetchProbe::Passed) {
            UnormRoute::Fetch
        } else {
            UnormRoute::Split
        }
    }

    /// The `crate::gpu_time` serial of the latest present this renderer timed.
    #[must_use]
    #[inline]
    pub const fn last_gpu_time_serial(&self) -> Option<u64> {
        self.last_gpu_time_serial
    }

    /// Whether this renderer's presents can report GPU time once a sink is installed.
    #[must_use]
    pub fn gpu_time_supported(&self) -> bool {
        #[cfg(target_os = "macos")]
        let metal = self.metal_armed;
        #[cfg(not(target_os = "macos"))]
        let metal = false;
        #[cfg(gl_arm)]
        let gl = self.gl_armed;
        #[cfg(not(gl_arm))]
        let gl = false;
        #[cfg(wgpu_arm)]
        let timestamps = self
            .ctx
            .device
            .features()
            .contains(wgpu::Features::TIMESTAMP_QUERY);
        #[cfg(not(wgpu_arm))]
        let timestamps = false;
        metal || (!gl && timestamps)
    }

    /// Settle what GPU-time readbacks have landed without presenting again.
    pub fn poll_gpu_time(&mut self) {
        #[cfg(wgpu_arm)]
        if let Some(timer) = self.wgpu_timer.as_mut() {
            timer.harvest(&crate::wgpu_timer::WgpuClock {
                device: &self.ctx.device,
                queue: &self.ctx.queue,
            });
        }
    }

    #[cfg(wgpu_arm)]
    fn wgpu_gpu_time_begin(&mut self) {
        self.last_gpu_time_serial = None;
        if !crate::gpu_time::sink_installed() {
            return;
        }
        if self.wgpu_timer.is_none() {
            self.wgpu_timer =
                crate::wgpu_timer::WgpuGpuTimer::new(&self.ctx.device, &self.ctx.queue);
        }
        if let Some(timer) = self.wgpu_timer.as_mut() {
            self.last_gpu_time_serial = timer.begin(&crate::wgpu_timer::WgpuClock {
                device: &self.ctx.device,
                queue: &self.ctx.queue,
            });
        }
    }

    #[cfg(wgpu_arm)]
    fn wgpu_gpu_time_finish(&mut self) {
        if let Some(timer) = self.wgpu_timer.as_mut() {
            timer.finish(&crate::wgpu_timer::WgpuClock {
                device: &self.ctx.device,
                queue: &self.ctx.queue,
            });
        }
    }

    #[cfg(wgpu_arm)]
    #[inline]
    fn wgpu_pass_timestamps(&self) -> Option<wgpu::RenderPassTimestampWrites<'_>> {
        self.wgpu_timer.as_ref()?.pass_writes()
    }

    /// Cell size (pixels), straight from the CPU renderer so geometry matches.
    #[inline]
    pub const fn cell_size(&self) -> (usize, usize) {
        self.cpu.cell_size()
    }

    /// Interior padding (px per edge), delegated to the inner CPU renderer.
    #[inline]
    pub const fn pad(&self) -> usize {
        self.cpu.pad()
    }

    /// Set the interior padding on the inner CPU renderer (read each frame).
    #[inline]
    pub fn set_pad(&mut self, pad: usize) {
        self.cpu.set_pad(pad);
    }

    /// Chrome headroom (px above the padded grid), delegated to the inner CPU renderer.
    #[inline]
    pub const fn head(&self) -> usize {
        self.cpu.head()
    }

    /// Set the chrome headroom on the inner CPU renderer.
    #[inline]
    pub fn set_head(&mut self, head_px: usize) {
        self.cpu.set_head(head_px);
    }

    /// Effective top interior pad (px), delegated to the inner CPU renderer.
    #[inline]
    pub fn pad_top(&self) -> usize {
        self.cpu.pad_top()
    }

    /// Override the top-only interior pad (read each frame) without changing the framebuffer size.
    #[inline]
    pub fn set_pad_top(&mut self, pad_top: usize) {
        self.cpu.set_pad_top(pad_top);
    }

    /// Declare the host-chrome top rows and the tone their surface extends into the padding.
    #[inline]
    pub fn set_chrome_bleed(&mut self, bleed: Option<aterm_render::ChromeBleed>) {
        self.cpu.set_chrome_bleed(bleed);
    }

    /// The chrome bleed in force, read from the inner CPU renderer.
    #[must_use]
    #[inline]
    pub const fn chrome_bleed(&self) -> Option<aterm_render::ChromeBleed> {
        self.cpu.chrome_bleed()
    }

    /// The chrome rows' frame extent, stamped on a window by `encode_frame`.
    #[must_use]
    #[inline]
    pub fn chrome_extent_px(&self, frame_h: usize) -> std::ops::Range<usize> {
        self.cpu.chrome_extent_px(frame_h)
    }

    /// Padded pixel size of a `rows`×`cols` grid (`cols·cell_w + 2·pad`, etc.).
    #[inline]
    pub const fn frame_size(&self, rows: usize, cols: usize) -> (usize, usize) {
        self.cpu.frame_size(rows, cols)
    }

    /// Whether the GPU device was reported lost (driver update, TDR, eGPU unplug).
    #[inline]
    pub fn device_lost(&self) -> bool {
        #[cfg(gl_arm)]
        if self
            .gl_arm
            .as_ref()
            .is_some_and(|live| live.dev.shared().lost())
        {
            return true;
        }
        self.ctx.device_lost()
    }

    /// The loss's cause — see [`GpuContext::device_loss_reason`].
    #[must_use]
    pub fn device_loss_reason(&self) -> Option<String> {
        #[cfg(gl_arm)]
        if let Some(live) = self.gl_arm.as_ref().filter(|live| live.dev.shared().lost()) {
            #[cfg(target_arch = "wasm32")]
            {
                let _ = live;
                return Some("webglcontextlost (the WebGL2 context was lost)".to_owned());
            }
            #[cfg(not(target_arch = "wasm32"))]
            return Some(
                live.dev
                    .shared()
                    .loss_reason()
                    .unwrap_or("EGL_CONTEXT_LOST (the GL context was lost)")
                    .to_owned(),
            );
        }
        self.ctx.device_loss_reason()
    }

    #[cfg(target_os = "macos")]
    fn metal_arm_ready(&mut self) -> bool {
        if !self.metal_armed {
            return false;
        }
        if self.metal_arm.is_some() {
            return true;
        }
        self.first_frame
            .await_item(crate::metal::warm::WarmItem::Library(ShaderLibrary::Cell));
        match MetalArmLive::new(self.ctx.metal_loss_latch_or_wire(), self.first_frame) {
            Ok(live) => {
                self.metal_arm = Some(MetalArmCell::new(live));
                if self.metal_warm_wanted {
                    self.metal_warm_pipelines();
                }
                true
            }
            Err(e) => {
                self.metal_mint_error = Some(e);
                self.metal_armed = false;
                false
            }
        }
    }

    #[cfg(target_os = "macos")]
    fn metal_start_error(&self) -> String {
        match &self.metal_mint_error {
            Some(e) => format!("the Metal renderer could not start: {e}"),
            None => "the Metal renderer could not start".to_owned(),
        }
    }

    /// Arm the Metal renderer path for this renderer in-process, bypassing the normal constructor.
    #[cfg(target_os = "macos")]
    #[cfg(test)]
    pub(crate) fn arm_metal_for_test(&mut self) {
        self.metal_armed = true;
    }

    /// Disarm the Metal arm in-process so the differentials' wgpu oracle twin reaches the wgpu arm.
    #[cfg(target_os = "macos")]
    #[cfg(test)]
    pub(crate) fn disarm_metal_for_test(&mut self) {
        self.metal_armed = false;
    }

    /// Integration-suite spelling of [`Self::disarm_metal_for_test`].
    #[doc(hidden)]
    #[cfg(all(target_os = "macos", wgpu_arm))]
    pub const fn disarm_metal_for_oracle(&mut self) {
        self.metal_armed = false;
    }

    /// Test probe.
    #[cfg(all(target_os = "macos", test, wgpu_arm))]
    pub(crate) fn metal_pipeline_probe_for_test(&mut self) -> (bool, u64) {
        self.metal_arm.as_mut().map_or((false, 0), |c| {
            let l = c.live_mut();
            (l.has_inflight_frame(), l.awaited_frames)
        })
    }

    /// Whether the armed Metal arm is live on this renderer.
    #[cfg(target_os = "macos")]
    #[must_use]
    #[inline]
    pub const fn metal_render_armed(&self) -> bool {
        self.metal_armed && self.metal_arm.is_some()
    }

    #[inline]
    fn resident_present_crop(
        &self,
        input: &RenderInput,
        crop: Option<PresentCrop>,
    ) -> Result<Option<PresentCrop>, ()> {
        let Some(crop) = crop else {
            return Ok(None);
        };
        let (_, logical_raw_height) = self.frame_size(input.rows, input.cols);
        let logical_raw_height = u32::try_from(logical_raw_height).unwrap_or(u32::MAX).max(1);
        let resident_raw_height = self.clamp_fb_dim(logical_raw_height);
        normalize_present_crop(crop, logical_raw_height, resident_raw_height)
            .map(Some)
            .ok_or(())
    }

    #[inline]
    fn fx_atlas_cap(&self) -> u32 {
        self.clamp_fb_dim(fx_gpu::FX_ATLAS_CAP)
    }

    #[inline]
    fn fx_frame_for(&self, input: &RenderInput) -> aterm_render::FxFrame {
        let (fw, fh) = self.cpu.frame_size(input.rows, input.cols);
        self.cpu.fx_frame(
            self.clamp_fb_dim(fw as u32) as usize,
            self.clamp_fb_dim(fh as u32) as usize,
            input.rows,
        )
    }

    #[inline]
    fn clamp_fb_dim(&self, d: u32) -> u32 {
        #[cfg(wgpu_arm)]
        {
            d.max(1)
                .min(self.ctx.device.limits().max_texture_dimension_2d)
        }
        #[cfg(not(wgpu_arm))]
        {
            d.clamp(1, self.first_party_max_texture_dim())
        }
    }

    #[cfg(not(wgpu_arm))]
    #[inline]
    fn first_party_max_texture_dim(&self) -> u32 {
        #[cfg(target_os = "macos")]
        {
            crate::device_layer::METAL_MAX_TEXTURE_DIM
        }
        #[cfg(gl_arm)]
        {
            self.ctx.gl_max_texture_dim
        }
    }

    /// Test/diagnostic.
    #[doc(hidden)]
    #[must_use]
    #[inline]
    pub fn max_texture_dim(&self) -> u32 {
        self.clamp_fb_dim(u32::MAX)
    }

    #[cfg(all(test, wgpu_arm))]
    fn atlas_tex_creations(&self) -> u64 {
        self.atlas_tex_creations
    }

    #[cfg(all(test, wgpu_arm))]
    fn atlas_tex_dims(&self) -> Option<((u32, u32), (u32, u32))> {
        let m = self.mono_res.as_ref()?;
        let c = self.color_res.as_ref()?;
        Some((
            (m.tex.wgpu().width(), m.tex.wgpu().height()),
            (c.tex.wgpu().width(), c.tex.wgpu().height()),
        ))
    }

    /// The adapter/backend the GPU device is running on (for diagnostics).
    #[inline]
    pub fn adapter(&self) -> (&str, &str) {
        (&self.ctx.adapter_name, &self.ctx.backend)
    }

    /// Whether the additive streams (aurora, sparkle-add, bloom halo) match CPU `add_sat` exactly.
    #[inline]
    pub const fn additive_is_byte_exact(&self) -> bool {
        self.ctx.srgb_offscreen
    }

    /// Mirror of [`Renderer::set_cursor_blink_phase`].
    #[inline]
    pub fn set_cursor_blink_phase(&mut self, on: bool) {
        self.cpu.set_cursor_blink_phase(on);
    }

    /// Mirror of [`Renderer::set_cursor_style_override`].
    #[inline]
    pub fn set_cursor_style_override(&mut self, style: Option<CursorStyle>) {
        self.cpu.set_cursor_style_override(style);
    }

    #[inline]
    fn drawable(cell: &aterm_core::terminal::RenderCell) -> bool {
        !cell.wide && cell.ch != ' ' && !cell.ch.is_control()
    }

    #[inline]
    fn cell_key(
        &mut self,
        cluster: Option<&str>,
        cell: &aterm_core::terminal::RenderCell,
        cell_span: u8,
    ) -> GlyphKey {
        self.cpu.resolve_cell_key_for_span(cluster, cell, cell_span)
    }

    fn create_atlas_texture(&mut self, atlas: Atlas) -> ResidentAtlas {
        #[cfg(wgpu_arm)]
        let handle = self.ctx.device_layer();
        #[cfg(wgpu_arm)]
        let bpp = atlas.kind.bpp();
        #[cfg(wgpu_arm)]
        let (format, label, bg_label) = match atlas.kind {
            AtlasKind::Mono => (
                crate::device_layer::TexelFormat::R8Unorm,
                "aterm-gpu atlas",
                "aterm-gpu atlas bg",
            ),
            AtlasKind::Color => (
                crate::device_layer::TexelFormat::Rgba8Unorm,
                "aterm-gpu colour atlas",
                "aterm-gpu colour atlas bg",
            ),
        };
        #[cfg(wgpu_arm)]
        let max_tex_dim = handle.max_texture_dim();
        #[cfg(not(wgpu_arm))]
        let max_tex_dim = self.first_party_max_texture_dim();
        let tex_h = (atlas.height + ATLAS_GROW_HEADROOM).min(max_tex_dim);
        #[cfg(wgpu_arm)]
        let tex = handle.create_texture_2d(
            label,
            format,
            atlas.width,
            tex_h,
            crate::device_layer::TexUsage::UPLOADED_SAMPLED,
            None,
        );
        self.atlas_tex_creations += 1;
        #[cfg(wgpu_arm)]
        handle.upload_texture_full(
            &tex,
            &atlas.data[..(atlas.width * atlas.height * bpp) as usize],
            atlas.width * bpp,
            atlas.width,
            atlas.height,
        );
        #[cfg(wgpu_arm)]
        let view = tex
            .wgpu()
            .create_view(&wgpu::TextureViewDescriptor::default());
        #[cfg(wgpu_arm)]
        let bind = self
            .ctx
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
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
            #[cfg(wgpu_arm)]
            tex,
            #[cfg(wgpu_arm)]
            bind,
            tex_h,
        }
    }

    fn ensure_deco_atlas(&mut self, cw: usize, ch: usize) {
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
        {
            let x0 = aterm_render::UNDERCURL_SPRITE * cw;
            let mask = self.cpu.undercurl_mask();
            for my in 0..ch {
                for mx in 0..cw {
                    data[my * atlas_w + x0 + mx] = mask[my * cw + mx];
                }
            }
        }
        #[cfg(wgpu_arm)]
        let bind = {
            let handle = self.ctx.device_layer();
            let tex = handle.create_texture_2d(
                "aterm-gpu deco atlas",
                crate::device_layer::TexelFormat::R8Unorm,
                atlas_w as u32,
                ch as u32,
                crate::device_layer::TexUsage::UPLOADED_SAMPLED,
                None,
            );
            handle.upload_texture_full(&tex, &data, atlas_w as u32, atlas_w as u32, ch as u32);
            let view = tex
                .wgpu()
                .create_view(&wgpu::TextureViewDescriptor::default());
            self.ctx
                .device
                .create_bind_group(&wgpu::BindGroupDescriptor {
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
                })
        };
        self.deco_atlas = Some(DecoAtlas {
            #[cfg(wgpu_arm)]
            bind,
            cw,
            ch,
            curl_band,
            atlas_w,
            #[cfg(any(target_os = "macos", gl_arm))]
            data,
        });
    }

    fn upload_atlas_rows(&self, res: &ResidentAtlas, y0: u32, y1: u32) {
        #[cfg(not(wgpu_arm))]
        let _ = (res, y0, y1);
        #[cfg(wgpu_arm)]
        {
            if y1 <= y0 {
                return;
            }
            let bpp = res.atlas.kind.bpp();
            let row = (res.atlas.width * bpp) as usize;
            let start = y0 as usize * row;
            let end = y1 as usize * row;
            self.ctx.device_layer().upload_texture_rows(
                &res.tex,
                y0,
                y1 - y0,
                &res.atlas.data[start..end],
                res.atlas.width * bpp,
                res.atlas.width,
            );
        }
    }

    fn ensure_atlases(&mut self, keys: &mut [GlyphKey]) {
        // Fast path: every key is resident in both atlases. Runs before the sort
        // (membership needs no order), so the common frame never sorts.
        if self.mono_res.is_some()
            && self.color_res.is_some()
            && keys.iter().all(|k| self.resident_keys.contains(k))
        {
            return;
        }

        keys.sort_unstable();

        let new_keys: Vec<GlyphKey> = keys
            .iter()
            .copied()
            .filter(|k| !self.resident_keys.contains(k))
            .collect();

        if self.mono_res.is_none() || self.color_res.is_none() {
            self.rebuild_atlases(keys);
            return;
        }

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
                let mono_res = self.mono_res.as_ref().unwrap();
                self.upload_atlas_rows(mono_res, mb.0, mb.1);
                let color_res = self.color_res.as_ref().unwrap();
                self.upload_atlas_rows(color_res, cb.0, cb.1);
                self.resident_keys.extend(new_keys);
            }
            _ => self.rebuild_atlases(keys),
        }
    }

    fn rebuild_atlases(&mut self, keys: &[GlyphKey]) {
        #[cfg(wgpu_arm)]
        let cap_h = self.ctx.device_layer().max_texture_dim();
        #[cfg(not(wgpu_arm))]
        let cap_h = self.first_party_max_texture_dim();
        #[cfg(any(target_os = "macos", gl_arm))]
        {
            self.atlas_epoch += 1;
        }
        let mono = build_atlas(&mut self.cpu, keys, cap_h);
        self.mono_res = Some(self.create_atlas_texture(mono));
        let color = build_color_atlas(&mut self.cpu, keys, cap_h);
        self.color_res = Some(self.create_atlas_texture(color));
        self.resident_keys = keys.iter().copied().collect();
    }

    #[must_use]
    fn pack_image_plane(items: &[(u32, u32, u32, &[u8])], tw: u32, th: u32) -> Vec<u8> {
        let mut data = vec![0u8; (tw * th * 4) as usize];
        for &(y0, dw, dh, rgba) in items {
            if dw == tw {
                let dst = (y0 * tw) as usize * 4;
                let len = (dw * dh * 4) as usize;
                data[dst..dst + len].copy_from_slice(&rgba[..len]);
            } else {
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

    fn build_image_plane(&mut self, win: &mut WindowGpu, input: &RenderInput) {
        let (cw, ch) = self.cpu.cell_size();
        let mut order = std::mem::take(&mut self.image_order_scratch);
        let mut seen = std::mem::take(&mut self.image_seen_scratch);
        let mut placements = std::mem::take(&mut self.image_placements_scratch);
        order.clear();
        seen.clear();
        placements.clear();
        let mut last: Option<((usize, usize, usize), usize)> = None;
        for row in &input.images {
            for cell in row.chunk_by(|a, b| a.0 == b.0) {
                for (depth, (_c, image)) in cell.iter().rev().enumerate() {
                    let (fp_w, fp_h) = aterm_render::placement_footprint(&image.image, (cw, ch));
                    if fp_w == 0 || fp_h == 0 {
                        continue;
                    }
                    let key = (std::sync::Arc::as_ptr(&image.image) as usize, fp_w, fp_h);
                    if last == Some((key, depth)) {
                        continue;
                    }
                    last = Some((key, depth));
                    match seen.get(&key) {
                        Some(&at) => order[at].3 = order[at].3.min(depth),
                        None => {
                            seen.insert(key, order.len());
                            order.push((image.image.clone(), fp_w, fp_h, depth));
                        }
                    }
                }
            }
        }
        order.sort_by_key(|&(.., depth)| depth);
        if order.is_empty() {
            win.image_plane = None;
            self.image_order_scratch = order;
            self.image_seen_scratch = seen;
            self.image_placements_scratch = placements;
            return;
        }

        win.image_cache.begin_frame(
            order
                .iter()
                .map(|(image, fp_w, fp_h, _)| (image, *fp_w, *fp_h)),
        );
        #[cfg(wgpu_arm)]
        let max_tex_dim = self.ctx.device.limits().max_texture_dimension_2d;
        #[cfg(not(wgpu_arm))]
        let max_tex_dim = self.first_party_max_texture_dim();
        let mut total_h: u32 = 0;
        let mut max_w: u32 = 0;
        for (image, fp_w, fp_h, _) in &order {
            let key = (std::sync::Arc::as_ptr(image) as usize, *fp_w, *fp_h);
            let d = win.image_cache.get_or_decode(image, *fp_w, *fp_h);
            let side = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
            let (empty, iw, ih) = (d.rgba.is_empty(), side(d.w), side(d.h));
            if empty || iw == 0 || ih == 0 {
                continue;
            }
            if iw > max_tex_dim || ih > max_tex_dim {
                continue;
            }
            if total_h + ih > max_tex_dim {
                break;
            }
            placements.insert(key, (total_h, iw, ih));
            total_h += ih;
            max_w = max_w.max(iw);
        }
        if placements.is_empty() || max_w == 0 || total_h == 0 {
            win.image_plane = None;
            order.clear();
            self.image_order_scratch = order;
            self.image_seen_scratch = seen;
            self.image_placements_scratch = placements;
            return;
        }

        let (tw, th) = (max_w, total_h);
        if let Some(p) = &win.image_plane
            && p.w == tw
            && p.h == th
            && p.placements == placements
        {
            order.clear();
            self.image_order_scratch = order;
            self.image_seen_scratch = seen;
            self.image_placements_scratch = placements;
            return;
        }
        let mut items: Vec<(u32, u32, u32, &[u8])> = Vec::with_capacity(order.len());
        let mut pinned_images: Vec<std::sync::Arc<aterm_core::grid::extra::ImageData>> =
            Vec::with_capacity(placements.len());
        for (image, fp_w, fp_h, _) in &order {
            let key = (std::sync::Arc::as_ptr(image) as usize, *fp_w, *fp_h);
            let Some(&(y0, dw, dh)) = placements.get(&key) else {
                continue;
            };
            pinned_images.push(std::sync::Arc::clone(image));
            let Some(decoded) = win.image_cache.peek(image, *fp_w, *fp_h) else {
                continue;
            };
            items.push((y0, dw, dh, decoded.rgba.as_slice()));
        }
        let data = Self::pack_image_plane(&items, tw, th);

        #[cfg(wgpu_arm)]
        let bind = {
            let handle = self.ctx.device_layer();
            let tex = handle.create_texture_2d(
                "aterm-gpu image plane",
                crate::device_layer::TexelFormat::Rgba8Unorm,
                tw,
                th,
                crate::device_layer::TexUsage::UPLOADED_SAMPLED,
                None,
            );
            handle.upload_texture_full(&tex, &data, tw * 4, tw, th);
            let view = tex
                .wgpu()
                .create_view(&wgpu::TextureViewDescriptor::default());
            self.ctx
                .device
                .create_bind_group(&wgpu::BindGroupDescriptor {
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
                })
        };
        #[cfg(target_os = "macos")]
        let (metal_texels, epoch) = if self.metal_armed {
            self.image_plane_epoch += 1;
            (Some(data), self.image_plane_epoch)
        } else {
            (None, 0)
        };
        #[cfg(gl_arm)]
        let (gl_texels, gl_epoch) = if self.gl_armed {
            self.image_plane_epoch += 1;
            (Some(data), self.image_plane_epoch)
        } else {
            (None, 0)
        };
        win.image_plane = Some(ImagePlane {
            #[cfg(target_os = "macos")]
            metal_atlas: None,
            #[cfg(wgpu_arm)]
            bind,
            w: tw,
            h: th,
            placements: std::mem::take(&mut placements),
            _pinned_images: pinned_images,
            #[cfg(target_os = "macos")]
            metal_texels,
            #[cfg(target_os = "macos")]
            epoch,
            #[cfg(gl_arm)]
            gl_texels,
            #[cfg(gl_arm)]
            gl_epoch,
            #[cfg(gl_arm)]
            gl_atlas: None,
        });
        order.clear();
        self.image_order_scratch = order;
        self.image_seen_scratch = seen;
        self.image_placements_scratch = placements;
    }

    /// Enable/disable the cursor-comet bloom (default on), read each present.
    #[inline]
    pub fn set_bloom(&mut self, on: bool) {
        let was = std::mem::replace(&mut self.enable_bloom, on);
        self.present_pass_switched(was, on);
    }

    /// A present switch went from `was` to `on` (or a first window went on screen, which may bring
    /// an f16 swapchain): once the Metal arm's warm was asked for
    /// ([`Self::warm_effect_pipelines`]), a pass it turned on gets its pipelines built on a worker
    /// before a frame binds them.
    #[inline]
    fn present_pass_switched(&mut self, was: bool, on: bool) {
        #[cfg(target_os = "macos")]
        if on && !was && self.metal_warm_wanted {
            self.metal_warm_pipelines();
        }
        #[cfg(not(target_os = "macos"))]
        let _ = (was, on);
    }

    /// Whether the GPU bloom layer is currently enabled.
    #[must_use]
    #[inline]
    pub const fn bloom_enabled(&self) -> bool {
        self.enable_bloom
    }

    /// Set the bloom tunables (`cursor_trail_bloom_strength`/`_radius`).
    #[inline]
    pub const fn set_bloom_params(&mut self, strength: f32, radius: f32) {
        self.bloom_strength = strength;
        self.bloom_radius = radius;
    }

    /// Enable/disable the heat shimmer (config `cursor_fire_shimmer`, default on).
    #[inline]
    pub fn set_shimmer(&mut self, on: bool) {
        let was = std::mem::replace(&mut self.enable_shimmer, on);
        self.present_pass_switched(was, on);
    }

    /// Whether the GPU heat-shimmer layer is currently enabled.
    #[must_use]
    #[inline]
    pub const fn shimmer_enabled(&self) -> bool {
        self.enable_shimmer
    }

    /// Test hook.
    #[doc(hidden)]
    #[inline]
    pub const fn set_shimmer_phase_for_test(&mut self, phase: Option<f32>) {
        self.shimmer_phase_pin = phase;
    }

    /// Test helper.
    #[doc(hidden)]
    #[must_use]
    pub fn shimmer_region_for_test(&self, input: &RenderInput) -> Option<(u32, u32, u32, u32)> {
        let (fw, fh) = self.frame_size(input.rows, input.cols);
        let w = self.clamp_fb_dim(fw as u32);
        let h = self.clamp_fb_dim(fh as u32);
        self.shimmer_region(input, w, h)
            .map(|r| (r.x0, r.y0, r.x1, r.y1))
    }

    #[inline]
    const fn shimmer_live(&self, input: &RenderInput) -> bool {
        self.enable_shimmer && !input.fire_patch.is_empty() && !input.cursor_glow_add.is_empty()
    }

    /// Config `hdr_glow` (default on).
    pub fn set_hdr_glow(&mut self, on: bool) {
        // The HDR swapchain and the DirectComposition visual swapchain are mutually exclusive:
        // once the instance is visual, `hdr_glow` stays off (its composition there is unverified,
        // and rebuilding the instance would tear down every window). Every config path funnels
        // through here.
        if on && self.ctx.visual_swapchain {
            static ONCE: std::sync::Once = std::sync::Once::new();
            ONCE.call_once(|| {
                crate::stderr_line!(
                    "aterm-gpu: hdr_glow is unavailable while background_material is active \
                     (the DirectComposition backdrop swapchain and the scRGB EDR swapchain \
                     are mutually exclusive); keeping the backdrop — restart with \
                     background_material = \"none\" for HDR"
                );
            });
            self.hdr_glow = false;
            return;
        }
        let was = std::mem::replace(&mut self.hdr_glow, on);
        self.present_pass_switched(was, on);
    }

    /// Mirror the frontend's `background_material != none` knob.
    #[inline]
    pub const fn set_backdrop_margins(&mut self, on: bool) {
        self.backdrop_margins = on;
    }

    #[inline]
    const fn backdrop_margins_active(&self) -> bool {
        self.backdrop_margins && self.ctx.visual_swapchain
    }

    /// Set the SDR glow-boost strength (`cursor_glow_sdr_boost`, 0..=1; 0 disables).
    #[inline]
    pub fn set_sdr_glow_boost(&mut self, strength: f32) {
        let was = std::mem::replace(&mut self.sdr_glow_boost, strength) > 0.0;
        self.present_pass_switched(was, strength > 0.0);
    }

    /// Start recording this window's swapchain frames submitted through the WSI present path, with
    /// [`crate::video_tap::CaptureOpts`].
    pub fn video_begin(
        &self,
        win: &mut WindowGpu,
        surf: &GpuSurface,
        opts: crate::video_tap::CaptureOpts,
    ) -> Result<(), String> {
        if !surf.copyable {
            return Err("surface not copyable on this backend (no COPY_SRC)".into());
        }
        #[cfg(target_os = "macos")]
        if self.metal_armed {
            let Some(cell) = self.metal_arm.as_ref() else {
                return Err(
                    "the Metal arm has not rendered its first frame yet — retry after \
                     the next present"
                        .into(),
                );
            };
            let (nw, nh, nf) = surf.neutral_config();
            let tap = crate::video_tap::VideoTap::new_metal(
                &cell.live_ref().mint,
                nw,
                nh,
                nf,
                win.capture_color_space,
                win.sdr_white_scale(),
                opts,
            )?;
            win.video = Some(tap);
            return Ok(());
        }
        #[cfg(all(gl_arm, not(wgpu_arm)))]
        if self.gl_armed && surf.gl.is_some() {
            let tap = crate::video_tap::VideoTap::new_gl(
                surf.neutral.width,
                surf.neutral.height,
                crate::device_layer::TexelFormat::Rgba8Unorm,
                win.capture_color_space,
                win.sdr_white_scale(),
                opts,
            )?;
            win.video = Some(tap);
            return Ok(());
        }
        #[cfg(not(wgpu_arm))]
        {
            Err("video: the wgpu tap ring is not in this build".to_string())
        }
        #[cfg(wgpu_arm)]
        {
            let tap = crate::video_tap::VideoTap::new(
                &self.ctx.device,
                surf.config.width,
                surf.config.height,
                crate::device_layer::TexelFormat::from_wgpu(surf.config.format)
                    .ok_or("video: swapchain format outside the closed set")?,
                win.capture_color_space,
                win.sdr_white_scale(),
                opts,
            )?;
            win.video = Some(tap);
            Ok(())
        }
    }

    /// Start recording a glass-less window's presents through a persistent `VirtualTarget`.
    pub fn virtual_begin(
        &mut self,
        win: &mut WindowGpu,
        w: u32,
        h: u32,
        opts: crate::video_tap::CaptureOpts,
    ) -> Result<(), String> {
        let w = self.clamp_fb_dim(w.max(1));
        let h = self.clamp_fb_dim(h.max(1));
        #[cfg(gl_arm)]
        if self.gl_armed && self.gl_arm_ready() {
            let tap = crate::video_tap::VideoTap::new_gl(
                w,
                h,
                crate::device_layer::TexelFormat::Rgba8Unorm,
                crate::video_tap::CaptureColorSpace::Srgb,
                1.0,
                opts,
            )?;
            win.virtual_presented_input_epoch = 0;
            win.video = Some(tap);
            return Ok(());
        }
        #[cfg(all(not(wgpu_arm), gl_arm))]
        {
            Err(format!("video: {}", self.gl_start_error()))
        }
        #[cfg(all(not(wgpu_arm), target_os = "macos"))]
        {
            if !self.metal_arm_ready() {
                return Err(format!("video: {}", self.metal_start_error()));
            }
            let cell = self.metal_arm.as_ref().expect("metal_arm_ready above");
            let tap = crate::video_tap::VideoTap::new_metal(
                &cell.live_ref().mint,
                w,
                h,
                VIRTUAL_PRESENT_TEXEL,
                crate::video_tap::CaptureColorSpace::Srgb,
                1.0,
                opts,
            )?;
            win.virtual_presented_input_epoch = 0;
            win.video = Some(tap);
            Ok(())
        }
        #[cfg(wgpu_arm)]
        {
            let tap = crate::video_tap::VideoTap::new(
                &self.ctx.device,
                w,
                h,
                VIRTUAL_PRESENT_TEXEL,
                crate::video_tap::CaptureColorSpace::Srgb,
                1.0,
                opts,
            )?;
            win.virtual_target = Some(self.make_virtual_target(w, h));
            win.virtual_presented_input_epoch = 0;
            win.video = Some(tap);
            Ok(())
        }
    }

    /// Post-submit hook.
    pub fn video_after_present(&self, win: &mut WindowGpu, t_us: u64) {
        if let Some(tap) = win.video.as_mut() {
            #[cfg(target_os = "macos")]
            if tap.ring_is_metal() {
                tap.metal_after_present(t_us);
            }
            #[cfg(gl_arm)]
            let on_gl = tap.ring_is_gl();
            #[cfg(all(not(gl_arm), wgpu_arm))]
            let on_gl = false;
            #[cfg(gl_arm)]
            if on_gl {
                tap.gl_after_present(t_us);
            }
            #[cfg(wgpu_arm)]
            if !on_gl {
                tap.after_present(&self.ctx.device, t_us);
            }
        }
    }

    /// Finalize the `video` recording (a blocking drain, off the hot path) and hand the frames out.
    pub fn video_finish(&self, win: &mut WindowGpu) -> Option<crate::video_tap::VideoTake> {
        win.video.take().map(|t| {
            #[cfg(target_os = "macos")]
            if t.ring_is_metal() {
                return t.metal_finish();
            }
            #[cfg(gl_arm)]
            if t.ring_is_gl() {
                return t.gl_finish();
            }
            #[cfg(wgpu_arm)]
            {
                t.finish(&self.ctx.device)
            }
            #[cfg(not(wgpu_arm))]
            unreachable!("a wgpu ring cannot exist on a wgpu-free build")
        })
    }

    /// The `video` recording's status: `(frames_so_far, resized_early_stop)`.
    #[must_use]
    pub fn video_status(&self, win: &WindowGpu) -> Option<(usize, bool)> {
        win.video.as_ref().map(|t| (t.frames_so_far(), t.resized()))
    }

    /// Arm a one-shot copy of this window's next successfully presented destination.
    pub fn presented_snapshot_begin(
        &self,
        win: &mut WindowGpu,
        surface: &GpuSurface,
    ) -> Result<(), String> {
        if win.presented_snapshot.is_some() {
            return Err("presented snapshot: capture already armed".to_string());
        }
        if !surface.copyable {
            return Err(
                "presented snapshot: surface not copyable on this backend (no COPY_SRC)"
                    .to_string(),
            );
        }
        #[cfg(target_os = "macos")]
        if self.metal_armed {
            let Some(cell) = self.metal_arm.as_ref() else {
                return Err(
                    "the Metal arm has not rendered its first frame yet — retry after \
                     the next present"
                        .to_string(),
                );
            };
            let (nw, nh, nf) = surface.neutral_config();
            win.presented_snapshot = Some(crate::video_tap::PresentedFrameTap::new_metal(
                &cell.live_ref().mint,
                nw,
                nh,
                nf,
                win.capture_color_space,
                win.sdr_white_scale(),
            )?);
            win.copy_src_hold.arm(aterm_time::Instant::now());
            return Ok(());
        }
        #[cfg(all(gl_arm, not(wgpu_arm)))]
        if self.gl_armed && surface.gl.is_some() {
            win.presented_snapshot = Some(crate::video_tap::PresentedFrameTap::new_gl(
                surface.neutral.width,
                surface.neutral.height,
                crate::device_layer::TexelFormat::Rgba8Unorm,
                win.capture_color_space,
                win.sdr_white_scale(),
            )?);
            return Ok(());
        }
        #[cfg(not(wgpu_arm))]
        {
            Err("snapshot: the wgpu tap ring is not in this build".to_string())
        }
        #[cfg(wgpu_arm)]
        {
            win.presented_snapshot = Some(crate::video_tap::PresentedFrameTap::new(
                &self.ctx.device,
                surface.config.width,
                surface.config.height,
                crate::device_layer::TexelFormat::from_wgpu(surface.config.format)
                    .ok_or("snapshot: swapchain format outside the closed set")?,
                win.capture_color_space,
                win.sdr_white_scale(),
            )?);
            win.copy_src_hold.arm(aterm_time::Instant::now());
            Ok(())
        }
    }

    /// Post-present half of [`Self::presented_snapshot_begin`].
    pub fn presented_snapshot_after_present(
        &self,
        win: &mut WindowGpu,
        t_us: u64,
    ) -> Result<(), String> {
        let tap = win
            .presented_snapshot
            .as_mut()
            .ok_or_else(|| "presented snapshot: no capture is armed".to_string())?;
        #[cfg(target_os = "macos")]
        if tap.tap_is_metal() {
            return tap.metal_after_present(t_us);
        }
        #[cfg(gl_arm)]
        if tap.tap_is_gl() {
            return tap.gl_after_present(t_us);
        }
        #[cfg(wgpu_arm)]
        {
            tap.after_present(&self.ctx.device, t_us)
        }
        #[cfg(not(wgpu_arm))]
        unreachable!("a wgpu one-shot cannot exist on a wgpu-free build")
    }

    /// Complete the one-shot map, blocking only after the capture has left the present hot path.
    pub fn presented_snapshot_finish(&self, win: &mut WindowGpu) -> Result<(), String> {
        let tap = win
            .presented_snapshot
            .as_mut()
            .ok_or_else(|| "presented snapshot: no capture is armed".to_string())?;
        #[cfg(target_os = "macos")]
        if tap.tap_is_metal() {
            return tap.metal_finish();
        }
        #[cfg(gl_arm)]
        if tap.tap_is_gl() {
            return tap.gl_finish();
        }
        #[cfg(wgpu_arm)]
        {
            tap.finish(&self.ctx.device)
        }
        #[cfg(not(wgpu_arm))]
        unreachable!("a wgpu one-shot cannot exist on a wgpu-free build")
    }

    /// Take the completed exact destination as its staging buffer mapped it
    /// ([`crate::video_tap::RawCapture`]).
    pub fn presented_snapshot_take(
        &self,
        win: &mut WindowGpu,
    ) -> Result<crate::video_tap::RawCapture, String> {
        win.presented_snapshot
            .take()
            .ok_or_else(|| "presented snapshot: no capture is armed".to_string())?
            .take_raw()
    }

    /// Copy and synchronously read the current persistent virtual destination without arming or
    /// forcing a present.
    pub fn virtual_presented_snapshot_current(
        &self,
        win: &WindowGpu,
        expected_input_epoch: u64,
        t_us: u64,
    ) -> Result<crate::video_tap::RawCapture, String> {
        if expected_input_epoch == 0 || expected_input_epoch == u64::MAX {
            return Err("presented snapshot: invalid virtual destination epoch".to_string());
        }
        if win.virtual_presented_input_epoch != expected_input_epoch {
            return Err(
                "presented snapshot: virtual destination advanced before capture".to_string(),
            );
        }
        #[cfg(gl_arm)]
        if self.gl_armed {
            return self.gl_virtual_snapshot(win, t_us);
        }
        #[cfg(all(not(wgpu_arm), target_os = "macos"))]
        {
            let target = win.metal_virtual_off.as_ref().ok_or_else(|| {
                "presented snapshot: current virtual destination is unavailable".to_string()
            })?;
            let cell = self
                .metal_arm
                .as_ref()
                .ok_or_else(|| "presented snapshot: the Metal arm is not live".to_string())?;
            let live = cell.live_ref();
            let mut tap = crate::video_tap::PresentedFrameTap::new_metal(
                &live.mint,
                target.width() as u32,
                target.height() as u32,
                VIRTUAL_PRESENT_TEXEL,
                crate::video_tap::CaptureColorSpace::Srgb,
                1.0,
            )?;
            let mut cb = live.session.begin()?;
            tap.metal_enqueue_copy(
                &mut cb,
                target,
                VIRTUAL_PRESENT_TEXEL,
                crate::video_tap::CaptureColorSpace::Srgb,
                1.0,
            )?;
            let submitted = cb.commit();
            tap.metal_note_submitted(crate::metal::encoder::CbProbe::of(&submitted));
            if submitted.wait_outcome() != crate::metal::loss::CbOutcome::Completed {
                return Err("presented snapshot: the copy did not complete".to_string());
            }
            tap.metal_after_present(t_us)?;
            tap.metal_finish()?;
            tap.take_raw()
        }
        #[cfg(all(not(wgpu_arm), gl_arm))]
        {
            Err(format!("presented snapshot: {}", self.gl_start_error()))
        }
        #[cfg(wgpu_arm)]
        {
            let target = win.virtual_target.as_ref().ok_or_else(|| {
                "presented snapshot: current virtual destination is unavailable".to_string()
            })?;
            let mut tap = crate::video_tap::PresentedFrameTap::new(
                &self.ctx.device,
                target.w,
                target.h,
                VIRTUAL_PRESENT_TEXEL,
                crate::video_tap::CaptureColorSpace::Srgb,
                1.0,
            )?;
            let mut encoder =
                self.ctx
                    .device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("aterm current virtual presented-frame snapshot"),
                    });
            let staged = tap.enqueue_copy(
                &mut encoder,
                &target.tex,
                crate::video_tap::CaptureColorSpace::Srgb,
                1.0,
            );
            crate::device_layer::submit_host_readable(
                &self.ctx.device,
                &self.ctx.queue,
                encoder.finish(),
                staged.as_slice(),
            );
            tap.after_present(&self.ctx.device, t_us)?;
            tap.finish(&self.ctx.device)?;
            tap.take_raw()
        }
    }

    /// Render a [`RenderInput`] snapshot (from [`aterm_core::terminal::Terminal::cell_frame_into`])
    /// on the GPU and read it back.
    pub fn render_input(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
        tray: Option<TrayQuad<'_>>,
    ) -> Frame {
        self.cpu
            .checked_frame_size(input.rows, input.cols)
            .expect("renderable framebuffer");
        let (texture, width, height) = self.render_input_target(win, input, tray);
        #[cfg(gl_arm)]
        if self.last_frame_arm_gl {
            return self
                .gl_read_back_offscreen(win, width, height)
                .unwrap_or_else(|e| {
                    crate::stderr_line!(
                        "aterm-gpu: gl readback failed ({e}); returning blank frame"
                    );
                    Frame {
                        width: width as usize,
                        height: height as usize,
                        pixels: vec![0; (width as usize) * (height as usize)],
                    }
                });
        }
        #[cfg(target_os = "macos")]
        if self.last_frame_arm_metal {
            return self
                .metal_read_back_offscreen(win, width, height)
                .unwrap_or_else(|e| {
                    crate::stderr_line!(
                        "aterm-gpu: metal readback failed ({e}); returning blank frame"
                    );
                    Frame {
                        width: width as usize,
                        height: height as usize,
                        pixels: vec![0; (width as usize) * (height as usize)],
                    }
                });
        }
        #[cfg(wgpu_arm)]
        {
            let texture = texture.expect("a wgpu frame vends its offscreen texture");
            self.ctx.read_back(&texture, width, height)
        }
        #[cfg(not(wgpu_arm))]
        {
            let _ = texture;
            crate::stderr_line!(
                "aterm-gpu: disarmed render_input on a wgpu-free build; blank frame"
            );
            Frame {
                width: width as usize,
                height: height as usize,
                pixels: vec![0; (width as usize) * (height as usize)],
            }
        }
    }

    /// Fallible twin of [`Self::render_input`], rejecting excessive dimensions before changing
    /// window tracking or encoding a frame.
    pub fn try_render_input(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
        tray: Option<TrayQuad<'_>>,
    ) -> Result<Frame, String> {
        self.cpu.checked_frame_size(input.rows, input.cols)?;
        let (texture, width, height) = self.render_input_target(win, input, tray);
        #[cfg(gl_arm)]
        if self.last_frame_arm_gl {
            return self.gl_read_back_offscreen(win, width, height);
        }
        #[cfg(target_os = "macos")]
        if self.last_frame_arm_metal {
            return self.metal_read_back_offscreen(win, width, height);
        }
        #[cfg(wgpu_arm)]
        {
            let texture = texture.expect("a wgpu frame vends its offscreen texture");
            self.ctx.try_read_back(&texture, width, height)
        }
        #[cfg(not(wgpu_arm))]
        {
            let _ = texture;
            Err("the renderer is disarmed and this build carries no wgpu arm".to_string())
        }
    }

    fn render_input_target(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
        tray: Option<TrayQuad<'_>>,
    ) -> (Option<OffscreenTexHandle>, u32, u32) {
        let mut dims;
        let mut pass = 1;
        loop {
            let epoch = self.cpu.font_epoch();
            win.present_prev = None;
            dims = self.encode_frame(win, input, &RepaintScope::Full);
            let _ = self.cpu.settle_in_flight_fallback_parses();
            if self.cpu.font_epoch() == epoch || pass == Renderer::SNAPSHOT_SETTLE_PASSES {
                break;
            }
            pass += 1;
        }
        let (w, h) = dims;
        #[cfg(gl_arm)]
        if self.last_frame_arm_gl {
            if self.enable_bloom
                && fx_gpu::bloom_source_live(input)
                && let Err(e) = self.gl_composite_bloom_in_place(win, input)
            {
                gl_arm::gl_arm_note(&format!(
                    "armed readback: in-place bloom failed ({e}); frame has no halo"
                ));
            }
            if self.shimmer_live(input)
                && let Err(e) = self.gl_shimmer_in_place(win, input)
            {
                gl_arm::gl_arm_note(&format!(
                    "armed readback: in-place shimmer failed ({e}); frame has no refraction"
                ));
            }
            if input.scroll_frac_px != 0 {
                self.gl_shift_offscreen_band(win, input);
            }
            if let Some(t) = tray
                && let Err(e) = self.gl_tray_into_offscreen(win, t)
            {
                gl_arm::gl_arm_note(&format!(
                    "armed readback: tray bake failed ({e}); frame has no settings card"
                ));
            }
            return (None, w, h);
        }
        if self.enable_bloom && fx_gpu::bloom_source_live(input) {
            #[cfg(target_os = "macos")]
            let armed = self.last_frame_arm_metal;
            #[cfg(not(target_os = "macos"))]
            let armed = false;
            if armed {
                #[cfg(target_os = "macos")]
                if let Err(e) = self.metal_composite_bloom_in_place(win, input) {
                    metal_arm_note(&format!(
                        "armed readback: in-place bloom failed ({e}); frame has no halo"
                    ));
                }
            } else {
                #[cfg(wgpu_arm)]
                self.composite_bloom_in_place(win, input);
            }
        }
        if self.shimmer_live(input) {
            #[cfg(target_os = "macos")]
            let armed = self.last_frame_arm_metal;
            #[cfg(not(target_os = "macos"))]
            let armed = false;
            if armed {
                #[cfg(target_os = "macos")]
                if let Err(e) = self.metal_shimmer_in_place(win, input) {
                    metal_arm_note(&format!(
                        "armed readback: in-place shimmer failed ({e}); frame has no \
                         refraction"
                    ));
                }
            } else {
                #[cfg(wgpu_arm)]
                self.shimmer_offscreen_in_place(win, input);
            }
        }
        self.shift_offscreen_band(win, input);
        if let Some(t) = tray {
            #[cfg(not(any(wgpu_arm, target_os = "macos")))]
            let _ = t;
            #[cfg(target_os = "macos")]
            let armed = self.last_frame_arm_metal;
            #[cfg(not(target_os = "macos"))]
            let armed = false;
            if armed {
                #[cfg(target_os = "macos")]
                if let Err(e) = self.metal_tray_into_offscreen(win, t) {
                    metal_arm_note(&format!(
                        "armed readback: tray bake failed ({e}); frame has no settings \
                         card"
                    ));
                }
            } else {
                #[cfg(wgpu_arm)]
                self.draw_tray_into_offscreen(win, t);
            }
        }
        #[cfg(target_os = "macos")]
        if self.last_frame_arm_metal {
            return (None, w, h);
        }
        #[cfg(wgpu_arm)]
        {
            let tex = win
                .offscreen
                .as_ref()
                .expect("encode_frame sets offscreen")
                .tex
                .clone();
            (Some(tex), w, h)
        }
        #[cfg(not(wgpu_arm))]
        {
            (None, w, h)
        }
    }

    #[cfg(target_os = "macos")]
    fn metal_read_back_offscreen(
        &mut self,
        win: &WindowGpu,
        width: u32,
        height: u32,
    ) -> Result<Frame, String> {
        let _pool = crate::metal::ffi::AutoreleasePool::new();
        let off = win
            .metal_offscreen
            .as_ref()
            .ok_or("armed frame lacks its metal offscreen")?;
        let live = self
            .metal_arm
            .as_mut()
            .ok_or("metal arm not minted")?
            .live_mut();
        crate::device_layer::metal_try_read_back(&live.session, &live.mint, &off.tex, width, height)
    }

    #[cfg(target_os = "macos")]
    fn metal_compose_present_readback(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
        width: u32,
        height: u32,
    ) -> Result<Frame, String> {
        use crate::metal::ffi as mtl;
        let _pool = mtl::AutoreleasePool::new();
        let throwaway = {
            let live = self
                .metal_arm
                .as_mut()
                .ok_or("metal arm not minted")?
                .live_mut();
            live.layer()
                .try_create_texture_2d(
                    "aterm-gpu metal throwaway present target",
                    crate::device_layer::TexelFormat::Bgra8Unorm,
                    width,
                    height,
                    crate::device_layer::TexUsage {
                        render: true,
                        ..crate::device_layer::TexUsage::default()
                    },
                    None,
                )?
                .into_metal()
        };
        let (submitted, composed_present_off) = self.metal_encode_submit_b(
            win,
            input,
            false,
            None,
            None,
            None,
            None,
            (width, height),
            (width, height),
            VIRTUAL_PRESENT_TEXEL,
            false,
            false,
            false,
            &throwaway,
        )?;
        if submitted.wait_outcome() != crate::metal::loss::CbOutcome::Completed {
            return Err("the effect-compose Submit B did not complete".to_owned());
        }
        let composed = if composed_present_off {
            win.metal_present_off
                .as_ref()
                .ok_or("Submit B composed without minting the present copy")?
                .clone_handle()
        } else {
            win.metal_offscreen
                .as_ref()
                .ok_or("armed frame lacks its metal offscreen")?
                .tex
                .clone_handle()
        };
        let live = self
            .metal_arm
            .as_mut()
            .ok_or("metal arm not minted")?
            .live_mut();
        crate::device_layer::metal_try_read_back(
            &live.session,
            &live.mint,
            &composed,
            width,
            height,
        )
    }

    #[cfg(target_os = "macos")]
    fn ensure_metal_tray_card(
        live: &mut MetalArmLive,
        win: &mut WindowGpu,
        t: &TrayQuad<'_>,
        reader: Option<u64>,
        tray_uploads: &mut u64,
        tray_upload_bytes: &mut u64,
    ) -> Result<(), String> {
        debug_assert!(
            t.scrim.is_none(),
            "the Metal arm draws no tray scrim (`TrayQuad::scrim`): macOS shows none"
        );
        let stride = t.pw as usize * 4;
        if let Some(card) = win.metal_tray_card.as_mut()
            && card.w == t.pw
            && card.h == t.ph
        {
            let Some(band) = changed_row_band(&card.pixels, t.rgba, stride) else {
                card.ring.read_unchanged(reader);
                return Ok(());
            };
            let write = card.ring.plan_change(band);
            if write.mint {
                let tex = live
                    .layer()
                    .try_create_texture_2d(
                        "aterm-gpu metal tray card",
                        crate::device_layer::TexelFormat::Rgba8Unorm,
                        t.pw,
                        t.ph,
                        crate::device_layer::TexUsage::UPLOADED_SAMPLED,
                        None,
                    )
                    .map_err(|e| format!("tray card mint: {e}"))?;
                card.texes.push(tex);
            }
            if let Some(seq) = write.settle_through {
                live.await_presents_through(seq);
            }
            let (y0, y1) = write.rows;
            let bytes = &t.rgba[y0 as usize * stride..y1 as usize * stride];
            live.layer().upload_texture_rows(
                &card.texes[write.slot],
                y0,
                y1 - y0,
                bytes,
                t.pw * 4,
                t.pw,
            );
            *tray_uploads += 1;
            *tray_upload_bytes += bytes.len() as u64;
            let (b0, b1) = (band.0 as usize * stride, band.1 as usize * stride);
            card.pixels[b0..b1].copy_from_slice(&t.rgba[b0..b1]);
            card.ring.land_change(write, band, reader);
            return Ok(());
        }
        let layer = live.layer();
        let tex = layer
            .try_create_texture_2d(
                "aterm-gpu metal tray card",
                crate::device_layer::TexelFormat::Rgba8Unorm,
                t.pw,
                t.ph,
                crate::device_layer::TexUsage::UPLOADED_SAMPLED,
                None,
            )
            .map_err(|e| format!("tray card mint: {e}"))?;
        layer.upload_texture_full(&tex, t.rgba, t.pw * 4, t.pw, t.ph);
        *tray_uploads += 1;
        *tray_upload_bytes += t.rgba.len() as u64;
        let mut pixels = win
            .metal_tray_card
            .take()
            .map_or_else(Vec::new, |c| c.pixels);
        pixels.clear();
        pixels.extend_from_slice(t.rgba);
        win.metal_tray_card = Some(MetalTrayCard {
            texes: vec![tex],
            ring: crate::tray_card_ring::TrayCardRing::fresh(t.ph, reader),
            w: t.pw,
            h: t.ph,
            pixels,
        });
        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn metal_composite_bloom_in_place(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
    ) -> Result<(), String> {
        use crate::metal::encoder::{RenderPassDesc, StoreAction};
        use crate::metal::ffi::{
            self as mtl, ClearColor, LoadAction, MtlScissorRect, PixelFormat as MPix, PrimitiveType,
        };
        let _pool = mtl::AutoreleasePool::new();
        let (glow_count, bbox, extract_first) = self.build_bloom_glow(input);
        if glow_count == 0 {
            return Ok(());
        }
        let (fw, fh, off_tex) = {
            let off = win
                .metal_offscreen
                .as_ref()
                .ok_or("armed frame lacks its metal offscreen")?;
            (off.w, off.h, off.tex.clone_handle())
        };
        let text_blend =
            u32::from(self.cpu.text_blending() == aterm_render::TextBlending::LinearCorrected);
        let live = self
            .metal_arm
            .as_mut()
            .ok_or("the Metal arm is not live")?
            .live_mut();
        let (bw, bh) = (
            (fw / BLOOM_DOWNSCALE).max(1) as usize,
            (fh / BLOOM_DOWNSCALE).max(1) as usize,
        );
        let half = live
            .layer()
            .try_create_texture_2d(
                "aterm-gpu metal bloom half",
                crate::device_layer::TexelFormat::Rgba8Unorm,
                bw as u32,
                bh as u32,
                crate::device_layer::TexUsage::OFFSCREEN,
                None,
            )?
            .into_metal();
        let mint_bytes = |live: &mut MetalArmLive, bytes: &[u8]| -> Result<mtl::Obj, String> {
            let layer = live.layer();
            let buf = layer
                .try_create_uniform_buffer("aterm-gpu metal bloom bytes", bytes.len() as u64)?;
            // SAFETY: fresh exactly-sized shared buffer; no GPU work on it.
            unsafe { layer.write_buffer_from_zero(&buf, bytes) };
            Ok(buf.into_metal().into_obj())
        };
        let insts: &[GlowInstance] = &self.bloom_glow_scratch;
        let stream = mint_bytes(live, aterm_bits::cast_slice(insts))?;
        let extract_offset = extract_first as usize * std::mem::size_of::<GlowInstance>();
        let cell_u = mint_bytes(
            live,
            aterm_bits::bytes_of(&Uniforms {
                screen: [fw as f32, fh as f32],
                text_blend: text_blend as f32,
                _pad: 0.0,
            }),
        )?;
        let bu = mint_bytes(
            live,
            aterm_bits::bytes_of(&BloomUniform {
                texel: [1.0 / bw as f32, 1.0 / bh as f32],
                strength: page_bloom_strength(self.bloom_strength, input, self.theme.bg),
                radius: self.bloom_radius,
                chroma_radius: bloom_chroma_radius(self.bloom_radius),
                extent: [1.0; 2],
                _pad: 0.0,
            }),
        )?;
        let clip = input.fx_clip.map(|(cx0, cy0, cx1, cy1)| {
            [
                u32::from(cx0),
                u32::from(cy0),
                u32::from(cx1),
                u32::from(cy1),
            ]
        });
        let scissor = if let Some([x0, y0, x1, y1]) = bbox {
            let d = aterm_render::bloom::reach_px(self.bloom_radius);
            Some([
                x0.saturating_sub(d),
                y0.saturating_sub(d),
                x1.saturating_add(d).min(fw),
                y1.saturating_add(d).min(fh),
            ])
        } else {
            clip.map(|_| [0, 0, fw, fh])
        };
        let scissor = match (scissor, clip) {
            (Some([sx0, sy0, sx1, sy1]), Some([cx0, cy0, cx1, cy1])) => Some([
                sx0.max(cx0),
                sy0.max(cy0),
                sx1.min(cx1).min(fw),
                sy1.min(cy1).min(fh),
            ]),
            (s, None) => s,
            (None, Some(_)) => unreachable!("clip-only case handled above"),
        };
        let extract_pso = live.present_pso(Pipeline::GlowAdd, MPix::Rgba8Unorm)?;
        let bloom_pso = live.present_pso(Pipeline::Bloom, MPix::Rgba8Unorm)?;
        let linear = &live.linear;
        let extract_binds = Pipeline::GlowAdd.spec().binds;
        let bloom_binds = Pipeline::Bloom.spec().binds;

        let mut cb = live.session.begin()?;
        {
            let pass = cb.render_pass(&RenderPassDesc {
                target: &half,
                load: LoadAction::Clear(ClearColor {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 0.0,
                }),
                store: StoreAction::Store,
                viewport: None,
                scissor: None,
            })?;
            pass.set_pipeline(&extract_pso);
            pass.set_vertex_buffer(
                &cell_u,
                extract_binds
                    .vertex_uniform
                    .expect("row 4 has a vertex uniform") as usize,
            )?;
            pass.set_instance_stream_at(&stream, extract_offset);
            pass.draw_instanced(PrimitiveType::Triangle, 6, glow_count as usize)?;
        }
        let halo = match (scissor, clip) {
            (Some([sx0, sy0, sx1, sy1]), _) if sx1 > sx0 && sy1 > sy0 => {
                let pass = cb.render_pass(&RenderPassDesc {
                    target: &off_tex,
                    load: LoadAction::Load,
                    store: StoreAction::Store,
                    viewport: None,
                    scissor: Some(MtlScissorRect {
                        x: sx0 as usize,
                        y: sy0 as usize,
                        width: (sx1 - sx0) as usize,
                        height: (sy1 - sy0) as usize,
                    }),
                })?;
                pass.set_pipeline(&bloom_pso);
                pass.set_fragment_texture(half.obj(), bloom_binds.fragment_textures[0] as usize);
                pass.set_fragment_sampler(linear, bloom_binds.fragment_samplers[0] as usize);
                pass.set_fragment_buffer(&bu, bloom_binds.fragment_buffers[0] as usize);
                pass.draw_fullscreen_triangle()?;
                Some([sx0, sy0, sx1, sy1])
            }
            (Some(_), Some(_)) => Some([0, 0, 0, 0]),
            (Some(_), None) | (None, _) => {
                let pass = cb.render_pass(&RenderPassDesc {
                    target: &off_tex,
                    load: LoadAction::Load,
                    store: StoreAction::Store,
                    viewport: None,
                    scissor: None,
                })?;
                pass.set_pipeline(&bloom_pso);
                pass.set_fragment_texture(half.obj(), bloom_binds.fragment_textures[0] as usize);
                pass.set_fragment_sampler(linear, bloom_binds.fragment_samplers[0] as usize);
                pass.set_fragment_buffer(&bu, bloom_binds.fragment_buffers[0] as usize);
                pass.draw_fullscreen_triangle()?;
                None
            }
        };
        if live.wait_inline(&cb.commit()) != crate::metal::loss::CbOutcome::Completed {
            return Err("in-place bloom command buffer did not complete".to_owned());
        }
        note_offscreen_written(win, halo, (fw, fh));
        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn metal_shimmer_in_place(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
    ) -> Result<(), String> {
        use crate::metal::encoder::{RenderPassDesc, StoreAction};
        use crate::metal::ffi::{self as mtl, LoadAction, MtlScissorRect, PixelFormat as MPix};
        let _pool = mtl::AutoreleasePool::new();
        let (fw, fh, off_tex) = {
            let off = win
                .metal_offscreen
                .as_ref()
                .ok_or("armed frame lacks its metal offscreen")?;
            (off.w, off.h, off.tex.clone_handle())
        };
        let Some(region) = self.shimmer_region(input, fw, fh) else {
            return Ok(());
        };
        let phase = self.shimmer_phase();
        let (cell_w, cell_h) = self.cpu.cell_size();
        let live = self
            .metal_arm
            .as_mut()
            .ok_or("the Metal arm is not live")?
            .live_mut();
        let scratch = live
            .layer()
            .try_create_texture_2d(
                "aterm-gpu metal shimmer scratch",
                crate::device_layer::TexelFormat::Rgba8Unorm,
                fw,
                fh,
                crate::device_layer::TexUsage::OFFSCREEN,
                None,
            )?
            .into_metal();
        let mut heat = [[0f32; 4]; SHIMMER_BANDS / 4];
        for (i, v) in region.heat.iter().enumerate() {
            heat[i / 4][i % 4] = *v;
        }
        let su = ShimmerUniform {
            frame: [fw as f32, fh as f32],
            region_min: [region.x0 as f32, region.y0 as f32],
            region_max: [region.x1 as f32, region.y1 as f32],
            hot_top: region.hot_top,
            rise: region.rise,
            amp: (cell_h as f32 / 18.0).clamp(0.75, SHIMMER_AMP_PX),
            period: (cell_h as f32).max(4.0),
            phase,
            band_x0: region.band_x0,
            band_w: region.band_w,
            rolloff: (cell_w as f32).max(1.0),
            _pad: [0.0; 2],
            heat,
        };
        let ub = {
            let layer = live.layer();
            let buf = layer.try_create_uniform_buffer(
                "aterm-gpu metal shimmer uniform",
                std::mem::size_of::<ShimmerUniform>() as u64,
            )?;
            // SAFETY: fresh exactly-sized shared buffer; no GPU work on it.
            unsafe { layer.write_buffer_from_zero(&buf, aterm_bits::bytes_of(&su)) };
            buf.into_metal().into_obj()
        };
        let m = SHIMMER_COPY_MARGIN;
        let [sx0, sy0, sx1, sy1] = [
            region.x0.saturating_sub(m),
            region.y0.saturating_sub(m),
            region.x1.saturating_add(m).min(fw),
            region.y1.saturating_add(m).min(fh),
        ];
        let pso = live.present_pso(Pipeline::Shimmer, MPix::Rgba8Unorm)?;
        let linear = &live.linear;
        let binds = Pipeline::Shimmer.spec().binds;
        let mut cb = live.session.begin()?;
        cb.copy_texture_sub_rect(
            &off_tex,
            (sx0 as usize, sy0 as usize),
            &scratch,
            (sx0 as usize, sy0 as usize),
            (sx1 - sx0) as usize,
            (sy1 - sy0) as usize,
        )?;
        {
            let pass = cb.render_pass(&RenderPassDesc {
                target: &off_tex,
                load: LoadAction::Load,
                store: StoreAction::Store,
                viewport: None,
                scissor: Some(MtlScissorRect {
                    x: region.x0 as usize,
                    y: region.y0 as usize,
                    width: (region.x1 - region.x0) as usize,
                    height: (region.y1 - region.y0) as usize,
                }),
            })?;
            pass.set_pipeline(&pso);
            pass.set_fragment_texture(scratch.obj(), binds.fragment_textures[0] as usize);
            pass.set_fragment_sampler(linear, binds.fragment_samplers[0] as usize);
            pass.set_fragment_buffer(&ub, binds.fragment_buffers[0] as usize);
            pass.draw_fullscreen_triangle()?;
        }
        if live.wait_inline(&cb.commit()) != crate::metal::loss::CbOutcome::Completed {
            return Err("in-place shimmer command buffer did not complete".to_owned());
        }
        note_offscreen_written(
            win,
            Some([region.x0, region.y0, region.x1, region.y1]),
            (fw, fh),
        );
        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn metal_tray_into_offscreen(
        &mut self,
        win: &mut WindowGpu,
        t: TrayQuad<'_>,
    ) -> Result<(), String> {
        use crate::metal::encoder::{RenderPassDesc, StoreAction};
        use crate::metal::ffi::{self as mtl, LoadAction, PixelFormat as MPix};
        let _pool = mtl::AutoreleasePool::new();
        let (fw, fh, off_tex) = {
            let off = win
                .metal_offscreen
                .as_ref()
                .ok_or("armed frame lacks its metal offscreen")?;
            (off.w, off.h, off.tex.clone_handle())
        };
        note_offscreen_written(win, None, (fw, fh));
        let live = self
            .metal_arm
            .as_mut()
            .ok_or("the Metal arm is not live")?
            .live_mut();
        Self::ensure_metal_tray_card(
            live,
            win,
            &t,
            None,
            &mut self.tray_uploads,
            &mut self.tray_upload_bytes,
        )?;
        let tu = {
            let layer = live.layer();
            let buf = layer.try_create_uniform_buffer(
                "aterm-gpu metal tray uniform",
                std::mem::size_of::<TrayUniform>() as u64,
            )?;
            // SAFETY: fresh exactly-sized shared buffer; no GPU work on it.
            unsafe {
                layer.write_buffer_from_zero(
                    &buf,
                    aterm_bits::bytes_of(&TrayUniform {
                        rect: [t.dx as f32, t.dy as f32, t.pw as f32, t.ph as f32],
                        fb: [fw as f32, fh as f32],
                        _pad: [0.0, 0.0],
                    }),
                );
            }
            buf.into_metal().into_obj()
        };
        let pso = live.present_pso(Pipeline::Tray, MPix::Rgba8Unorm)?;
        let linear = &live.linear;
        let binds = Pipeline::Tray.spec().binds;
        let card = win
            .metal_tray_card
            .as_ref()
            .expect("ensure_metal_tray_card set it above");
        let mut cb = live.session.begin()?;
        {
            let pass = cb.render_pass(&RenderPassDesc {
                target: &off_tex,
                load: LoadAction::Load,
                store: StoreAction::Store,
                viewport: None,
                scissor: None,
            })?;
            pass.set_pipeline(&pso);
            pass.set_vertex_buffer(
                &tu,
                binds.vertex_uniform.expect("row 17 binds at 2") as usize,
            )?;
            pass.set_fragment_texture(card.tex().obj(), binds.fragment_textures[0] as usize);
            pass.set_fragment_sampler(linear, binds.fragment_samplers[0] as usize);
            pass.draw_strip_quad()?;
        }
        if live.wait_inline(&cb.commit()) != crate::metal::loss::CbOutcome::Completed {
            return Err("tray bake command buffer did not complete".to_owned());
        }
        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn metal_ensure_shift_scratch(
        &mut self,
        win: &mut WindowGpu,
        w: u32,
        h: u32,
    ) -> Result<crate::metal::resources::SealedTexture, String> {
        let resident = win
            .metal_shift_scratch
            .as_ref()
            .is_some_and(|t| (t.width(), t.height()) == (w as usize, h as usize));
        if !resident {
            let _pool = crate::metal::ffi::AutoreleasePool::new();
            let cell = self.metal_arm.as_mut().ok_or("the Metal arm is not live")?;
            let live = cell.live_mut();
            win.metal_shift_scratch = Some(
                live.layer()
                    .try_create_texture_2d(
                        "aterm-gpu metal scroll shift scratch",
                        crate::device_layer::TexelFormat::Rgba8Unorm,
                        w,
                        h,
                        crate::device_layer::TexUsage::COPY_STAGING,
                        None,
                    )?
                    .into_metal(),
            );
        }
        Ok(win
            .metal_shift_scratch
            .as_ref()
            .expect("ensured above")
            .clone_handle())
    }

    #[cfg(target_os = "macos")]
    fn metal_band_shift_plan(
        &mut self,
        win: &mut WindowGpu,
        y0: usize,
        y1: usize,
        delta: i64,
    ) -> Option<BandShift> {
        let shift = band_shift_ops(y0, y1, delta)?;
        let (w, h) = win.metal_offscreen.as_ref().map(|o| (o.w, o.h))?;
        if let Err(e) = self.metal_ensure_shift_scratch(win, w, h) {
            metal_arm_note(&format!(
                "armed scroll shift: no scratch ({e}); band unshifted"
            ));
            return None;
        }
        Some(shift)
    }

    #[cfg(target_os = "macos")]
    fn metal_shift_offscreen_band_px(
        &mut self,
        win: &mut WindowGpu,
        y0: usize,
        y1: usize,
        delta: i64,
    ) -> bool {
        use crate::device_layer::{FrameEncoder, SubmittedFrame};
        let Some(shift) = self.metal_band_shift_plan(win, y0, y1, delta) else {
            return false;
        };
        let Some(scratch) = win.metal_shift_scratch.as_ref().map(|t| t.clone_handle()) else {
            return false;
        };
        if let Some(dims) = win.metal_offscreen.as_ref().map(|o| (o.w, o.h)) {
            note_offscreen_written(win, None, dims);
        }
        let Some(off) = win.metal_offscreen.as_ref() else {
            return false;
        };
        let w = off.w;
        let Some(cell) = self.metal_arm.as_mut() else {
            return false;
        };
        let live = cell.live_mut();
        let _pool = crate::metal::ffi::AutoreleasePool::new();
        let staged = (|| -> Result<(), String> {
            let mut enc = FrameEncoder::metal(&live.session)?;
            record_band_shift(&mut enc, &off.tex, &scratch, shift, w)?;
            match enc.submit() {
                SubmittedFrame::Metal(sub) => {
                    let outcome = live.wait_inline(&sub);
                    if outcome != crate::metal::loss::CbOutcome::Completed {
                        return Err(format!("scroll-shift command buffer ended {outcome:?}"));
                    }
                    Ok(())
                }
                #[cfg(wgpu_arm)]
                SubmittedFrame::Wgpu => unreachable!("a metal encoder commits a metal frame"),
            }
        })();
        match staged {
            Ok(()) => true,
            Err(e) => {
                metal_arm_note(&format!("armed scroll shift failed ({e}); band unshifted"));
                false
            }
        }
    }

    #[cfg(all(target_os = "macos", wgpu_arm))]
    fn metal_swapchain_config(
        config: &wgpu::SurfaceConfiguration,
    ) -> Result<crate::metal::swapchain::SwapchainConfig, String> {
        let format = crate::device_layer::TexelFormat::from_wgpu(config.format)
            .ok_or_else(|| {
                format!(
                    "present format {:?} is outside the map's closed set of six",
                    config.format
                )
            })?
            .metal();
        Ok(crate::metal::swapchain::SwapchainConfig {
            format,
            width: config.width as usize,
            height: config.height as usize,
            framebuffer_only: !config.usage.contains(wgpu::TextureUsages::COPY_SRC),
            display_sync: config.present_mode != wgpu::PresentMode::Immediate,
            maximum_drawables: 3,
            opaque: !matches!(
                config.alpha_mode,
                wgpu::CompositeAlphaMode::PostMultiplied | wgpu::CompositeAlphaMode::PreMultiplied
            ),
        })
    }

    /// Drop a window's Metal swapchain once the renderer is disarmed, so its stale layer is
    /// unparented and the wgpu layer beneath shows.
    #[cfg(target_os = "macos")]
    pub(crate) fn metal_disarm_sweep(
        &mut self,
        metal_surf: &mut Option<crate::metal::present::MetalWindowSurface>,
    ) {
        if !self.metal_armed && metal_surf.take().is_some() {
            metal_arm_note("disarmed: dropped this window's armed swapchain; wgpu layer presents");
        }
    }

    /// Test hook.
    #[cfg(all(target_os = "macos", test, wgpu_arm))]
    pub(crate) fn metal_attach_for_test(
        &mut self,
        parent: Option<&crate::metal::ffi::Obj>,
        w: u32,
        h: u32,
    ) -> Option<crate::metal::present::MetalWindowSurface> {
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: wgpu::TextureFormat::Bgra8Unorm,
            width: w,
            height: h,
            present_mode: wgpu::PresentMode::Immediate,
            desired_maximum_frame_latency: 2,
            alpha_mode: wgpu::CompositeAlphaMode::Opaque,
            view_formats: vec![],
        };
        self.metal_attach_surface(parent, &config)
    }

    #[cfg(all(target_os = "macos", wgpu_arm))]
    fn metal_attach_surface(
        &mut self,
        parent: Option<&crate::metal::ffi::Obj>,
        config: &wgpu::SurfaceConfiguration,
    ) -> Option<crate::metal::present::MetalWindowSurface> {
        let attached = (|| -> Result<crate::metal::present::MetalWindowSurface, String> {
            let parent = parent.ok_or("no AppKit view layer to attach under")?;
            if !self.metal_arm_ready() {
                return Err(self.metal_start_error());
            }
            let want = Self::metal_swapchain_config(config)?;
            let live = self
                .metal_arm
                .as_mut()
                .expect("metal_arm_ready above")
                .live_mut();
            let latch = Arc::clone(live.session.latch());
            crate::metal::present::MetalWindowSurface::attached(
                live.mint.device(),
                parent,
                want,
                latch,
            )
        })();
        match attached {
            Ok(ms) => Some(ms),
            Err(e) => {
                metal_arm_note(&format!(
                    "armed attach failed ({e}); disarmed, continuing on wgpu"
                ));
                self.metal_armed = false;
                None
            }
        }
    }

    #[cfg(target_os = "macos")]
    #[allow(
        clippy::too_many_arguments,
        reason = "the shared swapchain seam's established tuple"
    )]
    fn metal_present_input_with_crop(
        &mut self,
        win: &mut WindowGpu,
        surf: &mut GpuSurface,
        input: &RenderInput,
        invert: bool,
        overlay: Option<DropOverlay>,
        tray: Option<TrayQuad<'_>>,
        source_crop: Option<PresentCrop>,
        effects_transport_shift_y: i32,
    ) -> Result<(), SurfacePresentFailure> {
        let out = self.metal_present_input_with_crop_timed(
            win,
            surf,
            input,
            invert,
            overlay,
            tray,
            source_crop,
            effects_transport_shift_y,
        );
        self.metal_seal_gpu_frame();
        out
    }

    #[cfg(target_os = "macos")]
    fn metal_open_gpu_frame(&mut self) {
        self.last_gpu_time_serial = None;
        if !crate::gpu_time::sink_installed() || !self.metal_arm_ready() {
            return;
        }
        self.last_gpu_time_serial = self
            .metal_arm
            .as_ref()
            .and_then(|cell| cell.live_ref().session.open_gpu_frame());
    }

    #[cfg(target_os = "macos")]
    fn metal_seal_gpu_frame(&self) {
        if let Some(cell) = self.metal_arm.as_ref() {
            cell.live_ref().session.seal_gpu_frame();
        }
    }

    #[cfg(target_os = "macos")]
    #[allow(
        clippy::too_many_arguments,
        reason = "the shared swapchain seam's established tuple"
    )]
    fn metal_present_input_with_crop_timed(
        &mut self,
        win: &mut WindowGpu,
        surf: &mut GpuSurface,
        input: &RenderInput,
        invert: bool,
        overlay: Option<DropOverlay>,
        tray: Option<TrayQuad<'_>>,
        source_crop: Option<PresentCrop>,
        effects_transport_shift_y: i32,
    ) -> Result<(), SurfacePresentFailure> {
        let _pool = crate::metal::ffi::AutoreleasePool::new();
        let source_crop = self
            .resident_present_crop(input, source_crop)
            .map_err(|()| SurfacePresentFailure::Validation)?;
        if surf.metal.is_none() {
            metal_arm_note("armed present refused: window has no armed swapchain (re-attach it)");
            return Err(SurfacePresentFailure::Validation);
        }
        #[cfg(wgpu_arm)]
        let (translucent, premult, want) = {
            let want_alpha = self.surface_alpha_mode(surf.post_mult, surf.pre_mult);
            let want_usage = Self::swapchain_usage_for(
                surf.copyable,
                win.wants_copy_src(aterm_time::Instant::now()),
            );
            surf.config.alpha_mode = want_alpha;
            surf.config.usage = want_usage;
            let translucent = matches!(
                want_alpha,
                wgpu::CompositeAlphaMode::PostMultiplied | wgpu::CompositeAlphaMode::PreMultiplied
            );
            let premult = want_alpha == wgpu::CompositeAlphaMode::PreMultiplied;
            let want = match Self::metal_swapchain_config(&surf.config) {
                Ok(w) => w,
                Err(e) => {
                    metal_arm_note(&format!("armed present: config mapping failed ({e})"));
                    return Err(SurfacePresentFailure::Validation);
                }
            };
            (translucent, premult, want)
        };
        #[cfg(not(wgpu_arm))]
        let (translucent, premult, want) = self.metal_present_want(win, surf);
        {
            let Some(cell) = self.metal_arm.as_mut() else {
                metal_arm_note("armed present: the arm is not live");
                return Err(SurfacePresentFailure::Validation);
            };
            let live = cell.live_mut();
            live.drain_pending();
            let (_, ring_parked_ns) = live.take_park_ns();
            win.add_gpu_park(ring_parked_ns);
            let ms = surf.metal.as_mut().expect("checked above");
            let reconciled = ms.reconcile(live.mint.device(), &want);
            if let Some(waited_ns) = ms.take_completed_acquire_wait_ns() {
                win.record_surface_acquire(waited_ns);
            }
            if let Some(queued_ns) = ms.take_completed_acquire_queue_ns() {
                win.record_surface_acquire_queue(queued_ns);
            }
            match reconciled {
                Ok(_) => {}
                Err(e) => {
                    metal_arm_note(&format!("armed present: reconcile failed ({e})"));
                    return Err(SurfacePresentFailure::Reconfigured);
                }
            }
        }
        let hint = win.occluded_hint();
        let (dest_w, dest_h, dest_texel) = surf.neutral_config();
        let ms = surf.metal.as_mut().expect("checked above");
        let acquired = if ms.async_acquire() {
            ms.acquire()
        } else {
            win.measure_surface_acquire(|| ms.acquire())
        };
        let frame = match acquired {
            Ok(f) => f,
            Err(refusal) => {
                return Err(crate::metal::present::surface_present_failure(
                    &refusal, hint,
                ));
            }
        };
        let work_started = aterm_time::Instant::now();
        self.metal_open_gpu_frame();

        let (fw, fh) = self.encode_present_frame(win, input);
        #[allow(
            clippy::needless_bool_assign,
            reason = "symmetry with the wgpu arm's bookkeeping"
        )]
        if !self.last_frame_arm_metal {
            metal_arm_note("armed present dropped: frame encode degraded to wgpu");
            return Err(SurfacePresentFailure::Validation);
        }
        win.resident_input_effects_transport_shift_y = effects_transport_shift_y;
        if tray.is_none() {
            #[cfg(wgpu_arm)]
            {
                win.tray_overlay = None;
            }
            win.metal_tray_card = None;
        }

        let Some(seq) = self
            .metal_arm
            .as_mut()
            .map(|cell| cell.live_mut().take_present_seq())
        else {
            metal_arm_note("armed present: the arm vanished mid-present");
            return Err(SurfacePresentFailure::Validation);
        };
        let target = frame.render_target();
        let (submitted, _composed_present_off) = match self.metal_encode_submit_b(
            win,
            input,
            invert,
            overlay,
            tray,
            Some(seq),
            source_crop,
            (fw, fh),
            (dest_w, dest_h),
            dest_texel,
            translucent,
            premult,
            !want.framebuffer_only,
            &target,
        ) {
            Ok(sub) => sub,
            Err(e) => {
                metal_arm_note(&format!("armed present dropped: Submit B failed ({e})"));
                return Err(SurfacePresentFailure::Validation);
            }
        };
        let (frame_slot_parked_ns, late_ring_parked_ns) = self
            .metal_arm
            .as_mut()
            .map_or((0, 0), |cell| cell.live_mut().take_park_ns());
        win.add_gpu_park(frame_slot_parked_ns.saturating_add(late_ring_parked_ns));
        win.last_present_work_ns = encode_work_ns_excluding_park(
            u64::try_from(work_started.elapsed().as_nanos()).unwrap_or(u64::MAX),
            frame_slot_parked_ns,
        );
        win.last_present_encode_ns = win.last_present_work_ns.saturating_sub(late_ring_parked_ns);
        self.metal_seal_gpu_frame();
        let Some(cell) = self.metal_arm.as_mut() else {
            metal_arm_note("armed present: the arm vanished mid-present");
            return Err(SurfacePresentFailure::Validation);
        };
        let live = cell.live_mut();
        let ticket = match frame.present(&live.session) {
            Ok(t) => t,
            Err(e) => {
                let _ = submitted.wait_outcome();
                metal_arm_note(&format!("armed present: present refused ({e})"));
                return Err(SurfacePresentFailure::Validation);
            }
        };
        live.pending.push(PendingPresent {
            cb: submitted,
            ticket,
            seq,
        });
        if let Some(ms) = surf.metal.as_mut() {
            ms.prefetch_drawable();
        }
        self.metal_maybe_inject_loss();
        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn metal_maybe_inject_loss(&mut self) {
        let Some(n) = self.metal_inject_loss_after.as_mut() else {
            return;
        };
        if *n > 1 {
            *n -= 1;
            return;
        }
        self.metal_inject_loss_after = None;
        let Some(cell) = self.metal_arm.as_mut() else {
            return;
        };
        let outcome = crate::metal::loss::classify(
            crate::metal::loss::STATUS_ERROR,
            Some(crate::metal::loss::ERROR_PAGE_FAULT),
        );
        cell.live_mut().session.latch().record(&outcome);
        crate::stderr_line!(
            "aterm-gpu: ATERM_METAL_INJECT_LOSS fired — recorded the synthetic \
             MTLCommandBufferErrorPageFault into the armed loss latch (drill lever)"
        );
    }

    #[cfg(target_os = "macos")]
    #[allow(
        clippy::too_many_arguments,
        reason = "the armed twin preserves the shared virtual-present seam's tuple"
    )]
    fn metal_present_virtual_with_crop(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
        invert: bool,
        overlay: Option<DropOverlay>,
        tray: Option<TrayQuad<'_>>,
        source_crop: Option<PresentCrop>,
        effects_transport_shift_y: i32,
        destination: (u32, u32),
    ) -> Result<(), String> {
        let out = self.metal_present_virtual_with_crop_timed(
            win,
            input,
            invert,
            overlay,
            tray,
            source_crop,
            effects_transport_shift_y,
            destination,
        );
        self.metal_seal_gpu_frame();
        out
    }

    #[cfg(target_os = "macos")]
    #[allow(
        clippy::too_many_arguments,
        reason = "the armed twin preserves the shared virtual-present seam's tuple"
    )]
    fn metal_present_virtual_with_crop_timed(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
        invert: bool,
        overlay: Option<DropOverlay>,
        tray: Option<TrayQuad<'_>>,
        source_crop: Option<PresentCrop>,
        effects_transport_shift_y: i32,
        destination: (u32, u32),
    ) -> Result<(), String> {
        use crate::metal::ffi as mtl;
        let _pool = mtl::AutoreleasePool::new();
        let source_crop = self
            .resident_present_crop(input, source_crop)
            .map_err(|()| "virtual present crop outside the resident frame")?;
        let w = self.clamp_fb_dim(destination.0.max(1));
        let h = self.clamp_fb_dim(destination.1.max(1));
        #[cfg(wgpu_arm)]
        if win
            .virtual_target
            .as_ref()
            .is_none_or(|t| t.w != w || t.h != h)
        {
            win.virtual_target = Some(self.make_virtual_target(w, h));
        }
        let work_started = aterm_time::Instant::now();
        self.metal_open_gpu_frame();
        let (fw, fh) = self.encode_present_frame(win, input);
        if !self.last_frame_arm_metal {
            return Err("the frame encode degraded to wgpu mid-frame".to_owned());
        }
        win.resident_input_effects_transport_shift_y = effects_transport_shift_y;
        if tray.is_none() {
            #[cfg(wgpu_arm)]
            {
                win.tray_overlay = None;
            }
            win.metal_tray_card = None;
        }
        let needs_dest = win
            .metal_virtual_off
            .as_ref()
            .is_none_or(|t| (t.width(), t.height()) != (w as usize, h as usize));
        if needs_dest {
            let live = self
                .metal_arm
                .as_mut()
                .ok_or("the arm is not live")?
                .live_mut();
            win.metal_virtual_off = Some(
                live.layer()
                    .try_create_texture_2d(
                        "aterm-gpu metal virtual present target",
                        crate::device_layer::TexelFormat::Bgra8Unorm,
                        w,
                        h,
                        crate::device_layer::TexUsage::OFFSCREEN,
                        None,
                    )?
                    .into_metal(),
            );
        }
        let target = win
            .metal_virtual_off
            .as_ref()
            .expect("ensured above")
            .clone_handle();
        let (submitted, _composed_present_off) = self.metal_encode_submit_b(
            win,
            input,
            invert,
            overlay,
            tray,
            None,
            source_crop,
            (fw, fh),
            (w, h),
            VIRTUAL_PRESENT_TEXEL,
            false,
            false,
            cfg!(not(wgpu_arm)),
            &target,
        )?;
        self.metal_seal_gpu_frame();
        let encode_parked_ns = self.metal_arm.as_mut().map_or(0, |cell| {
            let (slot, ring) = cell.live_mut().take_park_ns();
            slot.saturating_add(ring)
        });
        win.last_present_encode_ns = elapsed_ns(work_started).saturating_sub(encode_parked_ns);
        if submitted.wait_outcome() != crate::metal::loss::CbOutcome::Completed {
            return Err("Submit B did not complete".to_owned());
        }
        #[cfg(wgpu_arm)]
        let row = (w as usize) * 4;
        #[cfg(wgpu_arm)]
        let bgra = {
            let live = self
                .metal_arm
                .as_mut()
                .ok_or("the arm is not live")?
                .live_mut();
            let rb = live
                .layer()
                .try_create_readback_buffer(
                    "aterm-gpu metal virtual present readback",
                    (row * h as usize) as u64,
                )?
                .into_metal()
                .into_obj();
            let mut cb = live.session.begin()?;
            cb.copy_texture_to_buffer(&target, w as usize, h as usize, &rb, row)?;
            if cb.commit().wait_outcome() != crate::metal::loss::CbOutcome::Completed {
                return Err("the virtual-present readback copy did not complete".to_owned());
            }
            // SAFETY: shared storage sized `row * h` whose writing command buffer has completed.
            unsafe { mtl::buffer_bytes(&rb, row * h as usize) }
        };
        #[cfg(wgpu_arm)]
        let tex = win
            .virtual_target
            .as_ref()
            .expect("ensured above")
            .tex
            .clone();
        #[cfg(wgpu_arm)]
        {
            self.ctx.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &bgra,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row as u32),
                    rows_per_image: Some(h),
                },
                wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
            );
            let capture_color_space = win.capture_color_space();
            let capture_sdr_white_scale = win.sdr_white_scale();
            let copy_armed = tex.usage().contains(wgpu::TextureUsages::COPY_SRC);
            let mut enc = self
                .ctx
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("aterm armed virtual present tap copy"),
                });
            let mut staged = Vec::new();
            if let Some(tap) = win.video.as_mut().filter(|_| copy_armed) {
                staged.extend(tap.enqueue_copy(
                    &mut enc,
                    &tex,
                    capture_color_space,
                    capture_sdr_white_scale,
                ));
            }
            if let Some(tap) = win.presented_snapshot.as_mut().filter(|_| copy_armed) {
                staged.extend(tap.enqueue_copy(
                    &mut enc,
                    &tex,
                    capture_color_space,
                    capture_sdr_white_scale,
                ));
            }
            crate::device_layer::submit_host_readable(
                &self.ctx.device,
                &self.ctx.queue,
                enc.finish(),
                staged.as_slice(),
            );
        }
        win.virtual_presented_input_epoch = win.resident_input_epoch;
        if let Some(cell) = self.metal_arm.as_mut() {
            let _ = cell.live_mut().take_park_ns();
        }
        win.last_present_work_ns =
            u64::try_from(work_started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        self.metal_maybe_inject_loss();
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "one §2 Submit B transcript, matching the replay's pinned order"
    )]
    fn metal_encode_submit_b(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
        invert: bool,
        overlay: Option<DropOverlay>,
        tray: Option<TrayQuad<'_>>,
        tray_reader: Option<u64>,
        source_crop: Option<PresentCrop>,
        (fw, fh): (u32, u32),
        (dw, dh): (u32, u32),
        dest_format: crate::device_layer::TexelFormat,
        translucent: bool,
        premult: bool,
        copy_armed: bool,
        target: &crate::metal::resources::SealedTexture,
    ) -> Result<(crate::metal::encoder::Submitted, bool), String> {
        use crate::metal::ffi::{self as mtl, PixelFormat as MPix};
        use crate::metal::present::{
            ApronStripPass, BandShiftPass, BloomPasses, CrownPass, PostPass, PresentSequence,
            ShimmerPass, TrayPass, encode_present_sequence,
        };

        let PresentUniforms {
            live_bg,
            want,
            run_hdr_glow,
            run_sdr_glow,
            crown_scissor,
            crown,
        } = self.present_uniforms(
            win,
            input,
            invert,
            overlay,
            source_crop,
            (fw, fh),
            (dw, dh),
            dest_format == crate::device_layer::TexelFormat::Rgba16Float,
            translucent,
            premult,
        );
        let want_bloom = self.enable_bloom && fx_gpu::bloom_source_live(input);
        let want_crown = run_hdr_glow || run_sdr_glow;
        let bloom_glow = (want_bloom || want_crown).then(|| self.build_bloom_glow(input));
        let bloom_state = bloom_glow.filter(|(count, _, _)| want_bloom && *count > 0);
        let crown_count = if want_crown {
            bloom_glow.map_or(0, |(n, _, _)| n)
        } else {
            0
        };
        let text_blend =
            u32::from(self.cpu.text_blending() == aterm_render::TextBlending::LinearCorrected);
        let shimmer_region = if self.shimmer_live(input) {
            self.shimmer_region(input, fw, fh)
        } else {
            None
        };
        let shimmer_phase = self.shimmer_phase();
        let (cell_w, cell_h) = self.cpu.cell_size();
        let (band_y0, band_y1) = aterm_render::scroll_translate::grid_band_px(
            self.cpu.grid_top(),
            cell_h,
            input.grid_top_row,
            input.grid_bot_row,
            fh as usize,
        );
        let band_shift = band_shift_ops(band_y0, band_y1, i64::from(input.scroll_frac_px));
        let band_fx: Option<[u32; 4]> = band_shift.map(|_| [0, band_y0 as u32, fw, band_y1 as u32]);
        let tray_fx: Option<[u32; 4]> = tray.as_ref().map(|t| {
            [
                t.dx.min(fw),
                t.dy.min(fh),
                t.dx.saturating_add(t.pw).min(fw),
                t.dy.saturating_add(t.ph).min(fh),
            ]
        });
        let shift_scratch = match band_shift {
            Some(_) => Some(self.metal_ensure_shift_scratch(win, fw, fh)?),
            None => None,
        };
        let apron_strip = match band_shift {
            Some(_) => match self.apron_strip_plan(win, input, fh as usize) {
                Some(strip) => Some((strip, self.metal_ensure_apron_tex(win, strip)?)),
                None => None,
            },
            None => None,
        };

        let off_tex = win
            .metal_offscreen
            .as_ref()
            .ok_or("the armed frame's offscreen is absent")?
            .tex
            .clone_handle();
        let use_present_off = bloom_state.is_some()
            || shimmer_region.is_some()
            || tray.is_some()
            || band_shift.is_some();
        let needs_off = use_present_off
            && win
                .metal_present_off
                .as_ref()
                .is_none_or(|t| (t.width(), t.height()) != (fw as usize, fh as usize));
        let cell = self.metal_arm.as_mut().ok_or("the Metal arm is not live")?;
        let live = cell.live_mut();
        let mint_scratch = |live: &MetalArmLive, label, w: usize, h: usize| {
            live.layer()
                .try_create_texture_2d(
                    label,
                    crate::device_layer::TexelFormat::Rgba8Unorm,
                    w as u32,
                    h as u32,
                    crate::device_layer::TexUsage::OFFSCREEN,
                    None,
                )
                .map(crate::device_layer::LayerTexture::into_metal)
        };
        if needs_off {
            win.offscreen_dirty_since_sync = None;
            win.present_offscreen_fx = None;
            win.metal_present_off = Some(mint_scratch(
                live,
                "aterm-gpu metal present offscreen",
                fw as usize,
                fh as usize,
            )?);
        }
        let present_off = use_present_off.then(|| {
            win.metal_present_off
                .as_ref()
                .expect("ensured above when `use_present_off`")
                .clone_handle()
        });
        let dest_mpix = dest_format.metal();
        live.advance_submit_b_slot();

        let (bw, bh) = (
            (fw / BLOOM_DOWNSCALE).max(1) as usize,
            (fh / BLOOM_DOWNSCALE).max(1) as usize,
        );
        if bloom_state.is_some()
            && win
                .metal_bloom_half
                .as_ref()
                .is_none_or(|t| (t.width(), t.height()) != (bw, bh))
        {
            win.metal_bloom_half = Some(mint_scratch(live, "aterm-gpu metal bloom half", bw, bh)?);
        }
        let bloom_parts = if let Some((count, bbox, extract_first)) = bloom_state {
            let half = win
                .metal_bloom_half
                .as_ref()
                .expect("ensured above when `bloom_state` is Some")
                .clone_handle();
            let insts: &[GlowInstance] = &self.bloom_glow_scratch;
            let sb_idx = live.submit_b_idx;
            let stream = MetalArmLive::upload_scratch_stream(
                crate::device_layer::DeviceHandle::Metal(&live.mint),
                &mut live.submit_b_slots[sb_idx].glow_stream,
                aterm_bits::cast_slice(insts),
            )?;
            let extract_offset = extract_first as usize * std::mem::size_of::<GlowInstance>();
            let cell_u = MetalArmLive::upload_scratch_stream(
                crate::device_layer::DeviceHandle::Metal(&live.mint),
                &mut live.submit_b_slots[sb_idx].bloom_cell_u,
                aterm_bits::bytes_of(&Uniforms {
                    screen: [fw as f32, fh as f32],
                    text_blend: text_blend as f32,
                    _pad: 0.0,
                }),
            )?;
            let bu = MetalArmLive::upload_scratch_stream(
                crate::device_layer::DeviceHandle::Metal(&live.mint),
                &mut live.submit_b_slots[sb_idx].bloom_u,
                aterm_bits::bytes_of(&BloomUniform {
                    texel: [1.0 / bw as f32, 1.0 / bh as f32],
                    strength: page_bloom_strength(self.bloom_strength, input, self.theme.bg),
                    radius: self.bloom_radius,
                    chroma_radius: bloom_chroma_radius(self.bloom_radius),
                    extent: [1.0; 2],
                    _pad: 0.0,
                }),
            )?;
            let clip = input.fx_clip.map(|(cx0, cy0, cx1, cy1)| {
                [
                    u32::from(cx0),
                    u32::from(cy0),
                    u32::from(cx1),
                    u32::from(cy1),
                ]
            });
            let scissor = if let Some([x0, y0, x1, y1]) = bbox {
                let d = aterm_render::bloom::reach_px(self.bloom_radius);
                Some([
                    x0.saturating_sub(d),
                    y0.saturating_sub(d),
                    x1.saturating_add(d).min(fw),
                    y1.saturating_add(d).min(fh),
                ])
            } else {
                clip.map(|_| [0, 0, fw, fh])
            };
            let scissor = match (scissor, clip) {
                (Some([sx0, sy0, sx1, sy1]), Some([cx0, cy0, cx1, cy1])) => Some([
                    sx0.max(cx0),
                    sy0.max(cy0),
                    sx1.min(cx1).min(fw),
                    sy1.min(cy1).min(fh),
                ]),
                (s, None) => s,
                (None, Some(_)) => unreachable!("clip-only case handled above"),
            }
            .filter(|[sx0, sy0, sx1, sy1]| sx1 > sx0 && sy1 > sy0);
            let extract_pso = live.present_pso(Pipeline::GlowAdd, dest_mpix)?;
            let bloom_pso = live.present_pso(Pipeline::Bloom, dest_mpix)?;
            Some((
                half,
                extract_pso,
                cell_u,
                Pipeline::GlowAdd.spec().binds,
                (stream, extract_offset),
                count as usize,
                bloom_pso,
                bu,
                Pipeline::Bloom.spec().binds,
                scissor,
            ))
        } else {
            None
        };

        if shimmer_region.is_some()
            && win
                .metal_shimmer_scratch
                .as_ref()
                .is_none_or(|t| (t.width(), t.height()) != (fw as usize, fh as usize))
        {
            win.metal_shimmer_scratch = Some(mint_scratch(
                live,
                "aterm-gpu metal shimmer scratch",
                fw as usize,
                fh as usize,
            )?);
        }
        let shimmer_parts = shimmer_region
            .map(|region| -> Result<_, String> {
                let scratch = win
                    .metal_shimmer_scratch
                    .as_ref()
                    .expect("ensured above when `shimmer_region` is Some")
                    .clone_handle();
                let mut heat = [[0f32; 4]; SHIMMER_BANDS / 4];
                for (i, v) in region.heat.iter().enumerate() {
                    heat[i / 4][i % 4] = *v;
                }
                let su = ShimmerUniform {
                    frame: [fw as f32, fh as f32],
                    region_min: [region.x0 as f32, region.y0 as f32],
                    region_max: [region.x1 as f32, region.y1 as f32],
                    hot_top: region.hot_top,
                    rise: region.rise,
                    amp: (cell_h as f32 / 18.0).clamp(0.75, SHIMMER_AMP_PX),
                    period: (cell_h as f32).max(4.0),
                    phase: shimmer_phase,
                    band_x0: region.band_x0,
                    band_w: region.band_w,
                    rolloff: (cell_w as f32).max(1.0),
                    _pad: [0.0; 2],
                    heat,
                };
                let sb_idx = live.submit_b_idx;
                let ub = MetalArmLive::upload_scratch_stream(
                    crate::device_layer::DeviceHandle::Metal(&live.mint),
                    &mut live.submit_b_slots[sb_idx].shimmer_u,
                    aterm_bits::bytes_of(&su),
                )?;
                let m = SHIMMER_COPY_MARGIN;
                let stage = [
                    region.x0.saturating_sub(m),
                    region.y0.saturating_sub(m),
                    region.x1.saturating_add(m).min(fw),
                    region.y1.saturating_add(m).min(fh),
                ];
                let pso = live.present_pso(Pipeline::Shimmer, dest_mpix)?;
                Ok((
                    scratch,
                    stage,
                    pso,
                    ub,
                    Pipeline::Shimmer.spec().binds,
                    [region.x0, region.y0, region.x1, region.y1],
                ))
            })
            .transpose()?;

        if let Some(t) = tray.as_ref() {
            Self::ensure_metal_tray_card(
                live,
                win,
                t,
                tray_reader,
                &mut self.tray_uploads,
                &mut self.tray_upload_bytes,
            )?;
        }
        let tray_parts = tray
            .map(|t| -> Result<_, String> {
                let sb_idx = live.submit_b_idx;
                let tu = MetalArmLive::upload_scratch_stream(
                    crate::device_layer::DeviceHandle::Metal(&live.mint),
                    &mut live.submit_b_slots[sb_idx].tray_u,
                    aterm_bits::bytes_of(&TrayUniform {
                        rect: [t.dx as f32, t.dy as f32, t.pw as f32, t.ph as f32],
                        fb: [fw as f32, fh as f32],
                        _pad: [0.0, 0.0],
                    }),
                )?;
                let pso = live.present_pso(Pipeline::Tray, MPix::Rgba8Unorm)?;
                Ok((tu, pso, Pipeline::Tray.spec().binds))
            })
            .transpose()?;

        let bu = live.ensure_submit_b_blit_u(want)?;
        let blit_pso = live.present_pso(Pipeline::Blit, dest_mpix)?;
        let crown_parts = if let Some(hu) = crown
            && crown_count > 0
        {
            if let Some((x, y, w, h)) = crown_scissor {
                let row = if run_hdr_glow {
                    Pipeline::HdrGlow
                } else {
                    Pipeline::SdrGlow
                };
                let sb_idx = live.submit_b_idx;
                let ubuf = MetalArmLive::upload_scratch_stream(
                    crate::device_layer::DeviceHandle::Metal(&live.mint),
                    &mut live.submit_b_slots[sb_idx].crown_u,
                    aterm_bits::bytes_of(&hu),
                )?;
                let stream = match bloom_parts.as_ref() {
                    Some((_, _, _, _, (bloom_stream, _), ..)) => bloom_stream.clone_retained(),
                    None => MetalArmLive::upload_scratch_stream(
                        crate::device_layer::DeviceHandle::Metal(&live.mint),
                        &mut live.submit_b_slots[sb_idx].glow_stream,
                        aterm_bits::cast_slice(&self.bloom_glow_scratch[..crown_count as usize]),
                    )?,
                };
                let pso = live.present_pso(row, dest_mpix)?;
                let binds = row.spec().binds;
                Some((pso, ubuf, binds, stream, crown_count as usize, (x, y, w, h)))
            } else {
                None
            }
        } else {
            None
        };

        debug_assert_eq!(
            use_present_off,
            bloom_parts.is_some()
                || shimmer_parts.is_some()
                || tray_parts.is_some()
                || band_shift.is_some(),
            "the present-copy gate and the passes that write it must agree"
        );
        let mut lost_compose = false;
        win.metal_present_off_pending
            .retain(|probe| match probe.try_terminal() {
                None => true,
                Some(crate::metal::loss::CbOutcome::Completed) => false,
                Some(_) => {
                    lost_compose = true;
                    false
                }
            });
        if lost_compose {
            win.offscreen_dirty_since_sync = None;
            win.present_offscreen_fx = None;
        }
        let copy_rect = present_copy_rect(
            win.offscreen_dirty_since_sync,
            win.present_offscreen_fx,
            (fw, fh),
        );
        #[cfg(test)]
        {
            win.metal_last_compose_rect = use_present_off.then_some(copy_rect);
        }
        let seq = PresentSequence {
            offscreen: &off_tex,
            present_off: present_off.as_ref(),
            copy_rect,
            bloom: bloom_parts.as_ref().map(
                |(
                    half,
                    extract_pso,
                    cell_u,
                    extract_binds,
                    stream,
                    count,
                    pso,
                    ubuf,
                    binds,
                    sc,
                )| {
                    BloomPasses {
                        half_target: half,
                        extract_pso,
                        cell_uniform: cell_u,
                        cell_uniform_slot: extract_binds
                            .vertex_uniform
                            .expect("row 4 has a vertex uniform")
                            as usize,
                        extract_stream: &stream.0,
                        extract_offset: stream.1,
                        extract_count: *count,
                        composite: PostPass {
                            pso,
                            tex: half.obj(),
                            tex_slot: binds.fragment_textures[0] as usize,
                            sampler: &live.linear,
                            sampler_slot: binds.fragment_samplers[0] as usize,
                            uniform: ubuf,
                            uniform_slot: binds.fragment_buffers[0] as usize,
                        },
                        composite_scissor: *sc,
                    }
                },
            ),
            shimmer: shimmer_parts
                .as_ref()
                .map(|(scratch, stage, pso, ubuf, binds, region)| ShimmerPass {
                    scratch,
                    stage_rect: *stage,
                    refract: PostPass {
                        pso,
                        tex: scratch.obj(),
                        tex_slot: binds.fragment_textures[0] as usize,
                        sampler: &live.linear,
                        sampler_slot: binds.fragment_samplers[0] as usize,
                        uniform: ubuf,
                        uniform_slot: binds.fragment_buffers[0] as usize,
                    },
                    region: *region,
                }),
            band_shift: band_shift
                .zip(shift_scratch.as_ref())
                .map(|(shift, scratch)| BandShiftPass { shift, scratch }),
            apron: apron_strip.as_ref().map(|(strip, tex)| ApronStripPass {
                tex,
                dst_y: strip.dst_y as usize,
                rows: strip.rows as usize,
            }),
            tray: tray_parts.as_ref().map(|(tu, pso, binds)| TrayPass {
                pso,
                card_tex: win
                    .metal_tray_card
                    .as_ref()
                    .expect("ensured alongside the tray above")
                    .tex()
                    .obj(),
                tex_slot: binds.fragment_textures[0] as usize,
                sampler: &live.linear,
                sampler_slot: binds.fragment_samplers[0] as usize,
                uniform: tu,
                uniform_slot: binds.vertex_uniform.expect("row 17 binds at 2") as usize,
            }),
            blit: PostPass {
                pso: &blit_pso,
                tex: match present_off.as_ref() {
                    Some(copy) => copy.obj(),
                    None => off_tex.obj(),
                },
                tex_slot: Pipeline::Blit.spec().binds.fragment_textures[0] as usize,
                sampler: &live.nearest,
                sampler_slot: Pipeline::Blit.spec().binds.fragment_samplers[0] as usize,
                uniform: &bu,
                uniform_slot: Pipeline::Blit.spec().binds.fragment_buffers[0] as usize,
            },
            clear: mtl::ClearColor {
                r: f64::from((live_bg >> 16) & 0xff) / 255.0,
                g: f64::from((live_bg >> 8) & 0xff) / 255.0,
                b: f64::from(live_bg & 0xff) / 255.0,
                a: 1.0,
            },
            crown: crown_parts
                .as_ref()
                .map(|(pso, ubuf, binds, stream, count, scissor)| CrownPass {
                    pso,
                    uniform: ubuf,
                    vertex_uniform_slot: binds
                        .vertex_uniform
                        .expect("the crown rows bind a vertex uniform")
                        as usize,
                    fragment_uniform_slot: binds.fragment_buffers[0] as usize,
                    stream,
                    count: *count,
                    scissor: *scissor,
                }),
        };
        let mut cb = live.session.begin()?;
        encode_present_sequence(&mut cb, target, (dw, dh), &seq)?;
        if use_present_off {
            let mut fx: Option<[u32; 4]> = Some([0, 0, 0, 0]);
            if let Some((.., sc)) = bloom_parts.as_ref() {
                fx = union_rect_opt(fx, *sc, (fw, fh));
            }
            if let Some((.., region)) = shimmer_parts.as_ref() {
                fx = union_rect_opt(fx, Some(*region), (fw, fh));
            }
            for footprint in [band_fx, tray_fx].into_iter().flatten() {
                fx = union_rect_opt(fx, Some(footprint), (fw, fh));
            }
            win.offscreen_dirty_since_sync = Some([0, 0, 0, 0]);
            win.present_offscreen_fx = fx;
        }
        if copy_armed {
            let capture_color_space = win.capture_color_space();
            let capture_sdr_white_scale = win.sdr_white_scale();
            let dest_texel = dest_format;
            if let Some(tap) = win.video.as_mut()
                && let Err(e) = tap.metal_enqueue_copy(
                    &mut cb,
                    target,
                    dest_texel,
                    capture_color_space,
                    capture_sdr_white_scale,
                )
            {
                metal_arm_note(&format!("armed present: video tap copy refused ({e})"));
            }
            if let Some(tap) = win.presented_snapshot.as_mut()
                && let Err(e) = tap.metal_enqueue_copy(
                    &mut cb,
                    target,
                    dest_texel,
                    capture_color_space,
                    capture_sdr_white_scale,
                )
            {
                metal_arm_note(&format!("armed present: snapshot tap copy refused ({e})"));
            }
        }
        let submitted = cb.commit();
        if use_present_off {
            win.metal_present_off_pending
                .push(crate::metal::encoder::CbProbe::of(&submitted));
        }
        if let Some(tap) = win.video.as_mut() {
            tap.metal_note_submitted(crate::metal::encoder::CbProbe::of(&submitted));
        }
        if let Some(tap) = win.presented_snapshot.as_mut() {
            tap.metal_note_submitted(crate::metal::encoder::CbProbe::of(&submitted));
        }
        Ok((submitted, use_present_off))
    }

    /// Test hook.
    #[cfg(all(target_os = "macos", test))]
    pub(crate) fn metal_present_bytes_for_test(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
        (dw, dh): (u32, u32),
        translucent: bool,
    ) -> Result<Vec<u8>, String> {
        self.metal_present_bytes_with_scope_for_test(win, input, (dw, dh), translucent, true)
    }

    /// [`Self::metal_present_bytes_for_test`] with the repaint scope as an axis.
    #[cfg(all(target_os = "macos", test))]
    pub(crate) fn metal_present_bytes_with_scope_for_test(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
        (dw, dh): (u32, u32),
        translucent: bool,
        full_repaint: bool,
    ) -> Result<Vec<u8>, String> {
        use crate::metal::ffi as mtl;
        self.metal_present_standalone_for_test(
            win,
            input,
            (dw, dh),
            (translucent, full_repaint),
            crate::device_layer::TexelFormat::Bgra8Unorm,
            |gpu, target| {
                let live = gpu
                    .metal_arm
                    .as_mut()
                    .ok_or("the arm is not live")?
                    .live_mut();
                let row = (dw * 4) as usize;
                let rb = live
                    .layer()
                    .try_create_readback_buffer(
                        "aterm-gpu metal readback",
                        (row * dh as usize) as u64,
                    )?
                    .into_metal()
                    .into_obj();
                let mut cb = live.session.begin()?;
                cb.copy_texture_to_buffer(target, dw as usize, dh as usize, &rb, row)?;
                if cb.commit().wait_outcome() != crate::metal::loss::CbOutcome::Completed {
                    return Err("the readback copy did not complete".to_owned());
                }
                // SAFETY: shared storage sized `row * dh` whose writing command buffer has
                // completed.
                let bgra = unsafe { mtl::buffer_bytes(&rb, row * dh as usize) };
                let mut rgba = vec![0u8; bgra.len()];
                for (dst, src) in rgba
                    .as_chunks_mut::<4>()
                    .0
                    .iter_mut()
                    .zip(bgra.as_chunks::<4>().0)
                {
                    *dst = [src[2], src[1], src[0], src[3]];
                }
                Ok(rgba)
            },
        )
    }

    /// [`Self::metal_present_bytes_with_scope_for_test`]'s present onto a standalone drawable of
    /// `format` (an EDR window's `Rgba16Float` too).
    #[cfg(all(target_os = "macos", test))]
    pub(crate) fn metal_present_standalone_for_test<T>(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
        (dw, dh): (u32, u32),
        (translucent, full_repaint): (bool, bool),
        format: crate::device_layer::TexelFormat,
        read: impl FnOnce(&mut Self, &crate::metal::resources::SealedTexture) -> Result<T, String>,
    ) -> Result<T, String> {
        use crate::metal::ffi as mtl;
        let _pool = mtl::AutoreleasePool::new();
        if full_repaint {
            win.present_prev = None;
        }
        let (fw, fh) = self.encode_present_frame(win, input);
        if !self.last_frame_arm_metal {
            return Err("the armed encode degraded to wgpu".to_owned());
        }
        let (device, latch) = {
            let live = self
                .metal_arm
                .as_mut()
                .ok_or("the arm is not live")?
                .live_mut();
            (
                live.mint.device().clone_ref(),
                Arc::clone(live.session.latch()),
            )
        };
        let config = crate::metal::swapchain::SwapchainConfig {
            format: format.metal(),
            width: dw as usize,
            height: dh as usize,
            framebuffer_only: false,
            display_sync: false,
            maximum_drawables: 3,
            opaque: !translucent,
        };
        let mut surface =
            crate::metal::present::MetalWindowSurface::standalone(&device, config, latch)?;
        let frame = surface
            .acquire()
            .map_err(|r| format!("acquire refused: {r:?}"))?;
        let target = frame.render_target();
        let (submitted, _composed_present_off) = self.metal_encode_submit_b(
            win,
            input,
            false,
            None,
            None,
            None,
            None,
            (fw, fh),
            (dw, dh),
            format,
            translucent,
            false,
            true,
            &target,
        )?;
        if submitted.wait_outcome() != crate::metal::loss::CbOutcome::Completed {
            return Err("Submit B did not complete".to_owned());
        }
        read(self, &target)
    }

    /// Test probe.
    #[cfg(all(target_os = "macos", test))]
    pub(crate) fn metal_built_for_test(&self) -> Vec<crate::metal::warm::WarmItem> {
        use crate::metal::warm::WarmItem;
        let Some(cell) = self.metal_arm.as_ref() else {
            return Vec::new();
        };
        let live = cell.live_ref();
        let mut built = vec![WarmItem::Library(ShaderLibrary::Cell)];
        built.extend(live.libs.iter().map(|(lib, _)| WarmItem::Library(*lib)));
        for (i, pso) in live.psos.iter().enumerate() {
            if pso.is_some() {
                let row = pipeline_table::ALL_PIPELINES[i];
                built.push(WarmItem::pipeline(
                    row,
                    crate::metal::ffi::PixelFormat::Bgra8Unorm,
                ));
            }
        }
        for ((row, present), _) in &live.present_psos {
            let item = WarmItem::pipeline(*row, *present);
            if !built.contains(&item) {
                built.push(item);
            }
        }
        for twin in pipeline_table::FetchTwin::ALL {
            if live.fetch_psos[twin as usize].is_some() {
                built.push(WarmItem::Twin(twin));
            }
        }
        built
    }

    /// Test hook.
    #[cfg(all(target_os = "macos", test, wgpu_arm))]
    pub(crate) fn wgpu_present_bytes_for_test(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
        (dw, dh): (u32, u32),
        translucent: bool,
    ) -> Vec<u8> {
        let tex = self.ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("w6a translucent-differential destination"),
            size: wgpu::Extent3d {
                width: dw,
                height: dh,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Bgra8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        win.present_prev = None;
        self.present_to_view(
            win,
            input,
            false,
            None,
            None,
            None,
            0,
            PresentDest {
                view: &view,
                tex: &tex,
                w: dw,
                h: dh,
                format: wgpu::TextureFormat::Bgra8Unorm,
                translucent,
                premult: false,
            },
        );
        let bgra = self.texture_bytes_tight_for_test(&tex, dw, dh, 4);
        let mut rgba = vec![0u8; bgra.len()];
        for (dst, src) in rgba
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(bgra.as_chunks::<4>().0)
        {
            *dst = [src[2], src[1], src[0], src[3]];
        }
        rgba
    }

    fn shift_offscreen_band(&mut self, win: &mut WindowGpu, input: &RenderInput) {
        let frac = i64::from(input.scroll_frac_px);
        if frac == 0 {
            return;
        }
        #[cfg(not(any(wgpu_arm, target_os = "macos")))]
        let _ = win;
        #[cfg(target_os = "macos")]
        if self.last_frame_arm_metal {
            let Some(h) = win.metal_offscreen.as_ref().map(|o| o.h) else {
                return;
            };
            let (y0, y1) = aterm_render::scroll_translate::grid_band_px(
                self.cpu.grid_top(),
                self.cell_size().1,
                input.grid_top_row,
                input.grid_bot_row,
                h as usize,
            );
            if self.metal_shift_offscreen_band_px(win, y0, y1, frac) {
                self.apron_into_offscreen_metal(win, input);
            }
        }
        #[cfg(wgpu_arm)]
        {
            let Some(off) = win.offscreen.as_ref() else {
                return;
            };
            let h = off.h;
            let (y0, y1) = aterm_render::scroll_translate::grid_band_px(
                self.cpu.grid_top(),
                self.cell_size().1,
                input.grid_top_row,
                input.grid_bot_row,
                h as usize,
            );
            self.shift_offscreen_band_px(win, y0, y1, frac);
            self.apron_into_offscreen_wgpu(win, input);
        }
    }

    fn shift_offscreen_rows(&mut self, win: &mut WindowGpu, delta_rows: i32, rows: usize) {
        #[cfg(gl_arm)]
        let on_gl = self.last_frame_arm_gl;
        #[cfg(all(not(gl_arm), wgpu_arm))]
        let on_gl = false;
        #[cfg(gl_arm)]
        if on_gl && let Some(h) = win.gl_offscreen.as_ref().map(|o| o.h as usize) {
            let cell_h = self.cell_size().1;
            let grid_top = self.cpu.grid_top();
            let y0 = grid_top.min(h);
            let y1 = grid_top.saturating_add(rows.saturating_mul(cell_h)).min(h);
            self.gl_shift_offscreen_band_px(
                win,
                y0,
                y1,
                i64::from(delta_rows) * cell_h as i64,
                None,
            );
        }
        #[cfg(target_os = "macos")]
        if self.last_frame_arm_metal {
            let Some(h) = win.metal_offscreen.as_ref().map(|o| o.h as usize) else {
                return;
            };
            let cell_h = self.cell_size().1;
            let grid_top = self.cpu.grid_top();
            let y0 = grid_top.min(h);
            let y1 = grid_top.saturating_add(rows.saturating_mul(cell_h)).min(h);
            self.metal_pending_band_shift =
                self.metal_band_shift_plan(win, y0, y1, i64::from(delta_rows) * cell_h as i64);
            if self.metal_pending_band_shift.is_some()
                && let Some(dims) = win.metal_offscreen.as_ref().map(|o| (o.w, o.h))
            {
                note_offscreen_written(win, None, dims);
            }
        }
        #[cfg(wgpu_arm)]
        if !on_gl {
            let Some(h) = win.offscreen.as_ref().map(|o| o.h as usize) else {
                return;
            };
            let cell_h = self.cell_size().1;
            let grid_top = self.cpu.grid_top();
            let y0 = grid_top.min(h);
            let y1 = grid_top.saturating_add(rows.saturating_mul(cell_h)).min(h);
            self.shift_offscreen_band_px(win, y0, y1, i64::from(delta_rows) * cell_h as i64);
        }
    }

    #[cfg(wgpu_arm)]
    fn shift_offscreen_band_px(&mut self, win: &mut WindowGpu, y0: usize, y1: usize, delta: i64) {
        let Some(off) = win.offscreen.as_ref() else {
            return;
        };
        let (w, h) = (off.w, off.h);
        let Some(shift) = band_shift_ops(y0, y1, delta) else {
            return;
        };
        let off_tex = off.tex.clone();
        note_offscreen_written(win, None, (w, h));
        let scratch_ok = win
            .shift_scratch
            .as_ref()
            .is_some_and(|t| t.width() == w && t.height() == h);
        if !scratch_ok {
            let format = crate::device_layer::TexelFormat::from_wgpu(self.ctx.offscreen_format())
                .expect("the offscreen format is inside the map's closed set of six");
            win.shift_scratch = Some(self.ctx.device_layer().create_texture_2d(
                "aterm-gpu m1b scroll shift scratch",
                format,
                w,
                h,
                crate::device_layer::TexUsage::COPY_STAGING,
                None,
            ));
        }
        let scratch = win.shift_scratch.as_ref().expect("just ensured");
        let mut enc = crate::device_layer::FrameEncoder::wgpu(
            &self.ctx.device,
            &self.ctx.queue,
            None,
            "aterm-gpu m1b scroll shift",
        );
        let off_copy = crate::device_layer::FrameCopyTexture::Wgpu(&off_tex);
        let scratch_copy = crate::device_layer::FrameCopyTexture::Wgpu(scratch.wgpu());
        enc.copy_texture_rect(
            off_copy,
            (0, shift.src_y),
            scratch_copy,
            (0, 0),
            w,
            shift.moved,
        )
        .expect("the wgpu copy arm has no fallible verb");
        enc.copy_texture_rect(
            scratch_copy,
            (0, 0),
            off_copy,
            (0, shift.dst_y),
            w,
            shift.moved,
        )
        .expect("the wgpu copy arm has no fallible verb");
        match enc.submit() {
            crate::device_layer::SubmittedFrame::Wgpu => {}
            #[cfg(target_os = "macos")]
            crate::device_layer::SubmittedFrame::Metal(_) => {
                unreachable!("the production scroll shift constructs the Wgpu arm only")
            }
            #[cfg(gl_arm)]
            crate::device_layer::SubmittedFrame::Gl(_) => {
                unreachable!("the production scroll shift constructs the Wgpu arm only")
            }
        }
    }

    /// Create an on-screen presentation surface for `target` at the given pixel size, on the
    /// offscreen renderer's instance and adapter.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn create_window_surface<W: WindowTarget>(
        &mut self,
        target: W,
        width: u32,
        height: u32,
    ) -> Result<GpuSurface, String> {
        #[cfg(target_os = "macos")]
        let metal_parent = if self.metal_armed {
            crate::metal::present::parent_layer_of(&target)
        } else {
            None
        };
        #[cfg(target_os = "macos")]
        {
            let was = std::mem::replace(&mut self.metal_on_screen, true);
            self.present_pass_switched(was, true);
        }
        #[cfg(all(not(wgpu_arm), target_os = "macos"))]
        {
            let format = metal_attach_format(self.hdr_glow, &target);
            let _ = (target, &metal_parent); // `target`'s layer was resolved above.
            let width = self.clamp_fb_dim(width.max(1));
            let height = self.clamp_fb_dim(height.max(1));
            let parent = metal_parent
                .as_ref()
                .ok_or("no AppKit view layer to attach the Metal swapchain under")?;
            self.attach_metal_window_surface(parent, format, width, height)
        }
        #[cfg(all(not(wgpu_arm), gl_arm))]
        {
            self.gl_create_window_surface(target, width, height, None)
        }
        #[cfg(wgpu_arm)]
        {
            #[cfg(gl_arm)]
            if self.gl_armed {
                gl_arm::gl_arm_note(
                    "a window attached on wgpu (the oracle build); GL disarmed for this renderer",
                );
                self.gl_armed = false;
            }
            let surf = self.create_window_surface_wgpu(target, width, height)?;
            #[cfg(target_os = "macos")]
            let surf = {
                let mut surf = surf;
                if self.metal_armed {
                    surf.metal = self.metal_attach_surface(metal_parent.as_ref(), &surf.config);
                }
                surf
            };
            Ok(surf)
        }
    }

    /// [`Self::create_window_surface`] with `sink`, the frontend's presenter ([`FrameSink`]), which
    /// the GL arm posts the window's frames through where a window surface would stall the window
    /// system.
    #[cfg(target_os = "linux")]
    pub fn create_window_surface_with_sink<W: WindowTarget>(
        &mut self,
        target: W,
        width: u32,
        height: u32,
        sink: Box<dyn FrameSink>,
    ) -> Result<GpuSurface, String> {
        #[cfg(all(not(wgpu_arm), gl_arm))]
        {
            self.gl_create_window_surface(target, width, height, Some(sink))
        }
        #[cfg(not(all(not(wgpu_arm), gl_arm)))]
        {
            drop(sink);
            self.create_window_surface(target, width, height)
        }
    }

    #[cfg(wgpu_arm)]
    fn create_window_surface_wgpu<W: WindowTarget>(
        &mut self,
        target: W,
        width: u32,
        height: u32,
    ) -> Result<GpuSurface, String> {
        #[cfg(target_os = "macos")]
        let target = AppKitSurfaceTarget(target);
        #[cfg(target_os = "linux")]
        let target = NativeSurfaceTarget(target);
        let surface = self
            .ctx
            .instance
            .create_surface(target)
            .map_err(|e| format!("create_surface failed: {e}"))?;
        if !self.ctx.adapter.is_surface_supported(&surface) {
            return Err(format!(
                "{} ({}) cannot present to this window",
                self.ctx.adapter_name, self.ctx.backend
            ));
        }
        if self.hdr_glow {
            let caps = surface.get_capabilities(&self.ctx.adapter);
            let supports_f16 = caps.formats.contains(&wgpu::TextureFormat::Rgba16Float);
            if crate::format_plan::hdr_swapchain_wants_f16(self.hdr_glow, supports_f16) {
                let sdr_format = Self::pick_surface_format(&caps)?;
                let format = wgpu::TextureFormat::Rgba16Float;
                self.ensure_blit_pipeline(format);
                let post_mult = Self::caps_support_post_multiplied(&caps);
                let pre_mult = Self::caps_support_pre_multiplied(&caps);
                let (usage, copyable) = Self::surface_usage(&caps);
                let config = wgpu::SurfaceConfiguration {
                    usage,
                    format,
                    width: self.clamp_fb_dim(width),
                    height: self.clamp_fb_dim(height),
                    present_mode: Self::pick_present_mode(&caps),
                    alpha_mode: self.surface_alpha_mode(post_mult, pre_mult),
                    view_formats: vec![],
                    desired_maximum_frame_latency: self.desired_frame_latency(),
                };
                self.configure_first_attach(&surface, &config)?;
                if Self::tag_swapchain_scrgb(&surface) {
                    if crate::verbose() {
                        crate::stderr_line!(
                            "aterm-gpu: EDR swapchain (Rgba16Float, scRGB), present mode = {:?}",
                            config.present_mode
                        );
                    }
                    return Ok(GpuSurface {
                        acquire_id: next_surface_acquire_id(),
                        surface: live_surface::LiveSurface::attached(surface),
                        config,
                        sdr_format,
                        supports_f16: true,
                        #[cfg(wgpu_arm)]
                        last_hdr_probe: Some(aterm_time::Instant::now()),
                        post_mult,
                        #[cfg(wgpu_arm)]
                        pre_mult,
                        copyable,
                        #[cfg(target_os = "macos")]
                        metal: None,
                    });
                }
                if crate::verbose() {
                    crate::stderr_line!(
                        "aterm-gpu: f16 requested but scRGB unsupported (Windows HDR off?) — SDR swapchain"
                    );
                }
            }
        }
        let caps = surface.get_capabilities(&self.ctx.adapter);
        let format = match self.cached_surface_format {
            Some(f) => {
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
        self.ensure_blit_pipeline(format);
        let post_mult = Self::caps_support_post_multiplied(&caps);
        let pre_mult = Self::caps_support_pre_multiplied(&caps);
        let (usage, copyable) = Self::surface_usage(&caps);
        let config = wgpu::SurfaceConfiguration {
            usage,
            format,
            width: self.clamp_fb_dim(width),
            height: self.clamp_fb_dim(height),
            present_mode: Self::pick_present_mode(&caps),
            alpha_mode: self.surface_alpha_mode(post_mult, pre_mult),
            view_formats: vec![],
            desired_maximum_frame_latency: self.desired_frame_latency(),
        };
        self.configure_first_attach(&surface, &config)?;
        if crate::verbose() {
            crate::stderr_line!("aterm-gpu: present mode = {:?}", config.present_mode);
        }
        Ok(GpuSurface {
            acquire_id: next_surface_acquire_id(),
            surface: live_surface::LiveSurface::attached(surface),
            config,
            sdr_format: format,
            supports_f16: caps.formats.contains(&wgpu::TextureFormat::Rgba16Float),
            #[cfg(wgpu_arm)]
            last_hdr_probe: Some(aterm_time::Instant::now()),
            post_mult,
            #[cfg(wgpu_arm)]
            pre_mult,
            copyable,
            #[cfg(target_os = "macos")]
            metal: None,
        })
    }

    #[cfg(wgpu_arm)]
    fn configure_first_attach(
        &self,
        surface: &wgpu::Surface<'static>,
        config: &wgpu::SurfaceConfiguration,
    ) -> Result<(), String> {
        configure_scoped(&self.ctx.device, surface, config).map_err(|err| {
            if self.ctx.visual_swapchain {
                format!("DirectComposition visual swapchain configure failed: {err}")
            } else {
                format!("window surface configure failed: {err}")
            }
        })
    }

    /// Configure the canvas surface [`crate::GpuContext::for_canvas`] made, at `width` x `height`
    /// (clamped like every framebuffer).
    #[cfg(target_arch = "wasm32")]
    pub fn configure_canvas_surface(
        &self,
        surface: crate::web::CanvasSurface,
        width: u32,
        height: u32,
    ) -> Result<GpuSurface, String> {
        if !Arc::ptr_eq(self.ctx.gl_canvas.shared(), surface.0.shared()) {
            return Err("canvas surface belongs to a different GPU context".to_owned());
        }
        let width = self.clamp_fb_dim(width.max(1));
        let height = self.clamp_fb_dim(height.max(1));
        let canvas = crate::gl::canvas::GlCanvasSurface::new(&surface.0, width, height)?;
        Ok(GpuSurface {
            acquire_id: next_surface_acquire_id(),
            neutral: NeutralSurfaceConfig {
                width,
                height,
                format: crate::device_layer::TexelFormat::Rgba8Unorm,
            },
            supports_f16: false,
            post_mult: false,
            copyable: true,
            gl: Some(canvas),
        })
    }

    #[cfg(wgpu_arm)]
    #[inline]
    const fn surface_usage(caps: &wgpu::SurfaceCapabilities) -> (wgpu::TextureUsages, bool) {
        let copyable = caps.usages.contains(wgpu::TextureUsages::COPY_SRC);
        (wgpu::TextureUsages::RENDER_ATTACHMENT, copyable)
    }

    #[must_use]
    #[cfg(any(wgpu_arm, target_os = "macos"))]
    #[inline]
    const fn tap_wants_copy_src(recording: bool, snapshot: bool, held: bool) -> bool {
        recording || snapshot || held
    }

    #[cfg(wgpu_arm)]
    #[inline]
    fn swapchain_usage_for(copyable: bool, recording: bool) -> wgpu::TextureUsages {
        if copyable && recording {
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC
        } else {
            wgpu::TextureUsages::RENDER_ATTACHMENT
        }
    }

    #[cfg(wgpu_arm)]
    fn pick_present_mode(caps: &wgpu::SurfaceCapabilities) -> wgpu::PresentMode {
        // Mailbox where it exists: non-blocking, newest-frame-wins, the lowest-latency mode;
        // input latency outranks animation smoothness.
        if caps.present_modes.contains(&wgpu::PresentMode::Mailbox) {
            return wgpu::PresentMode::Mailbox;
        }
        let forced = aterm_types::dev_seam!("ATERM_GPU_PRESENT_MODE")
            .map(|v| v.to_string_lossy().into_owned());
        match forced.as_deref().map(str::trim) {
            Some("fifo") => {
                crate::stderr_line!(
                    "aterm-gpu: $ATERM_GPU_PRESENT_MODE=fifo — forcing Fifo present. \
                     This RE-ARMS the unbounded nextDrawable() park the default \
                     avoids; input latency regressions of ~84ms have been measured \
                     in this mode. Unset it to restore the shipped default."
                );
                return wgpu::PresentMode::Fifo;
            }
            Some("immediate") if caps.present_modes.contains(&wgpu::PresentMode::Immediate) => {
                crate::stderr_line!(
                    "aterm-gpu: $ATERM_GPU_PRESENT_MODE=immediate — forcing Immediate \
                     present (the shipped default on macOS; an override elsewhere)."
                );
                return wgpu::PresentMode::Immediate;
            }
            Some(other) => {
                crate::stderr_line!(
                    "aterm-gpu: unknown ATERM_GPU_PRESENT_MODE {other:?} \
                     (want fifo|immediate); using the default"
                );
            }
            None => {}
        }
        if cfg!(target_os = "macos") && caps.present_modes.contains(&wgpu::PresentMode::Immediate) {
            wgpu::PresentMode::Immediate
        } else {
            wgpu::PresentMode::Fifo
        }
    }

    #[cfg(wgpu_arm)]
    fn desired_frame_latency(&self) -> u32 {
        Self::default_frame_latency(
            cfg!(target_os = "macos"),
            self.ctx.adapter.get_info().backend == wgpu::Backend::Vulkan,
        )
    }

    /// The frame latency.
    #[cfg(wgpu_arm)]
    #[inline]
    pub(crate) const fn default_frame_latency(is_metal: bool, is_vulkan: bool) -> u32 {
        if is_metal || is_vulkan { 2 } else { 1 }
    }

    #[cfg(wgpu_arm)]
    fn pick_surface_format(
        caps: &wgpu::SurfaceCapabilities,
    ) -> Result<wgpu::TextureFormat, String> {
        use wgpu::TextureFormat::{Bgra8Unorm, Rgba8Unorm};
        if caps.formats.is_empty() {
            return Err("surface offers no formats: the adapter cannot present to it".to_string());
        }
        if caps.formats.contains(&Bgra8Unorm) {
            return Ok(Bgra8Unorm);
        }
        if caps.formats.contains(&Rgba8Unorm) {
            return Ok(Rgba8Unorm);
        }
        caps.formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb() && *f != wgpu::TextureFormat::Rgba16Float)
            .ok_or_else(|| "surface offers no non-sRGB format".to_string())
    }

    #[cfg(wgpu_arm)]
    #[inline]
    fn caps_support_post_multiplied(caps: &wgpu::SurfaceCapabilities) -> bool {
        caps.alpha_modes
            .contains(&wgpu::CompositeAlphaMode::PostMultiplied)
    }

    #[cfg(wgpu_arm)]
    #[inline]
    fn caps_support_pre_multiplied(caps: &wgpu::SurfaceCapabilities) -> bool {
        caps.alpha_modes
            .contains(&wgpu::CompositeAlphaMode::PreMultiplied)
    }

    #[cfg(wgpu_arm)]
    #[inline]
    const fn present_alpha_mode(opacity: f32, post_mult: bool) -> wgpu::CompositeAlphaMode {
        if post_mult && aterm_render::vibrancy::is_translucent(opacity) {
            wgpu::CompositeAlphaMode::PostMultiplied
        } else {
            wgpu::CompositeAlphaMode::Opaque
        }
    }

    #[cfg(wgpu_arm)]
    #[inline]
    fn surface_alpha_mode(&self, post_mult: bool, pre_mult: bool) -> wgpu::CompositeAlphaMode {
        if self.ctx.visual_swapchain {
            return if self.backdrop_margins_active() && pre_mult {
                wgpu::CompositeAlphaMode::PreMultiplied
            } else {
                wgpu::CompositeAlphaMode::Opaque
            };
        }
        Self::present_alpha_mode(self.cpu.background_opacity(), post_mult)
    }

    const BACKDROP_MARGIN_ALPHA: f32 = 0.45;

    #[inline]
    fn backdrop_margin_alpha(&self) -> f32 {
        let glass = f32::from(aterm_render::vibrancy::bg_quad_alpha(
            self.cpu.background_opacity(),
        )) / 255.0;
        if self.backdrop_margins_active() {
            glass.min(Self::BACKDROP_MARGIN_ALPHA)
        } else {
            glass
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the present tuple both arms carry, plus the destination's three format/alpha facts"
    )]
    fn present_uniforms(
        &self,
        win: &mut WindowGpu,
        input: &RenderInput,
        invert: bool,
        overlay: Option<DropOverlay>,
        source_crop: Option<PresentCrop>,
        (fw, fh): (u32, u32),
        (dw, dh): (u32, u32),
        f16_dest: bool,
        translucent: bool,
        premult: bool,
    ) -> PresentUniforms {
        let glow_present = fx_gpu::crown_glow_present(input);
        let plan = crate::format_plan::hdr_present_plan(self.hdr_glow, f16_dest, glow_present);
        let headroom = aterm_render::hdr::additive_headroom(win.edr_max);
        let run_hdr_glow = plan.glow_boost_pass && headroom > 0.0;
        let live_bg = frame_live_bg(input, self.theme.bg);
        let sdr_budget_target = aterm_render::hdr::sdr_glow_budget(
            aterm_render::hdr::packed_luma(live_bg),
            self.sdr_glow_boost,
        );
        let sdr_budget = win.advance_sdr_glow_budget(
            glow_present,
            sdr_budget_target,
            aterm_time::Instant::now(),
        );
        let run_sdr_glow = crate::format_plan::sdr_boost_pass(f16_dest, glow_present, sdr_budget);

        let mut want = present_blit_uniform(invert, overlay, source_crop, fw, fh, dw, dh, live_bg);
        want.chrome_y0 = win.chrome_y0 as f32;
        want.chrome_y1 = win.chrome_y1 as f32;
        want.encode_srgb = if self.ctx.srgb_offscreen { 0.0 } else { 1.0 };
        want.hdr = if plan.blit_linear_encode { 1.0 } else { 0.0 };
        want.sdr_white_scale = if plan.blit_linear_encode {
            win.sdr_white_scale.max(1.0)
        } else {
            1.0
        };
        if translucent {
            want = want.with_translucency(self.backdrop_margin_alpha());
            if premult {
                want = want.with_premultiplied_output();
            }
        }
        let (band_y0, band_y1) = aterm_render::scroll_translate::grid_band_px(
            self.cpu.grid_top(),
            self.cell_size().1,
            input.grid_top_row,
            input.grid_bot_row,
            fh as usize,
        );
        let band_shift = band_shift_ops(band_y0, band_y1, i64::from(input.scroll_frac_px));
        let crown_scissor = crown_band_scissor(&want, band_shift, dw, dh);
        let crown_content_off = match band_shift {
            Some(_) => [
                want.content_off[0],
                want.content_off[1] - input.scroll_frac_px as f32,
            ],
            None => want.content_off,
        };
        let crown = if run_hdr_glow {
            Some(HdrGlowUniform {
                screen: [dw as f32, dh as f32],
                content_off: crown_content_off,
                boost: aterm_render::hdr::HDR_GLOW_BOOST * win.sdr_white_scale.max(1.0),
                headroom: headroom * win.sdr_white_scale.max(1.0),
                _pad: [0.0, 0.0],
            })
        } else if run_sdr_glow {
            Some(HdrGlowUniform {
                screen: [dw as f32, dh as f32],
                content_off: crown_content_off,
                boost: 1.0,
                headroom: sdr_budget,
                _pad: [0.0, 0.0],
            })
        } else {
            None
        };
        PresentUniforms {
            live_bg,
            want,
            run_hdr_glow,
            run_sdr_glow,
            crown_scissor,
            crown,
        }
    }

    /// Resize an on-screen surface and reconfigure it.
    pub fn resize_surface(
        &mut self,
        win: &mut WindowGpu,
        surf: &mut GpuSurface,
        width: u32,
        height: u32,
    ) {
        let w = self.clamp_fb_dim(width);
        let h = self.clamp_fb_dim(height);
        #[cfg(not(wgpu_arm))]
        {
            let _ = win;
            if surf.neutral.width == w && surf.neutral.height == h {
                return;
            }
            surf.discard_pending_acquire();
            surf.neutral.width = w;
            surf.neutral.height = h;
            #[cfg(target_arch = "wasm32")]
            if let Some(gs) = surf.gl.as_mut() {
                gs.resize(w, h);
            }
        }
        #[cfg(wgpu_arm)]
        {
            if surf.config.width == w && surf.config.height == h {
                return;
            }
            surf.discard_pending_acquire();
            surf.config.width = w;
            surf.config.height = h;
            self.configure_surface_retagging_scrgb(win, surf, "resize");
        }
    }

    #[cfg(all(not(wgpu_arm), target_os = "macos"))]
    fn metal_present_want(
        &self,
        win: &WindowGpu,
        surf: &GpuSurface,
    ) -> (bool, bool, crate::metal::swapchain::SwapchainConfig) {
        let translucent = self.cpu.background_opacity() < 1.0 && surf.post_mult;
        let copy = surf.copyable && win.wants_copy_src(aterm_time::Instant::now());
        (
            translucent,
            false,
            crate::metal::swapchain::SwapchainConfig {
                format: surf.neutral.format.metal(),
                width: surf.neutral.width as usize,
                height: surf.neutral.height as usize,
                framebuffer_only: !copy,
                display_sync: false,
                maximum_drawables: 3,
                opaque: !translucent,
            },
        )
    }

    #[cfg(all(not(wgpu_arm), target_os = "macos"))]
    fn attach_metal_window_surface(
        &mut self,
        parent: &crate::metal::ffi::Obj,
        format: crate::device_layer::TexelFormat,
        width: u32,
        height: u32,
    ) -> Result<GpuSurface, String> {
        if !self.metal_arm_ready() {
            return Err(self.metal_start_error());
        }
        let want = crate::metal::swapchain::SwapchainConfig {
            format: format.metal(),
            width: width as usize,
            height: height as usize,
            framebuffer_only: true,
            display_sync: false,
            maximum_drawables: 3,
            opaque: self.cpu.background_opacity() >= 1.0,
        };
        let live = self
            .metal_arm
            .as_mut()
            .expect("metal_arm_ready above")
            .live_mut();
        let latch = Arc::clone(live.session.latch());
        let ms = crate::metal::present::MetalWindowSurface::attached(
            live.mint.device(),
            parent,
            want,
            latch,
        )?;
        Ok(GpuSurface {
            acquire_id: next_surface_acquire_id(),
            neutral: NeutralSurfaceConfig {
                width,
                height,
                format,
            },
            supports_f16: true,
            post_mult: true,
            copyable: true,
            metal: Some(ms),
        })
    }

    /// Re-pick the swapchain format for the screen the window now sits on (macOS Metal arm).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn upgrade_surface_for_screen<W: WindowTarget>(
        &mut self,
        win: &mut WindowGpu,
        surf: &mut GpuSurface,
        target: &W,
    ) -> bool {
        #[cfg(all(not(wgpu_arm), gl_arm))]
        {
            let _ = (win, target);
            debug_assert!(!surf.supports_f16, "a GL surface offers no f16 format");
            surf.supports_f16 && surf.is_hdr()
        }
        #[cfg(all(not(wgpu_arm), target_os = "macos"))]
        {
            self.upgrade_surface_on_screen(win, surf, || {
                crate::metal::present::screen_edr_potential_of(target)
            })
        }
        #[cfg(wgpu_arm)]
        {
            let _ = (win, surf, target);
            false
        }
    }

    #[cfg(all(not(wgpu_arm), target_os = "macos"))]
    fn upgrade_surface_on_screen(
        &mut self,
        win: &mut WindowGpu,
        surf: &mut GpuSurface,
        read_potential: impl FnOnce() -> Option<f32>,
    ) -> bool {
        // The decision reads the screen only past the cheap gates: f16 surfaces
        // and gate-refused windows never pay the AppKit read.
        if !crate::format_plan::hdr_screen_upgrade_decision(
            self.hdr_glow,
            surf.supports_f16,
            surf.is_hdr(),
            read_potential,
        ) {
            return false;
        }
        let previous = surf.neutral.format;
        surf.neutral.format = crate::device_layer::TexelFormat::Rgba16Float;
        let (_, _, want) = self.metal_present_want(win, surf);
        let flipped = match (self.metal_arm.as_mut(), surf.metal.as_mut()) {
            (Some(cell), Some(ms)) => {
                let live = cell.live_mut();
                live.drain_pending();
                let device = live.mint.device().clone_ref();
                ms.reconcile(&device, &want).and_then(|_| {
                    if ms.config().format == want.format {
                        Ok(())
                    } else {
                        Err("a drawable acquisition is in flight, so the layer was not \
                             reconfigured in this call"
                            .to_owned())
                    }
                })
            }
            (None, _) => Err("the arm is not live".to_owned()),
            (_, None) => Err("this window has no armed swapchain".to_owned()),
        };
        if let Err(e) = flipped {
            surf.neutral.format = previous;
            metal_arm_note(&format!(
                "screen re-pick: f16 flip refused ({e}); keeping 8-bit swapchain"
            ));
            return false;
        }
        win.apply_hdr_surface_upgrade();
        true
    }

    #[cfg(wgpu_arm)]
    fn configure_surface_retagging_scrgb(
        &mut self,
        win: &mut WindowGpu,
        surf: &mut GpuSurface,
        reason: &'static str,
    ) {
        let was_hdr = surf.is_hdr();
        let scrgb_retagged =
            match surf
                .surface
                .configure_retagging(&self.ctx.device, &surf.config, was_hdr)
            {
                Ok(tagged) => tagged,
                Err(err) => {
                    self.ctx
                        .latch_surface_loss(format!("the {reason} configure was rejected: {err}"));
                    return;
                }
            };
        self.finish_surface_color_space_recovery(
            win,
            surf,
            was_hdr,
            scrgb_retagged,
            reason,
            aterm_time::Instant::now(),
        );
    }

    #[cfg(wgpu_arm)]
    fn finish_surface_color_space_recovery(
        &mut self,
        win: &mut WindowGpu,
        surf: &mut GpuSurface,
        was_hdr: bool,
        scrgb_retagged: bool,
        reason: &'static str,
        validated_at: aterm_time::Instant,
    ) {
        let plan = crate::format_plan::hdr_reconfigure_plan(was_hdr, scrgb_retagged);
        if plan == crate::format_plan::HdrReconfigurePlan::FallbackToSdr {
            debug_assert_ne!(
                surf.sdr_format,
                wgpu::TextureFormat::Rgba16Float,
                "the retained fallback must be an SDR format"
            );
            self.ensure_blit_pipeline(surf.sdr_format);
            surf.config.format = surf.sdr_format;
            if let Err(err) =
                surf.surface
                    .configure_sdr_escape(&self.ctx.device, &surf.config, plan)
            {
                self.ctx.latch_surface_loss(format!(
                    "the SDR fallback configure after {reason} was rejected: {err}"
                ));
                return;
            }
            self.cached_surface_format = Some(surf.sdr_format);
            if crate::verbose() {
                crate::stderr_line!(
                    "aterm-gpu: scRGB re-tag failed after {reason}; fell back to {:?}",
                    surf.sdr_format
                );
            }
        }
        win.apply_hdr_reconfigure_plan(plan);
        surf.last_hdr_probe = Some(validated_at);
    }

    #[cfg(wgpu_arm)]
    fn reconcile_live_hdr_state_if_due(&mut self, win: &mut WindowGpu, surf: &mut GpuSurface) {
        if !cfg!(windows) {
            return;
        }
        if !surf.is_hdr() && !(self.hdr_glow && surf.supports_f16) {
            return;
        }
        let now = aterm_time::Instant::now();
        if !hdr_color_space_probe_due(surf.last_hdr_probe, now) {
            return;
        }
        if surf.is_hdr() {
            let scrgb_retagged = surf.surface.retag();
            self.finish_surface_color_space_recovery(
                win,
                surf,
                true,
                scrgb_retagged,
                "live HDR-state revalidation",
                now,
            );
            return;
        }

        let output_hdr_enabled = surf.surface.output_hdr_enabled();
        if !crate::format_plan::hdr_live_upgrade_wants_f16(
            self.hdr_glow,
            surf.supports_f16,
            output_hdr_enabled,
        ) {
            surf.last_hdr_probe = Some(now);
            return;
        }
        self.ensure_blit_pipeline(wgpu::TextureFormat::Rgba16Float);
        surf.config.format = wgpu::TextureFormat::Rgba16Float;
        self.configure_surface_retagging_scrgb(win, surf, "live HDR-state upgrade");
        if surf.is_hdr() {
            win.apply_hdr_surface_upgrade();
        }
    }

    #[cfg(not(windows))]
    #[cfg(wgpu_arm)]
    const fn surface_output_hdr_enabled(_surface: &wgpu::Surface) -> bool {
        false
    }

    #[cfg(windows)]
    #[cfg(wgpu_arm)]
    fn dx12_swap_chain(surface: &wgpu::Surface) -> Option<aterm_win32::com::Com> {
        // SAFETY: the hal-surface guard borrows the live surface only for this scope and destroys
        // nothing through it. `swap_chain()` returns wgpu-hal's `windows` `IDXGISwapChain3` by
        // value, an `AddRef`'d clone: one non-null interface pointer, `#[repr(transparent)]`,
        // whose drop does nothing but release it, which `Com` takes over and releases once
        // instead. The surface keeps its own reference.
        unsafe {
            let hal_surf = surface.as_hal::<wgpu::hal::api::Dx12>()?;
            Some(aterm_win32::com::Com::from_interface(
                hal_surf.swap_chain()?,
            ))
        }
    }

    #[cfg(windows)]
    #[cfg(wgpu_arm)]
    fn surface_output_hdr_enabled(surface: &wgpu::Surface) -> bool {
        use aterm_win32::com::Com;
        use aterm_win32::dxgi::{
            DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020, DXGI_OUTPUT_DESC1, IDXGIOutput6_Vtbl,
            IDXGISwapChain3_Vtbl, IID_IDXGIOutput6,
        };

        let Some(sc) = Self::dx12_swap_chain(surface) else {
            return false;
        };
        let mut out = core::ptr::null_mut();
        // SAFETY: `sc` is a live `IDXGISwapChain3` and `GetContainingOutput` writes one owned
        // `IDXGIOutput` on success.
        let hr = unsafe {
            (sc.vtable::<IDXGISwapChain3_Vtbl>().GetContainingOutput)(sc.as_raw(), &mut out)
        };
        // SAFETY: as above.
        let Some(output) = (hr >= 0).then(|| unsafe { Com::from_raw(out) }).flatten() else {
            return false;
        };
        let Ok(output) = output.query(&IID_IDXGIOutput6) else {
            return false;
        };
        let mut desc = DXGI_OUTPUT_DESC1::default();
        // SAFETY: `output` is an IDXGIOutput6; GetDesc1 fills `desc`.
        let hr =
            unsafe { (output.vtable::<IDXGIOutput6_Vtbl>().GetDesc1)(output.as_raw(), &mut desc) };
        hr >= 0 && desc.ColorSpace == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020
    }

    #[cfg(target_os = "macos")]
    #[cfg(wgpu_arm)]
    const fn tag_swapchain_scrgb(_surface: &wgpu::Surface) -> bool {
        true
    }

    #[cfg(all(not(target_os = "macos"), not(windows)))]
    #[cfg(wgpu_arm)]
    fn tag_swapchain_scrgb(_surface: &wgpu::Surface) -> bool {
        false
    }

    #[cfg(windows)]
    #[cfg(wgpu_arm)]
    fn tag_swapchain_scrgb(surface: &wgpu::Surface) -> bool {
        use aterm_win32::dxgi::{
            DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709,
            DXGI_SWAP_CHAIN_COLOR_SPACE_SUPPORT_FLAG_PRESENT, IDXGISwapChain3_Vtbl,
        };
        if !Self::surface_output_hdr_enabled(surface) {
            return false;
        }
        let Some(sc) = Self::dx12_swap_chain(surface) else {
            return false;
        };
        // SAFETY: `sc` is an IDXGISwapChain3, held for this scope.
        let vt = unsafe { sc.vtable::<IDXGISwapChain3_Vtbl>() };
        let cs = DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709;
        let mut flags = 0u32;
        // SAFETY: result-checked calls on the live swap chain; `flags` is written only on success.
        let supported = unsafe { (vt.CheckColorSpaceSupport)(sc.as_raw(), cs, &mut flags) } >= 0
            && flags & DXGI_SWAP_CHAIN_COLOR_SPACE_SUPPORT_FLAG_PRESENT as u32 != 0;
        // SAFETY: as above.
        supported && unsafe { (vt.SetColorSpace1)(sc.as_raw(), cs) } >= 0
    }

    /// Render the frame offscreen and present it by blitting that texture into `surf`'s swapchain.
    #[inline]
    pub fn present_input(
        &mut self,
        win: &mut WindowGpu,
        surf: &mut GpuSurface,
        input: &RenderInput,
        invert: bool,
        overlay: Option<DropOverlay>,
        tray: Option<TrayQuad<'_>>,
    ) -> Result<(), SurfacePresentFailure> {
        self.present_input_with_crop(win, surf, input, invert, overlay, tray, None, 0)
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the cropped public twin preserves the present tuple and records one transport translation"
    )]
    #[inline]
    pub fn present_input_cropped_with_effects_transport(
        &mut self,
        win: &mut WindowGpu,
        surf: &mut GpuSurface,
        input: &RenderInput,
        invert: bool,
        overlay: Option<DropOverlay>,
        tray: Option<TrayQuad<'_>>,
        crop: PresentCrop,
        effects_transport_shift_y: i32,
    ) -> Result<(), SurfacePresentFailure> {
        self.present_input_with_crop(
            win,
            surf,
            input,
            invert,
            overlay,
            tray,
            Some(crop),
            effects_transport_shift_y,
        )
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
        effects_transport_shift_y: i32,
    ) -> Result<(), SurfacePresentFailure> {
        self.cpu
            .checked_frame_size(input.rows, input.cols)
            .map_err(|_| SurfacePresentFailure::Validation)?;
        win.begin_surface_present();
        #[cfg(target_os = "macos")]
        self.metal_disarm_sweep(&mut surf.metal);
        #[cfg(target_os = "macos")]
        if self.metal_armed {
            return self.metal_present_input_with_crop(
                win,
                surf,
                input,
                invert,
                overlay,
                tray,
                source_crop,
                effects_transport_shift_y,
            );
        }
        #[cfg(all(gl_arm, not(wgpu_arm)))]
        if self.gl_armed {
            return self.gl_present_input_with_crop(
                win,
                surf,
                input,
                invert,
                overlay,
                tray,
                source_crop,
                effects_transport_shift_y,
            );
        }
        #[cfg(not(wgpu_arm))]
        {
            first_party_note("present refused: renderer disarmed, no wgpu arm in this build");
            Err(SurfacePresentFailure::Validation)
        }
        #[cfg(wgpu_arm)]
        {
            #[cfg(windows)]
            if win.parks_while_minimized() {
                return Err(SurfacePresentFailure::Occluded);
            }
            let source_crop = self
                .resident_present_crop(input, source_crop)
                .map_err(|()| SurfacePresentFailure::Validation)?;
            let want_alpha = self.surface_alpha_mode(surf.post_mult, surf.pre_mult);
            let want_usage = Self::swapchain_usage_for(
                surf.copyable,
                win.wants_copy_src(aterm_time::Instant::now()),
            );
            if surf.config.alpha_mode != want_alpha || surf.config.usage != want_usage {
                surf.config.alpha_mode = want_alpha;
                surf.config.usage = want_usage;
                self.configure_surface_retagging_scrgb(win, surf, "composite-alpha/usage change");
            }
            self.reconcile_live_hdr_state_if_due(win, surf);
            if self.ctx.device_lost() {
                return Err(SurfacePresentFailure::Validation);
            }

            use wgpu::CurrentSurfaceTexture as C;
            let acquired = win.measure_surface_acquire(|| surf.surface.get_current_texture());
            let frame = match acquired {
                C::Success(f) | C::Suboptimal(f) => f,
                C::Outdated | C::Lost => {
                    self.configure_surface_retagging_scrgb(win, surf, "surface loss recovery");
                    return Err(SurfacePresentFailure::Reconfigured);
                }
                C::Timeout => return Err(SurfacePresentFailure::Timeout),
                C::Occluded => return Err(SurfacePresentFailure::Occluded),
                C::Validation => return Err(SurfacePresentFailure::Validation),
            };
            let view = frame
                .texture
                .create_view(&wgpu::TextureViewDescriptor::default());
            let work_started = aterm_time::Instant::now();
            self.present_to_view(
                win,
                input,
                invert,
                overlay,
                tray,
                source_crop,
                effects_transport_shift_y,
                PresentDest {
                    view: &view,
                    tex: &frame.texture,
                    w: surf.config.width,
                    h: surf.config.height,
                    format: surf.config.format,
                    translucent: matches!(
                        want_alpha,
                        wgpu::CompositeAlphaMode::PostMultiplied
                            | wgpu::CompositeAlphaMode::PreMultiplied
                    ),
                    premult: want_alpha == wgpu::CompositeAlphaMode::PreMultiplied,
                },
            );
            win.last_present_work_ns =
                u64::try_from(work_started.elapsed().as_nanos()).unwrap_or(u64::MAX);
            win.last_present_encode_ns = win.last_present_work_ns;
            frame.present();
            Ok(())
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the common application-present/headless encoder needs the existing frame effects, source crop, and typed destination"
    )]
    #[cfg(wgpu_arm)]
    fn present_to_view(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
        invert: bool,
        overlay: Option<DropOverlay>,
        tray: Option<TrayQuad<'_>>,
        source_crop: Option<PresentCrop>,
        effects_transport_shift_y: i32,
        dest: PresentDest<'_>,
    ) {
        let tray_over_copy = tray.is_some() || win.tray_overlay.is_some();
        let frac = input.scroll_frac_px;
        self.wgpu_gpu_time_begin();
        let (fw, fh) = self.encode_present_frame(win, input);
        win.resident_input_effects_transport_shift_y = effects_transport_shift_y;

        let bloom_glow_present = self.enable_bloom && fx_gpu::bloom_source_live(input);
        let shimmer_present = self.shimmer_live(input);
        let fx_present = bloom_glow_present || shimmer_present;
        let mut ungated_built: Option<u32> = None;

        if tray.is_none() {
            win.tray_overlay = None;
            #[cfg(target_os = "macos")]
            {
                win.metal_tray_card = None;
            }
        }

        let use_present_off = fx_present || tray_over_copy || frac != 0;
        let mut enc = self
            .ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aterm-gpu present (composite + blit)"),
            });
        if use_present_off && let Some(n) = self.compose_present_offscreen(win, input, &mut enc) {
            ungated_built = Some(n);
        }
        if frac != 0 {
            self.shift_present_copy_band(win, input, &mut enc);
        }
        if tray_over_copy && let Some(t) = tray {
            self.draw_tray_over_present_copy(win, t, &mut enc);
        }

        let format = dest.format;
        self.ensure_blit_pipeline(format);
        let PresentUniforms {
            live_bg,
            want,
            run_hdr_glow,
            run_sdr_glow,
            crown_scissor,
            crown,
        } = self.present_uniforms(
            win,
            input,
            invert,
            overlay,
            source_crop,
            (fw, fh),
            (dest.w, dest.h),
            format == wgpu::TextureFormat::Rgba16Float,
            dest.translucent,
            dest.premult,
        );
        if run_hdr_glow {
            self.ensure_hdr_glow_pipeline();
        }
        if run_sdr_glow {
            self.ensure_sdr_glow_pipeline(format);
        }
        let crown_count = if run_hdr_glow || run_sdr_glow {
            match ungated_built {
                Some(n) => n,
                None => self.build_bloom_glow(input).0,
            }
        } else {
            0
        };
        let pipeline = &self.blit_pipelines[&format];

        if self.blit_uniform_written != Some(want) {
            self.ctx
                .queue
                .write_buffer(&self.blit_uniform_buf, 0, aterm_bits::bytes_of(&want));
            self.blit_uniform_written = Some(want);
        }
        if let Some(hu) = crown {
            self.ctx.queue.write_buffer(
                self.hdr_glow_uniform_buf.as_ref().expect(
                    "ensure_hdr_glow_pipeline / ensure_sdr_glow_pipeline set the uniform buf",
                ),
                0,
                aterm_bits::bytes_of(&hu),
            );
        }
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

        let (sw, sh) = (dest.w, dest.h);
        let bg = live_bg;
        let clear = wgpu::Color {
            r: ((bg >> 16) & 0xff) as f64 / 255.0,
            g: ((bg >> 8) & 0xff) as f64 / 255.0,
            b: (bg & 0xff) as f64 / 255.0,
            a: 1.0,
        };
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("aterm-gpu blit pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: dest.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(clear),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: self.wgpu_pass_timestamps(),
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, bind, &[]);
            pass.set_viewport(0.0, 0.0, sw as f32, sh as f32, 0.0, 1.0);
            pass.set_scissor_rect(0, 0, sw, sh);
            pass.draw(0..3, 0..1);
            if run_hdr_glow
                && crown_count > 0
                && let Some((x, y, width, height)) = crown_scissor
            {
                pass.set_scissor_rect(x, y, width, height);
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
            } else if run_sdr_glow
                && crown_count > 0
                && let Some((x, y, width, height)) = crown_scissor
            {
                pass.set_scissor_rect(x, y, width, height);
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
        let capture_color_space = win.capture_color_space();
        let capture_sdr_white_scale = win.sdr_white_scale();
        let copy_armed = dest.tex.usage().contains(wgpu::TextureUsages::COPY_SRC);
        let mut staged = Vec::new();
        if let Some(tap) = win.video.as_mut().filter(|_| copy_armed) {
            staged.extend(tap.enqueue_copy(
                &mut enc,
                dest.tex,
                capture_color_space,
                capture_sdr_white_scale,
            ));
        }
        if let Some(tap) = win.presented_snapshot.as_mut().filter(|_| copy_armed) {
            staged.extend(tap.enqueue_copy(
                &mut enc,
                dest.tex,
                capture_color_space,
                capture_sdr_white_scale,
            ));
        }
        crate::device_layer::submit_host_readable(
            &self.ctx.device,
            &self.ctx.queue,
            enc.finish(),
            &staged,
        );
        self.wgpu_gpu_time_finish();
    }

    #[cfg(wgpu_arm)]
    fn shift_present_copy_band(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
        enc: &mut wgpu::CommandEncoder,
    ) {
        let Some((w, h)) = win.present_offscreen.as_ref().map(|p| (p.w, p.h)) else {
            return;
        };
        let (y0, y1) = aterm_render::scroll_translate::grid_band_px(
            self.cpu.grid_top(),
            self.cell_size().1,
            input.grid_top_row,
            input.grid_bot_row,
            h as usize,
        );
        let Some(shift) = band_shift_ops(y0, y1, i64::from(input.scroll_frac_px)) else {
            return;
        };
        let scratch_ok = win
            .shift_scratch
            .as_ref()
            .is_some_and(|t| t.width() == w && t.height() == h);
        if !scratch_ok {
            let format = crate::device_layer::TexelFormat::from_wgpu(self.ctx.offscreen_format())
                .expect("the offscreen format is inside the map's closed set of six");
            win.shift_scratch = Some(self.ctx.device_layer().create_texture_2d(
                "aterm-gpu m1b scroll shift scratch",
                format,
                w,
                h,
                crate::device_layer::TexUsage::COPY_STAGING,
                None,
            ));
        }
        let apron = self.apron_strip_plan(win, input, h as usize);
        if let Some(strip) = apron {
            self.ensure_apron_texture(win, strip);
        }
        win.present_offscreen_fx = union_rect_opt(
            win.present_offscreen_fx,
            Some([0, y0 as u32, w, y1 as u32]),
            (w, h),
        );
        let po = win.present_offscreen.as_ref().expect("dims read above");
        let scratch = win.shift_scratch.as_ref().expect("just ensured").wgpu();
        const fn rect(tex: &wgpu::Texture, y: u32) -> wgpu::TexelCopyTextureInfo<'_> {
            wgpu::TexelCopyTextureInfo {
                texture: tex,
                mip_level: 0,
                origin: wgpu::Origin3d { x: 0, y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            }
        }
        let extent = wgpu::Extent3d {
            width: w,
            height: shift.moved,
            depth_or_array_layers: 1,
        };
        enc.copy_texture_to_texture(rect(&po.tex, shift.src_y), rect(scratch, 0), extent);
        enc.copy_texture_to_texture(rect(scratch, 0), rect(&po.tex, shift.dst_y), extent);
        if let Some(strip) = apron
            && let Some(ap) = win.apron_tex.as_ref()
        {
            enc.copy_texture_to_texture(
                rect(ap.wgpu(), 0),
                rect(&po.tex, strip.dst_y),
                wgpu::Extent3d {
                    width: strip.w.min(w),
                    height: strip.rows,
                    depth_or_array_layers: 1,
                },
            );
        }
    }

    fn apron_strip_plan(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
        h: usize,
    ) -> Option<ApronStrip> {
        if !aterm_render::scroll_translate::incoming_row_applies(input) {
            return None;
        }
        let cell_h = self.cell_size().1;
        let (y0, y1) = aterm_render::scroll_translate::grid_band_px(
            self.cpu.grid_top(),
            cell_h,
            input.grid_top_row,
            input.grid_bot_row,
            h,
        );
        let (dst_y, rows) = aterm_render::scroll_translate::incoming_strip(
            y0,
            y1,
            i64::from(input.scroll_frac_px),
        )?;
        if !self.cpu.rasterize_apron_row(input, &mut win.apron_scratch) {
            return None;
        }
        let (w, band_rows) = (win.apron_scratch.width(), win.apron_scratch.rows());
        if w == 0 || band_rows == 0 {
            return None;
        }
        let rows = rows.min(band_rows);
        let raster_gen = win.apron_scratch.generation();
        if win.apron_rgba_gen != raster_gen || win.apron_rgba.len() != w * band_rows * 4 {
            win.apron_rgba.clear();
            win.apron_rgba.reserve(w * band_rows * 4);
            for p in win.apron_scratch.band() {
                win.apron_rgba.extend_from_slice(&[
                    (p >> 16) as u8,
                    (p >> 8) as u8,
                    *p as u8,
                    255 - (p >> 24) as u8,
                ]);
            }
            win.apron_rgba_gen = raster_gen;
        }
        Some(ApronStrip {
            dst_y: dst_y as u32,
            rows: rows as u32,
            w: w as u32,
            cell_h: band_rows as u32,
        })
    }

    #[cfg(wgpu_arm)]
    fn ensure_apron_texture(&mut self, win: &mut WindowGpu, strip: ApronStrip) {
        let (w, h) = (strip.w, strip.cell_h);
        let resident = win
            .apron_tex
            .as_ref()
            .is_some_and(|t| t.width() == w && t.height() == h);
        if !resident {
            let format = crate::device_layer::TexelFormat::from_wgpu(self.ctx.offscreen_format())
                .expect("the offscreen format is inside the map's closed set of six");
            win.apron_tex = Some(self.ctx.device_layer().create_texture_2d(
                "aterm-gpu m1b apron row",
                format,
                w,
                h,
                crate::device_layer::TexUsage::COPY_STAGING,
                None,
            ));
            win.apron_tex_bytes.clear();
            win.apron_tex_gen = 0;
        }
        if win.apron_tex_gen == win.apron_rgba_gen
            || win.apron_tex_bytes.as_slice() == win.apron_rgba.as_slice()
        {
            win.apron_tex_gen = win.apron_rgba_gen;
            return;
        }
        let tex = win.apron_tex.as_ref().expect("ensured above");
        self.ctx
            .device_layer()
            .upload_texture_full(tex, &win.apron_rgba, w * 4, w, h);
        win.apron_tex_bytes.clear();
        win.apron_tex_bytes.extend_from_slice(&win.apron_rgba);
        win.apron_tex_gen = win.apron_rgba_gen;
    }

    #[cfg(wgpu_arm)]
    fn apron_into_offscreen_wgpu(&mut self, win: &mut WindowGpu, input: &RenderInput) {
        let Some(h) = win.offscreen.as_ref().map(|o| o.h as usize) else {
            return;
        };
        let Some(strip) = self.apron_strip_plan(win, input, h) else {
            return;
        };
        self.ensure_apron_texture(win, strip);
        let (Some(off), Some(ap)) = (win.offscreen.as_ref(), win.apron_tex.as_ref()) else {
            return;
        };
        const fn at(tex: &wgpu::Texture, y: u32) -> wgpu::TexelCopyTextureInfo<'_> {
            wgpu::TexelCopyTextureInfo {
                texture: tex,
                mip_level: 0,
                origin: wgpu::Origin3d { x: 0, y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            }
        }
        let mut enc = self
            .ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aterm-gpu m1b apron strip"),
            });
        enc.copy_texture_to_texture(
            at(ap.wgpu(), 0),
            at(&off.tex, strip.dst_y),
            wgpu::Extent3d {
                width: strip.w.min(off.w),
                height: strip.rows,
                depth_or_array_layers: 1,
            },
        );
        self.ctx.queue.submit([enc.finish()]);
    }

    #[cfg(target_os = "macos")]
    fn metal_ensure_apron_tex(
        &mut self,
        win: &mut WindowGpu,
        strip: ApronStrip,
    ) -> Result<crate::metal::resources::SealedTexture, String> {
        let (w, h) = (strip.w as usize, strip.cell_h as usize);
        let unchanged = win
            .metal_apron_tex
            .as_ref()
            .is_some_and(|t| (t.width(), t.height()) == (w, h))
            && (win.metal_apron_gen == win.apron_rgba_gen
                || win.metal_apron_bytes.as_slice() == win.apron_rgba.as_slice());
        if !unchanged {
            let _pool = crate::metal::ffi::AutoreleasePool::new();
            let cell = self.metal_arm.as_mut().ok_or("the Metal arm is not live")?;
            let live = cell.live_mut();
            let layer = live.layer();
            let tex = layer
                .try_create_texture_2d(
                    "aterm-gpu metal apron row",
                    crate::device_layer::TexelFormat::Rgba8Unorm,
                    w as u32,
                    h as u32,
                    crate::device_layer::TexUsage::COPY_STAGING,
                    None,
                )
                .map_err(|e| format!("apron row mint: {e}"))?;
            layer.upload_texture_full(&tex, &win.apron_rgba, (w * 4) as u32, w as u32, h as u32);
            win.metal_apron_tex = Some(tex.into_metal());
            win.metal_apron_bytes.clear();
            win.metal_apron_bytes.extend_from_slice(&win.apron_rgba);
        }
        win.metal_apron_gen = win.apron_rgba_gen;
        Ok(win
            .metal_apron_tex
            .as_ref()
            .expect("ensured above")
            .clone_handle())
    }

    #[cfg(target_os = "macos")]
    fn apron_into_offscreen_metal(&mut self, win: &mut WindowGpu, input: &RenderInput) {
        use crate::device_layer::{FrameCopyTexture, FrameEncoder, SubmittedFrame};
        let Some(h) = win.metal_offscreen.as_ref().map(|o| o.h as usize) else {
            return;
        };
        let Some(strip) = self.apron_strip_plan(win, input, h) else {
            return;
        };
        let tex = match self.metal_ensure_apron_tex(win, strip) {
            Ok(t) => t,
            Err(e) => {
                metal_arm_note(&format!(
                    "armed apron strip: no texture ({e}); strip left as placeholder"
                ));
                return;
            }
        };
        let Some(off) = win.metal_offscreen.as_ref() else {
            return;
        };
        let Some(cell) = self.metal_arm.as_mut() else {
            return;
        };
        let live = cell.live_mut();
        let _pool = crate::metal::ffi::AutoreleasePool::new();
        let copied = (|| -> Result<(), String> {
            let mut enc = FrameEncoder::metal(&live.session)?;
            enc.copy_texture_rect(
                FrameCopyTexture::Metal(&tex),
                (0, 0),
                FrameCopyTexture::Metal(&off.tex),
                (0, strip.dst_y),
                strip.w.min(off.w),
                strip.rows,
            )?;
            match enc.submit() {
                SubmittedFrame::Metal(sub) => {
                    let outcome = live.wait_inline(&sub);
                    if outcome != crate::metal::loss::CbOutcome::Completed {
                        return Err(format!("apron strip command buffer ended {outcome:?}"));
                    }
                    Ok(())
                }
                #[cfg(wgpu_arm)]
                SubmittedFrame::Wgpu => unreachable!("a metal encoder commits a metal frame"),
            }
        })();
        if let Err(e) = copied {
            metal_arm_note(&format!(
                "armed apron strip failed ({e}); strip left as placeholder"
            ));
        }
    }

    /// Present `input` into this window's persistent `VirtualTarget`.
    #[inline]
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
            0,
            (fw as u32, fh as u32),
        )
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the virtual cropped twin preserves the present tuple and records one transport translation"
    )]
    #[inline]
    pub fn present_virtual_cropped_with_effects_transport(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
        invert: bool,
        overlay: Option<DropOverlay>,
        tray: Option<TrayQuad<'_>>,
        crop: PresentCrop,
        effects_transport_shift_y: i32,
        destination: (u32, u32),
    ) -> bool {
        self.present_virtual_with_crop(
            win,
            input,
            invert,
            overlay,
            tray,
            Some(crop),
            effects_transport_shift_y,
            destination,
        )
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
        effects_transport_shift_y: i32,
        destination: (u32, u32),
    ) -> bool {
        #[cfg(target_os = "macos")]
        if self.metal_armed {
            return match self.metal_present_virtual_with_crop(
                win,
                input,
                invert,
                overlay,
                tray,
                source_crop,
                effects_transport_shift_y,
                destination,
            ) {
                Ok(()) => true,
                Err(e) => {
                    metal_arm_note(&format!(
                        "armed virtual present refused ({e}) — dropping this present"
                    ));
                    false
                }
            };
        }
        #[cfg(gl_arm)]
        if self.gl_armed && self.gl_arm_ready() {
            return match self.gl_present_virtual_with_crop(
                win,
                input,
                invert,
                overlay,
                tray,
                source_crop,
                effects_transport_shift_y,
                destination,
            ) {
                Ok(()) => true,
                Err(e) => {
                    gl_arm::gl_arm_note(&format!(
                        "armed virtual present refused ({e}); dropping this present"
                    ));
                    false
                }
            };
        }
        #[cfg(not(wgpu_arm))]
        {
            first_party_note(
                "virtual present: the renderer is disarmed and this build carries \
                 no wgpu arm — dropping this present",
            );
            false
        }
        #[cfg(wgpu_arm)]
        {
            let Ok(source_crop) = self.resident_present_crop(input, source_crop) else {
                return false;
            };
            let w = self.clamp_fb_dim(destination.0.max(1));
            let h = self.clamp_fb_dim(destination.1.max(1));
            if win
                .virtual_target
                .as_ref()
                .is_none_or(|t| t.w != w || t.h != h)
            {
                win.virtual_target = Some(self.make_virtual_target(w, h));
                win.virtual_presented_input_epoch = 0;
            }
            let tex = win
                .virtual_target
                .as_ref()
                .expect("ensured above")
                .tex
                .clone();
            let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
            let work_started = aterm_time::Instant::now();
            self.present_to_view(
                win,
                input,
                invert,
                overlay,
                tray,
                source_crop,
                effects_transport_shift_y,
                PresentDest {
                    view: &view,
                    tex: &tex,
                    w,
                    h,
                    format: VIRTUAL_PRESENT_FORMAT,
                    translucent: false,
                    premult: false,
                },
            );
            win.virtual_presented_input_epoch = win.resident_input_epoch;
            win.last_present_work_ns =
                u64::try_from(work_started.elapsed().as_nanos()).unwrap_or(u64::MAX);
            win.last_present_encode_ns = win.last_present_work_ns;
            true
        }
    }

    #[cfg(wgpu_arm)]
    fn make_virtual_target(&self, w: u32, h: u32) -> VirtualTarget {
        let tex = self
            .ctx
            .device_layer()
            .create_texture_2d(
                "aterm-gpu virtual present target",
                crate::device_layer::TexelFormat::from_wgpu(VIRTUAL_PRESENT_FORMAT)
                    .expect("the virtual present format is inside the closed set of six"),
                w,
                h,
                crate::device_layer::TexUsage {
                    sampled: false,
                    render: true,
                    copy_src: true,
                    copy_dst: true,
                },
                None,
            )
            .into_wgpu();
        VirtualTarget { tex, w, h }
    }

    /// Build every effect pipeline not yet resident, off the frame path: the wgpu arm's here; the
    /// Metal arm's on a worker that hands each over as it lands, covering every row and twin its
    /// frames and presents can bind under the present switches, from its mint if it has not
    /// minted yet and again for a pass a switch turns on later. A no-op on the GL arm.
    pub fn warm_effect_pipelines(&mut self) {
        #[cfg(wgpu_arm)]
        for which in EFFECT_PIPELINES {
            self.effects.ensure(&self.ctx.device, which);
        }
        #[cfg(target_os = "macos")]
        {
            self.metal_warm_wanted = true;
            self.metal_warm_pipelines();
        }
    }

    /// Start the Metal arm's pipeline warm under the present switches, if the arm is live.
    #[cfg(target_os = "macos")]
    fn metal_warm_pipelines(&mut self) {
        let knobs = crate::metal::warm::PresentKnobs {
            bloom: self.enable_bloom,
            shimmer: self.enable_shimmer,
            hdr_glow: self.hdr_glow && self.metal_on_screen,
            sdr_glow: self.sdr_glow_boost > 0.0,
        };
        if let Some(cell) = self.metal_arm.as_mut() {
            cell.live_mut().warm_pipelines(knobs);
        }
    }

    /// Test seam: wait (a hang detector of `hang`) until the Metal arm's warms built all they owe,
    /// leaving each build for the frames to take; returns the items whose build failed.
    #[cfg(all(target_os = "macos", test))]
    pub(crate) fn await_pipeline_warm_for_test(
        &mut self,
        hang: std::time::Duration,
    ) -> Vec<crate::metal::warm::WarmItem> {
        self.metal_arm
            .as_mut()
            .map_or_else(Vec::new, |cell| cell.live_mut().await_warmed(hang))
    }

    /// Test seam: what the Metal arm's pipeline warms were asked to build and it has not taken.
    #[cfg(all(target_os = "macos", test))]
    pub(crate) fn metal_warm_owed_for_test(&self) -> Vec<crate::metal::warm::WarmItem> {
        self.metal_arm
            .as_ref()
            .map_or_else(Vec::new, |cell| cell.live_ref().warm_owed.clone())
    }

    /// Test seam: the pipeline states the Metal arm built on its own thread, in order.
    #[cfg(all(target_os = "macos", test))]
    pub(crate) fn metal_built_here_for_test(&self) -> Vec<crate::metal::warm::WarmItem> {
        self.metal_arm
            .as_ref()
            .map_or_else(Vec::new, |cell| cell.live_ref().built_here.clone())
    }

    /// Which effect pipelines this renderer has built, in [`EFFECT_PIPELINE_NAMES`] order.
    #[inline]
    pub fn effect_pipelines_resident(&self) -> [bool; EFFECT_PIPELINE_COUNT] {
        #[cfg(wgpu_arm)]
        {
            self.effects.resident()
        }
        #[cfg(not(wgpu_arm))]
        {
            [false; EFFECT_PIPELINE_COUNT]
        }
    }

    /// `(count, nanoseconds)` spent building effect pipelines since construction.
    #[inline]
    pub const fn effect_pipeline_build_cost(&self) -> (u32, u64) {
        #[cfg(wgpu_arm)]
        {
            (self.effects.builds, self.effects.build_ns)
        }
        #[cfg(not(wgpu_arm))]
        {
            (0, 0)
        }
    }

    #[cfg(wgpu_arm)]
    fn ensure_hdr_glow_pipeline(&mut self) {
        if self.hdr_glow_pipeline.is_some() {
            return;
        }
        self.ensure_glow_boost_shared();
        let pipeline =
            self.build_glow_boost_pipeline(Pipeline::HdrGlow, self.ctx.pipeline_targets());
        self.hdr_glow_pipeline = Some(pipeline);
    }

    #[cfg(wgpu_arm)]
    fn ensure_sdr_glow_pipeline(&mut self, format: wgpu::TextureFormat) {
        if matches!(&self.sdr_glow_pipeline, Some((f, _)) if *f == format) {
            return;
        }
        self.ensure_glow_boost_shared();
        let pipeline = self.build_glow_boost_pipeline(
            Pipeline::SdrGlow,
            self.ctx.pipeline_targets().with_present(format),
        );
        self.sdr_glow_pipeline = Some((format, pipeline));
    }

    #[cfg(wgpu_arm)]
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
        let uniform_buf = self
            .ctx
            .device_layer()
            .create_uniform_buffer(
                "aterm-gpu glow-boost uniform",
                std::mem::size_of::<HdrGlowUniform>() as u64,
            )
            .into_wgpu();
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

    #[cfg(wgpu_arm)]
    fn build_glow_boost_pipeline(
        &self,
        row: Pipeline,
        targets: TargetFormats,
    ) -> wgpu::RenderPipeline {
        let device = &self.ctx.device;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("aterm-gpu glow-boost shader"),
            source: wgpu::ShaderSource::Wgsl(wgsl_source(row.spec().library).into()),
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
        crate::pipeline_table::build_wgpu(row.spec(), device, &shader, &layout, targets)
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AsymmetricPadLayout",
            action = "PrimeLayoutCache",
            project = "aterm_gpu::GpuRenderer::project_asymmetric_pad_layout"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AsymmetricPadLayout",
            action = "RenderWithLayoutCache",
            project = "aterm_gpu::GpuRenderer::project_asymmetric_pad_layout"
        )
    )]
    fn encode_present_frame(&mut self, win: &mut WindowGpu, input: &RenderInput) -> (u32, u32) {
        let cur_blink = self.cpu.cursor_blink_phase();
        let cur_override = self.cpu.cursor_style_override();
        let (cw, ch) = self.cpu.cell_size();
        let pad = self.cpu.pad();
        let head = self.cpu.head();
        let (w, h) = (
            self.clamp_fb_dim((input.cols * cw + 2 * pad) as u32),
            self.clamp_fb_dim((input.rows * ch + 2 * pad + head) as u32),
        );

        #[cfg(target_os = "macos")]
        let offscreen_holds_prev = if self.metal_armed {
            win.present_prev.as_ref().is_some_and(|p| p.on_metal)
                && matches!(&win.metal_offscreen, Some(o) if o.w == w && o.h == h)
        } else {
            #[cfg(wgpu_arm)]
            {
                win.present_prev.as_ref().is_none_or(|p| !p.on_metal)
                    && matches!(&win.offscreen, Some(o) if o.w == w && o.h == h)
            }
            #[cfg(not(wgpu_arm))]
            {
                // Disarmed with no wgpu offscreen: nothing resident to diff.
                false
            }
        };
        #[cfg(gl_arm)]
        let offscreen_holds_prev = if self.gl_armed {
            win.present_prev.as_ref().is_some_and(|p| p.on_gl)
                && self.gl_arm.as_ref().is_some_and(|live| {
                    matches!(&win.gl_offscreen,
                        Some(o) if o.w == w && o.h == h && o.tex.on(&live.dev))
                })
        } else {
            #[cfg(wgpu_arm)]
            {
                win.present_prev.as_ref().is_none_or(|p| !p.on_gl)
                    && matches!(&win.offscreen, Some(o) if o.w == w && o.h == h)
            }
            #[cfg(not(wgpu_arm))]
            {
                false
            }
        };
        #[cfg(not(any(target_os = "macos", gl_arm)))]
        let offscreen_holds_prev = matches!(&win.offscreen, Some(o) if o.w == w && o.h == h);

        let mut dirty = std::mem::take(&mut self.dirty_scratch);
        let prev_present = win.present_prev.take();
        let prev_input = std::mem::take(&mut win.prev_input);
        let mut scroll_delta: Option<i32> = None;
        let scope = match (&prev_present, offscreen_holds_prev) {
            (Some(prev), true) if prev.grid_top == self.cpu.grid_top() => match compute_dirty_rows(
                &prev_input,
                input,
                prev.blink_phase,
                prev.cursor_style_override,
                cur_blink,
                cur_override,
                self.cpu.row_bands(),
                &mut dirty,
            ) {
                DirtyDecision::Rows(_) => {
                    self.cpu.mark_overshoot_rows(&prev_input, input, &mut dirty);
                    RepaintScope::Dirty(&dirty)
                }
                DirtyDecision::FullRepaint => {
                    scroll_delta = aterm_render::scroll_blit_plan(
                        &prev_input,
                        input,
                        prev.blink_phase,
                        prev.cursor_style_override,
                        cur_blink,
                        cur_override,
                        self.cpu.row_bands(),
                        &mut dirty,
                    );
                    if scroll_delta.is_some() {
                        RepaintScope::Dirty(&dirty)
                    } else {
                        RepaintScope::Full
                    }
                }
            },
            _ => RepaintScope::Full,
        };

        match &scope {
            RepaintScope::Dirty(_) => self.scissor_taken += 1,
            RepaintScope::Full => self.full_repaints += 1,
        }
        if let Some(delta) = scroll_delta {
            self.scroll_rescues += 1;
            self.shift_offscreen_rows(win, delta, input.rows);
        }
        let was_dirty = matches!(scope, RepaintScope::Dirty(_));
        let dims = self.encode_frame(win, input, &scope);
        self.dirty_scratch = dirty;
        #[cfg(target_os = "macos")]
        let dims = if self.metal_armed && !self.last_frame_arm_metal && was_dirty {
            metal_arm_note(
                "armed present: mid-frame degrade under a Dirty scope — re-encoded \
                 FULL for coherence",
            );
            self.full_repaints += 1;
            self.encode_frame(win, input, &RepaintScope::Full)
        } else {
            dims
        };
        #[cfg(gl_arm)]
        let dims = if self.gl_armed && !self.last_frame_arm_gl && was_dirty {
            gl_arm::gl_arm_note(
                "armed present: mid-frame degrade under a Dirty scope; re-encoded FULL for \
                 coherence",
            );
            self.full_repaints += 1;
            self.encode_frame(win, input, &RepaintScope::Full)
        } else {
            dims
        };
        let dims = if was_dirty && self.overshoot_surprise {
            self.full_repaints += 1;
            self.encode_frame(win, input, &RepaintScope::Full)
        } else {
            dims
        };

        let mut prev_input = prev_input;
        aterm_render::clone_input_stamped(&mut prev_input, input);
        win.prev_input = prev_input;
        win.advance_resident_input_epoch();
        win.present_prev = Some(PresentPrev {
            blink_phase: cur_blink,
            cursor_style_override: cur_override,
            grid_top: self.cpu.grid_top(),
            #[cfg(target_os = "macos")]
            on_metal: self.last_frame_arm_metal,
            #[cfg(gl_arm)]
            on_gl: self.last_frame_arm_gl,
        });
        dims
    }

    fn build_bloom_glow(&mut self, input: &RenderInput) -> (u32, Option<[u32; 4]>, u32) {
        let mut quads = std::mem::take(&mut self.bloom_quad_scratch);
        let f = self.fx_frame_for(input);
        let src = self
            .fx
            .bloom_source(&f, input)
            .unwrap_or(&input.cursor_glow_add);
        let (bbox, extract_first) =
            aterm_render::bloom::source_quads_into(src, input.rows, &mut quads);
        let mut v = std::mem::take(&mut self.bloom_glow_scratch);
        v.clear();
        v.extend(
            quads
                .iter()
                .map(|q| GlowInstance::of([q.x, q.y, q.w, q.h], q)),
        );
        let n = if extract_first > 0 {
            extract_first
        } else {
            v.len()
        } as u32;
        #[cfg(wgpu_arm)]
        if n > 0 {
            let device = &self.ctx.device;
            let queue = &self.ctx.queue;
            self.vbufs
                .bloom_glow
                .upload(device, queue, aterm_bits::cast_slice(&v));
        }
        self.bloom_glow_scratch = v;
        self.bloom_quad_scratch = quads;
        (n, bbox, extract_first as u32)
    }

    #[cfg(wgpu_arm)]
    fn ensure_present_offscreen(&mut self, win: &mut WindowGpu, w: u32, h: u32) {
        if matches!(&win.present_offscreen, Some(p) if p.w == w && p.h == h) {
            return;
        }
        win.offscreen_dirty_since_sync = None;
        win.present_offscreen_fx = None;
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

    #[allow(
        clippy::too_many_arguments,
        reason = "one GPU pass encoder threading the encoder, target, uniforms and glyph buffers"
    )]
    #[cfg(wgpu_arm)]
    fn encode_bloom_halo(
        &self,
        enc: &mut wgpu::CommandEncoder,
        target_view: &wgpu::TextureView,
        bt: &BloomTarget,
        glow_count: u32,
        bbox: Option<[u32; 4]>,
        dims: (u32, u32),
        extract_first: u32,
        input: &RenderInput,
    ) -> Option<[u32; 4]> {
        let bu = BloomUniform {
            texel: [1.0 / bt.bw as f32, 1.0 / bt.bh as f32],
            strength: page_bloom_strength(self.bloom_strength, input, self.theme.bg),
            radius: self.bloom_radius,
            chroma_radius: bloom_chroma_radius(self.bloom_radius),
            extent: [1.0; 2],
            _pad: 0.0,
        };
        self.ctx
            .queue
            .write_buffer(&self.bloom_uniform_buf, 0, aterm_bits::bytes_of(&bu));
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
                timestamp_writes: self.wgpu_pass_timestamps(),
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(self.effects.get(EffectPipeline::GlowAdd));
            pass.set_bind_group(0, &self.uniform_bg, &[]);
            pass.set_vertex_buffer(0, self.vbufs.bloom_glow.buf.slice(..));
            pass.draw(0..6, extract_first..extract_first + glow_count);
        }
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
                timestamp_writes: self.wgpu_pass_timestamps(),
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.bloom_pipeline);
            pass.set_bind_group(0, &bt.bind, &[]);
            let clip = input.fx_clip.map(|(cx0, cy0, cx1, cy1)| {
                [
                    u32::from(cx0),
                    u32::from(cy0),
                    u32::from(cx1),
                    u32::from(cy1),
                ]
            });
            let (w, h) = dims;
            let scissor = if let Some([x0, y0, x1, y1]) = bbox {
                let d = aterm_render::bloom::reach_px(self.bloom_radius);
                let sx0 = x0.saturating_sub(d);
                let sy0 = y0.saturating_sub(d);
                let sx1 = x1.saturating_add(d).min(w);
                let sy1 = y1.saturating_add(d).min(h);
                Some([sx0, sy0, sx1, sy1])
            } else {
                clip.map(|_| [0, 0, w, h])
            };
            let scissor = match (scissor, clip) {
                (Some([sx0, sy0, sx1, sy1]), Some([cx0, cy0, cx1, cy1])) => Some([
                    sx0.max(cx0),
                    sy0.max(cy0),
                    sx1.min(cx1).min(w),
                    sy1.min(cy1).min(h),
                ]),
                (s, None) => s,
                (None, Some(_)) => unreachable!("clip-only case handled above"),
            };
            match (scissor, clip) {
                (Some([sx0, sy0, sx1, sy1]), _) if sx1 > sx0 && sy1 > sy0 => {
                    pass.set_scissor_rect(sx0, sy0, sx1 - sx0, sy1 - sy0);
                    pass.draw(0..3, 0..1);
                    Some([sx0, sy0, sx1, sy1])
                }
                (Some(_), Some(_)) => Some([0, 0, 0, 0]),
                (Some(_), None) | (None, _) => {
                    pass.draw(0..3, 0..1);
                    None
                }
            }
        }
    }

    #[cfg(wgpu_arm)]
    fn compose_present_offscreen(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
        enc: &mut wgpu::CommandEncoder,
    ) -> Option<u32> {
        let (w, h) = win.offscreen.as_ref().map(|o| (o.w, o.h))?;
        self.ensure_present_offscreen(win, w, h);
        let bloom = self.enable_bloom && fx_gpu::bloom_source_live(input);
        let (glow_count, glow_bbox, extract_first) = if bloom {
            self.build_bloom_glow(input)
        } else {
            (0, None, 0)
        };
        if glow_count > 0 {
            self.effects
                .ensure(&self.ctx.device, EffectPipeline::GlowAdd);
            self.ensure_bloom_target(win);
        }
        let shimmer_region = if self.shimmer_live(input) {
            self.shimmer_region(input, w, h)
        } else {
            None
        };
        if shimmer_region.is_some() {
            self.ensure_shimmer_scratch(win, w, h);
        }
        let copy_rect = present_copy_rect(
            win.offscreen_dirty_since_sync,
            win.present_offscreen_fx,
            (w, h),
        );
        let off = win.offscreen.as_ref().expect("offscreen resident");
        let po = win
            .present_offscreen
            .as_ref()
            .expect("ensure_present_offscreen set it");
        if copy_rect[2] > copy_rect[0] && copy_rect[3] > copy_rect[1] {
            enc.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &off.tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: copy_rect[0],
                        y: copy_rect[1],
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyTextureInfo {
                    texture: &po.tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: copy_rect[0],
                        y: copy_rect[1],
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width: copy_rect[2] - copy_rect[0],
                    height: copy_rect[3] - copy_rect[1],
                    depth_or_array_layers: 1,
                },
            );
        }
        let mut fx: Option<[u32; 4]> = Some([0, 0, 0, 0]);
        if glow_count > 0
            && let Some(bt) = off.bloom.as_ref()
        {
            let halo = self.encode_bloom_halo(
                enc,
                &po.view,
                bt,
                glow_count,
                glow_bbox,
                (w, h),
                extract_first,
                input,
            );
            fx = union_rect_opt(fx, halo, (w, h));
        }
        if let Some(region) = &shimmer_region {
            let scratch = win.shimmer_scratch.as_ref().expect("ensured above");
            self.encode_shimmer(enc, scratch, &po.tex, &po.view, region);
            fx = union_rect_opt(
                fx,
                Some([region.x0, region.y0, region.x1, region.y1]),
                (w, h),
            );
        }
        win.offscreen_dirty_since_sync = Some([0, 0, 0, 0]);
        win.present_offscreen_fx = fx;
        bloom.then_some(glow_count)
    }

    #[cfg(wgpu_arm)]
    fn composite_bloom_in_place(&mut self, win: &mut WindowGpu, input: &RenderInput) -> u32 {
        let (glow_count, glow_bbox, extract_first) = self.build_bloom_glow(input);
        if glow_count == 0 {
            return glow_count;
        }
        self.effects
            .ensure(&self.ctx.device, EffectPipeline::GlowAdd);
        self.ensure_bloom_target(win);
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
        let halo = self.encode_bloom_halo(
            &mut enc,
            &off.view,
            bt,
            glow_count,
            glow_bbox,
            dims,
            extract_first,
            input,
        );
        self.ctx.queue.submit([enc.finish()]);
        note_offscreen_written(win, halo, dims);
        glow_count
    }

    fn shimmer_phase(&self) -> f32 {
        self.shimmer_phase_pin
            .unwrap_or_else(|| self.shimmer_epoch.elapsed().as_secs_f32() % SHIMMER_PHASE_WRAP_S)
    }

    /// The foot (frame px) of the lowest row holding a glyph in columns `x0..x1` that reaches into
    /// the band `y0..y1`, or `0` when every such row is blank. A row in a smooth scroll reaches its
    /// offset further each way.
    fn lettered_foot(
        &self,
        input: &RenderInput,
        (x0, x1): (u32, u32),
        (y0, y1): (u32, u32),
        (cw, ch): (u32, u32),
    ) -> u32 {
        if cw == 0 || ch == 0 {
            return 0;
        }
        let top = self.cpu.grid_top() as i64;
        let pad = self.cpu.pad() as u32;
        let slack = i64::from(input.scroll_frac_px).abs();
        let c0 = (x0.saturating_sub(pad) / cw) as usize;
        let c1 = (x1.saturating_sub(pad)).div_ceil(cw) as usize;
        for (r, line) in input.cells.iter().enumerate().take(input.rows).rev() {
            let r_top = top + r as i64 * i64::from(ch) - slack;
            let r_foot = r_top + i64::from(ch) + 2 * slack;
            if r_foot <= i64::from(y0) || r_top >= i64::from(y1) {
                continue;
            }
            let lettered = line
                .get(c0.min(line.len())..c1.min(line.len()))
                .is_some_and(|cells| cells.iter().any(|c| c.ch != ' ' && c.ch != '\0'));
            if lettered {
                return u32::try_from(r_foot.max(0)).unwrap_or(u32::MAX);
            }
        }
        0
    }

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
        // The haze never bends a letter: it rises through the blank rows over the hot band and
        // fades out at the foot of the first lettered row.
        let foot = self.lettered_foot(input, (x0, x1), (y0, y1), (cw, ch));
        let (y0, rise) = if foot > y0 {
            (foot.min(y1), y1.saturating_sub(foot) as f32)
        } else {
            (y0, rise)
        };
        if x0 >= x1 || y0 >= y1 {
            return None;
        }
        let fx = input.fx_clip.map_or((0, 0, w, h), |(cx0, cy0, cx1, cy1)| {
            (
                u32::from(cx0),
                u32::from(cy0),
                u32::from(cx1),
                u32::from(cy1),
            )
        });
        let (rx0, ry0, rx1, ry1) = (x0.max(fx.0), y0.max(fx.1), x1.min(fx.2), y1.min(fx.3));
        if rx0 >= rx1 || ry0 >= ry1 {
            return None;
        }
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
        for _ in 0..2 {
            let prev = heat;
            for (i, slot) in heat.iter_mut().enumerate() {
                let l = prev[i.saturating_sub(1)];
                let r = prev[(i + 1).min(SHIMMER_BANDS - 1)];
                *slot = 0.25 * l + 0.5 * prev[i] + 0.25 * r;
            }
        }
        Some(ShimmerRegion {
            x0: rx0,
            y0: ry0,
            x1: rx1,
            y1: ry1,
            #[cfg(wgpu_arm)]
            fw: w,
            #[cfg(wgpu_arm)]
            fh: h,
            hot_top: hot_top as f32,
            rise,
            band_x0: x0 as f32,
            band_w,
            heat,
        })
    }

    #[cfg(wgpu_arm)]
    fn ensure_bloom_target(&mut self, win: &mut WindowGpu) {
        if !self.enable_bloom {
            return;
        }
        let Some((w, h)) = win.offscreen.as_ref().map(|o| (o.w, o.h)) else {
            return;
        };
        let bw = (w / BLOOM_DOWNSCALE).max(1);
        let bh = (h / BLOOM_DOWNSCALE).max(1);
        if matches!(win.offscreen.as_ref().and_then(|o| o.bloom.as_ref()),
            Some(b) if b.bw == bw && b.bh == bh)
        {
            return;
        }
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
        if let Some(off) = win.offscreen.as_mut() {
            off.bloom = Some(BloomTarget {
                tex: btex,
                view: bview,
                bind,
                bw,
                bh,
            });
        }
    }

    #[cfg(wgpu_arm)]
    fn ensure_shimmer_resources(&mut self) {
        if self.shimmer.is_none() {
            self.shimmer = Some(build_shimmer_resources(
                self.ctx.device_layer(),
                self.ctx.pipeline_targets(),
            ));
        }
    }

    #[cfg(wgpu_arm)]
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

    #[cfg(wgpu_arm)]
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
            .write_buffer(&sr.uniform_buf, 0, aterm_bits::bytes_of(&su));
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
                timestamp_writes: self.wgpu_pass_timestamps(),
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

    #[cfg(wgpu_arm)]
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
        note_offscreen_written(
            win,
            Some([region.x0, region.y0, region.x1, region.y1]),
            (w, h),
        );
    }

    /// Test helper.
    #[doc(hidden)]
    pub fn present_input_readback(&mut self, win: &mut WindowGpu, input: &RenderInput) -> Frame {
        let (w, h) = self.encode_present_frame(win, input);
        #[cfg(target_os = "macos")]
        if self.last_frame_arm_metal {
            let effect_frame =
                (self.enable_bloom && fx_gpu::bloom_source_live(input)) || self.shimmer_live(input);
            let read = if effect_frame {
                self.metal_compose_present_readback(win, input, w, h)
            } else {
                self.metal_read_back_offscreen(win, w, h)
            };
            return read.unwrap_or_else(|e| {
                crate::stderr_line!(
                    "aterm-gpu: armed present readback failed ({e}); returning \
                     blank frame"
                );
                Frame {
                    width: w as usize,
                    height: h as usize,
                    pixels: vec![0; (w as usize) * (h as usize)],
                }
            });
        }
        #[cfg(gl_arm)]
        if self.last_frame_arm_gl {
            return self
                .gl_present_readback(win, input, (w, h))
                .unwrap_or_else(|e| {
                    crate::stderr_line!(
                        "aterm-gpu: armed GL present readback failed ({e}); returning blank frame"
                    );
                    Frame {
                        width: w as usize,
                        height: h as usize,
                        pixels: vec![0; (w as usize) * (h as usize)],
                    }
                });
        }
        #[cfg(not(wgpu_arm))]
        {
            let _ = win;
            Frame {
                width: w as usize,
                height: h as usize,
                pixels: vec![0; (w as usize) * (h as usize)],
            }
        }
        #[cfg(wgpu_arm)]
        {
            let tex = if (self.enable_bloom && fx_gpu::bloom_source_live(input))
                || self.shimmer_live(input)
            {
                let mut enc =
                    self.ctx
                        .device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("aterm-gpu readback present-offscreen composite"),
                        });
                self.compose_present_offscreen(win, input, &mut enc);
                self.ctx.queue.submit([enc.finish()]);
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
    }

    /// Test helper.
    #[doc(hidden)]
    #[cfg(wgpu_arm)]
    pub fn present_encode_poll(&mut self, win: &mut WindowGpu, input: &RenderInput) {
        let _ = self.encode_present_frame(win, input);
        self.ctx
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("GPU poll failed");
    }

    /// Test helper.
    #[doc(hidden)]
    #[cfg(wgpu_arm)]
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
            0,
            PresentDest {
                view: &view,
                tex: &tex,
                w: w.max(1),
                h: h.max(1),
                format: VIRTUAL_PRESENT_FORMAT,
                translucent: false,
                premult: false,
            },
        );
    }

    /// Test helper.
    #[doc(hidden)]
    #[cfg(wgpu_arm)]
    pub fn video_begin_standin_for_test(
        &self,
        win: &mut WindowGpu,
        w: u32,
        h: u32,
        opts: crate::video_tap::CaptureOpts,
    ) -> Result<(), String> {
        let tap = crate::video_tap::VideoTap::new(
            &self.ctx.device,
            w,
            h,
            VIRTUAL_PRESENT_TEXEL,
            crate::video_tap::CaptureColorSpace::Srgb,
            1.0,
            opts,
        )?;
        win.video = Some(tap);
        Ok(())
    }

    /// Test helper.
    #[cfg(all(target_os = "macos", test))]
    pub(crate) fn video_begin_metal_standin_for_test(
        &self,
        win: &mut WindowGpu,
        w: u32,
        h: u32,
        opts: crate::video_tap::CaptureOpts,
    ) -> Result<(), String> {
        let cell = self
            .metal_arm
            .as_ref()
            .ok_or("arm not minted — render an armed frame first")?;
        let tap = crate::video_tap::VideoTap::new_metal(
            &cell.live_ref().mint,
            w,
            h,
            crate::device_layer::TexelFormat::Bgra8Unorm,
            win.capture_color_space,
            win.sdr_white_scale(),
            opts,
        )?;
        win.video = Some(tap);
        Ok(())
    }

    /// Test helper: the armed one-shot stand-in.
    #[cfg(all(target_os = "macos", test))]
    pub(crate) fn presented_snapshot_begin_metal_standin_for_test(
        &self,
        win: &mut WindowGpu,
        w: u32,
        h: u32,
    ) -> Result<(), String> {
        let cell = self
            .metal_arm
            .as_ref()
            .ok_or("arm not minted — render an armed frame first")?;
        win.presented_snapshot = Some(crate::video_tap::PresentedFrameTap::new_metal(
            &cell.live_ref().mint,
            w,
            h,
            crate::device_layer::TexelFormat::Bgra8Unorm,
            win.capture_color_space,
            win.sdr_white_scale(),
        )?);
        Ok(())
    }

    /// Test helper.
    #[doc(hidden)]
    #[cfg(wgpu_arm)]
    pub fn presented_snapshot_begin_standin_for_test(
        &self,
        win: &mut WindowGpu,
        w: u32,
        h: u32,
    ) -> Result<(), String> {
        if win.presented_snapshot.is_some() {
            return Err("presented snapshot: capture already armed".to_string());
        }
        win.presented_snapshot = Some(crate::video_tap::PresentedFrameTap::new(
            &self.ctx.device,
            w.max(1),
            h.max(1),
            VIRTUAL_PRESENT_TEXEL,
            crate::video_tap::CaptureColorSpace::Srgb,
            1.0,
        )?);
        Ok(())
    }

    /// Test helper: reset one window's SDR glow-boost envelope.
    #[doc(hidden)]
    pub const fn reset_glow_ease_for_test(&self, win: &mut WindowGpu) {
        win.sdr_glow_level = 0.0;
        win.sdr_glow_level_at = None;
    }

    /// The exact `BlitUniform` bytes the last present-path blit wrote.
    #[cfg(all(target_os = "macos", test, wgpu_arm))]
    pub(crate) fn last_blit_uniform_bytes(&self) -> Option<Vec<u8>> {
        self.blit_uniform_written
            .as_ref()
            .map(|u| aterm_bits::bytes_of(u).to_vec())
    }

    /// Test helper.
    #[doc(hidden)]
    #[cfg(wgpu_arm)]
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

    /// Test helper.
    #[doc(hidden)]
    #[cfg(wgpu_arm)]
    pub fn blit_to_sized_for_test(
        &mut self,
        win: &mut WindowGpu,
        invert: bool,
        dst_w: u32,
        dst_h: u32,
    ) -> Frame {
        self.blit_to_sized_with_options_for_test(win, invert, None, None, dst_w, dst_h)
    }

    /// Test helper.
    #[doc(hidden)]
    #[cfg(wgpu_arm)]
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

    #[cfg(wgpu_arm)]
    fn blit_to_sized_with_options_for_test(
        &mut self,
        win: &mut WindowGpu,
        invert: bool,
        overlay: Option<DropOverlay>,
        source_crop: Option<PresentCrop>,
        dst_w: u32,
        dst_h: u32,
    ) -> Frame {
        let fx = BlitTestEffects {
            invert,
            overlay,
            crop: source_crop,
            ..BlitTestEffects::PLAIN
        };
        let dst =
            self.blit_effect_target_for_test(win, fx, BlitTestTarget::Rgba8Unorm, dst_w, dst_h);
        self.ctx.read_back(&dst, dst_w.max(1), dst_h.max(1))
    }

    #[cfg(wgpu_arm)]
    fn blit_effect_target_for_test(
        &mut self,
        win: &mut WindowGpu,
        fx: BlitTestEffects,
        target: BlitTestTarget,
        dst_w: u32,
        dst_h: u32,
    ) -> wgpu::Texture {
        let (invert, overlay, source_crop) = (fx.invert, fx.overlay, fx.crop);
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

        let format = target.wgpu();
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

        let mut want =
            present_blit_uniform(invert, overlay, source_crop, fw, fh, w, h, self.theme.bg);
        want.chrome_y0 = win.chrome_y0 as f32;
        want.chrome_y1 = win.chrome_y1 as f32;
        if let Some(band_a) = fx.translucent {
            want = want.with_translucency(band_a);
            if fx.premult {
                want = want.with_premultiplied_output();
            }
        }
        want.encode_srgb = if fx.encode_srgb || !self.ctx.srgb_offscreen {
            1.0
        } else {
            0.0
        };
        want.hdr = f32::from(u8::from(fx.hdr));
        want.sdr_white_scale = fx.sdr_white_scale;
        self.ctx
            .queue
            .write_buffer(&self.blit_uniform_buf, 0, aterm_bits::bytes_of(&want));
        self.blit_uniform_written = Some(want);

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
        dst
    }

    /// Read back [`Self::blit_effect_target_for_test`] as tightly packed RGBA bytes.
    #[cfg(all(target_os = "macos", test, wgpu_arm))]
    pub(crate) fn blit_effect_bytes_for_test(
        &mut self,
        win: &mut WindowGpu,
        fx: BlitTestEffects,
        target: BlitTestTarget,
        dst_w: u32,
        dst_h: u32,
    ) -> Vec<u8> {
        let (w, h) = (dst_w.max(1), dst_h.max(1));
        let dst = self.blit_effect_target_for_test(win, fx, target, w, h);
        self.texture_bytes_tight_for_test(&dst, w, h, target.bytes_per_texel())
    }

    #[cfg(all(target_os = "macos", test, wgpu_arm))]
    fn texture_bytes_tight_for_test(
        &self,
        tex: &wgpu::Texture,
        w: u32,
        h: u32,
        texel: usize,
    ) -> Vec<u8> {
        let row = w as usize * texel;
        let padded = row.div_ceil(256) * 256;
        let buffer = self.ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aterm-gpu test raw readback"),
            size: (padded * h as usize) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = self
            .ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aterm-gpu test raw readback"),
            });
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: tex,
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
        crate::device_layer::submit_host_readable(
            &self.ctx.device,
            &self.ctx.queue,
            enc.finish(),
            std::slice::from_ref(&buffer),
        );
        let slice = buffer.slice(..);
        #[expect(
            clippy::disallowed_methods,
            reason = "the copy was submitted by device_layer::submit_host_readable"
        )]
        slice.map_async(wgpu::MapMode::Read, |_| {});
        self.ctx
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("GPU poll failed");
        let data = slice.get_mapped_range();
        let mut out = Vec::with_capacity(row * h as usize);
        for y in 0..h as usize {
            out.extend_from_slice(&data[y * padded..y * padded + row]);
        }
        drop(data);
        buffer.unmap();
        out
    }

    /// Test helper for the HDR clamp laws.
    #[doc(hidden)]
    #[cfg(wgpu_arm)]
    pub fn present_hdr_for_test(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
        boost_pass: bool,
    ) -> (Vec<f32>, u32, u32) {
        self.present_hdr_sized_for_test(win, input, boost_pass, 0, 0)
    }

    /// Test helper.
    #[doc(hidden)]
    #[cfg(wgpu_arm)]
    pub fn present_hdr_sized_for_test(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
        boost_pass: bool,
        extra_w: u32,
        extra_h: u32,
    ) -> (Vec<f32>, u32, u32) {
        let (fw, fh) = self.encode_present_frame(win, input);
        let (w, h) = (fw + extra_w, fh + extra_h);
        let format = wgpu::TextureFormat::Rgba16Float;

        let glow_present = fx_gpu::crown_glow_present(input);
        let plan = crate::format_plan::hdr_present_plan(
            self.hdr_glow,
            true, // the stand-in target is Rgba16Float
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

        let mut want = BlitUniform::bell(false).with_bands(fw, fh, w, h, self.theme.bg);
        want.encode_srgb = if self.ctx.srgb_offscreen { 0.0 } else { 1.0 };
        want.hdr = if plan.blit_linear_encode { 1.0 } else { 0.0 };
        want.sdr_white_scale = if plan.blit_linear_encode {
            win.sdr_white_scale.max(1.0)
        } else {
            1.0
        };
        self.ctx
            .queue
            .write_buffer(&self.blit_uniform_buf, 0, aterm_bits::bytes_of(&want));
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
                aterm_bits::bytes_of(&hu),
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
            if run_hdr_glow && glow_count > 0 {
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
        crate::device_layer::submit_host_readable(
            &self.ctx.device,
            &self.ctx.queue,
            enc.finish(),
            std::slice::from_ref(&buffer),
        );
        let slice = buffer.slice(..);
        #[expect(
            clippy::disallowed_methods,
            reason = "the copy was submitted by device_layer::submit_host_readable"
        )]
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

    #[cfg(wgpu_arm)]
    fn ensure_blit_pipeline(&mut self, format: wgpu::TextureFormat) {
        if self.blit_pipelines.contains_key(&format) {
            return;
        }
        let pipeline = crate::pipeline_table::build_wgpu(
            Pipeline::Blit.spec(),
            &self.ctx.device,
            &self.blit_shader,
            &self.blit_layout,
            self.ctx.pipeline_targets().with_present(format),
        );
        self.blit_pipelines.insert(format, pipeline);
    }

    #[cfg(wgpu_arm)]
    fn ensure_tray_pipeline(&mut self, format: wgpu::TextureFormat) {
        if self.tray_pipelines.contains_key(&format) {
            return;
        }
        let pipeline = crate::pipeline_table::build_wgpu(
            Pipeline::Tray.spec(),
            &self.ctx.device,
            &self.tray_shader,
            &self.tray_layout,
            self.ctx.pipeline_targets().with_present(format),
        );
        self.tray_pipelines.insert(format, pipeline);
    }

    #[cfg(wgpu_arm)]
    fn ensure_tray_overlay(&mut self, win: &mut WindowGpu, rgba: &[u8], w: u32, h: u32) {
        let recreate = match &win.tray_overlay {
            Some(t) => t.w != w || t.h != h,
            None => true,
        };
        if recreate {
            let texture = self.ctx.device_layer().create_texture_2d(
                "aterm-gpu tray overlay",
                crate::device_layer::TexelFormat::Rgba8Unorm,
                w,
                h,
                crate::device_layer::TexUsage::UPLOADED_SAMPLED,
                None,
            );
            let view = texture
                .wgpu()
                .create_view(&wgpu::TextureViewDescriptor::default());
            let bind = self
                .ctx
                .device
                .create_bind_group(&wgpu::BindGroupDescriptor {
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
                bind,
                w,
                h,
                pixels: Vec::new(),
            });
        }
        let overlay = win.tray_overlay.as_mut().expect("tray_overlay set above");
        let stride = w as usize * 4;
        let (y0, y1) = if recreate || overlay.pixels.len() != rgba.len() {
            (0, h)
        } else {
            match changed_row_band(&overlay.pixels, rgba, stride) {
                Some(band) => band,
                None => return,
            }
        };
        let band = &rgba[y0 as usize * stride..y1 as usize * stride];
        self.ctx
            .device_layer()
            .upload_texture_rows(&overlay.texture, y0, y1 - y0, band, w * 4, w);
        if overlay.pixels.len() == rgba.len() {
            overlay.pixels[y0 as usize * stride..y1 as usize * stride].copy_from_slice(band);
        } else {
            overlay.pixels.clear();
            overlay.pixels.extend_from_slice(rgba);
        }
        self.tray_uploads += 1;
        self.tray_upload_bytes += band.len() as u64;
    }

    #[cfg(wgpu_arm)]
    fn place_tray_scrim(&mut self, scrim: TrayScrim, (fw, fh): (u32, u32)) -> Option<[u32; 4]> {
        let top = scrim.top.min(fh);
        if top >= fh || fw == 0 {
            return None;
        }
        let fresh = self.tray_scrim.is_none();
        if fresh {
            let texture = self.ctx.device_layer().create_texture_2d(
                "aterm-gpu tray scrim",
                crate::device_layer::TexelFormat::Rgba8Unorm,
                1,
                1,
                crate::device_layer::TexUsage::UPLOADED_SAMPLED,
                None,
            );
            let view = texture
                .wgpu()
                .create_view(&wgpu::TextureViewDescriptor::default());
            let uniform = self
                .ctx
                .device_layer()
                .create_uniform_buffer(
                    "aterm-gpu tray scrim placement uniform",
                    std::mem::size_of::<TrayUniform>() as u64,
                )
                .into_wgpu();
            let bind = self
                .ctx
                .device
                .create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("aterm-gpu tray scrim bg"),
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
                            resource: uniform.as_entire_binding(),
                        },
                    ],
                });
            self.tray_scrim = Some(TrayScrimLayer {
                texture,
                bind,
                uniform,
                rgba: scrim.rgba,
            });
        }
        let layer = self.tray_scrim.as_mut().expect("tray_scrim set above");
        if fresh || layer.rgba != scrim.rgba {
            self.ctx
                .device_layer()
                .upload_texture_full(&layer.texture, &scrim.rgba, 4, 1, 1);
            layer.rgba = scrim.rgba;
        }
        let want = TrayUniform {
            rect: [0.0, top as f32, fw as f32, (fh - top) as f32],
            fb: [fw as f32, fh as f32],
            _pad: [0.0, 0.0],
        };
        self.ctx
            .queue
            .write_buffer(&layer.uniform, 0, aterm_bits::bytes_of(&want));
        Some([0, top, fw, fh])
    }

    #[cfg(wgpu_arm)]
    fn draw_tray_into_offscreen(&mut self, win: &mut WindowGpu, tray: TrayQuad<'_>) {
        let format = self.ctx.offscreen_format();

        self.ensure_tray_pipeline(format);
        self.ensure_tray_overlay(win, tray.rgba, tray.pw, tray.ph);

        let (fw, fh) = {
            let off = win.offscreen.as_ref().expect("encode_frame sets offscreen");
            (off.w, off.h)
        };
        note_offscreen_written(win, None, (fw, fh));
        let scrim = tray
            .scrim
            .and_then(|scrim| self.place_tray_scrim(scrim, (fw, fh)));
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
            .write_buffer(&self.tray_uniform_buf, 0, aterm_bits::bytes_of(&want));

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
                timestamp_writes: self.wgpu_pass_timestamps(),
                occlusion_query_set: None,
                multiview_mask: None,
            });
            let t = win
                .tray_overlay
                .as_ref()
                .expect("ensure_tray_overlay set it above");
            pass.set_pipeline(&self.tray_pipelines[&format]);
            if scrim.is_some()
                && let Some(layer) = self.tray_scrim.as_ref()
            {
                pass.set_bind_group(0, &layer.bind, &[]);
                pass.draw(0..4, 0..1);
            }
            pass.set_bind_group(0, &t.bind, &[]);
            pass.draw(0..4, 0..1); // 4-vert triangle-strip quad
        }
        self.ctx.queue.submit([enc.finish()]);
    }

    #[cfg(wgpu_arm)]
    fn draw_tray_over_present_copy(
        &mut self,
        win: &mut WindowGpu,
        tray: TrayQuad<'_>,
        enc: &mut wgpu::CommandEncoder,
    ) {
        let format = self.ctx.offscreen_format();
        self.ensure_tray_pipeline(format);
        self.ensure_tray_overlay(win, tray.rgba, tray.pw, tray.ph);
        let Some((fw, fh)) = win.present_offscreen.as_ref().map(|p| (p.w, p.h)) else {
            return;
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
            .write_buffer(&self.tray_uniform_buf, 0, aterm_bits::bytes_of(&want));
        let rect = [
            tray.dx.min(fw),
            tray.dy.min(fh),
            tray.dx.saturating_add(tray.pw).min(fw),
            tray.dy.saturating_add(tray.ph).min(fh),
        ];
        win.present_offscreen_fx = union_rect_opt(win.present_offscreen_fx, Some(rect), (fw, fh));
        let scrim = tray
            .scrim
            .and_then(|scrim| self.place_tray_scrim(scrim, (fw, fh)));
        if let Some(rect) = scrim {
            win.present_offscreen_fx =
                union_rect_opt(win.present_offscreen_fx, Some(rect), (fw, fh));
        }
        let po = win
            .present_offscreen
            .as_ref()
            .expect("the dims read above imply a resident copy");
        let t = win
            .tray_overlay
            .as_ref()
            .expect("ensure_tray_overlay set it above");
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("aterm-gpu tray present-copy pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &po.view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: self.wgpu_pass_timestamps(),
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.tray_pipelines[&format]);
        if scrim.is_some()
            && let Some(layer) = self.tray_scrim.as_ref()
        {
            pass.set_bind_group(0, &layer.bind, &[]);
            pass.draw(0..4, 0..1);
        }
        pass.set_bind_group(0, &t.bind, &[]);
        pass.draw(0..4, 0..1); // 4-vert triangle-strip quad
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AsymmetricPadLayout",
            action = "PrimeLayoutCache",
            project = "aterm_gpu::GpuRenderer::project_asymmetric_pad_layout"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
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
        let cur_blink = self.cpu.cursor_blink_phase();
        let cur_override = self.cpu.cursor_style_override();

        let (raw_w, raw_h) = self
            .cpu
            .checked_frame_size(input.rows, input.cols)
            .expect("renderable framebuffer");
        let (w, h) = (
            self.clamp_fb_dim(raw_w as u32) as usize,
            self.clamp_fb_dim(raw_h as u32) as usize,
        );

        let hit = match &win.gate_cache {
            Some(c) => {
                c.frame.width == w
                    && c.frame.height == h
                    && c.grid_top == self.cpu.grid_top()
                    && self.fx.prefetch_admitted(&input.fx_prefetch)
                    && is_unchanged_frame(
                        &c.input,
                        c.blink_phase,
                        c.cursor_style_override,
                        input,
                        cur_blink,
                        cur_override,
                        self.cpu.row_bands(),
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

        self.gate_misses += 1;
        let mut damage_input = win
            .gate_cache
            .take()
            .map_or_else(RenderInput::empty, |c| c.input);
        aterm_render::clone_damage_cache_from_stamped(&mut damage_input, input);
        let frame = self.render_input(win, input, None);
        win.gate_cache = Some(GpuGateCache {
            input: damage_input,
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

    /// Encode and submit the GPU work and wait for completion without reading pixels back.
    #[cfg(wgpu_arm)]
    pub fn render_no_readback(&mut self, win: &mut WindowGpu, input: &RenderInput) {
        win.present_prev = None;
        let _ = self.encode_frame(win, input, &RepaintScope::Full);
        self.ctx
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("GPU poll failed");
    }

    fn ensure_sprite_texture(
        &mut self,
        slot: &mut Option<Arc<SpriteTex>>,
        source: Option<&Arc<SceneAtlas>>,
        label: &'static str,
    ) {
        let Some(source) = source else {
            *slot = None;
            return;
        };
        let src = source.as_ref();
        let need = (src.width as usize)
            .saturating_mul(src.height as usize)
            .saturating_mul(4);
        if src.width == 0 || src.height == 0 || src.rgba.len() < need {
            *slot = None;
            return;
        }
        if slot.as_ref().is_some_and(|s| s.matches(source)) {
            return;
        }
        if let Some(texture) = self.sprite_textures.find(source) {
            *slot = Some(texture);
            return;
        }
        #[cfg(wgpu_arm)]
        let bind = {
            let handle = self.ctx.device_layer();
            let tex = handle.create_texture_2d(
                label,
                crate::device_layer::TexelFormat::Rgba8Unorm,
                src.width,
                src.height,
                crate::device_layer::TexUsage::UPLOADED_SAMPLED,
                None,
            );
            handle.upload_texture_full(
                &tex,
                &src.rgba[..need],
                src.width * 4,
                src.width,
                src.height,
            );
            let view = tex
                .wgpu()
                .create_view(&wgpu::TextureViewDescriptor::default());
            self.ctx
                .device
                .create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some(label),
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
                })
        };
        #[cfg(not(wgpu_arm))]
        let _ = label;
        let texture = Arc::new(SpriteTex {
            #[cfg(target_os = "macos")]
            metal_atlas: std::sync::OnceLock::new(),
            #[cfg(gl_arm)]
            gl_atlas: std::sync::Mutex::new(None),
            #[cfg(wgpu_arm)]
            bind,
            w: src.width,
            h: src.height,
            src: Arc::clone(source),
        });
        self.sprite_textures.insert(&texture);
        *slot = Some(texture);
    }

    #[inline]
    fn ensure_wallpaper_tex(&mut self, win: &mut WindowGpu, input: &RenderInput) {
        self.ensure_sprite_texture(
            &mut win.wallpaper_tex,
            input.wallpaper.as_ref(),
            "aterm-gpu wallpaper",
        );
    }

    fn record_frame_instance_totals(
        &mut self,
        passes: u32,
        build_ns: u64,
        stage_ns: u64,
        unorm: PlanUnorm,
        fx_staged: bool,
    ) {
        let mut shape = FrameShape {
            passes,
            build_ns,
            stage_ns,
            ablation: unorm.ablation,
            unorm_draws: unorm.draws,
            unorm_instances: unorm.instances,
            route: unorm.route,
            under_store: unorm.under_store,
            fetch_probe: self.fetch_probe(),
            ..FrameShape::default()
        };
        for id in StreamId::ALL {
            if id.is_fx() && !fx_staged {
                continue;
            }
            let (bytes, count) = self.inst.stream_bytes(id);
            shape.instances[id as usize] = count;
            shape.instance_bytes = shape.instance_bytes.saturating_add(bytes.len() as u64);
        }
        let fx_instances = if fx_staged {
            self.inst.fx_instances()
        } else {
            0
        };
        shape.fx_instances = u32::try_from(fx_instances).unwrap_or(u32::MAX);
        shape.fx_upload_bytes = (fx_instances * std::mem::size_of::<fx_gpu::FxInstance>()) as u64
            + if fx_instances > 0 {
                (self.fx.palette.len() * 4) as u64
            } else {
                0
            }
            + self.fx.atlas_bytes;
        shape.fx_atlas_bytes = self.fx.atlas_bytes;
        shape.fx_refused = self.fx.refused;
        (shape.fx_reader_waits, shape.fx_reader_wait_ns) = self.fx.reader_wait;
        self.frame_shape = shape;
        self.last_bg_instances = self.inst.bg.len();
        self.last_instances = self.inst.bg.len()
            + self.inst.image_below_bg.len()
            + self.inst.image_bg_cover.len()
            + self.inst.image_under.len()
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
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "gpu_encode",
            action = "Append",
            project = "aterm_gpu::renderer::project_bg_encode"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
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
        let build_started = aterm_time::Instant::now();
        self.overshoot_surprise = false;
        #[cfg(target_os = "macos")]
        let pending_band_shift = self.metal_pending_band_shift.take();
        #[cfg(gl_arm)]
        {
            self.last_frame_arm_gl = false;
        }
        self.cpu.evict_caches_if_large();
        let (rows, cols) = (input.rows, input.cols);
        let (cw, ch) = self.cpu.cell_size();
        let baseline = self.cpu.baseline();
        let chrome_room = self.cpu.chrome_room_for(input);
        let ring_floor = self.cpu.chrome_ring_floor();
        let pad = self.cpu.pad();
        let grid_top = self.cpu.grid_top();
        let (fb_w, fb_h) = self.cpu.frame_size(rows, cols);
        let w = self.clamp_fb_dim(fb_w as u32);
        let h = self.clamp_fb_dim(fb_h as u32);
        let chrome = self.cpu.chrome_extent_px(h as usize);
        win.chrome_y0 = u32::try_from(chrome.start).unwrap_or(h);
        win.chrome_y1 = u32::try_from(chrome.end).unwrap_or(h);
        let vis_rows = (h as usize / ch).saturating_add(2).min(rows);
        let clip_cols = (w as usize) < fb_w;
        let place = |r: usize, c: usize, row_uniform: bool, rcw: usize| {
            if row_uniform {
                (c * rcw, rcw, None)
            } else {
                let (x, cw_run, x0, x1) = aterm_render::cell_x_run(input, r, c, cw);
                (x, cw_run, Some((pad + x0, pad + x1)))
            }
        };
        let off_right = |cx: usize, ccw: usize| clip_cols && pad + cx >= w as usize + ccw;
        let run_scale = |r: usize, c: usize, (rx0, rx1): (usize, usize)| {
            let (run_size, _, _) = input.line_size_run_at(r, c);
            let (mut s, a) = aterm_render::chrome_lifted(
                input,
                r,
                grid_top + r * ch,
                ch,
                chrome_room,
                aterm_render::row_scale(run_size, grid_top + r * ch, ch, r + 1 == input.rows),
            );
            s.clip_x0 = s.clip_x0.max(rx0 as i32);
            s.clip_x1 = s.clip_x1.min(rx1 as i32);
            (s, a)
        };

        self.ensure_deco_atlas(cw, ch);
        self.ensure_wallpaper_tex(win, input);

        let row_active = |r: usize| -> bool {
            match scope {
                RepaintScope::Full => true,
                RepaintScope::Dirty(dirty) => dirty.get(r).copied().unwrap_or(false),
            }
        };
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

        let wallpaper_on = input.wallpaper.is_some()
            && win
                .wallpaper_tex
                .as_ref()
                .is_some_and(|s| s.w == w && s.h == h);
        let wallpaper_quad = |x: f32, y: f32, rw: f32, rh: f32| GlyphInstance {
            rect: [x, y, rw, rh],
            uv: [x / w as f32, y / h as f32, rw / w as f32, rh / h as f32],
            color: [255, 255, 255, 255],
            bg: [0, 0, 0, 0],
        };
        let rendered: &[Vec<aterm_core::terminal::RenderCell>] = &input.cells;

        let (cr, cc) = (input.cursor_row, input.cursor_col);
        let cursor_in = cr < rows && cc < cols;
        let style = aterm_render::effective_cursor_style(input, self.cpu.cursor_style_override());
        let cursor_drawn = cursor_in
            && input.cursor_visible
            && aterm_render::cursor_shown(style, self.cpu.cursor_blink_phase());
        let block_cursor =
            cursor_drawn && matches!(style, CursorStyle::BlinkingBlock | CursorStyle::SteadyBlock);

        self.inst.clear();

        if wallpaper_on && matches!(scope, RepaintScope::Full) {
            self.inst
                .wallpaper
                .push(wallpaper_quad(0.0, 0.0, w as f32, h as f32));
        }

        let mut glyph_win = std::mem::take(&mut self.glyph_win_scratch);
        glyph_windows(
            &mut self.cpu,
            input,
            scope,
            grid_top,
            ch,
            vis_rows,
            &mut glyph_win,
        );
        let mut row_plans = std::mem::take(&mut self.row_plans);
        row_plans.resize_with(rendered.len(), Vec::new);
        let mut break_cols = std::mem::take(&mut self.ligature_break_scratch);
        for (r, plan) in row_plans.iter_mut().enumerate().take(vis_rows) {
            if glyph_win[r].is_none() {
                continue;
            }
            self.cpu
                .ligature_break_cols_for_row_into(input, r, &mut break_cols);
            self.cpu.row_glyph_plan(input, r, &break_cols, plan);
        }
        self.ligature_break_scratch = break_cols;

        let (cur_x, cur_cw, cur_run) = if cr < rows {
            if aterm_render::row_is_uniform(input, cr) {
                let rcw = aterm_render::row_cell_w(input.line_sizes[cr], cw);
                (cc * rcw, rcw, None)
            } else {
                let (x, rcw, x0, x1) = aterm_render::cell_x_run(input, cr, cc, cw);
                (x, rcw, Some((pad + x0, pad + x1)))
            }
        } else {
            (cc * cw, cw, None)
        };
        let cursor_wide_lead = block_cursor
            && rendered
                .get(cr)
                .and_then(|row| row.get(cc + 1))
                .is_some_and(|n| n.wide);
        let cur_block_w = if cursor_wide_lead { 2 * cur_cw } else { cur_cw };
        let cursor_cell = rendered.get(cr).and_then(|row| row.get(cc));
        let cut_out_on = block_cursor
            && cursor_cell
                .is_some_and(|cell| !cell.wide && cell.ch != ' ' && !cell.ch.is_control());
        let (cut_lo, cut_hi) = if cut_out_on {
            aterm_render::cursor_cutout_cols(row_plans.get(cr).map_or(&[][..], Vec::as_slice), cc)
        } else {
            (1, 0)
        };
        let (cut_x0, cut_x1) = if cut_out_on {
            let x0 = (pad + cur_x) as i32;
            let (mut lo, mut hi) = (x0, x0 + cur_block_w as i32);
            if let Some((rx0, rx1)) = cur_run {
                lo = lo.max(rx0 as i32);
                hi = hi.min(rx1 as i32);
            }
            (lo, hi)
        } else {
            (0, 0)
        };
        let cutout_color = cursor_cell.map_or([0, 0, 0, 0], |cell| rgb4(cell.bg));

        let mut keys = std::mem::take(&mut self.inst.keys);
        let mut key_order = std::mem::take(&mut self.inst.key_order);
        let mut key_scratch = std::mem::take(&mut self.key_scratch);
        key_scratch.resize(vis_rows * cols, None);
        for (r, cells) in rendered.iter().enumerate().take(vis_rows) {
            // A Dirty repaint needs keys only for the rows it draws glyphs on;
            // resident atlases hold the rest.
            if glyph_win[r].is_none() {
                continue;
            }
            let plan = &row_plans[r];
            let row_raster = input.chrome_raster(r);
            let rcw = aterm_render::row_cell_w(
                input
                    .line_sizes
                    .get(r)
                    .copied()
                    .unwrap_or(aterm_core::grid::LineSize::SingleWidth),
                cw,
            );
            let row_uniform = aterm_render::row_is_uniform(input, r);
            let row_has_images = input.images.get(r).is_some_and(|v| !v.is_empty());
            let row_has_clusters = input.clusters.get(r).is_some_and(|v| !v.is_empty());
            let row_has_combining = input.combining.get(r).is_some_and(|v| !v.is_empty());
            for (c, cell) in cells.iter().take(cols).enumerate() {
                let (cell_x, ccw, _) = place(r, c, row_uniform, rcw);
                if off_right(cell_x, ccw) {
                    if row_uniform {
                        break; // off the right edge of the clamped framebuffer — see vis_rows
                    }
                    continue; // a mixed row's runs are not monotone in x
                }
                let slot = &mut key_scratch[r * cols + c];
                if !Self::drawable(cell) || (row_has_images && input.image_hides_glyph_at(r, c)) {
                    *slot = None;
                    continue;
                }
                let cluster = if row_has_clusters {
                    input.cluster_at(r, c)
                } else {
                    None
                };
                let key = match plan
                    .get(c)
                    .copied()
                    .unwrap_or(aterm_render::ColumnGlyph::PerCell)
                {
                    aterm_render::ColumnGlyph::Ligated(gid) => {
                        self.cpu.ligature_key(gid, aterm_render::cell_style(cell))
                    }
                    aterm_render::ColumnGlyph::LigatedSlice { gid, k, .. } => self
                        .cpu
                        .ligature_slice_key(gid, k, aterm_render::cell_style(cell)),
                    aterm_render::ColumnGlyph::PerCell => self.cell_key(
                        cluster,
                        cell,
                        aterm_render::materialized_cell_span(cells, c),
                    ),
                };
                let key = self
                    .cpu
                    .chrome_icon_key(row_raster, cells, c)
                    .unwrap_or(key);
                let key = aterm_render::shade_phase_key(key, pad + cell_x, grid_top + r * ch);
                *slot = Some(key);
                if keys.insert(key) {
                    key_order.push(key);
                }
                if row_has_combining
                    && cluster.is_none()
                    && let Some(marks) = input.combining_at(r, c)
                {
                    for &m in marks {
                        let mk = self.cpu.combining_mark_key(m, cell);
                        if keys.insert(mk) {
                            key_order.push(mk);
                        }
                    }
                }
            }
        }
        self.ensure_atlases(&mut key_order);
        self.inst.keys = keys;
        self.inst.key_order = key_order;
        #[cfg(target_os = "macos")]
        let armed = {
            let was_armed = self.metal_armed;
            let armed = self.metal_arm_ready();
            if was_armed && !armed {
                crate::stderr_line!("aterm-gpu: {}", self.metal_start_error());
            }
            armed
        };
        #[cfg(gl_arm)]
        let gl_armed = self.gl_arm_ready();
        #[cfg(not(wgpu_arm))]
        let fx_ok = true;
        #[cfg(wgpu_arm)]
        let fx_ok = {
            #[cfg(target_os = "macos")]
            let first_party = armed;
            #[cfg(gl_arm)]
            let first_party = gl_armed;
            #[cfg(not(any(target_os = "macos", gl_arm)))]
            let first_party = false;
            first_party
                || input.fx_layers.is_empty()
                || self
                    .effects
                    .fx_layouts(&self.ctx.device)
                    .inspect_err(|why| note_fx_closed(why))
                    .is_ok()
        };
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
        let (aw, ah) = (atlas.width as f32, mono_res.tex_h as f32);
        let (caw, cah) = (color_atlas.width as f32, color_res.tex_h as f32);
        #[cfg(wgpu_arm)]
        let atlas_bind = &mono_res.bind;
        #[cfg(wgpu_arm)]
        let color_bind = &color_res.bind;
        #[cfg(wgpu_arm)]
        let deco_bind = self.deco_atlas.as_ref().map(|d| &d.bind);

        let backdrop_margins_active = self.backdrop_margins_active();
        let backdrop_margin_alpha_byte = (self.backdrop_margin_alpha() * 255.0).round() as u8;
        let bg_inst = &mut self.inst.bg;
        let glyph_inst = &mut self.inst.glyph;
        let glyph_halo_inst = &mut self.inst.glyph_halo;
        let fire_present = !input.fire_patch.is_empty();
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
        let frame_bg = frame_live_bg(input, self.theme.bg);
        let frame_cursor = if input.cursor_color == aterm_core::render::COLOR_UNSET {
            self.theme.cursor
        } else {
            input.cursor_color
        };
        let bg_opacity = self.cpu.background_opacity();
        let bg_alpha: u8 = aterm_render::vibrancy::bg_quad_alpha(bg_opacity);
        let cursor_cov: u8 = (self.cpu.cursor_opacity() * 255.0).round() as u8;
        let cursor_opaque = cursor_cov == 255;
        let theme_bg = {
            let mut c = rgb4_u32(frame_bg);
            c[3] = bg_alpha;
            c
        };
        let backdrop_margins_on = backdrop_margins_active && !wallpaper_on;
        let margin_bg = {
            let mut c = rgb4_u32(frame_bg);
            c[3] = backdrop_margin_alpha_byte;
            c
        };
        let strip_bg = if backdrop_margins_on {
            margin_bg
        } else {
            theme_bg
        };
        let selection_palette = self.cpu.selection_palette(input);
        let min_contrast = self.cpu.effective_minimum_contrast();
        let may_have_sel = input.any_selection();
        let cell_selection =
            |r: usize,
             c: usize,
             cells: &[aterm_core::terminal::RenderCell],
             cell: &aterm_core::terminal::RenderCell| {
                let hit = if may_have_sel {
                    input.selection_hit(r, c, cells.get(c + 1).is_some_and(|n| n.wide), cell.wide)
                } else {
                    None
                };
                let colors = selection_palette.at_hit(hit);
                let bg_under = if hit.is_some() {
                    rgb4_u32(colors.bg)
                } else {
                    rgb4(cell.bg)
                };
                (hit.is_some(), colors, bg_under)
            };
        let base_cursor = match input.cursor_fill_override {
            Some(fill) => fill,
            None => frame_cursor,
        };
        let cursor_default_bg = aterm_render::resolved_default_bg_at(input, cr, cc, self.theme.bg);
        let cursor_fill = aterm_render::floor_cursor_fill(
            base_cursor,
            cursor_cell.map_or(cursor_default_bg, |cell| aterm_render::rgb_to_u32(cell.bg)),
            min_contrast,
        );
        if matches!(scope, RepaintScope::Dirty(_))
            && let Some((f, last)) = dirty_band
        {
            if f == 0 && grid_top > 0 {
                if wallpaper_on {
                    self.inst
                        .wallpaper
                        .push(wallpaper_quad(0.0, 0.0, w as f32, grid_top as f32));
                } else {
                    bg_inst.push(BgInstance {
                        rect: [0, 0, w as u16, grid_top as u16],
                        color: strip_bg,
                    });
                }
            }
            let grid_bot = ((grid_top + rows * ch) as u32).min(h);
            if last + 1 >= rows && grid_bot < h {
                if wallpaper_on {
                    self.inst.wallpaper.push(wallpaper_quad(
                        0.0,
                        grid_bot as f32,
                        w as f32,
                        (h - grid_bot) as f32,
                    ));
                } else {
                    bg_inst.push(BgInstance {
                        rect: [0, grid_bot as u16, w as u16, (h - grid_bot) as u16],
                        color: strip_bg,
                    });
                }
            }
        }
        if backdrop_margins_on && matches!(scope, RepaintScope::Full) {
            let grid_bot = ((grid_top + rows * ch) as u32).min(h);
            if grid_top > 0 {
                bg_inst.push(BgInstance {
                    rect: [0, 0, w as u16, sat_pos_u16(grid_top)],
                    color: margin_bg,
                });
            }
            if grid_bot < h {
                bg_inst.push(BgInstance {
                    rect: [0, grid_bot as u16, w as u16, (h - grid_bot) as u16],
                    color: margin_bg,
                });
            }
            if pad > 0 {
                let gutter_h = grid_bot.saturating_sub(grid_top as u32) as u16;
                let right = sat_pos_u16((w as usize).saturating_sub(pad));
                bg_inst.push(BgInstance {
                    rect: [0, sat_pos_u16(grid_top), sat_pos_u16(pad), gutter_h],
                    color: margin_bg,
                });
                bg_inst.push(BgInstance {
                    rect: [right, sat_pos_u16(grid_top), sat_pos_u16(pad), gutter_h],
                    color: margin_bg,
                });
            }
        }
        for (r, cells) in rendered.iter().enumerate().take(vis_rows) {
            if !row_active(r) {
                continue;
            }
            let y0u = sat_pos_u16(grid_top + r * ch);
            let rcw = aterm_render::row_cell_w(input.line_sizes[r], cw);
            let row_uniform = aterm_render::row_is_uniform(input, r);
            let mut bg_run: Option<BgRun> = None;
            if matches!(scope, RepaintScope::Dirty(_)) {
                if wallpaper_on {
                    self.inst.wallpaper.push(wallpaper_quad(
                        0.0,
                        (grid_top + r * ch) as f32,
                        w as f32,
                        ch as f32,
                    ));
                } else {
                    bg_inst.push(BgInstance {
                        rect: [0, y0u, w as u16, ch as u16],
                        color: theme_bg,
                    });
                    if backdrop_margins_on && pad > 0 {
                        let right = sat_pos_u16((w as usize).saturating_sub(pad));
                        bg_inst.push(BgInstance {
                            rect: [0, y0u, sat_pos_u16(pad), ch as u16],
                            color: margin_bg,
                        });
                        bg_inst.push(BgInstance {
                            rect: [right, y0u, sat_pos_u16(pad), ch as u16],
                            color: margin_bg,
                        });
                    }
                }
            }
            let (bg_y, bg_h) = if r == 0
                && grid_top > 0
                && self
                    .cpu
                    .chrome_bleed()
                    .is_some_and(|b| b.first == 0 && b.rows > 0 && b.top_extends_cells)
            {
                (0u16, sat_pos_u16(grid_top + ch))
            } else {
                (y0u, ch as u16)
            };
            if let Some(bleed) = self
                .cpu
                .chrome_bleed()
                .filter(|b| (b.first..b.rows).contains(&r))
            {
                let tone = |rgb: u32| {
                    let mut c = rgb4_u32(rgb);
                    if backdrop_margins_on {
                        c[3] = margin_bg[3];
                    }
                    c
                };
                let color = tone(bleed.color);
                let (left_tone, right_tone) = bleed.gutter_tones(r);
                let right = sat_pos_u16((w as usize).saturating_sub(pad));
                bg_inst.push(BgInstance {
                    rect: [0, y0u, sat_pos_u16(pad), ch as u16],
                    color: tone(left_tone),
                });
                bg_inst.push(BgInstance {
                    rect: [right, y0u, sat_pos_u16(pad), ch as u16],
                    color: tone(right_tone),
                });
                if r == 0 && grid_top > 0 {
                    bg_inst.push(BgInstance {
                        rect: [0, 0, w as u16, grid_top as u16],
                        color,
                    });
                }
                if let Some(seam) = bleed.seam.filter(|_| {
                    r + 1 == bleed.rows
                        && !aterm_render::chrome_rail_covers_gutters(input.chrome_raster(r))
                }) {
                    let deco = self.cpu.deco_metrics();
                    let t = deco.underline_t.max(1).min(ch);
                    let uy = sat_pos_u16(grid_top + r * ch + deco.underline_y.min(ch - t));
                    let seam = rgb4_u32(seam);
                    bg_inst.push(BgInstance {
                        rect: [0, uy, sat_pos_u16(pad), t as u16],
                        color: seam,
                    });
                    bg_inst.push(BgInstance {
                        rect: [right, uy, sat_pos_u16(pad), t as u16],
                        color: seam,
                    });
                }
            }
            let has_pane_defaults = input
                .default_bg_spans
                .get(r)
                .is_some_and(|spans| !spans.is_empty());
            let row_has_below_bg_images = input.images.get(r).is_some_and(|imgs| {
                imgs.iter().any(|(_, image)| {
                    aterm_render::kitty_image_is_below_non_default_bg(image.image.z_index)
                })
            });
            for (c, cell) in cells.iter().take(cols).enumerate() {
                let (cx, ccw, run) = place(r, c, row_uniform, rcw);
                if off_right(cx, ccw) {
                    if row_uniform {
                        break;
                    }
                    continue;
                }
                let x0u = sat_pos_u16(pad + cx);
                let hit = if may_have_sel {
                    let is_wide_lead = cells.get(c + 1).is_some_and(|n| n.wide);
                    input.selection_hit(r, c, is_wide_lead, cell.wide)
                } else {
                    None
                };
                let selected = hit.is_some();
                let theme_selection = selection_palette.at_hit(hit).bg;
                let cell_bg = aterm_render::rgb_to_u32(cell.bg);
                let default_bg = if has_pane_defaults {
                    aterm_render::resolved_default_bg_at(input, r, c, self.theme.bg)
                } else {
                    frame_bg
                };
                if wallpaper_on && !selected && cell_bg == default_bg {
                    continue;
                }
                let color = if selected {
                    rgb4_u32(theme_selection)
                } else {
                    let mut c = rgb4(cell.bg);
                    if bg_alpha != 255 && cell_bg == default_bg {
                        c[3] = bg_alpha;
                    }
                    c
                };
                let (bx, bw) = match run {
                    None => (x0u, ccw as u16),
                    Some((rx0, rx1)) => {
                        let Some((sx, sw)) = clip_x_span(pad + cx, ccw, rx0, rx1) else {
                            continue;
                        };
                        (sat_pos_u16(sx), sw as u16)
                    }
                };
                let instance = BgInstance {
                    rect: [bx, y0u, bw, ch as u16],
                    color,
                };
                push_bg_run(bg_inst, &mut bg_run, bg_y, bg_h, bx, bw, color);
                if row_has_below_bg_images
                    && (selected || cell_bg != default_bg)
                    && input.images_at(r, c).iter().any(|(_, image)| {
                        aterm_render::kitty_image_is_below_non_default_bg(image.image.z_index)
                    })
                {
                    self.inst.image_bg_cover.push(instance);
                }
            }
            let materialized = cells.len().min(cols);
            let selection_reaches_tail = input
                .selection_row_span(r)
                .is_some_and(|(_, end)| usize::from(end) >= materialized);
            if materialized < cols && (selection_reaches_tail || has_pane_defaults) {
                for c in materialized..cols {
                    let (cx, ccw, run) = place(r, c, row_uniform, rcw);
                    if off_right(cx, ccw) {
                        if row_uniform {
                            break;
                        }
                        continue;
                    }
                    let hit = if selection_reaches_tail {
                        input.selection_hit(r, c, false, false)
                    } else {
                        None
                    };
                    let selected = hit.is_some();
                    let theme_selection = selection_palette.at_hit(hit).bg;
                    let default_bg = if has_pane_defaults {
                        aterm_render::resolved_default_bg_at(input, r, c, self.theme.bg)
                    } else {
                        frame_bg
                    };
                    let color = if selected {
                        rgb4_u32(theme_selection)
                    } else if wallpaper_on {
                        continue;
                    } else if default_bg != frame_bg {
                        let mut color = rgb4_u32(default_bg);
                        color[3] = bg_alpha;
                        color
                    } else {
                        continue;
                    };
                    let (bx, bw) = match run {
                        None => (sat_pos_u16(pad + cx), ccw as u16),
                        Some((rx0, rx1)) => {
                            let Some((sx, sw)) = clip_x_span(pad + cx, ccw, rx0, rx1) else {
                                continue;
                            };
                            (sat_pos_u16(sx), sw as u16)
                        }
                    };
                    let instance = BgInstance {
                        rect: [bx, y0u, bw, ch as u16],
                        color,
                    };
                    push_bg_run(bg_inst, &mut bg_run, bg_y, bg_h, bx, bw, color);
                    if selected
                        && row_has_below_bg_images
                        && input.images_at(r, c).iter().any(|(_, image)| {
                            aterm_render::kitty_image_is_below_non_default_bg(image.image.z_index)
                        })
                    {
                        self.inst.image_bg_cover.push(instance);
                    }
                }
            }
            flush_bg_run(bg_inst, &mut bg_run, bg_y, bg_h);
            let raster = input.chrome_raster(r);
            if let Some(m) = raster {
                let rail_h = aterm_render::chrome_fit(m, chrome_room).1.min(ch);
                aterm_render::visit_chrome_raster_runs(
                    m,
                    w as usize,
                    pad,
                    cw,
                    cols,
                    |x0, x1, color, rail| {
                        let (y, hgt) = if rail {
                            (y0u + (ch - rail_h) as u16, rail_h as u16)
                        } else {
                            (y0u, ch as u16)
                        };
                        bg_inst.push(BgInstance {
                            rect: [sat_pos_u16(x0), y, sat_pos_u16(x1 - x0), hgt],
                            color: rgb4_u32(color & 0x00ff_ffff),
                        });
                    },
                );
                for (x0, x1, ya, yb, color) in
                    aterm_render::chrome_ring_runs(m, w as usize, pad, cw, ch, ring_floor)
                {
                    let (ya, yb) = (ya.min(ch), yb.min(ch));
                    if yb <= ya || x1 <= x0 {
                        continue;
                    }
                    bg_inst.push(BgInstance {
                        rect: [
                            sat_pos_u16(x0),
                            y0u + ya as u16,
                            sat_pos_u16(x1 - x0),
                            (yb - ya) as u16,
                        ],
                        color: rgb4_u32(color),
                    });
                }
            }
        }
        for (r, cells) in rendered.iter().enumerate().take(vis_rows) {
            let Some(win) = glyph_win[r] else {
                continue;
            };
            let line_size = input.line_sizes[r];
            let rcw = aterm_render::row_cell_w(line_size, cw);
            let (row_own, row_anchor) = aterm_render::chrome_lifted(
                input,
                r,
                grid_top + r * ch,
                ch,
                chrome_room,
                aterm_render::row_scale(line_size, grid_top + r * ch, ch, r + 1 == input.rows),
            );
            let row_uniform = aterm_render::row_is_uniform(input, r);
            let (mut ink_walk, mut char_fg_walk) = fg_walks(input, r);
            let mut halo_walk =
                aterm_render::FireHaloWalk::new(u16::try_from(r).map_or(&[][..], |row| {
                    aterm_render::fire_halo_row_slice(&input.fire_halo, row)
                }));
            let raster_split = input.chrome_raster(r).and_then(|m| m.split);
            let windowed = win != (i32::MIN, i32::MAX);
            let mut ink_top = i32::MAX;
            type FgMemo = ((u32, [u8; 3], Option<usize>), ([u8; 4], [u8; 4]));
            let mut fg_memo: Option<FgMemo> = None;
            for (c, cell) in cells.iter().take(cols).enumerate() {
                let (cx, ccw, run) = place(r, c, row_uniform, rcw);
                if off_right(cx, ccw) {
                    if row_uniform {
                        break; // off the right edge of the clamped framebuffer — see vis_rows
                    }
                    continue; // a mixed row's runs are not monotone in x
                }
                let Some(key) = key_scratch[r * cols + c] else {
                    continue;
                };
                let (own, anchor_y) = match run {
                    None => (row_own, row_anchor),
                    Some(run_box) => run_scale(r, c, run_box),
                };
                let scale = clip_rows(own, win);
                let in_cutout =
                    cut_out_on && cursor_opaque && r == cr && c >= cut_lo && c <= cut_hi;
                let cut_scale = aterm_render::Scale {
                    clip_x0: scale.clip_x0.max(cut_x0),
                    clip_x1: scale.clip_x1.min(cut_x1),
                    ..scale
                };
                if let Some(slot) = color_atlas.map.get(&key) {
                    if slot.gw == 0 || slot.gh == 0 {
                        continue;
                    }
                    ink_top = ink_top.min(slot.ink_reach(anchor_y, baseline, own));
                    let Some((rect, uv)) =
                        slot.quad((pad + cx) as f32, anchor_y, baseline, scale, (caw, cah))
                    else {
                        continue;
                    };
                    color_inst.push(GlyphInstance {
                        rect,
                        uv,
                        color: [0, 0, 0, 0],
                        bg: [0, 0, 0, 0],
                    });
                    if in_cutout
                        && let Some((rect, uv)) =
                            slot.quad((pad + cx) as f32, anchor_y, baseline, cut_scale, (caw, cah))
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
                };
                ink_top = ink_top.min(slot.ink_reach(anchor_y, baseline, own));
                let base_fg = cell_base_fg(&mut ink_walk, &mut char_fg_walk, c, cell);
                let hit = if may_have_sel {
                    input.selection_hit(r, c, cells.get(c + 1).is_some_and(|n| n.wide), cell.wide)
                } else {
                    None
                };
                let cell_selected = hit.is_some();
                let aterm_render::SelectionColors {
                    bg: theme_selection,
                    fg: selection_fg,
                } = selection_palette.at_hit(hit);
                let fg_key = (base_fg, cell.bg, hit);
                let (glyph_color, bg_under) = match fg_memo {
                    Some((k, pair)) if k == fg_key => pair,
                    _ => {
                        let gc = rgb4_u32(aterm_render::effective_glyph_fg(
                            selection_fg,
                            min_contrast,
                            base_fg,
                            aterm_render::rgb_to_u32(cell.bg),
                            cell_selected,
                            theme_selection,
                        ));
                        let bu = if cell_selected {
                            rgb4_u32(theme_selection)
                        } else {
                            rgb4(cell.bg)
                        };
                        fg_memo = Some((fg_key, (gc, bu)));
                        (gc, bu)
                    }
                };
                let quad = slot.quad((pad + cx) as f32, anchor_y, baseline, scale, (aw, ah));
                if quad.is_none() && !windowed {
                    continue;
                }
                if fire_present && let Some(strength) = halo_walk.at(c as u16) {
                    let a = aterm_render::fire_halo_alpha(strength);
                    for &(dx, dy) in &aterm_render::HALO_DILATE_OFFSETS {
                        let (hrect, huv) = match (run, quad) {
                            (None, Some((rect, uv))) if !windowed => (
                                [rect[0] + dx as f32, rect[1] + dy as f32, rect[2], rect[3]],
                                uv,
                            ),
                            _ => {
                                let stamp = if windowed {
                                    clip_rows(
                                        own,
                                        (win.0.saturating_sub(dy), win.1.saturating_sub(dy)),
                                    )
                                } else {
                                    scale
                                };
                                let Some((hr, hu)) = slot.quad(
                                    (pad + cx) as f32 + dx as f32,
                                    anchor_y,
                                    baseline,
                                    stamp,
                                    (aw, ah),
                                ) else {
                                    continue;
                                };
                                ([hr[0], hr[1] + dy as f32, hr[2], hr[3]], hu)
                            }
                        };
                        glyph_halo_inst.push(GlyphInstance {
                            rect: hrect,
                            uv: huv,
                            color: [halo_rgb[0], halo_rgb[1], halo_rgb[2], a],
                            bg: [0, 0, 0, 0],
                        });
                    }
                }
                let Some((rect, uv)) = quad else {
                    continue;
                };
                match raster_split.filter(|sp| usize::from(sp.col) == c && !cell_selected) {
                    Some(sp) => {
                        let at = i32::try_from(sp.x).unwrap_or(i32::MAX);
                        let left_color = rgb4_u32(aterm_render::effective_glyph_fg(
                            selection_fg,
                            min_contrast,
                            sp.ink,
                            sp.bg,
                            false,
                            theme_selection,
                        ));
                        let halves = [
                            (
                                aterm_render::Scale {
                                    clip_x1: scale.clip_x1.min(at),
                                    ..scale
                                },
                                left_color,
                                rgb4_u32(sp.bg),
                            ),
                            (
                                aterm_render::Scale {
                                    clip_x0: scale.clip_x0.max(at),
                                    ..scale
                                },
                                glyph_color,
                                bg_under,
                            ),
                        ];
                        for (half, color, bg) in halves {
                            if let Some((rect, uv)) =
                                slot.quad((pad + cx) as f32, anchor_y, baseline, half, (aw, ah))
                            {
                                glyph_inst.push(GlyphInstance {
                                    rect,
                                    uv,
                                    color,
                                    bg,
                                });
                            }
                        }
                    }
                    None => glyph_inst.push(GlyphInstance {
                        rect,
                        uv,
                        color: glyph_color,
                        bg: bg_under,
                    }),
                }
                if in_cutout
                    && let Some((rect, uv)) =
                        slot.quad((pad + cx) as f32, anchor_y, baseline, cut_scale, (aw, ah))
                {
                    cursor_glyph_inst.push(GlyphInstance {
                        rect,
                        uv,
                        color: cutout_color,
                        bg: rgb4_u32(cursor_fill),
                    });
                }
            }
            let band_top = i32::try_from(grid_top + r * ch).unwrap_or(i32::MAX);
            if self
                .cpu
                .catch_unpredicted_overshoot(input, r, ink_top, band_top)
            {
                self.overshoot_surprise = true;
            }
        }
        self.row_plans = row_plans;
        self.key_scratch = key_scratch;
        for (r, cells) in rendered.iter().enumerate().take(vis_rows) {
            let Some(win) = glyph_win[r] else {
                continue;
            };
            if input.combining[r].is_empty() {
                continue;
            }
            let line_size = input.line_sizes[r];
            let rcw = aterm_render::row_cell_w(line_size, cw);
            let (scale, anchor_y) = aterm_render::chrome_lifted(
                input,
                r,
                grid_top + r * ch,
                ch,
                chrome_room,
                aterm_render::row_scale(line_size, grid_top + r * ch, ch, r + 1 == input.rows),
            );
            let row_uniform = aterm_render::row_is_uniform(input, r);
            let (mut ink_walk, mut char_fg_walk) = fg_walks(input, r);
            for entries in input.combining[r].chunk_by(|a, b| a.0 == b.0) {
                let c = entries[0].0;
                if c >= cols {
                    break;
                }
                let Some(cell) = cells.get(c) else {
                    break;
                };
                let (cx, ccw, run) = place(r, c, row_uniform, rcw);
                if off_right(cx, ccw) {
                    if row_uniform {
                        break; // off the right edge of the clamped framebuffer — see vis_rows
                    }
                    continue; // a mixed row's runs are not monotone in x
                }
                if !Self::drawable(cell)
                    || input.cluster_at(r, c).is_some()
                    || input.image_hides_glyph_at(r, c)
                {
                    continue;
                }
                let Some(marks) = input.combining_at(r, c) else {
                    continue;
                };
                let (scale, anchor_y) = match run {
                    None => (scale, anchor_y),
                    Some(run_box) => run_scale(r, c, run_box),
                };
                let scale = clip_rows(scale, win);
                let base_fg = cell_base_fg(&mut ink_walk, &mut char_fg_walk, c, cell);
                let (
                    cell_selected,
                    aterm_render::SelectionColors {
                        bg: theme_selection,
                        fg: selection_fg,
                    },
                    bg_under,
                ) = cell_selection(r, c, cells, cell);
                let mark_fg = rgb4_u32(aterm_render::effective_glyph_fg(
                    selection_fg,
                    min_contrast,
                    base_fg,
                    aterm_render::rgb_to_u32(cell.bg),
                    cell_selected,
                    theme_selection,
                ));
                let mut stack = aterm_render::MarkStack::default();
                for &m in marks {
                    let key = self.cpu.combining_mark_key(m, cell);
                    let Some(slot) = atlas.map.get(&key) else {
                        continue;
                    };
                    if slot.gw == 0 || slot.gh == 0 {
                        continue;
                    }
                    let Some((ink_h, ink_ymin)) = self.cpu.glyph_ink_box_cached(key) else {
                        continue;
                    };
                    if ink_h == 0 {
                        continue;
                    }
                    let mx = if row_uniform {
                        aterm_render::mark_cell_x(c, rcw, slot.gw as usize, slot.xmin, scale)
                            + pad as i32
                    } else {
                        aterm_render::mark_cell_x(0, ccw, slot.gw as usize, slot.xmin, scale)
                            + (pad + cx) as i32
                    } + aterm_render::mark_script_shift(cell.script, ccw);
                    let dy = stack.dy(ink_h, ink_ymin, baseline, ch, scale.ys);
                    let Some((rect, uv)) =
                        slot.quad(mx as f32, anchor_y + dy, baseline, scale, (aw, ah))
                    else {
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
        self.glyph_win_scratch = glyph_win;
        if let Some(plane) = win.image_plane.as_ref() {
            let (pw, ph) = (plane.w as f32, plane.h as f32);
            for (r, row_images) in input.images.iter().enumerate().take(vis_rows) {
                if !row_active(r) || row_images.is_empty() {
                    continue;
                }
                let rcw = aterm_render::row_cell_w(input.line_sizes[r], cw);
                let row_uniform = aterm_render::row_is_uniform(input, r);
                let y0 = (grid_top + r * ch) as f32;
                for (c, image) in row_images {
                    if *c >= cols {
                        continue;
                    }
                    let (cx, ccw, run) = place(r, *c, row_uniform, rcw);
                    if off_right(cx, ccw) {
                        continue;
                    }
                    let fp_w = image.image.cols as usize * cw;
                    let lift = usize::from(image.image.band_lift_px);
                    let fp_h = image.image.rows as usize * ch + lift;
                    let key = (std::sync::Arc::as_ptr(&image.image) as usize, fp_w, fp_h);
                    let Some(&(img_y0, dw, dh)) = plane.placements.get(&key) else {
                        continue;
                    };
                    let (fw, fh) = (fp_w as u32, fp_h as u32);
                    let lip = if image.cell_row == 0 { lift } else { 0 };
                    let sx0 = (image.cell_col as usize * cw) as u32;
                    let sy0 = (image.cell_row as usize * ch + lift - lip) as u32;
                    if sx0 >= fw || sy0 >= fh {
                        continue;
                    }
                    let mut tile_w = (cw as u32).min(fw - sx0);
                    let tile_h = ((ch + lip) as u32).min(fh - sy0);
                    if let Some((_, rx1)) = run {
                        let vis = u32::try_from(rx1.saturating_sub(pad + cx)).unwrap_or(u32::MAX);
                        tile_w = tile_w.min(vis);
                        if tile_w == 0 {
                            continue;
                        }
                    }
                    let x0 = (pad + cx) as f32;
                    let rect = [x0, y0 - lip as f32, tile_w as f32, tile_h as f32];
                    let uv = if (dw, dh) == (fw, fh) {
                        [
                            sx0 as f32 / pw,
                            (img_y0 + sy0) as f32 / ph,
                            tile_w as f32 / pw,
                            tile_h as f32 / ph,
                        ]
                    } else {
                        let (kx, ky) = (dw as f32 / fw as f32, dh as f32 / fh as f32);
                        [
                            sx0 as f32 * kx / pw,
                            (img_y0 as f32 + sy0 as f32 * ky) / ph,
                            tile_w as f32 * kx / pw,
                            tile_h as f32 * ky / ph,
                        ]
                    };
                    let image_inst =
                        if aterm_render::kitty_image_is_below_non_default_bg(image.image.z_index) {
                            &mut self.inst.image_below_bg
                        } else if image.image.z_index < 0 {
                            &mut self.inst.image_under
                        } else {
                            &mut self.inst.image
                        };
                    image_inst.push(GlyphInstance {
                        rect,
                        uv,
                        color: [0, 0, 0, 0],
                        bg: [0, 0, 0, 0],
                    });
                }
            }
        }
        if block_cursor && row_active(cr) {
            let mut color = rgb4_u32(cursor_fill);
            color[3] = cursor_cov;
            let fill = match cur_run {
                None => Some((pad + cur_x, cur_block_w)),
                Some((rx0, rx1)) => clip_x_span(pad + cur_x, cur_block_w, rx0, rx1),
            };
            if let Some((fx, fw)) = fill {
                self.inst.cursor_block.push(BgInstance {
                    rect: [
                        sat_pos_u16(fx),
                        sat_pos_u16(grid_top + cr * ch),
                        fw as u16,
                        ch as u16,
                    ],
                    color,
                });
            }
        }

        let dm = self.cpu.deco_metrics();
        let curly_mask_ok = aterm_render::undercurl_supported(cw, ch) && self.deco_atlas.is_some();
        let curl_atlas_w = self.deco_atlas.as_ref().map_or(0, |d| d.atlas_w);
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
            let row_uniform = aterm_render::row_is_uniform(input, r);
            let (mut ink_walk, mut char_fg_walk) = fg_walks(input, r);
            for (c, cell) in cells.iter().take(cols).enumerate() {
                let (cx, ccw, run) = place(r, c, row_uniform, rcw);
                if off_right(cx, ccw) {
                    if row_uniform {
                        break; // off the right edge of the clamped framebuffer — see vis_rows
                    }
                    continue; // a mixed row's runs are not monotone in x
                }
                if cell.wide
                    || (matches!(cell.underline, UnderlineStyle::None)
                        && !cell.strikethrough
                        && !cell.overline)
                {
                    continue;
                }
                let x = pad + cx;
                let is_wide_lead = cells.get(c + 1).is_some_and(|n| n.wide);
                let dw = if is_wide_lead { 2 * ccw } else { ccw };
                let base_fg = cell_base_fg(&mut ink_walk, &mut char_fg_walk, c, cell);
                let hit = if may_have_sel {
                    input.selection_hit(r, c, is_wide_lead, cell.wide)
                } else {
                    None
                };
                let cell_selected = hit.is_some();
                let aterm_render::SelectionColors {
                    bg: theme_selection,
                    fg: selection_fg,
                } = selection_palette.at_hit(hit);
                let inks = aterm_render::deco_inks(
                    cell,
                    selection_fg,
                    min_contrast,
                    base_fg,
                    cell_selected,
                    theme_selection,
                );
                let ucolor = rgb4_u32(inks.underline);
                let is_curly = matches!(cell.underline, UnderlineStyle::Curly);
                aterm_render::underline_rects_into(
                    &mut deco_rects,
                    cell.underline,
                    x,
                    y0,
                    dw,
                    ccw,
                    ch,
                    dm,
                    curly_mask_ok,
                );
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
                    let clipped = match run {
                        None => Some((rx, rw)),
                        Some((qx0, qx1)) => clip_x_span(rx, rw, qx0, qx1),
                    };
                    let Some((rx, rw)) = clipped else {
                        continue;
                    };
                    self.inst.deco.push(BgInstance {
                        rect: [sat_pos_u16(rx), sat_pos_u16(ry), rw as u16, rh as u16],
                        color: ucolor,
                    });
                }
                if is_curly && curly_mask_ok {
                    if !skip {
                        curl_spans.clear();
                        curl_spans.push((x, dw));
                    }
                    let curl_x0 = aterm_render::UNDERCURL_SPRITE * cw;
                    for ti in 0..dw / ccw.max(1) {
                        let tx0 = x + ti * ccw;
                        for &(sx, sw) in &curl_spans {
                            let mut lo = sx.max(tx0);
                            let mut hi = (sx + sw).min(tx0 + ccw);
                            if let Some((qx0, qx1)) = run {
                                lo = lo.max(qx0);
                                hi = hi.min(qx1);
                            }
                            if lo >= hi {
                                continue;
                            }
                            let scale = cw as f32 / ccw as f32;
                            let u0 = curl_x0 as f32 + (lo - tx0) as f32 * scale;
                            let uw = (hi - lo) as f32 * scale;
                            self.inst.curl.push(GlyphInstance {
                                rect: [lo as f32, y0 as f32, (hi - lo) as f32, ch as f32],
                                uv: [u0 / curl_atlas_w as f32, 0.0, uw / curl_atlas_w as f32, 1.0],
                                color: ucolor,
                                bg: [0, 0, 0, 0],
                            });
                        }
                    }
                }
                for (rect, ink) in [
                    (
                        cell.strikethrough
                            .then(|| aterm_render::strike_rect(x, y0, dw, ch, dm))
                            .flatten(),
                        inks.strike,
                    ),
                    (
                        cell.overline
                            .then(|| aterm_render::overline_rect(x, y0, dw, ch, dm))
                            .flatten(),
                        inks.overline,
                    ),
                ] {
                    let Some([rx, ry, rw, rh]) = rect else {
                        continue;
                    };
                    let clipped = match run {
                        None => Some((rx, rw)),
                        Some((qx0, qx1)) => clip_x_span(rx, rw, qx0, qx1),
                    };
                    let Some((rx, rw)) = clipped else {
                        continue;
                    };
                    self.inst.deco.push(BgInstance {
                        rect: [sat_pos_u16(rx), sat_pos_u16(ry), rw as u16, rh as u16],
                        color: rgb4_u32(ink),
                    });
                }
            }
        }
        self.deco_rects_scratch = deco_rects;
        self.deco_skip_scratch = skip_rects;
        self.curl_ink_scratch = curl_ink;
        self.curl_spans_scratch = curl_spans;
        if !input.cursor_trail.is_empty() {
            let trail_color = input.cursor_trail_color;
            for t in &input.cursor_trail {
                if t.row >= rows || t.col >= cols || !row_active(t.row) {
                    continue;
                }
                let (tx, tcw, bound) = if aterm_render::row_is_uniform(input, t.row) {
                    let rcw = aterm_render::row_cell_w(input.line_sizes[t.row], cw);
                    (t.col * rcw, rcw, cols * cw)
                } else {
                    let (x, cw_run, _, x1) = aterm_render::cell_x_run(input, t.row, t.col, cw);
                    (x, cw_run, x1)
                };
                if tx + tcw > bound {
                    continue;
                }
                let default_bg =
                    aterm_render::resolved_default_bg_at(input, t.row, t.col, self.theme.bg);
                let cell_bg = rendered
                    .get(t.row)
                    .and_then(|r| r.get(t.col))
                    .map_or(default_bg, |c| aterm_render::rgb_to_u32(c.bg));
                let color = aterm_render::blend_rgb(cell_bg, trail_color, t.alpha);
                let mut c4 = rgb4_u32(color);
                if bg_alpha != 255 && cell_bg == default_bg {
                    c4[3] = bg_alpha;
                }
                self.inst.trail.push(BgInstance {
                    rect: [
                        sat_pos_u16(pad + tx),
                        sat_pos_u16(grid_top + t.row * ch),
                        tcw as u16,
                        ch as u16,
                    ],
                    color: c4,
                });
            }
        }
        if !input.glow_under.is_empty() {
            for q in &input.glow_under {
                if (q.row as usize) >= rows || !row_active(q.row as usize) {
                    continue;
                }
                self.inst
                    .glow_under
                    .push(GlowInstance::of([q.x, q.y, q.w, q.h], q));
            }
        }

        if !input.fire_patch.is_empty() {
            let pad16 = pad as u16;
            for q in &input.fire_patch {
                if (q.row as usize) >= rows || !row_active(q.row as usize) {
                    continue;
                }
                let inst = FireInstance {
                    rect: [q.x, q.y, q.w, q.h],
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

        self.fx.atlas_bytes = 0;
        self.fx.reader_wait = (0, 0);
        self.fx.refused = 0;
        if fx_ok && !(input.fx_layers.is_empty() && input.fx_prefetch.is_empty()) {
            let cap = self.fx_atlas_cap();
            // The companions' bodies cut where their letters lie, as the CPU composite draws
            // them (`aterm_render::Renderer::cut_knock_layers`).
            let cut = self.cpu.cut_knock_layers(input, w as usize, h as usize);
            let layers = if cut {
                self.cpu.cut_layers()
            } else {
                input.fx_layers.as_slice()
            };
            self.fx.admit(layers, &input.fx_prefetch, cap);
            let f = self.cpu.fx_frame(w as usize, h as usize, rows);
            self.fx.build(
                &f,
                layers,
                &input.fx_occluders,
                row_active,
                &mut self.inst.fx,
            );
        }

        if !input.cursor_glow_add.is_empty() {
            for q in &input.cursor_glow_add {
                if (q.row as usize) >= rows || !row_active(q.row as usize) {
                    continue;
                }
                self.inst
                    .glow_add
                    .push(GlowInstance::of([q.x, q.y, q.w, q.h], q));
            }
        }

        if !input.glow_halo.is_empty() {
            for q in &input.glow_halo {
                if (q.row as usize) >= rows || !row_active(q.row as usize) {
                    continue;
                }
                let inst = RainGlowInstance {
                    rect: [q.x, q.y, q.w, q.h],
                    color: rgb4_u32(q.color),
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

        if !input.nova_add.is_empty() {
            let pad16 = pad as u16;
            let grid_top16 = grid_top as u16;
            for q in &input.nova_add {
                if (q.row as usize) >= rows || !row_active(q.row as usize) {
                    continue;
                }
                self.inst.nova_add.push(GlowInstance::of(
                    [
                        pad16.saturating_add(q.x),
                        grid_top16.saturating_add(q.y),
                        q.w,
                        q.h,
                    ],
                    q,
                ));
            }
        }

        if !input.rain_add.is_empty() {
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

        if !input.word_decorations.is_empty()
            && let Some(datlas_w) = self.deco_atlas.as_ref().map(|d| d.atlas_w)
        {
            for d in &input.word_decorations {
                let (row, col) = (d.row as usize, d.col as usize);
                if row >= rows || col >= cols || !row_active(row) {
                    continue;
                }
                if matches!(d.blend, aterm_render::DecoBlend::Add) && input.any_selection() {
                    let row_cells = &input.cells[row];
                    let is_wide_lead = row_cells.get(col + 1).is_some_and(|n| n.wide);
                    let cell_wide = row_cells.get(col).is_some_and(|n| n.wide);
                    if input.selection_contains_cell(row, col, is_wide_lead, cell_wide) {
                        continue;
                    }
                }
                let (deco_x, rcw, run) = if aterm_render::row_is_uniform(input, row) {
                    let rcw = aterm_render::row_cell_w(input.line_sizes[row], cw);
                    (col * rcw, rcw, None)
                } else {
                    let (x, cw_run, x0, x1) = aterm_render::cell_x_run(input, row, col, cw);
                    (x, cw_run, Some((pad + x0, pad + x1)))
                };
                let idx = deco_sprite_index(d.glyph);
                let mut rect = [
                    (pad + deco_x) as f32 + f32::from(d.dx),
                    grid_top as f32 + (row * ch) as f32 + f32::from(d.dy),
                    rcw as f32,
                    ch as f32,
                ];
                let mut uv = [
                    (idx * cw) as f32 / datlas_w as f32,
                    0.0,
                    cw as f32 / datlas_w as f32,
                    1.0,
                ];
                if !clip_textured_quad_x(&mut rect, &mut uv, run) {
                    continue;
                }
                let color = [
                    (d.color >> 16) as u8,
                    (d.color >> 8) as u8,
                    d.color as u8,
                    d.alpha,
                ];
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

        if cursor_drawn && !block_cursor && row_active(cr) {
            let cursor_color = {
                let mut c = rgb4_u32(cursor_fill);
                c[3] = cursor_cov;
                c
            };
            aterm_render::for_each_cursor_rect(
                style,
                pad + cur_x,
                grid_top + cr * ch,
                cur_cw,
                ch,
                |[x, y, rw, rh]| {
                    let clipped = match cur_run {
                        None => Some((x, rw)),
                        Some((rx0, rx1)) => clip_x_span(x, rw, rx0, rx1),
                    };
                    if let Some((x, rw)) = clipped {
                        self.inst.cursor.push(BgInstance {
                            rect: [sat_pos_u16(x), sat_pos_u16(y), rw as u16, rh as u16],
                            color: cursor_color,
                        });
                    }
                },
            );
        }

        let text_blend =
            u32::from(self.cpu.text_blending() == aterm_render::TextBlending::LinearCorrected);
        #[cfg(wgpu_arm)]
        {
            if self.uniform_written != Some((w, h, text_blend)) {
                self.ctx.queue.write_buffer(
                    &self.uniform_buf,
                    0,
                    aterm_bits::bytes_of(&Uniforms {
                        screen: [w as f32, h as f32],
                        text_blend: text_blend as f32,
                        _pad: 0.0,
                    }),
                );
                self.uniform_written = Some((w, h, text_blend));
            }
        }

        #[cfg(wgpu_arm)]
        ensure_effect_pipelines(
            &mut self.effects,
            &self.ctx.device,
            &self.inst,
            input,
            cursor_opaque,
        );
        #[cfg(gl_arm)]
        if gl_armed {
            let build_ns = elapsed_ns(build_started);
            let live = self.gl_arm.as_mut().expect("gl_arm_ready minted it");
            let tail = gl_arm::encode_tail(
                live,
                win,
                &mut self.frame_plan,
                gl_arm::GlTailIn {
                    inst: &self.inst,
                    mono: mono_res,
                    color: color_res,
                    atlas_epoch: self.atlas_epoch,
                    deco: self.deco_atlas.as_ref(),
                    scope,
                    dirty_band,
                    grid_top,
                    ch,
                    rows,
                    dims: (w, h),
                    clear: theme_color_alpha(frame_bg, f64::from(bg_opacity)),
                    text_blend,
                    cursor_opaque,
                    ablation: self.ablation,
                    under_opaque: fx_gpu::under_draws_opaque(input),
                    fx: &self.fx,
                },
            );
            match tail {
                Ok(out) => {
                    self.fx.atlas_bytes = out.fx_atlas_bytes;
                    self.last_frame_passes = out.passes as u32;
                    self.last_frame_load = out.load0;
                    self.last_frame_scissor = out.scissor;
                    self.record_frame_instance_totals(
                        out.passes as u32,
                        build_ns,
                        out.stage_ns,
                        out.unorm,
                        out.fx_staged,
                    );
                    self.last_frame_arm_gl = true;
                    return (w, h);
                }
                Err(e) => gl_arm::gl_arm_note(&format!(
                    "armed frame failed ({e}); {}",
                    if cfg!(wgpu_arm) {
                        "this frame draws on wgpu"
                    } else {
                        "this frame is lost"
                    }
                )),
            }
        }
        #[cfg(target_os = "macos")]
        if armed {
            'armed: {
                let _pool = crate::metal::ffi::AutoreleasePool::new();
                use crate::metal::loss::CbOutcome;

                let mut counts: [Option<u32>; STREAM_COUNT] = [None; STREAM_COUNT];
                let build_ns = elapsed_ns(build_started);
                let stage_ns;
                let mut fx_palette: Option<crate::metal::ffi::Obj> = None;
                {
                    let live = self
                        .metal_arm
                        .as_mut()
                        .expect("armed implies the arm minted")
                        .live_mut();
                    live.await_frame_slot();
                    let stage_started = aterm_time::Instant::now();
                    let fx_drawing = self.inst.fx_instances() > 0;
                    let fx_staged = live.sync_fx(&self.fx.atlas, fx_drawing).and_then(|bytes| {
                        self.fx.atlas_bytes = bytes;
                        self.fx.reader_wait = live.fx_reader_wait;
                        if !fx_drawing {
                            return Ok(None);
                        }
                        #[cfg(any(test, debug_assertions))]
                        if self.fx_fail_seam {
                            return Err("the staging seam refused".to_owned());
                        }
                        #[cfg(any(test, debug_assertions))]
                        if self.fx_palette_shared_seam {
                            live.write_shared_palette(&self.fx.palette)?;
                        }
                        let palette = live.upload_fx_palette(&self.fx.palette)?;
                        live.fx_hazard.bound(&palette);
                        Ok(Some(palette))
                    });
                    match fx_staged {
                        Ok(p) => fx_palette = p,
                        Err(e) => metal_arm_note(&format!(
                            "armed frame: fx residency failed ({e}); fx draws nothing"
                        )),
                    }
                    for id in StreamId::ALL {
                        if id.is_fx() && fx_palette.is_none() {
                            continue;
                        }
                        let (bytes, count) = self.inst.stream_bytes(id);
                        match live.upload_stream(id, bytes) {
                            Ok(true) => counts[id as usize] = Some(count),
                            Ok(false) => {}
                            Err(e) => {
                                metal_arm_note(&format!(
                                    "armed frame: stream staging failed ({e})"
                                ));
                                break 'armed;
                            }
                        }
                    }
                    stage_ns = elapsed_ns(stage_started);
                }
                let fx_staged = fx_palette.is_some();

                let armed_image_plane = win
                    .image_plane
                    .as_ref()
                    .is_some_and(|p| p.metal_texels.is_some());
                if win.image_plane.is_some() && !armed_image_plane {
                    metal_arm_note(
                        "armed frame: an image plane without resident texels — image \
                         draws withheld from the Metal plan (built before arming?)",
                    );
                }
                let mut plan = std::mem::take(&mut self.frame_plan);
                let mut unorm = build_frame_plan(
                    &mut plan,
                    &counts,
                    FramePlanCtx {
                        has_image_plane: armed_image_plane,
                        has_deco: self.deco_atlas.is_some(),
                        has_wallpaper: win.wallpaper_tex.is_some(),
                        cursor_opaque,
                        ablation: self.ablation,
                        under_opaque: fx_gpu::under_draws_opaque(input),
                    },
                );
                let seam = self.fetch_seam();
                let route = Self::metal_unorm_route(
                    self.metal_arm.as_mut(),
                    self.unorm_route,
                    seam,
                    unorm.draws > 0,
                );
                route_frame_plan(&mut plan, &mut unorm, route);
                macro_rules! armed_try {
                    ($what:literal, $e:expr) => {
                        match $e {
                            Ok(v) => v,
                            Err(e) => {
                                metal_arm_note(&format!(
                                    concat!("armed frame: ", $what, " ({})"),
                                    e
                                ));
                                self.frame_plan = plan;
                                break 'armed;
                            }
                        }
                    };
                }

                let recreate = win
                    .metal_offscreen
                    .as_ref()
                    .is_none_or(|o| o.w != w || o.h != h);
                if recreate {
                    let live = self
                        .metal_arm
                        .as_mut()
                        .expect("armed implies the arm minted")
                        .live_mut();
                    let tex = armed_try!(
                        "offscreen mint failed",
                        live.layer()
                            .try_create_texture_2d(
                                "aterm-gpu metal offscreen",
                                crate::device_layer::TexelFormat::Rgba8Unorm,
                                w,
                                h,
                                crate::device_layer::TexUsage::OFFSCREEN,
                                Some(crate::device_layer::TexelFormat::Rgba8UnormSrgb),
                            )
                            .map(crate::device_layer::LayerTexture::into_metal)
                    );
                    let srgb = armed_try!(
                        "offscreen sRGB alias failed",
                        tex.alias_view(crate::device_layer::TexelFormat::Rgba8UnormSrgb.metal())
                            .ok_or_else(|| "newTextureViewWithPixelFormat returned nil".to_owned())
                    );
                    win.metal_offscreen = Some(MetalOffscreen { tex, srgb, w, h });
                }

                let (load_op, scissor) = frame_load_and_scissor(
                    scope,
                    recreate,
                    dirty_band,
                    grid_top,
                    ch,
                    rows,
                    (w, h),
                    theme_color_alpha(frame_bg, f64::from(bg_opacity)),
                );
                note_offscreen_written(
                    win,
                    scissor.map(|(sx, sy, sw, sh)| [sx, sy, sx + sw, sy + sh]),
                    (w, h),
                );
                let load0 = load_op;

                let rig = {
                    let live = self
                        .metal_arm
                        .as_mut()
                        .expect("armed implies the arm minted")
                        .live_mut();
                    live.ensure_uniform(w, h, text_blend);
                    let mut uniform_vertex_slot: Option<u32> = None;
                    let mut uniform_fragment_slot: Option<u32> = None;
                    let mut psos: [Option<crate::metal::ffi::Obj>; pipeline_table::PIPELINE_COUNT] =
                        std::array::from_fn(|_| None);
                    let mut fetch_psos: [Option<crate::metal::ffi::Obj>;
                        pipeline_table::FETCH_TWIN_COUNT] = std::array::from_fn(|_| None);
                    let mut rig_err: Option<String> = None;
                    for item in &plan {
                        let row = item.pipe.row();
                        let spec = row.spec();
                        let vu = spec.binds.vertex_uniform.unwrap_or_else(|| {
                            panic!("frame row {row:?} declares no vertex uniform")
                        });
                        match uniform_vertex_slot {
                            None => uniform_vertex_slot = Some(vu),
                            Some(prev) => assert_eq!(
                                prev, vu,
                                "one shared uniform bind per pass requires one slot; \
                                 {row:?} disagrees"
                            ),
                        }
                        if let Some(&fb) = spec.binds.fragment_buffers.first() {
                            assert_eq!(
                                spec.binds.fragment_buffers.len(),
                                1,
                                "frame rows re-bind at most the one shared uniform block"
                            );
                            match uniform_fragment_slot {
                                None => uniform_fragment_slot = Some(fb),
                                Some(prev) => {
                                    assert_eq!(prev, fb, "{row:?} disagrees on the fragment slot")
                                }
                            }
                        }
                        let built = if item.fetch {
                            let twin = pipeline_table::FetchTwin::of(row).unwrap_or_else(|| {
                                panic!("a fetch draw's row {row:?} has no fetch twin")
                            });
                            let slot = &mut fetch_psos[twin as usize];
                            if slot.is_some() {
                                continue;
                            }
                            live.fetch_pso(twin)
                                .map(|pso| *slot = Some(pso.clone_retained()))
                        } else {
                            let slot = &mut psos[row as usize];
                            if slot.is_some() {
                                continue;
                            }
                            live.pso(row).map(|pso| *slot = Some(pso.clone_retained()))
                        };
                        if let Err(e) = built {
                            rig_err = Some(e);
                            break;
                        }
                    }

                    let mut atlases: [Option<(
                        crate::metal::resources::SealedTexture,
                        crate::metal::ffi::Obj,
                    )>; DRAW_ATLAS_COUNT] = std::array::from_fn(|_| None);
                    if rig_err.is_none() {
                        for item in &plan {
                            let Some(a) = item.atlas else { continue };
                            if atlases[a as usize].is_some() {
                                continue;
                            }
                            let (format, key, write_gen, cur_py, bytes, slot) = match a {
                                DrawAtlas::Fx => continue,
                                DrawAtlas::Mono => (
                                    crate::device_layer::TexelFormat::R8Unorm,
                                    (mono_res.atlas.width, mono_res.tex_h, self.atlas_epoch),
                                    mono_res.atlas.write_gen,
                                    mono_res.atlas.py,
                                    &mono_res.atlas.data
                                        [..(mono_res.atlas.width * mono_res.atlas.height) as usize],
                                    &mut live.atlases[a as usize],
                                ),
                                DrawAtlas::Color => (
                                    crate::device_layer::TexelFormat::Rgba8Unorm,
                                    (color_res.atlas.width, color_res.tex_h, self.atlas_epoch),
                                    color_res.atlas.write_gen,
                                    color_res.atlas.py,
                                    &color_res.atlas.data[..(color_res.atlas.width
                                        * color_res.atlas.height
                                        * 4)
                                        as usize],
                                    &mut live.atlases[a as usize],
                                ),
                                DrawAtlas::Deco => {
                                    let d =
                                        self.deco_atlas.as_ref().expect("has_deco gated this item");
                                    (
                                        crate::device_layer::TexelFormat::R8Unorm,
                                        (
                                            d.atlas_w as u32,
                                            d.ch as u32,
                                            ((d.curl_band.0 as u64) << 32)
                                                | (d.curl_band.1 as u64 & 0xffff_ffff),
                                        ),
                                        0,
                                        0,
                                        &d.data[..],
                                        &mut live.atlases[a as usize],
                                    )
                                }
                                DrawAtlas::Wallpaper => {
                                    let sprite = win.wallpaper_tex.as_ref().unwrap_or_else(|| {
                                        panic!("plan samples {a:?} but its SpriteTex is absent")
                                    });
                                    match sprite.metal_atlas(
                                        crate::device_layer::DeviceHandle::Metal(&live.mint),
                                        a,
                                    ) {
                                        Ok(resident) => {
                                            atlases[a as usize] = Some((
                                                resident.tex.metal().clone_handle(),
                                                live.nearest.clone_retained(),
                                            ));
                                        }
                                        Err(e) => {
                                            rig_err = Some(e);
                                            break;
                                        }
                                    }
                                    continue;
                                }
                                DrawAtlas::Image => {
                                    let p = win
                                        .image_plane
                                        .as_mut()
                                        .expect("has_image_plane gated this item");
                                    (
                                        crate::device_layer::TexelFormat::Rgba8Unorm,
                                        (p.w, p.h, p.epoch),
                                        0,
                                        0,
                                        p.metal_texels
                                            .as_deref()
                                            .expect("armed_image_plane requires resident texels"),
                                        &mut p.metal_atlas,
                                    )
                                }
                            };
                            match MetalArmLive::ensure_atlas(
                                crate::device_layer::DeviceHandle::Metal(&live.mint),
                                slot,
                                a,
                                key,
                                write_gen,
                                cur_py,
                                format,
                                bytes,
                                &mut live.frame_slots,
                            ) {
                                Ok((consumed, parked_ns)) => {
                                    live.awaited_frames += consumed;
                                    live.frame_slot_wait_ns =
                                        live.frame_slot_wait_ns.saturating_add(parked_ns);
                                }
                                Err(e) => {
                                    rig_err = Some(e);
                                    break;
                                }
                            }
                            let resident = slot.as_ref().expect("ensure_atlas filled the slot");
                            atlases[a as usize] = Some((
                                resident.tex.metal().clone_handle(),
                                live.nearest.clone_retained(),
                            ));
                        }
                    }
                    if let Some(e) = rig_err {
                        metal_arm_note(&format!("armed frame: rig assembly failed ({e})"));
                        self.frame_plan = plan;
                        break 'armed;
                    }
                    let fx = fx_palette.map(|palette| {
                        let tex = |k: fx_gpu::FxAtlasKind| {
                            live.fx_tex[k as usize]
                                .as_ref()
                                .expect("sync_fx mints both atlases on an fx frame")
                                .tex
                                .metal()
                                .clone_handle()
                        };
                        MetalFxBind {
                            r8: tex(fx_gpu::FxAtlasKind::R8),
                            rgba: tex(fx_gpu::FxAtlasKind::Rgba),
                            palette,
                        }
                    });

                    let mut streams: [Option<crate::metal::resources::SharedBuffer>; STREAM_COUNT] =
                        std::array::from_fn(|_| None);
                    for item in &plan {
                        let idx = item.stream as usize;
                        if streams[idx].is_none() {
                            streams[idx] = Some(crate::metal::resources::SharedBuffer::new(
                                live.current_slot().streams[idx]
                                    .as_ref()
                                    .unwrap_or_else(|| {
                                        panic!(
                                            "planned stream {:?} was not staged — the \
                                             staging loop's count coupling broke",
                                            item.stream
                                        )
                                    })
                                    .buf
                                    .metal()
                                    .clone_retained(),
                            ));
                        }
                    }

                    let off = win.metal_offscreen.as_ref().expect("ensured above");
                    MetalFrameRig {
                        offscreen: off.tex.clone_handle(),
                        offscreen_srgb: off.srgb.clone_handle(),
                        psos,
                        fetch_psos,
                        uniform: live.current_slot().uniform.metal().clone_retained(),
                        uniform_vertex_slot: uniform_vertex_slot.unwrap_or(0),
                        uniform_fragment_slot,
                        streams,
                        atlases,
                        fx,
                    }
                };

                let passes = {
                    let live = self
                        .metal_arm
                        .as_mut()
                        .expect("armed implies the arm minted")
                        .live_mut();
                    let mut enc = armed_try!(
                        "encoder refused",
                        crate::device_layer::FrameEncoder::metal(&live.session)
                    );
                    if let Some(shift) = pending_band_shift {
                        let scratch = armed_try!(
                            "band shift lacks its resident scratch",
                            win.metal_shift_scratch
                                .as_ref()
                                .filter(|t| (t.width(), t.height()) == (w as usize, h as usize))
                                .ok_or_else(|| format!("no {w}x{h} scratch resident"))
                        );
                        armed_try!(
                            "band shift copies refused",
                            record_band_shift(&mut enc, &rig.offscreen, scratch, shift, w)
                        );
                        live.encode_band_shifts
                            .set(live.encode_band_shifts.get().saturating_add(1));
                    }
                    let res = FrameRes::Metal(&rig);
                    let passes = armed_try!(
                        "frame-plan walk failed",
                        run_frame_plan(&mut enc, &res, &plan, load0, scissor)
                    );
                    match enc.submit() {
                        crate::device_layer::SubmittedFrame::Metal(submitted) => {
                            match submitted.try_outcome() {
                                Some(outcome) if outcome != CbOutcome::Completed => {
                                    metal_arm_note(&format!(
                                        "armed frame: command buffer ended {outcome:?} — the \
                                         loss latch has it; frame lost; the loss poll will downgrade to the CPU renderer"
                                    ));
                                    self.frame_plan = plan;
                                    break 'armed;
                                }
                                _ => live.current_slot_mut().inflight = Some(submitted),
                            }
                        }
                        #[cfg(wgpu_arm)]
                        crate::device_layer::SubmittedFrame::Wgpu => {
                            unreachable!("a metal encoder commits a metal frame")
                        }
                    }
                    passes
                };

                self.last_frame_passes = passes as u32;
                self.frame_plan = plan;
                self.last_frame_load = load0;
                self.last_frame_scissor = scissor;
                self.record_frame_instance_totals(
                    passes as u32,
                    build_ns,
                    stage_ns,
                    unorm,
                    fx_staged,
                );
                self.last_frame_arm_metal = true;
                return (w, h);
            }
        }

        #[cfg(not(wgpu_arm))]
        {
            #[cfg(target_os = "macos")]
            {
                self.last_frame_arm_metal = false;
            }
            (w, h)
        }
        #[cfg(wgpu_arm)]
        {
            let (device, queue) = (&self.ctx.device, &self.ctx.queue);
            let build_ns = elapsed_ns(build_started);
            let stage_started = aterm_time::Instant::now();
            let fx_ready = fx_wgpu_sync(
                &mut self.fx_wgpu,
                &mut self.fx,
                &mut self.effects,
                device,
                queue,
                &self.inst,
                fx_gpu::under_draws_opaque(input),
            );
            let mut streams: [Option<wgpu::BufferSlice<'_>>; STREAM_COUNT] = [None; STREAM_COUNT];
            let mut counts: [Option<u32>; STREAM_COUNT] = [None; STREAM_COUNT];
            for (&id, vb) in StreamId::ALL.iter().zip(self.vbufs.streams.iter_mut()) {
                if id.is_fx() && !fx_ready {
                    continue;
                }
                let (bytes, count) = self.inst.stream_bytes(id);
                if let Some(slice) = vb.upload(device, queue, bytes) {
                    streams[id as usize] = Some(slice);
                    counts[id as usize] = Some(count);
                }
            }
            let stage_ns = elapsed_ns(stage_started);

            let recreate = match &win.offscreen {
                Some(o) => o.w != w || o.h != h,
                None => true,
            };
            if recreate {
                let tex = self.ctx.offscreen_texture(w, h);
                let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
                let view_srgb = tex.create_view(&wgpu::TextureViewDescriptor {
                    format: Some(self.ctx.offscreen_srgb_view_format()),
                    ..Default::default()
                });
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
                win.offscreen = Some(Offscreen {
                    tex,
                    view,
                    view_srgb,
                    blit_bind,
                    w,
                    h,
                    bloom: None,
                });
            }

            let (load_op, scissor) = frame_load_and_scissor(
                scope,
                recreate,
                dirty_band,
                grid_top,
                ch,
                rows,
                (w, h),
                theme_color_alpha(frame_bg, f64::from(bg_opacity)),
            );
            note_offscreen_written(
                win,
                scissor.map(|(sx, sy, sw, sh)| [sx, sy, sx + sw, sy + sh]),
                (w, h),
            );

            #[cfg(wgpu_arm)]
            let wallpaper_bind = win.wallpaper_tex.as_ref().map(|s| &s.bind);

            let off = win.offscreen.as_ref().expect("offscreen set above");
            let view = &off.view;
            let view_srgb = &off.view_srgb;

            let mut plan = std::mem::take(&mut self.frame_plan);
            let unorm = build_frame_plan(
                &mut plan,
                &counts,
                FramePlanCtx {
                    has_image_plane: win.image_plane.is_some(),
                    has_deco: deco_bind.is_some(),
                    has_wallpaper: wallpaper_bind.is_some(),
                    cursor_opaque,
                    ablation: self.ablation,
                    under_opaque: fx_gpu::under_draws_opaque(input),
                },
            );
            let load0 = load_op;
            let res = FrameRes::Wgpu(WgpuFrameRes {
                bg_pipeline: &self.bg_pipeline,
                glyph_pipeline: &self.glyph_pipeline,
                color_glyph_pipeline: &self.color_glyph_pipeline,
                effects: &self.effects,
                uniform_bg: &self.uniform_bg,
                view,
                view_srgb,
                atlas_mono: atlas_bind,
                atlas_color: color_bind,
                atlas_deco: deco_bind,
                atlas_image: win.image_plane.as_ref().map(|p| &p.bind),
                atlas_wallpaper: wallpaper_bind,
                atlas_fx: self
                    .fx_wgpu
                    .bind
                    .as_ref()
                    .filter(|_| fx_ready)
                    .map(|(_, b)| b),
                streams,
            });
            let mut fenc = crate::device_layer::FrameEncoder::wgpu(
                &self.ctx.device,
                &self.ctx.queue,
                self.wgpu_timer.as_ref(),
                "aterm-gpu frame",
            );
            let passes = run_frame_plan(&mut fenc, &res, &plan, load0, scissor)
                .expect("the wgpu frame-plan arm has no fallible verb");

            match fenc.submit() {
                crate::device_layer::SubmittedFrame::Wgpu => {}
                #[cfg(target_os = "macos")]
                crate::device_layer::SubmittedFrame::Metal(_) => {
                    unreachable!("the production encode constructs the Wgpu arm only")
                }
                #[cfg(gl_arm)]
                crate::device_layer::SubmittedFrame::Gl(_) => {
                    unreachable!("the production encode constructs the Wgpu arm only")
                }
            }
            self.last_frame_passes = passes as u32;
            self.frame_plan = plan;
            self.last_frame_load = load0;
            self.last_frame_scissor = scissor;
            self.record_frame_instance_totals(passes as u32, build_ns, stage_ns, unorm, fx_ready);
            #[cfg(target_os = "macos")]
            {
                self.last_frame_arm_metal = false;
            }
            (w, h)
        }
    }
}

#[inline]
fn sat_pos_u16(v: usize) -> u16 {
    u16::try_from(v).unwrap_or(u16::MAX)
}

#[inline]
fn clip_x_span(x: usize, w: usize, x0: usize, x1: usize) -> Option<(usize, usize)> {
    let lo = x.max(x0);
    let hi = (x + w).min(x1);
    if hi <= lo {
        return None;
    }
    Some((lo, hi - lo))
}

#[inline]
fn clip_textured_quad_x(
    rect: &mut [f32; 4],
    uv: &mut [f32; 4],
    clip_x: Option<(usize, usize)>,
) -> bool {
    let Some((clip_x0, clip_x1)) = clip_x else {
        return rect[2] > 0.0;
    };
    let old_x = rect[0];
    let old_w = rect[2];
    if old_w <= 0.0 {
        return false;
    }
    let x0 = old_x.max(clip_x0 as f32);
    let x1 = (old_x + old_w).min(clip_x1 as f32);
    if x1 <= x0 {
        return false;
    }
    let left = (x0 - old_x) / old_w;
    let kept = (x1 - x0) / old_w;
    uv[0] += uv[2] * left;
    uv[2] *= kept;
    rect[0] = x0;
    rect[2] = x1 - x0;
    true
}

#[cfg(wgpu_arm)]
#[inline]
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

#[inline]
fn fg_walks(
    input: &RenderInput,
    r: usize,
) -> (aterm_render::InkWalk<'_>, aterm_render::CharFgWalk<'_>) {
    (
        aterm_render::InkWalk::new(aterm_render::sparse_row_slice(&input.ink, r)),
        aterm_render::CharFgWalk::new(aterm_render::sparse_row_slice(&input.char_fg, r)),
    )
}

#[inline]
fn cell_base_fg(
    ink_walk: &mut aterm_render::InkWalk<'_>,
    char_fg_walk: &mut aterm_render::CharFgWalk<'_>,
    c: usize,
    cell: &aterm_core::terminal::RenderCell,
) -> u32 {
    match ink_walk.at(c as u16) {
        Some(ink) => aterm_render::rgb_to_u32(ink),
        None => char_fg_walk
            .at(c as u16)
            .unwrap_or_else(|| aterm_render::rgb_to_u32(cell.fg)),
    }
}

#[inline]
const fn rgb4([r, g, b]: [u8; 3]) -> [u8; 4] {
    [r, g, b, 255]
}

#[inline]
const fn rgb4_u32(c: u32) -> [u8; 4] {
    rgb4([(c >> 16) as u8, (c >> 8) as u8, c as u8])
}

#[inline]
const fn glow8(q: &aterm_render::GlowQuad) -> [u8; 8] {
    [
        (q.color >> 16) as u8,
        (q.color >> 8) as u8,
        q.color as u8,
        q.alpha,
        (q.color2 >> 16) as u8,
        (q.color2 >> 8) as u8,
        q.color2 as u8,
        q.alpha2,
    ]
}

#[inline]
const fn with_over_cap(mut inst: RainGlowInstance, color: u32) -> RainGlowInstance {
    inst.color[3] = (color >> 24) as u8;
    inst
}

fn glyph_windows(
    cpu: &mut Renderer,
    input: &RenderInput,
    scope: &RepaintScope,
    grid_top: usize,
    ch: usize,
    rows: usize,
    out: &mut Vec<Option<(i32, i32)>>,
) {
    const OPEN: (i32, i32) = (i32::MIN, i32::MAX);
    out.clear();
    let RepaintScope::Dirty(dirty) = scope else {
        out.resize(rows, Some(OPEN));
        return;
    };
    let top = |r: usize| i32::try_from(grid_top + r * ch).unwrap_or(i32::MAX);
    let dirty_at = |r: usize| dirty.get(r).copied().unwrap_or(false);
    out.extend((0..rows).map(|r| {
        let above = r > 0 && dirty_at(r - 1);
        if dirty_at(r) {
            Some(if r == 0 || above {
                OPEN
            } else {
                (top(r), i32::MAX)
            })
        } else if above && cpu.row_may_ink_above(input, r) {
            Some((top(r - 1), top(r)))
        } else {
            None
        }
    }));
}

#[inline]
fn clip_rows(scale: aterm_render::Scale, (y0, y1): (i32, i32)) -> aterm_render::Scale {
    aterm_render::Scale {
        clip_y0: scale.clip_y0.max(y0),
        clip_y1: scale.clip_y1.min(y1),
        ..scale
    }
}

type FrameLoadScissor = (crate::device_layer::FrameLoad, Option<(u32, u32, u32, u32)>);

#[allow(
    clippy::too_many_arguments,
    reason = "the frame-geometry tuple of one seam"
)]
fn frame_load_and_scissor(
    scope: &RepaintScope,
    recreate: bool,
    dirty_band: Option<(usize, usize)>,
    grid_top: usize,
    ch: usize,
    rows: usize,
    (w, h): (u32, u32),
    clear: crate::device_layer::ClearColor4,
) -> FrameLoadScissor {
    match &scope {
        RepaintScope::Dirty(_) if !recreate => match dirty_band {
            Some((f, last)) => {
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
                    crate::device_layer::FrameLoad::Load,
                    Some((0u32, y0, w, y1.saturating_sub(y0))),
                )
            }
            None => (crate::device_layer::FrameLoad::Load, Some((0, 0, 0, 0))),
        },
        _ => {
            debug_assert!(
                matches!(scope, RepaintScope::Full),
                "scissored Load requires the prior frame resident (no recreate)"
            );
            (crate::device_layer::FrameLoad::Clear(clear), None)
        }
    }
}

#[inline]
fn theme_color_alpha(c: u32, a: f64) -> crate::device_layer::ClearColor4 {
    fn s2l(b: u32) -> f64 {
        let c = b as f64 / 255.0;
        if c <= 0.040_45 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }
    crate::device_layer::ClearColor4 {
        r: s2l((c >> 16) & 0xff),
        g: s2l((c >> 8) & 0xff),
        b: s2l(c & 0xff),
        a,
    }
}

/// One present differential's result.
#[cfg(all(target_os = "macos", test, wgpu_arm))]
pub(crate) type PresentDifferential = (Vec<u8>, Vec<u8>, (u32, u32), [bool; 3]);

/// The Metal present replay's yield.
#[cfg(all(target_os = "macos", test, wgpu_arm))]
pub(crate) type PresentReplay = (Vec<u8>, [bool; 3]);

#[cfg(all(target_os = "macos", test))]
impl GpuRenderer {
    /// The streams the most recent frame's plan drew, so differential fixtures can prove they
    /// drive the rows they claim.
    #[cfg(wgpu_arm)]
    pub(crate) fn last_frame_plan_streams_for_test(&self) -> Vec<(StreamId, DrawPipe)> {
        self.frame_plan
            .iter()
            .map(|item| (item.stream, item.pipe))
            .collect()
    }

    /// The recorded pass-0 load op, so a fixture can prove it exercised the scissored `Load` path.
    #[cfg(wgpu_arm)]
    pub(crate) fn last_frame_load_for_test(&self) -> crate::device_layer::FrameLoad {
        self.last_frame_load
    }

    /// The recorded per-pass scissor — `None` == a full-frame encode.
    #[cfg(wgpu_arm)]
    pub(crate) fn last_frame_scissor_for_test(&self) -> Option<(u32, u32, u32, u32)> {
        self.last_frame_scissor
    }

    /// Full-frame differential.
    #[cfg(wgpu_arm)]
    pub(crate) fn frame_differential_for_test(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
    ) -> Result<Option<(Frame, Frame)>, String> {
        let expected = self.try_render_input(win, input, None)?;
        let (w, h) = (expected.width as u32, expected.height as u32);
        Ok(self
            .metal_replay_recorded_plan_for_test(win, input, (w, h), None)?
            .map(|actual| (expected, actual)))
    }

    /// The Metal half of the differential.
    #[cfg(wgpu_arm)]
    pub(crate) fn metal_replay_recorded_plan_for_test(
        &mut self,
        win: &WindowGpu,
        input: &RenderInput,
        (w, h): (u32, u32),
        seed: Option<&Frame>,
    ) -> Result<Option<Frame>, String> {
        use crate::device_layer::{
            DeviceHandle, FrameCopyTexture, FrameEncoder, LayerTexture, SubmittedFrame, TexUsage,
            TexelFormat, metal_try_read_back,
        };
        use crate::metal::ffi::{Device as MtlDevice, Obj, PixelFormat, SamplerDesc};
        use crate::metal::loss::{CbOutcome, LossLatch};
        use crate::metal::resources::{MetalResourceDevice, SealedTexture, SharedBuffer};
        use std::sync::Arc;

        assert!(
            !self.enable_bloom && !self.enable_shimmer,
            "the W4 differential replays the ENCODE plan; bloom/shimmer are \
             present-time layers (W5) — call set_bloom(false)/set_shimmer(false)"
        );
        let _pool = crate::metal::ffi::AutoreleasePool::new();
        let Some(mdev) = MtlDevice::preferred() else {
            return Ok(None);
        };

        let plan = self.frame_plan.clone();
        let load0 = self.last_frame_load;
        let scissor = self.last_frame_scissor;
        assert!(
            matches!(load0, crate::device_layer::FrameLoad::Clear(_)) || seed.is_some(),
            "a recorded LOAD plan reads the prior frame's texels — the caller \
             must seed the Metal offscreen with the pre-encode offscreen bytes"
        );
        let mut apron_win = WindowGpu::default();
        let apron_plan = self.apron_strip_plan(&mut apron_win, input, h as usize);

        let latch = Arc::new(LossLatch::new());
        let session = crate::metal::encoder::EncodeSession::new(&mdev, Arc::clone(&latch))?;
        let mint = MetalResourceDevice::new(&mdev, Arc::clone(&latch));
        let handle = DeviceHandle::Metal(&mint);

        let off_tex = handle.create_texture_2d(
            "w4 differential offscreen",
            TexelFormat::Rgba8Unorm,
            w,
            h,
            TexUsage::OFFSCREEN,
            Some(TexelFormat::Rgba8UnormSrgb),
        );
        if let Some(seed) = seed {
            assert_eq!(
                (seed.width as u32, seed.height as u32),
                (w, h),
                "the seed frame must be the pre-encode offscreen at these dims"
            );
            let mut bytes = Vec::with_capacity(seed.pixels.len() * 4);
            for p in &seed.pixels {
                bytes.extend_from_slice(&[
                    (p >> 16) as u8,
                    (p >> 8) as u8,
                    *p as u8,
                    255 - (p >> 24) as u8,
                ]);
            }
            handle.upload_texture_full(&off_tex, &bytes, w * 4, w, h);
        }
        let LayerTexture::Metal(offscreen) = off_tex else {
            unreachable!("the Metal handle mints Metal textures")
        };
        let offscreen_srgb = offscreen
            .alias_view(PixelFormat::Rgba8UnormSrgb)
            .ok_or_else(|| "the offscreen's sRGB alias view failed to vend".to_owned())?;

        let lib = crate::metal::pipelines::compile_library(&mdev, ShaderLibrary::Cell)?;
        let mut psos: [Option<Obj>; pipeline_table::PIPELINE_COUNT] = std::array::from_fn(|_| None);
        let mut uniform_vertex_slot: Option<u32> = None;
        let mut uniform_fragment_slot: Option<u32> = None;
        for item in &plan {
            assert!(
                !item.fetch,
                "the differential replays a wgpu plan, which keeps the split"
            );
            let row = item.pipe.row();
            let spec = row.spec();
            assert!(
                matches!(spec.library, ShaderLibrary::Cell),
                "every frame-plan row lives in cell.metal; {row:?} says {:?}",
                spec.library
            );
            let vu = spec
                .binds
                .vertex_uniform
                .unwrap_or_else(|| panic!("frame row {row:?} declares no vertex uniform"));
            match uniform_vertex_slot {
                None => uniform_vertex_slot = Some(vu),
                Some(prev) => assert_eq!(
                    prev, vu,
                    "one shared uniform bind per pass requires one slot; {row:?} disagrees"
                ),
            }
            if let Some(&fb) = spec.binds.fragment_buffers.first() {
                assert_eq!(
                    spec.binds.fragment_buffers.len(),
                    1,
                    "frame rows re-bind at most the one shared uniform block"
                );
                match uniform_fragment_slot {
                    None => uniform_fragment_slot = Some(fb),
                    Some(prev) => assert_eq!(prev, fb, "{row:?} disagrees on the fragment slot"),
                }
            }
            if psos[row as usize].is_none() {
                psos[row as usize] = Some(crate::metal::pipelines::build(
                    &mdev,
                    &lib,
                    spec,
                    PixelFormat::Bgra8Unorm,
                )?);
            }
        }

        let (uw, uh, tb) = self
            .uniform_written
            .expect("encode_frame wrote the shared uniforms");
        assert_eq!(
            (uw, uh),
            (w, h),
            "the differential replays the frame that was just encoded"
        );
        let ubuf = mint.buffer(std::mem::size_of::<Uniforms>())?;
        // SAFETY: `Uniforms` is `Pod` into an exactly-sized fresh shared buffer with no GPU work in
        // flight.
        unsafe {
            crate::metal::ffi::buffer_write(
                &ubuf,
                aterm_bits::bytes_of(&Uniforms {
                    screen: [w as f32, h as f32],
                    text_blend: tb as f32,
                    _pad: 0.0,
                }),
            );
        }

        let mut streams: [Option<SharedBuffer>; STREAM_COUNT] = std::array::from_fn(|_| None);
        for item in &plan {
            let idx = item.stream as usize;
            if streams[idx].is_some() {
                continue;
            }
            let (bytes, _) = self.inst.stream_bytes(item.stream);
            assert!(!bytes.is_empty(), "a planned stream has bytes");
            let buf = mint.buffer(bytes.len())?;
            // SAFETY: fresh exactly-sized shared buffer; no GPU work in flight.
            unsafe { crate::metal::ffi::buffer_write(&buf, bytes) };
            streams[idx] = Some(SharedBuffer::new(buf));
        }

        let mut atlases: [Option<(SealedTexture, Obj)>; DRAW_ATLAS_COUNT] =
            std::array::from_fn(|_| None);
        for item in &plan {
            let Some(a) = item.atlas else { continue };
            if atlases[a as usize].is_some() {
                continue;
            }
            let (format, tw, th, occupied): (TexelFormat, u32, u32, &[u8]) = match a {
                DrawAtlas::Fx => continue,
                DrawAtlas::Mono => {
                    let res = self.mono_res.as_ref().expect("encode set mono_res");
                    (
                        TexelFormat::R8Unorm,
                        res.atlas.width,
                        res.tex_h,
                        &res.atlas.data[..(res.atlas.width * res.atlas.height) as usize],
                    )
                }
                DrawAtlas::Color => {
                    let res = self.color_res.as_ref().expect("encode set color_res");
                    (
                        TexelFormat::Rgba8Unorm,
                        res.atlas.width,
                        res.tex_h,
                        &res.atlas.data[..(res.atlas.width * res.atlas.height * 4) as usize],
                    )
                }
                DrawAtlas::Wallpaper => {
                    let src = win
                        .wallpaper_tex
                        .as_ref()
                        .unwrap_or_else(|| panic!("plan samples {a:?} but its SpriteTex is absent"))
                        .src
                        .as_ref();
                    (
                        TexelFormat::Rgba8Unorm,
                        src.width,
                        src.height,
                        &src.rgba[..(src.width * src.height * 4) as usize],
                    )
                }
                DrawAtlas::Deco => {
                    let d = self
                        .deco_atlas
                        .as_ref()
                        .expect("plan samples the deco atlas");
                    (
                        TexelFormat::R8Unorm,
                        d.atlas_w as u32,
                        d.ch as u32,
                        &d.data[..],
                    )
                }
                DrawAtlas::Image => {
                    return Err(
                        "W4 differential fixture reached the image plane (W5+); keep \
                         fixtures inside the sweep"
                            .to_string(),
                    );
                }
            };
            let bpp = format.bytes_per_texel();
            let mut data = vec![0u8; (tw * th * bpp) as usize];
            data[..occupied.len()].copy_from_slice(occupied);
            let tex = handle.create_texture_2d(
                "w4 differential atlas",
                format,
                tw,
                th,
                TexUsage::UPLOADED_SAMPLED,
                None,
            );
            handle.upload_texture_full(&tex, &data, tw * bpp, tw, th);
            let LayerTexture::Metal(tex) = tex else {
                unreachable!("the Metal handle mints Metal textures")
            };
            let sampler = mint.sampler(SamplerDesc::NEAREST_CLAMP)?;
            atlases[a as usize] = Some((tex, sampler));
        }

        let fx = if plan.iter().any(|i| i.atlas == Some(DrawAtlas::Fx)) {
            let mut texes = [None, None];
            for kind in [fx_gpu::FxAtlasKind::R8, fx_gpu::FxAtlasKind::Rgba] {
                fx_gpu::sync_fx_tex(
                    handle,
                    &mut texes[kind as usize],
                    self.fx.atlas.side(kind),
                    |_| true,
                    || {},
                )?;
            }
            let palette_bytes: &[u8] = aterm_bits::cast_slice(&self.fx.palette);
            let palette = mint.buffer(palette_bytes.len().max(16))?;
            // SAFETY: fresh shared buffer of at least the palette's length with no GPU work in
            // flight.
            unsafe { crate::metal::ffi::buffer_write(&palette, palette_bytes) };
            let [r8, rgba] =
                texes.map(|t| t.expect("sync_fx_tex mints a texture").tex.into_metal());
            Some(MetalFxBind { r8, rgba, palette })
        } else {
            None
        };

        let rig = MetalFrameRig {
            offscreen,
            offscreen_srgb,
            psos,
            fetch_psos: std::array::from_fn(|_| None),
            uniform: ubuf,
            uniform_vertex_slot: uniform_vertex_slot.unwrap_or(0),
            uniform_fragment_slot,
            streams,
            atlases,
            fx,
        };

        let mut enc = FrameEncoder::metal(&session)?;
        let res = FrameRes::Metal(&rig);
        let passes = run_frame_plan(&mut enc, &res, &plan, load0, scissor)?;
        assert_eq!(
            passes as u32, self.last_frame_passes,
            "same plan + same pure coalescer must yield the same pass count"
        );
        match enc.submit() {
            SubmittedFrame::Metal(submitted) => {
                let outcome = submitted.wait_outcome();
                if outcome != CbOutcome::Completed {
                    return Err(format!("the Metal frame replay failed: {outcome:?}"));
                }
            }
            #[cfg(wgpu_arm)]
            SubmittedFrame::Wgpu => unreachable!("a metal encoder commits a metal frame"),
        }

        if input.scroll_frac_px != 0 {
            let (y0, y1) = aterm_render::scroll_translate::grid_band_px(
                self.cpu.grid_top(),
                self.cell_size().1,
                input.grid_top_row,
                input.grid_bot_row,
                h as usize,
            );
            if let Some(shift) = band_shift_ops(y0, y1, i64::from(input.scroll_frac_px)) {
                let scratch = handle.create_texture_2d(
                    "aterm-gpu m1b scroll shift scratch",
                    TexelFormat::Rgba8Unorm,
                    w,
                    h,
                    TexUsage {
                        sampled: false,
                        render: false,
                        copy_src: true,
                        copy_dst: true,
                    },
                    None,
                );
                let mut enc = FrameEncoder::metal(&session)?;
                enc.copy_texture_rect(
                    FrameCopyTexture::Metal(&rig.offscreen),
                    (0, shift.src_y),
                    FrameCopyTexture::Metal(scratch.metal()),
                    (0, 0),
                    w,
                    shift.moved,
                )?;
                enc.copy_texture_rect(
                    FrameCopyTexture::Metal(scratch.metal()),
                    (0, 0),
                    FrameCopyTexture::Metal(&rig.offscreen),
                    (0, shift.dst_y),
                    w,
                    shift.moved,
                )?;
                match enc.submit() {
                    SubmittedFrame::Metal(submitted) => {
                        let outcome = submitted.wait_outcome();
                        if outcome != CbOutcome::Completed {
                            return Err(format!("the Metal scroll twin failed: {outcome:?}"));
                        }
                    }
                    #[cfg(wgpu_arm)]
                    SubmittedFrame::Wgpu => unreachable!("a metal encoder commits a metal frame"),
                }
            }
        }
        {
            if let Some(strip) = apron_plan {
                let tex = handle.create_texture_2d(
                    "aterm-gpu m1b apron row",
                    TexelFormat::Rgba8Unorm,
                    strip.w,
                    strip.cell_h,
                    TexUsage {
                        sampled: false,
                        render: false,
                        copy_src: true,
                        copy_dst: true,
                    },
                    None,
                );
                handle.upload_texture_full(
                    &tex,
                    &apron_win.apron_rgba,
                    strip.w * 4,
                    strip.w,
                    strip.cell_h,
                );
                let mut enc = FrameEncoder::metal(&session)?;
                enc.copy_texture_rect(
                    FrameCopyTexture::Metal(tex.metal()),
                    (0, 0),
                    FrameCopyTexture::Metal(&rig.offscreen),
                    (0, strip.dst_y),
                    strip.w.min(w),
                    strip.rows,
                )?;
                match enc.submit() {
                    SubmittedFrame::Metal(submitted) => {
                        let outcome = submitted.wait_outcome();
                        if outcome != CbOutcome::Completed {
                            return Err(format!("the Metal apron twin failed: {outcome:?}"));
                        }
                    }
                    #[cfg(wgpu_arm)]
                    SubmittedFrame::Wgpu => unreachable!("a metal encoder commits a metal frame"),
                }
            }
        }

        let actual = metal_try_read_back(&session, &mint, &rig.offscreen, w, h)?;
        assert!(
            !latch.is_lost(),
            "the differential's loss domain stayed healthy"
        );
        Ok(Some(actual))
    }

    /// Present-path differential.
    #[cfg(wgpu_arm)]
    pub(crate) fn present_differential_for_test(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
        tray: Option<TrayQuad<'_>>,
        crop: PresentCrop,
        destination: (u32, u32),
    ) -> Result<Option<PresentDifferential>, String> {
        if crate::metal::ffi::Device::preferred().is_none() {
            return Ok(None);
        }
        assert!(
            self.sdr_glow_boost == 0.0,
            "the W5 present differential runs crown-free — set_sdr_glow_boost(0.0); \
             rows 14/15 are armed by their own byte differentials"
        );
        assert!(
            !self.enable_shimmer || self.shimmer_phase_pin.is_some(),
            "pin the shimmer phase (set_shimmer_phase_for_test) — the pass's one \
             wall-clock input must be identical on both arms"
        );
        self.virtual_begin(
            win,
            destination.0,
            destination.1,
            crate::video_tap::CaptureOpts {
                half_res: false,
                budget_bytes: 64 << 20,
                fps_cap: None,
                requested_ms: 0,
            },
        )?;
        let presented = self.present_virtual_with_crop(
            win,
            input,
            false,
            None,
            tray,
            Some(crop),
            0,
            destination,
        );
        assert!(presented, "the virtual present cannot fail to acquire");
        let expected = self
            .virtual_presented_snapshot_current(win, win.virtual_presented_input_epoch, 1)?
            .convert()?;
        assert_eq!(
            (expected.w, expected.h),
            destination,
            "the tap captured the whole virtual destination"
        );
        let (fw, fh) = {
            let off = win.offscreen.as_ref().expect("the present set it");
            (off.w, off.h)
        };
        let clean = {
            let off = win.offscreen.as_ref().expect("resident");
            self.texture_bytes_tight_for_test(&off.tex, fw, fh, 4)
        };
        let (actual, armed) = self
            .metal_present_replay_for_test(input, tray, Some(crop), destination, &clean, (fw, fh))?
            .expect("a Metal device existed above");
        let _ = self.video_finish(win);
        Ok(Some((expected.rgba, actual, destination, armed)))
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the whole present replay reads as one §2 Submit B transcript; \
                  splitting it would scatter the pass order the map pins"
    )]
    #[cfg(wgpu_arm)]
    pub(crate) fn metal_present_replay_for_test(
        &mut self,
        input: &RenderInput,
        tray: Option<TrayQuad<'_>>,
        source_crop: Option<PresentCrop>,
        (dw, dh): (u32, u32),
        clean_offscreen: &[u8],
        (fw, fh): (u32, u32),
    ) -> Result<Option<PresentReplay>, String> {
        use crate::metal::encoder::EncodeSession;
        use crate::metal::ffi::{
            self as mtl, Device as MtlDevice, MtlRegion, PixelFormat, SamplerDesc,
            TEXTURE_USAGE_RENDER_TARGET, TEXTURE_USAGE_SHADER_READ,
        };
        use crate::metal::loss::{CbOutcome, LossLatch};
        use crate::metal::present::{
            BloomPasses, MetalWindowSurface, PostPass, PresentSequence, ShimmerPass, TrayPass,
            encode_present_sequence,
        };
        use crate::metal::resources::MetalResourceDevice;
        use crate::metal::swapchain::SwapchainConfig;
        use std::sync::Arc;

        let _pool = crate::metal::ffi::AutoreleasePool::new();
        let Some(mdev) = MtlDevice::preferred() else {
            return Ok(None);
        };
        assert_eq!(
            clean_offscreen.len(),
            (fw * fh * 4) as usize,
            "the seed is the tight clean offscreen"
        );

        let latch = Arc::new(LossLatch::new());
        let session = EncodeSession::new(&mdev, Arc::clone(&latch))?;
        let mint = MetalResourceDevice::new(&mdev, Arc::clone(&latch));
        let usage = TEXTURE_USAGE_RENDER_TARGET | TEXTURE_USAGE_SHADER_READ;

        let config = SwapchainConfig {
            format: PixelFormat::Bgra8Unorm,
            width: dw as usize,
            height: dh as usize,
            framebuffer_only: false,
            display_sync: false,
            maximum_drawables: 3,
            opaque: true,
        };
        let mut surface = MetalWindowSurface::standalone(&mdev, config, Arc::clone(&latch))?;
        let reconfigured = surface.reconcile(&mdev, &config)?;
        assert!(!reconfigured, "a steady present reconciles to no drift");

        let offscreen =
            mint.texture_2d(PixelFormat::Rgba8Unorm, fw as usize, fh as usize, usage)?;
        // SAFETY: fresh 2D managed texture of `fw x fh` seeded with `fw * fh * 4` tightly packed
        // bytes.
        unsafe {
            mtl::texture_upload(
                offscreen.obj(),
                MtlRegion::full_2d(fw as usize, fh as usize),
                clean_offscreen,
                (fw * 4) as usize,
            );
        }
        let present_off =
            mint.texture_2d(PixelFormat::Rgba8Unorm, fw as usize, fh as usize, usage)?;
        let linear = mint.sampler(SamplerDesc::LINEAR_CLAMP)?;
        let nearest = mint.sampler(SamplerDesc::NEAREST_CLAMP)?;

        let bloom_state = if self.enable_bloom && fx_gpu::bloom_source_live(input) {
            let (count, bbox, extract_first) = self.build_bloom_glow(input);
            (count > 0).then_some((count, bbox, extract_first))
        } else {
            None
        };
        let (bw, bh) = (
            (fw / BLOOM_DOWNSCALE).max(1) as usize,
            (fh / BLOOM_DOWNSCALE).max(1) as usize,
        );
        let bloom_parts = if let Some((count, bbox, extract_first)) = bloom_state {
            let half = mint.texture_2d(PixelFormat::Rgba8Unorm, bw, bh, usage)?;
            let insts: &[GlowInstance] = &self.bloom_glow_scratch;
            let bytes: &[u8] = aterm_bits::cast_slice(insts);
            let stream = mint.buffer(bytes.len())?;
            // SAFETY: fresh exactly-sized shared buffer; no GPU work on it.
            unsafe { mtl::buffer_write(&stream, bytes) };
            let extract_offset = extract_first as usize * std::mem::size_of::<GlowInstance>();
            let (uw, uh, tb) = self.uniform_written.unwrap_or((fw, fh, 0));
            assert_eq!((uw, uh), (fw, fh), "the encode wrote this frame's uniforms");
            let cell = mint.buffer(std::mem::size_of::<Uniforms>())?;
            // SAFETY: Pod into an exactly-sized fresh shared buffer.
            unsafe {
                mtl::buffer_write(
                    &cell,
                    aterm_bits::bytes_of(&Uniforms {
                        screen: [fw as f32, fh as f32],
                        text_blend: tb as f32,
                        _pad: 0.0,
                    }),
                );
            }
            let bu = mint.buffer(std::mem::size_of::<BloomUniform>())?;
            // SAFETY: as above.
            unsafe {
                mtl::buffer_write(
                    &bu,
                    aterm_bits::bytes_of(&BloomUniform {
                        texel: [1.0 / bw as f32, 1.0 / bh as f32],
                        strength: page_bloom_strength(self.bloom_strength, input, self.theme.bg),
                        radius: self.bloom_radius,
                        chroma_radius: bloom_chroma_radius(self.bloom_radius),
                        extent: [1.0; 2],
                        _pad: 0.0,
                    }),
                );
            }
            let clip = input.fx_clip.map(|(cx0, cy0, cx1, cy1)| {
                [
                    u32::from(cx0),
                    u32::from(cy0),
                    u32::from(cx1),
                    u32::from(cy1),
                ]
            });
            let scissor = if let Some([x0, y0, x1, y1]) = bbox {
                let d = aterm_render::bloom::reach_px(self.bloom_radius);
                Some([
                    x0.saturating_sub(d),
                    y0.saturating_sub(d),
                    x1.saturating_add(d).min(fw),
                    y1.saturating_add(d).min(fh),
                ])
            } else {
                clip.map(|_| [0, 0, fw, fh])
            };
            let scissor = match (scissor, clip) {
                (Some([sx0, sy0, sx1, sy1]), Some([cx0, cy0, cx1, cy1])) => Some([
                    sx0.max(cx0),
                    sy0.max(cy0),
                    sx1.min(cx1).min(fw),
                    sy1.min(cy1).min(fh),
                ]),
                (s, None) => s,
                (None, Some(_)) => unreachable!("clip-only case handled above"),
            }
            .filter(|[sx0, sy0, sx1, sy1]| sx1 > sx0 && sy1 > sy0);
            let extract_spec = Pipeline::GlowAdd.spec();
            let extract_lib = crate::metal::pipelines::compile(&mdev, extract_spec)?;
            let extract_pso = crate::metal::pipelines::build(
                &mdev,
                &extract_lib,
                extract_spec,
                PixelFormat::Bgra8Unorm,
            )?;
            let bloom_spec = Pipeline::Bloom.spec();
            let bloom_lib = crate::metal::pipelines::compile(&mdev, bloom_spec)?;
            let bloom_pso = crate::metal::pipelines::build(
                &mdev,
                &bloom_lib,
                bloom_spec,
                PixelFormat::Bgra8Unorm,
            )?;
            Some((
                half,
                extract_pso,
                cell,
                extract_spec.binds,
                (stream, extract_offset),
                count as usize,
                bloom_pso,
                bu,
                bloom_spec.binds,
                scissor,
            ))
        } else {
            None
        };

        let shimmer_parts = if self.shimmer_live(input) {
            self.shimmer_region(input, fw, fh)
        } else {
            None
        }
        .map(|region| -> Result<_, String> {
            let scratch =
                mint.texture_2d(PixelFormat::Rgba8Unorm, fw as usize, fh as usize, usage)?;
            let (cw, ch) = self.cpu.cell_size();
            let mut heat = [[0f32; 4]; SHIMMER_BANDS / 4];
            for (i, v) in region.heat.iter().enumerate() {
                heat[i / 4][i % 4] = *v;
            }
            let su = ShimmerUniform {
                frame: [fw as f32, fh as f32],
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
            let ub = mint.buffer(std::mem::size_of::<ShimmerUniform>())?;
            // SAFETY: Pod into an exactly-sized fresh shared buffer.
            unsafe { mtl::buffer_write(&ub, aterm_bits::bytes_of(&su)) };
            let m = SHIMMER_COPY_MARGIN;
            let stage = [
                region.x0.saturating_sub(m),
                region.y0.saturating_sub(m),
                region.x1.saturating_add(m).min(fw),
                region.y1.saturating_add(m).min(fh),
            ];
            let spec = Pipeline::Shimmer.spec();
            let lib = crate::metal::pipelines::compile(&mdev, spec)?;
            let pso = crate::metal::pipelines::build(&mdev, &lib, spec, PixelFormat::Bgra8Unorm)?;
            Ok((
                scratch,
                stage,
                pso,
                ub,
                spec.binds,
                [region.x0, region.y0, region.x1, region.y1],
            ))
        })
        .transpose()?;

        let tray_parts = tray
            .map(|t| -> Result<_, String> {
                let card = mint.texture_2d(
                    PixelFormat::Rgba8Unorm,
                    t.pw as usize,
                    t.ph as usize,
                    TEXTURE_USAGE_SHADER_READ,
                )?;
                // SAFETY: fresh managed texture of `pw x ph` seeded with `pw * ph * 4` tightly
                // packed RGBA bytes.
                unsafe {
                    mtl::texture_upload(
                        card.obj(),
                        MtlRegion::full_2d(t.pw as usize, t.ph as usize),
                        t.rgba,
                        (t.pw * 4) as usize,
                    );
                }
                let tu = mint.buffer(std::mem::size_of::<TrayUniform>())?;
                // SAFETY: Pod into an exactly-sized fresh shared buffer.
                unsafe {
                    mtl::buffer_write(
                        &tu,
                        aterm_bits::bytes_of(&TrayUniform {
                            rect: [t.dx as f32, t.dy as f32, t.pw as f32, t.ph as f32],
                            fb: [fw as f32, fh as f32],
                            _pad: [0.0, 0.0],
                        }),
                    );
                }
                let spec = Pipeline::Tray.spec();
                let lib = crate::metal::pipelines::compile(&mdev, spec)?;
                let pso =
                    crate::metal::pipelines::build(&mdev, &lib, spec, PixelFormat::Rgba8Unorm)?;
                Ok((card, tu, pso, spec.binds))
            })
            .transpose()?;

        let live_bg = frame_live_bg(input, self.theme.bg);
        let mut want = present_blit_uniform(false, None, source_crop, fw, fh, dw, dh, live_bg);
        let chrome = self.cpu.chrome_extent_px(fh as usize);
        want.chrome_y0 = chrome.start as f32;
        want.chrome_y1 = chrome.end as f32;
        want.encode_srgb = if self.ctx.srgb_offscreen { 0.0 } else { 1.0 };
        let bu = mint.buffer(std::mem::size_of::<BlitUniform>())?;
        // SAFETY: Pod into an exactly-sized fresh shared buffer.
        unsafe { mtl::buffer_write(&bu, aterm_bits::bytes_of(&want)) };
        let blit_spec = Pipeline::Blit.spec();
        let blit_lib = crate::metal::pipelines::compile(&mdev, blit_spec)?;
        let blit_pso =
            crate::metal::pipelines::build(&mdev, &blit_lib, blit_spec, PixelFormat::Bgra8Unorm)?;

        let use_present_off =
            bloom_parts.is_some() || shimmer_parts.is_some() || tray_parts.is_some();
        let seq = PresentSequence {
            offscreen: &offscreen,
            present_off: use_present_off.then_some(&present_off),
            copy_rect: [0, 0, fw, fh],
            band_shift: None,
            apron: None,
            bloom: bloom_parts.as_ref().map(
                |(half, extract_pso, cell, extract_binds, stream, count, pso, ubuf, binds, sc)| {
                    BloomPasses {
                        half_target: half,
                        extract_pso,
                        cell_uniform: cell,
                        cell_uniform_slot: extract_binds
                            .vertex_uniform
                            .expect("row 4 has a vertex uniform")
                            as usize,
                        extract_stream: &stream.0,
                        extract_offset: stream.1,
                        extract_count: *count,
                        composite: PostPass {
                            pso,
                            tex: half.obj(),
                            tex_slot: binds.fragment_textures[0] as usize,
                            sampler: &linear,
                            sampler_slot: binds.fragment_samplers[0] as usize,
                            uniform: ubuf,
                            uniform_slot: binds.fragment_buffers[0] as usize,
                        },
                        composite_scissor: *sc,
                    }
                },
            ),
            shimmer: shimmer_parts
                .as_ref()
                .map(|(scratch, stage, pso, ubuf, binds, region)| ShimmerPass {
                    scratch,
                    stage_rect: *stage,
                    refract: PostPass {
                        pso,
                        tex: scratch.obj(),
                        tex_slot: binds.fragment_textures[0] as usize,
                        sampler: &linear,
                        sampler_slot: binds.fragment_samplers[0] as usize,
                        uniform: ubuf,
                        uniform_slot: binds.fragment_buffers[0] as usize,
                    },
                    region: *region,
                }),
            tray: tray_parts.as_ref().map(|(card, tu, pso, binds)| TrayPass {
                pso,
                card_tex: card.obj(),
                tex_slot: binds.fragment_textures[0] as usize,
                sampler: &linear,
                sampler_slot: binds.fragment_samplers[0] as usize,
                uniform: tu,
                uniform_slot: binds.vertex_uniform.expect("row 17 binds at 2") as usize,
            }),
            blit: PostPass {
                pso: &blit_pso,
                tex: if use_present_off {
                    present_off.obj()
                } else {
                    offscreen.obj()
                },
                tex_slot: blit_spec.binds.fragment_textures[0] as usize,
                sampler: &nearest,
                sampler_slot: blit_spec.binds.fragment_samplers[0] as usize,
                uniform: &bu,
                uniform_slot: blit_spec.binds.fragment_buffers[0] as usize,
            },
            clear: mtl::ClearColor {
                r: f64::from((live_bg >> 16) & 0xff) / 255.0,
                g: f64::from((live_bg >> 8) & 0xff) / 255.0,
                b: f64::from(live_bg & 0xff) / 255.0,
                a: 1.0,
            },
            crown: None,
        };

        let frame = surface
            .acquire()
            .map_err(|refusal| format!("replay acquire refused: {refusal:?}"))?;
        let target = frame.render_target();
        let row = (dw * 4) as usize;
        let readback = mdev
            .new_buffer(row * dh as usize)
            .ok_or_else(|| "the replay's tap readback buffer failed to allocate".to_owned())?;
        let mut cb = session.begin()?;
        encode_present_sequence(&mut cb, &target, (dw, dh), &seq)?;
        cb.copy_texture_to_buffer(&target, dw as usize, dh as usize, &readback, row)?;
        let submitted = cb.commit();
        let ticket = frame.present(&session)?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            match (submitted.try_outcome(), ticket.try_outcome()) {
                (Some(s), Some(t)) => {
                    if s != CbOutcome::Completed || t != CbOutcome::Completed {
                        return Err(format!("replay frame did not complete: {s:?} / {t:?}"));
                    }
                    break;
                }
                _ if std::time::Instant::now() > deadline => {
                    return Err("replay frame never reached a terminal status".into());
                }
                _ => std::thread::yield_now(),
            }
        }
        assert!(!latch.is_lost(), "the replay's loss domain stayed healthy");

        // SAFETY: shared storage sized `row * dh` after both command buffers have completed.
        let bgra = unsafe { mtl::buffer_bytes(&readback, row * dh as usize) };
        let mut rgba = vec![0u8; bgra.len()];
        for (dst, src) in rgba
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(bgra.as_chunks::<4>().0)
        {
            *dst = [src[2], src[1], src[0], src[3]];
        }
        Ok(Some((
            rgba,
            [
                bloom_parts.is_some(),
                shimmer_parts.is_some(),
                tray_parts.is_some(),
            ],
        )))
    }

    #[cfg(wgpu_arm)]
    fn seeded_target_for_test(
        &self,
        format: wgpu::TextureFormat,
        bytes: &[u8],
        w: u32,
        h: u32,
        texel: u32,
    ) -> (wgpu::Texture, wgpu::TextureView) {
        assert_eq!(bytes.len(), (w * h * texel) as usize, "tight seed");
        let tex = self.ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aterm-gpu test seeded target"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.ctx.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w * texel),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        (tex, view)
    }

    #[cfg(wgpu_arm)]
    fn stream_buffer_for_test(&self, bytes: &[u8]) -> wgpu::Buffer {
        let buf = self.ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aterm-gpu test stream"),
            size: bytes.len() as u64,
            usage: wgpu::BufferUsages::VERTEX,
            mapped_at_creation: true,
        });
        buf.slice(..).get_mapped_range_mut().copy_from_slice(bytes);
        buf.unmap();
        buf
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the crown fixture states screen geometry, boost and headroom explicitly"
    )]
    #[cfg(wgpu_arm)]
    pub(crate) fn crown_row_bytes_for_test(
        &mut self,
        hdr: bool,
        instances: &[([u16; 4], [u8; 4])],
        content_off: [f32; 2],
        boost: f32,
        headroom: f32,
        seed: &[u8],
        scissor: [u32; 4],
        w: u32,
        h: u32,
    ) -> Vec<u8> {
        let (format, texel) = if hdr {
            self.ensure_hdr_glow_pipeline();
            (wgpu::TextureFormat::Rgba16Float, 8u32)
        } else {
            self.ensure_sdr_glow_pipeline(wgpu::TextureFormat::Bgra8Unorm);
            (wgpu::TextureFormat::Bgra8Unorm, 4u32)
        };
        let (dst, dst_view) = self.seeded_target_for_test(format, seed, w, h, texel);
        self.ctx.queue.write_buffer(
            self.hdr_glow_uniform_buf
                .as_ref()
                .expect("the ensure above set the shared uniform buf"),
            0,
            aterm_bits::bytes_of(&HdrGlowUniform {
                screen: [w as f32, h as f32],
                content_off,
                boost,
                headroom,
                _pad: [0.0, 0.0],
            }),
        );
        let insts: Vec<GlowInstance> = instances
            .iter()
            .map(|&(rect, color)| GlowInstance {
                rect,
                color,
                color2: color,
            })
            .collect();
        let stream = self.stream_buffer_for_test(aterm_bits::cast_slice(&insts));
        let mut enc = self
            .ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aterm-gpu test crown draw"),
            });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("aterm-gpu test crown pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &dst_view,
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
            let pipeline = if hdr {
                self.hdr_glow_pipeline
                    .as_ref()
                    .expect("ensure_hdr_glow_pipeline set it")
            } else {
                &self
                    .sdr_glow_pipeline
                    .as_ref()
                    .expect("ensure_sdr_glow_pipeline set it")
                    .1
            };
            pass.set_pipeline(pipeline);
            pass.set_bind_group(
                0,
                self.hdr_glow_bg.as_ref().expect("the ensure set it"),
                &[],
            );
            let [x0, y0, x1, y1] = scissor;
            pass.set_scissor_rect(x0, y0, x1 - x0, y1 - y0);
            pass.set_vertex_buffer(0, stream.slice(..));
            pass.draw(0..6, 0..insts.len() as u32);
        }
        self.ctx.queue.submit([enc.finish()]);
        self.texture_bytes_tight_for_test(&dst, w, h, texel as usize)
    }
}

/// A GPU renderer for a test, on `family` (the system default for `None`).
#[cfg(test)]
pub(crate) fn gpu_renderer_for_test(family: Option<&str>, px: f32, theme: Theme) -> GpuRenderer {
    let ctx = GpuContext::new()
        .unwrap_or_else(|e| aterm_spec::host::could_not_run(format!("no GPU: {e}")));
    let cpu = Renderer::from_system_with_family(family, px, theme)
        .unwrap_or_else(|| aterm_spec::host::could_not_run("no system monospace font"));
    GpuRenderer::from_parts(ctx, cpu, family.map(String::from), theme)
        .expect("the GPU renderer assembles on an acquired device")
}

#[cfg(test)]
mod tests {
    use aterm_render::{FaceId, GlowQuad, GlyphClass, StyleBits};

    #[cfg(wgpu_arm)]
    use super::{
        EFFECT_PIPELINE_COUNT, EFFECT_PIPELINE_NAMES, EffectPipeline, Instances,
        frame_effect_pipelines,
    };
    use super::{FRAME_GROUPS, GROUP_SRGB, WindowGpu};

    #[test]
    fn a_minimized_window_parks_all_but_a_captures_present() {
        let mut win = WindowGpu::new();
        assert!(!win.parks_while_minimized(), "a visible window presents");
        win.set_occluded_hint(true);
        assert!(win.parks_while_minimized(), "a minimized one parks");
        win.set_capture_present(true);
        assert!(!win.parks_while_minimized(), "but presents for a capture");
        win.set_capture_present(false);
        assert!(win.parks_while_minimized(), "and parks again after it");
        win.set_occluded_hint(false);
        win.set_capture_present(true);
        assert!(!win.parks_while_minimized());
    }

    #[test]
    fn band_shift_ops_states_the_staged_move_law() {
        use super::{BandShift, band_shift_ops};
        assert_eq!(
            band_shift_ops(10, 30, 3),
            Some(BandShift {
                src_y: 13,
                dst_y: 10,
                moved: 17
            })
        );
        assert_eq!(
            band_shift_ops(10, 30, -3),
            Some(BandShift {
                src_y: 10,
                dst_y: 13,
                moved: 17
            })
        );
        assert_eq!(band_shift_ops(10, 30, 0), None);
        assert_eq!(band_shift_ops(10, 10, 5), None);
        assert_eq!(band_shift_ops(30, 10, 5), None);
        assert_eq!(band_shift_ops(10, 30, 20), None);
        assert_eq!(band_shift_ops(10, 30, -20), None);
        assert_eq!(
            band_shift_ops(0, 8, 7),
            Some(BandShift {
                src_y: 7,
                dst_y: 0,
                moved: 1
            })
        );
    }

    #[cfg(wgpu_arm)]
    fn resident_names(want: [bool; EFFECT_PIPELINE_COUNT]) -> Vec<&'static str> {
        EFFECT_PIPELINE_NAMES
            .iter()
            .zip(want)
            .filter(|(_, w)| *w)
            .map(|(n, _)| *n)
            .collect()
    }

    #[test]
    #[cfg(wgpu_arm)]
    fn a_translucent_cursor_demands_the_blend_pipeline_even_with_no_cursor_quads() {
        let inst = Instances::default(); // blink-off: both cursor streams empty
        let input = aterm_render::RenderInput::default();
        assert!(
            inst.cursor_block.is_empty() && inst.cursor.is_empty(),
            "precondition: this frame draws no cursor at all"
        );
        let want = frame_effect_pipelines(&inst, &input, false);
        assert!(
            want[EffectPipeline::CursorBlend as usize],
            "a translucent cursor pre-warms the blend pipeline (keyed on the cursor mode) \
             so it exists by the first frame that draws a cursor quad; demanding {:?} \
             leaves it unbuilt",
            resident_names(want)
        );
        let mut expected = [false; EFFECT_PIPELINE_COUNT];
        expected[EffectPipeline::CursorBlend as usize] = true;
        assert_eq!(want, expected);
    }

    #[test]
    #[cfg(wgpu_arm)]
    fn a_row_gated_glow_still_demands_glow_add_for_the_bloom_pass() {
        let inst = Instances::default(); // row gate emptied every stream
        let mut input = aterm_render::RenderInput::default();
        input.cursor_glow_add.push(GlowQuad {
            row: 0,
            x: 0,
            y: 0,
            w: 4,
            h: 4,
            color: 0x0020_2020,
            alpha: 0,
            color2: 0x0020_2020,
            alpha2: 0,
        });
        let want = frame_effect_pipelines(&inst, &input, true);
        let mut expected = [false; EFFECT_PIPELINE_COUNT];
        expected[EffectPipeline::GlowAdd as usize] = true;
        assert_eq!(want, expected);
    }

    #[test]
    fn sdr_glow_attack_is_owned_per_window() {
        let now = aterm_time::Instant::now();
        let mut a = WindowGpu::new();
        let mut b = WindowGpu::new();

        let a_first = a.advance_sdr_glow_budget(true, 0.30, now);
        let a_second =
            a.advance_sdr_glow_budget(true, 0.30, now + std::time::Duration::from_millis(16));
        assert!(a_first > 0.0 && a_second > a_first);

        assert_eq!(b.advance_sdr_glow_budget(false, 0.30, now), 0.0);
        assert_eq!(
            a.sdr_glow_level, a_second,
            "an empty present in window B cannot reset window A's crown"
        );
        assert!(a.sdr_glow_level_at.is_some());

        let b_first = b.advance_sdr_glow_budget(true, 0.30, now);
        assert!(
            (b_first - a_first).abs() < f32::EPSILON,
            "window B begins its own attack instead of inheriting A's level"
        );
        assert_eq!(a.advance_sdr_glow_budget(false, 0.30, now), 0.0);
        assert_eq!(
            b.sdr_glow_level, b_first,
            "resetting window A cannot disturb window B"
        );
    }

    #[test]
    fn frame_passes_coalesce_maximally_without_reordering() {
        for bits in 0u32..(1 << FRAME_GROUPS) {
            let mut enabled = [false; FRAME_GROUPS];
            for (g, e) in enabled.iter_mut().enumerate() {
                *e = bits & (1 << g) != 0;
            }
            let (pass_of, pass_srgb, passes) = super::coalesce_frame_passes(&enabled, &GROUP_SRGB);
            let live: Vec<usize> = (0..FRAME_GROUPS).filter(|&g| enabled[g]).collect();
            assert_eq!(
                passes,
                live.windows(2)
                    .filter(|w| GROUP_SRGB[w[0]] != GROUP_SRGB[w[1]])
                    .count()
                    + usize::from(!live.is_empty()),
                "pass count must be exactly one plus the number of VIEW CHANGES \
                 between consecutive live groups (bits {bits:#010b})"
            );
            for &g in &live {
                assert_eq!(
                    pass_srgb[pass_of[g]], GROUP_SRGB[g],
                    "group {g} must be emitted into a pass attaching ITS view"
                );
            }
            for w in live.windows(2) {
                let (a, b) = (w[0], w[1]);
                if GROUP_SRGB[a] == GROUP_SRGB[b] {
                    assert_eq!(
                        pass_of[a], pass_of[b],
                        "same-view neighbours {a}/{b} must share one pass — an \
                         unmerged boundary is a whole-framebuffer tile load+store"
                    );
                } else {
                    assert_eq!(
                        pass_of[b],
                        pass_of[a] + 1,
                        "a view change must advance by exactly one pass, never \
                         reorder ({a} -> {b})"
                    );
                }
            }
            for g in 0..FRAME_GROUPS {
                if !enabled[g] {
                    assert_eq!(
                        pass_of[g],
                        usize::MAX,
                        "a disabled group must match no pass index"
                    );
                }
            }
        }
    }

    #[test]
    fn the_unorm_ablation_drops_or_thins_exactly_the_one_one_groups() {
        let counts = [Some(7u32); STREAM_COUNT];
        let ctx = |ablation| FramePlanCtx {
            has_image_plane: true,
            has_deco: true,
            has_wallpaper: true,
            cursor_opaque: true,
            ablation,
            under_opaque: false,
        };
        let passes_of = |plan: &[DrawItem]| super::plan_passes(plan).2;
        let plan = |ablation| {
            let mut plan = Vec::new();
            let record = build_frame_plan(&mut plan, &counts, ctx(ablation));
            let unorm: Vec<&DrawItem> = plan
                .iter()
                .filter(|i| !GROUP_SRGB[i.group as usize])
                .collect();
            assert_eq!(
                record,
                PlanUnorm {
                    ablation,
                    draws: unorm.len() as u32,
                    instances: unorm.iter().map(|i| u64::from(i.count)).sum(),
                    route: UnormRoute::Split,
                    under_store: false,
                }
            );
            plan
        };
        let (full, dropped, thinned) = (
            plan(UnormAblation::Off),
            plan(UnormAblation::Drop),
            plan(UnormAblation::Thin),
        );
        let unorm = |i: &DrawItem| !GROUP_SRGB[i.group as usize];
        assert!(
            full.iter().any(unorm),
            "the full plan draws One/One streams, or there is nothing to ablate"
        );
        let kept: Vec<(u8, StreamId)> = full
            .iter()
            .filter(|i| !unorm(i))
            .map(|i| (i.group, i.stream))
            .collect();
        let got: Vec<(u8, StreamId)> = dropped.iter().map(|i| (i.group, i.stream)).collect();
        assert_eq!(got, kept, "Drop removes only Unorm items, in order");
        assert_eq!(
            passes_of(&dropped),
            1,
            "all-sRGB groups coalesce to one pass"
        );
        assert!(
            passes_of(&full) > passes_of(&dropped),
            "the Unorm groups cost passes: {} vs {}",
            passes_of(&full),
            passes_of(&dropped)
        );
        assert_eq!(
            thinned
                .iter()
                .map(|i| (i.group, i.stream))
                .collect::<Vec<_>>(),
            full.iter().map(|i| (i.group, i.stream)).collect::<Vec<_>>(),
            "Thin keeps every item, in order"
        );
        assert_eq!(passes_of(&thinned), passes_of(&full), "and so every pass");
        for (thin, item) in thinned.iter().zip(&full) {
            let want = if unorm(item) { 1 } else { item.count };
            assert_eq!(thin.count, want, "{:?}", item.stream);
        }
    }

    /// The fetch route moves exactly the Unorm-law draws onto their twins, in order and count, so a
    /// full plan is one pass.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_fetch_route_draws_every_unorm_law_row_through_its_twin_in_one_pass() {
        let counts = [Some(7u32); STREAM_COUNT];
        let ctx = FramePlanCtx {
            has_image_plane: true,
            has_deco: true,
            has_wallpaper: true,
            cursor_opaque: false,
            ablation: UnormAblation::Off,
            under_opaque: false,
        };
        let (mut split, mut fetch) = (Vec::new(), Vec::new());
        let mut split_record = build_frame_plan(&mut split, &counts, ctx);
        route_frame_plan(&mut split, &mut split_record, UnormRoute::Split);
        let mut fetch_record = build_frame_plan(&mut fetch, &counts, ctx);
        route_frame_plan(&mut fetch, &mut fetch_record, UnormRoute::Fetch);
        assert_eq!(
            fetch_record,
            PlanUnorm {
                route: UnormRoute::Fetch,
                ..split_record
            }
        );
        assert!(
            split_record.draws > 0,
            "the full plan holds Unorm-law draws"
        );
        let key = |i: &DrawItem| (i.group, i.pipe, i.atlas, i.stream, i.count);
        assert_eq!(
            fetch.iter().map(key).collect::<Vec<_>>(),
            split.iter().map(key).collect::<Vec<_>>(),
            "the route changes no draw but its view and pipeline"
        );
        let mut twins = std::collections::BTreeSet::new();
        for item in &fetch {
            let unorm = !GROUP_SRGB[item.group as usize];
            assert_eq!(item.fetch, unorm, "{:?}", item.stream);
            assert!(item.srgb(), "{:?} draws in the sRGB view", item.stream);
            if unorm {
                let row = item.pipe.row();
                let twin = pipeline_table::FetchTwin::of(row)
                    .unwrap_or_else(|| panic!("{row:?} draws by the Unorm law but has no twin"));
                assert_eq!(twin.row(), row);
                let (spec, base) = (twin.spec(), row.spec());
                assert_eq!(spec.target, pipeline_table::TargetRole::OffscreenSrgb);
                assert!(spec.blend.is_none(), "{twin:?} blends in its fragment");
                assert_eq!(
                    (spec.vs, spec.write_mask, spec.library),
                    (base.vs, base.write_mask, base.library),
                    "{twin:?} keeps its row's vertex stage, mask and library"
                );
                twins.insert(twin);
            }
        }
        assert_eq!(
            twins.into_iter().collect::<Vec<_>>(),
            pipeline_table::FetchTwin::ALL.to_vec(),
            "every twin stands in for a draw of the full plan"
        );
        assert!(!split.iter().any(|i| i.fetch), "the split routes nothing");
        assert!(super::plan_passes(&split).2 > 1);
        assert_eq!(
            super::plan_passes(&fetch).2,
            1,
            "the fetch route is one pass"
        );
    }

    /// Route (1): an opaque Exact Under fx stores at the end of the base pass only while no cursor
    /// glow draws under the text.
    #[test]
    fn an_opaque_under_fx_stores_in_the_base_pass_only_without_a_glow_under() {
        let ctx = |under_opaque| FramePlanCtx {
            has_image_plane: false,
            has_deco: false,
            has_wallpaper: false,
            cursor_opaque: true,
            ablation: UnormAblation::Off,
            under_opaque,
        };
        let mut counts = [None; STREAM_COUNT];
        for id in [
            StreamId::Bg,
            StreamId::Trail,
            StreamId::FxUnder,
            StreamId::FireAdd,
            StreamId::Glyph,
        ] {
            counts[id as usize] = Some(3);
        }
        let order = |plan: &[DrawItem]| plan.iter().map(|i| i.stream).collect::<Vec<_>>();
        let under = |plan: &[DrawItem]| {
            let item = plan
                .iter()
                .find(|i| i.stream == StreamId::FxUnder)
                .expect("the Under fx draws");
            (usize::from(item.group), item.pipe)
        };
        let (mut kept, mut stored) = (Vec::new(), Vec::new());
        let kept_rec = build_frame_plan(&mut kept, &counts, ctx(false));
        let stored_rec = build_frame_plan(&mut stored, &counts, ctx(true));
        assert!(!kept_rec.under_store && stored_rec.under_store);
        assert_eq!(under(&kept), (G_GLOW_UNDER, DrawPipe::Fx));
        assert_eq!(under(&stored), (G_BASE_BG, DrawPipe::FxStore));
        assert_eq!(
            order(&stored),
            order(&kept),
            "the store draws where the CPU draws the layer"
        );
        assert_eq!(stored_rec.draws + 1, kept_rec.draws);
        assert_eq!(
            (super::plan_passes(&kept).2, super::plan_passes(&stored).2),
            (3, 3),
            "the fire still opens its own group"
        );
        counts[StreamId::FireAdd as usize] = None;
        build_frame_plan(&mut kept, &counts, ctx(false));
        build_frame_plan(&mut stored, &counts, ctx(true));
        assert_eq!(
            (super::plan_passes(&kept).2, super::plan_passes(&stored).2),
            (3, 1),
            "with the Under group empty its passes merge away"
        );

        counts[StreamId::GlowUnder as usize] = Some(1);
        let mut glow = Vec::new();
        let glow_rec = build_frame_plan(&mut glow, &counts, ctx(true));
        assert!(!glow_rec.under_store);
        assert_eq!(
            under(&glow),
            (G_GLOW_UNDER, DrawPipe::Fx),
            "a cursor glow under the text keeps the Under fx after it"
        );

        counts[StreamId::GlowUnder as usize] = None;
        counts[StreamId::FxUnder as usize] = None;
        let mut none = Vec::new();
        assert!(
            !build_frame_plan(&mut none, &counts, ctx(true)).under_store,
            "a frame without Under fx stores nothing"
        );
    }

    #[test]
    fn the_unorm_route_seam_reads_its_modes() {
        assert_eq!(UnormRoute::from_seam("split"), UnormRoute::Split);
        assert_eq!(UnormRoute::from_seam("fetch"), UnormRoute::Fetch);
        assert_eq!(UnormRoute::from_seam(""), UnormRoute::Fetch);
        for route in [UnormRoute::Split, UnormRoute::Fetch] {
            assert_eq!(UnormRoute::from_seam(route.name()), route);
        }
    }

    #[test]
    fn the_ablation_seam_reads_its_modes() {
        assert_eq!(UnormAblation::from_seam("drop"), UnormAblation::Drop);
        assert_eq!(UnormAblation::from_seam("1"), UnormAblation::Drop);
        assert_eq!(UnormAblation::from_seam("thin"), UnormAblation::Thin);
        assert_eq!(UnormAblation::from_seam("0"), UnormAblation::Off);
        assert_eq!(UnormAblation::from_seam(""), UnormAblation::Off);
        for mode in [UnormAblation::Off, UnormAblation::Drop, UnormAblation::Thin] {
            let back = UnormAblation::from_seam(mode.name());
            assert_eq!(back, mode, "{}", mode.name());
        }
    }

    #[test]
    fn union_rect_opt_is_conservative_and_clamped() {
        let dims = (100, 50);
        assert_eq!(super::union_rect_opt(None, Some([0, 0, 1, 1]), dims), None);
        assert_eq!(super::union_rect_opt(Some([0, 0, 1, 1]), None, dims), None);
        assert_eq!(
            super::union_rect_opt(Some([0, 0, 0, 0]), Some([2, 3, 4, 5]), dims),
            Some([2, 3, 4, 5])
        );
        assert_eq!(
            super::union_rect_opt(Some([2, 3, 4, 5]), Some([9, 9, 9, 9]), dims),
            Some([2, 3, 4, 5])
        );
        assert_eq!(
            super::union_rect_opt(Some([1, 2, 3, 4]), Some([10, 0, 200, 80]), dims),
            Some([1, 0, 100, 50])
        );
    }

    #[test]
    #[cfg(any(wgpu_arm, target_os = "macos"))]
    fn copy_src_is_armed_only_by_a_live_tap_on_a_capable_surface() {
        let wants = super::GpuRenderer::tap_wants_copy_src;
        assert!(!wants(false, false, false), "idle: no readback, no cost");
        assert!(
            wants(true, false, false),
            "a live recording needs the readback"
        );
        assert!(
            wants(false, true, false),
            "a ONE-SHOT still needs it just as much"
        );
        assert!(
            wants(true, true, false),
            "a still taken mid-recording needs it too"
        );
        assert!(wants(false, false, true), "a recent still holds it");
        #[cfg(wgpu_arm)]
        {
            use wgpu::TextureUsages as U;
            let usage = super::GpuRenderer::swapchain_usage_for;
            assert_eq!(usage(true, false), U::RENDER_ATTACHMENT);
            assert_eq!(usage(true, true), U::RENDER_ATTACHMENT | U::COPY_SRC);
            assert_eq!(
                usage(false, true),
                U::RENDER_ATTACHMENT,
                "a surface with no COPY_SRC cap must never be asked for it"
            );
        }
    }

    #[test]
    #[cfg(any(wgpu_arm, target_os = "macos"))]
    fn polled_snapshots_inside_the_hold_reconfigure_the_swapchain_once() {
        struct Swapchain {
            copyable: bool,
            reconfigures: u32,
        }
        impl Swapchain {
            fn present(
                &mut self,
                hold: super::CopySrcHold,
                snapshot: bool,
                at: aterm_time::Instant,
            ) {
                let want = super::GpuRenderer::tap_wants_copy_src(false, snapshot, hold.holds(at));
                if want != self.copyable {
                    self.copyable = want;
                    self.reconfigures += 1;
                }
            }
        }
        let frame = std::time::Duration::from_millis(16);
        let t0 = aterm_time::Instant::now();
        let mut hold = super::CopySrcHold::default();
        let mut swapchain = Swapchain {
            copyable: false,
            reconfigures: 0,
        };
        let mut capture = |swapchain: &mut Swapchain, at: aterm_time::Instant| {
            hold.arm(at);
            swapchain.present(hold, true, at);
            swapchain.present(hold, false, at + frame);
            hold
        };

        capture(&mut swapchain, t0);
        let hold = capture(&mut swapchain, t0 + std::time::Duration::from_millis(500));
        assert_eq!(
            swapchain.reconfigures, 1,
            "two polled captures: one reconfigure"
        );
        assert!(swapchain.copyable);

        let expired = t0 + std::time::Duration::from_millis(500) + super::SNAPSHOT_COPY_SRC_HOLD;
        swapchain.present(hold, false, expired);
        assert!(!swapchain.copyable, "past the hold the bit drops");
        capture(&mut swapchain, expired + frame);
        assert_eq!(
            swapchain.reconfigures, 3,
            "a capture after the hold reconfigures again"
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
        if cfg!(target_os = "linux") {
            assert_eq!(uniform.content_off, [-2.0, 0.0]);
            assert_eq!(visible_source_scissor(&uniform, 7, 8), Some((0, 1, 7, 6)));
        } else {
            assert_eq!(uniform.content_off, [-2.0, -2.0]);
            assert_eq!(visible_source_scissor(&uniform, 7, 8), Some((0, 0, 7, 5)));
        }

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
        if cfg!(target_os = "linux") {
            assert_eq!(
                visible_source_scissor(&wholly_above, 1, 1),
                Some((0, 0, 1, 1)),
                "the pinned top row stays on screen"
            );
        } else {
            assert_eq!(
                visible_source_scissor(&wholly_above, 1, 1),
                None,
                "a fully clipped crown must not issue a zero/underflowed scissor"
            );
        }
    }

    #[test]
    fn crown_scissor_clips_both_scroll_directions_to_independent_destination_bounds() {
        let mut uniform = present_blit_uniform(false, None, None, 40, 60, 40, 60, 0);
        let up = BandShift {
            src_y: 13,
            dst_y: 10,
            moved: 17,
        };
        let down = BandShift {
            src_y: 10,
            dst_y: 13,
            moved: 17,
        };
        assert_eq!(
            crown_band_scissor(&uniform, None, 40, 60),
            Some((0, 0, 40, 60))
        );
        assert_eq!(
            crown_band_scissor(&uniform, Some(up), 40, 60),
            Some((0, 10, 40, 17))
        );
        assert_eq!(
            crown_band_scissor(&uniform, Some(down), 40, 60),
            Some((0, 13, 40, 17))
        );

        uniform.visible_y = 15.0;
        uniform.visible_h = 5.0;
        for shift in [up, down] {
            assert_eq!(
                crown_band_scissor(&uniform, Some(shift), 40, 60),
                Some((0, 15, 40, 5))
            );
        }
        uniform.visible_y = 30.0;
        uniform.visible_h = 10.0;
        for shift in [up, down] {
            assert_eq!(crown_band_scissor(&uniform, Some(shift), 40, 60), None);
        }

        uniform.content_off = [-3.0, -12.0];
        uniform.visible_y = 0.0;
        uniform.visible_h = 60.0;
        assert_eq!(
            crown_band_scissor(&uniform, Some(up), 30, 30),
            Some((0, 0, 30, 15))
        );
        assert_eq!(
            crown_band_scissor(&uniform, Some(down), 30, 30),
            Some((0, 1, 30, 17))
        );
        assert_eq!(
            crown_band_scissor(&uniform, Some(up), 30, 3),
            Some((0, 0, 30, 3))
        );
        assert_eq!(
            crown_band_scissor(&uniform, Some(down), 30, 3),
            Some((0, 1, 30, 2))
        );
    }

    #[test]
    fn crown_band_scissor_and_present_uniforms_track_fractional_scroll_band() {
        let mut gpu = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
        gpu.set_hdr_glow(true);
        let (rows, cols) = (6usize, 12usize);
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process(b"hello");
        let mut input = term.cell_frame(rows, cols);
        input.grid_top_row = 1;
        input.grid_bot_row = rows - 1;
        input.cursor_glow_add.push(GlowQuad {
            x: 8,
            y: 24,
            w: 8,
            h: 16,
            color: 0xffffff,
            alpha: 0,
            color2: 0xffffff,
            alpha2: 0,
            row: 2,
        });
        let (fw, fh) = gpu.frame_size(rows, cols);
        let (fw, fh) = (fw as u32, fh as u32);
        let (dw, dh) = (fw + 10, fh + 6);
        let mut win = WindowGpu::new();
        win.edr_max = 2.0;

        input.scroll_frac_px = 0;
        let base = gpu.present_uniforms(
            &mut win,
            &input,
            false,
            None,
            None,
            (fw, fh),
            (dw, dh),
            true,
            false,
            false,
        );
        let base_crown = base.crown.expect("HDR present with glow emits a crown");
        assert_eq!(base_crown.content_off, base.want.content_off);

        input.scroll_frac_px = 3;
        let shifted = gpu.present_uniforms(
            &mut win,
            &input,
            false,
            None,
            None,
            (fw, fh),
            (dw, dh),
            true,
            false,
            false,
        );
        let shifted_crown = shifted.crown.expect("HDR present with glow emits a crown");
        assert_eq!(
            shifted_crown.content_off,
            [base.want.content_off[0], base.want.content_off[1] - 3.0],
            "crown placement must translate with the sub-row scroll band"
        );
        let (band_y0, band_y1) = aterm_render::scroll_translate::grid_band_px(
            gpu.cpu.grid_top(),
            gpu.cell_size().1,
            input.grid_top_row,
            input.grid_bot_row,
            fh as usize,
        );
        let expected_shift =
            band_shift_ops(band_y0, band_y1, 3).expect("nonzero frac inside non-empty band");
        assert_eq!(
            shifted.crown_scissor,
            crown_band_scissor(&shifted.want, Some(expected_shift), dw, dh),
            "crown scissor must clip to the moved grid band"
        );
    }

    #[test]
    fn atlas_texel_bytes_match_cpu_glyph_bytes_exactly() {
        let Some(mut cpu) = Renderer::from_system(18.0, Theme::default()) else {
            aterm_spec::host::could_not_run("no system monospace font");
        };

        cpu.debug_block_on_lazy_fallbacks();
        let keys: Vec<GlyphKey> = "RXab\u{65E5}\u{672C}"
            .chars()
            .map(|c| cpu.glyph_key(c))
            .collect();
        assert_atlas_holds_the_cpu_bytes(&mut cpu, keys, 5);
    }

    #[test]
    fn fitted_fallback_styles_reach_the_atlas_as_the_cpu_fit_them() {
        const PRIMARY: &str = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../aterm-render/assets/bundled/CascadiaMonoPL-Regular.ttf"
        );
        let mut cpu = Renderer::from_system_with_family(Some(PRIMARY), 18.0, Theme::default())
            .unwrap_or_else(|| panic!("{PRIMARY} does not load"));
        cpu.debug_block_on_lazy_fallbacks();
        let warning = cpu.glyph_key('\u{26A0}');
        if !matches!(
            warning.source,
            FaceId::Fallback
                | FaceId::SymbolFallback
                | FaceId::RuntimeFallback
                | FaceId::DisplayMix
        ) || warning.glyph_class != GlyphClass::Mono
        {
            aterm_spec::host::could_not_run(format!(
                "this host serves U+26A0 from no mono fallback face ({warning:?})"
            ));
        }
        let (cell_w, cell_h) = cpu.cell_size();
        let mut keys = Vec::new();
        for style in [
            StyleBits::REGULAR,
            StyleBits::BOLD,
            StyleBits::ITALIC,
            StyleBits(StyleBits::BOLD.0 | StyleBits::ITALIC.0),
        ] {
            let key = cpu.glyph_key_styled('\u{26A0}', style);
            assert_eq!(key.cell_span, 1);
            let img = cpu.glyph_image(key);
            assert!(
                img.xmin() >= 0 && img.xmin() + img.width() as i32 <= cell_w as i32,
                "{style:?} warning escaped its one-cell CPU mask before atlas packing"
            );
            assert!(img.height() <= cell_h);
            assert_eq!(img.advance(), cell_w as f32);
            keys.push(key);
        }
        assert_atlas_holds_the_cpu_bytes(&mut cpu, keys, 1);
    }

    fn assert_atlas_holds_the_cpu_bytes(
        cpu: &mut Renderer,
        mut keys: Vec<GlyphKey>,
        at_least: usize,
    ) {
        keys.sort_unstable();
        keys.dedup();
        assert!(
            keys.len() >= at_least,
            "the fixture contributes at least {at_least} distinct glyphs, got {}",
            keys.len()
        );

        let atlas = build_atlas(cpu, &keys, u32::MAX);
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

    #[test]
    fn vs15_and_bare_primary_glyphs_pack_distinct_atlas_entries() {
        let primary_bytes = include_bytes!("../../aterm-render/assets/DejaVuSansMono.ttf");
        let mut primary = Renderer::from_bytes(primary_bytes, 18.0, Theme::default())
            .expect("fixture primary renderer");
        let bolt = '\u{26A1}';
        let bare_primary = primary.glyph_key(bolt);
        assert_eq!(bare_primary.source, FaceId::Primary);
        assert_eq!(bare_primary.glyph_class, GlyphClass::MonoGid);
        assert_eq!(bare_primary.cell_span, 0);

        let mut primary_term = Terminal::new(1, 3);
        primary_term.process("⚡\u{FE0E}".as_bytes());
        let primary_input = primary_term.cell_frame(1, 3);
        let text_primary = primary.resolve_cell_key_for_span(None, &primary_input.cells[0][0], 1);
        assert_eq!(text_primary.source, bare_primary.source);
        assert_eq!(text_primary.glyph_class, bare_primary.glyph_class);
        assert_eq!(text_primary.ch_or_id, bare_primary.ch_or_id);
        assert_eq!(text_primary.cell_span, 1);
        assert_ne!(text_primary, bare_primary);

        let primary_atlas = build_atlas(&mut primary, &[bare_primary, text_primary], u32::MAX);
        let bare_slot = primary_atlas
            .map
            .get(&bare_primary)
            .expect("bare primary key reaches mono atlas");
        let text_slot = primary_atlas
            .map
            .get(&text_primary)
            .expect("VS15 primary key reaches mono atlas");
        assert_ne!(
            (bare_slot.ax, bare_slot.ay),
            (text_slot.ax, text_slot.ay),
            "bare and VS15 primary rasters must occupy distinct atlas entries"
        );
    }

    #[test]
    fn atlas_place_degrades_glyph_wider_than_atlas() {
        let px_q = GlyphKey::quantize_px(18.0);
        let ow = (ATLAS_WIDTH as usize) * 2 + 100;
        let overwide = GlyphImage::Mono {
            width: ow,
            height: 10,
            xmin: 3,
            ymin: 7,
            advance: 12.0,
            bytes: vec![0xAB; ow * 10],
        };

        let mut mono = Atlas {
            kind: AtlasKind::Mono,
            width: ATLAS_WIDTH,
            height: 1,
            data: vec![0u8; ATLAS_WIDTH as usize],
            map: FxHashMap::default(),
            px: 0,
            py: 0,
            shelf_h: 0,
            write_gen: 0,
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
        assert_eq!(
            (mono.px, mono.py, mono.shelf_h),
            (0, 0, 0),
            "cursor unmoved"
        );
        let slot = *mono.map.get(&key).expect("Mono records every key");
        assert_eq!((slot.gw, slot.gh), (0, 0), "degraded slot is zero-sized");
        assert_eq!(
            (slot.xmin, slot.ymin),
            (3, 7),
            "placement offsets preserved"
        );
        let before = mono.data.clone();
        mono.blit(&overwide, &slot);
        assert_eq!(mono.data, before, "zero-slot blit writes nothing");

        let mut color = Atlas {
            kind: AtlasKind::Color,
            width: ATLAS_WIDTH,
            height: 1,
            data: vec![0u8; (ATLAS_WIDTH * 4) as usize],
            map: FxHashMap::default(),
            px: 0,
            py: 0,
            shelf_h: 0,
            write_gen: 0,
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

    #[test]
    fn set_face_preserves_text_shaping() {
        let theme = Theme::default();
        let mut gpu = crate::renderer::gpu_renderer_for_test(Some("Menlo"), 16.0, theme);
        gpu.set_text_shaping(aterm_render::TextShapingConfig {
            ligature_mode: aterm_types::text_shaping::LigatureMode::Disabled,
            ..Default::default()
        });
        let Some(face) = Renderer::from_system(24.0, theme) else {
            aterm_spec::host::could_not_run("no system monospace font");
        };
        gpu.set_face(face, theme);
        assert_eq!(
            gpu.cpu.text_shaping().ligature_mode,
            aterm_types::text_shaping::LigatureMode::Disabled,
            "shaping must survive the face rebuild, not reset to Enabled"
        );
    }

    /// A prepared face draws the weight it was prepared at: a config reload of
    /// `font_weight`, and the headless build's deferred GPU, install a face that already
    /// carries the config's variation requests. Before, installing it put back the
    /// requests of the face it replaced, so a weight changed in Settings (or a headless
    /// instance's configured weight) never reached the GPU's glyphs.
    #[test]
    fn a_prepared_face_keeps_the_weight_it_was_prepared_at() {
        let theme = Theme::default();
        let mut gpu = crate::renderer::gpu_renderer_for_test(Some("Menlo"), 16.0, theme);
        assert!(gpu.cpu.font_variation_requests().is_empty());
        let Some(mut prepared) = Renderer::from_system(16.0, theme) else {
            aterm_spec::host::could_not_run("no system monospace font");
        };
        let weight = [(aterm_render::variation::WGHT_TAG, 600.0)];
        let _ = prepared.set_font_variations(&weight, 0.0);
        gpu.install_prepared_font(None, prepared, theme);
        assert_eq!(gpu.cpu.font_variation_requests(), weight);
    }

    #[test]
    #[cfg(wgpu_arm)]
    fn admitted_rebuild_preserves_device_and_exact_sources() {
        const FONT: &[u8] = include_bytes!("../../aterm-render/assets/DejaVuSansMono.ttf");
        let theme = Theme::default();
        let mut cpu = aterm_render::Renderer::from_bytes(FONT, 16.0, theme)
            .expect("embedded fixture font parses");
        cpu.set_styled_font_bytes(1, FONT)
            .expect("styled fixture parses");
        cpu.set_fallback_bytes(FONT)
            .expect("fallback fixture parses");
        cpu.set_symbol_fallback_bytes(FONT)
            .expect("symbol fixture parses");
        cpu.set_color_font_arc(aterm_render::intern_font_bytes(FONT.to_vec()))
            .expect("emoji fixture parses");
        let sources = cpu.seal_admitted_font_sources();
        let ctx = match GpuContext::new() {
            Ok(ctx) => ctx,
            Err(error) => {
                aterm_spec::host::could_not_run(format!("no GPU available: {error}"));
            }
        };
        let adapter_name = ctx.adapter_name.clone();
        let backend = ctx.backend.clone();
        let limits = ctx.device.limits();
        let mut gpu = GpuRenderer::from_parts(ctx, cpu, None, theme).expect("GPU renderer");
        gpu.rebuild_font_from_admitted(24.0, theme)
            .expect("sealed bytes-only GPU rebuild");
        assert_eq!(gpu.admitted_font_sources(), sources);
        assert_eq!(gpu.ctx.adapter_name, adapter_name);
        assert_eq!(gpu.ctx.backend, backend);
        assert_eq!(gpu.ctx.device.limits(), limits);
    }

    #[test]
    fn deco_atlas_drops_on_oversized_cell_height_no_abort() {
        let mut gpu = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
        let (cw, ch) = gpu.cell_size();
        gpu.ensure_deco_atlas(cw, ch);
        assert!(
            gpu.deco_atlas.is_some(),
            "a realistic {cw}x{ch} cell must build the deco atlas (guard must not over-trigger)"
        );
        gpu.ensure_deco_atlas(cw, 10_000);
        assert!(
            gpu.deco_atlas.is_none(),
            "an oversized cell height must DROP the deco atlas (graceful), not abort"
        );
    }

    #[test]
    #[cfg(wgpu_arm)]
    fn new_glyph_grows_atlas_in_place_without_recreating_texture() {
        let theme = Theme::default();
        let px = 18.0;
        let mut gpu = crate::renderer::gpu_renderer_for_test(None, px, theme);
        let (rows, cols) = (2usize, 8usize);

        let mut win = WindowGpu::new();
        let render = |gpu: &mut GpuRenderer, win: &mut WindowGpu, bytes: &[u8]| {
            let mut term = Terminal::new(rows as u16, cols as u16);
            term.process(bytes);
            let input = term.cell_frame(rows, cols);
            gpu.render_input(win, &input, None);
        };

        render(&mut gpu, &mut win, b"\x1b[?25labc");
        let creations_after_cold = gpu.atlas_tex_creations();
        let dims_after_cold = gpu.atlas_tex_dims().expect("atlases resident");

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

    #[test]
    fn present_prev_is_per_window_no_cross_window_leak() {
        let theme = Theme::default();
        let px = 18.0;
        let mut gpu = crate::renderer::gpu_renderer_for_test(None, px, theme);

        let (rows, cols) = (4usize, 10usize);
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

        let mut last_a = None;
        let mut last_b = None;
        for i in 0..a_frames.len() {
            last_a = Some(gpu.present_input_readback(&mut win_a, &a_frames[i]));
            last_b = Some(gpu.present_input_readback(&mut win_b, &b_frames[i]));
        }
        let got_a = last_a.expect("at least one frame driven");
        let got_b = last_b.expect("at least one frame driven");

        assert!(
            gpu.scissor_taken() > 0,
            "scissor path must be exercised by the changed frames"
        );

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

    #[test]
    fn asymmetric_top_pad_change_invalidates_gpu_present_and_readback_caches() {
        let model = aterm_models_render::asymmetric_pad_layout_model();
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

        let mut gpu = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
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

    #[test]
    fn bloom_glow_frames_scissor_and_match_full_render() {
        let mut gpu = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
        assert!(gpu.bloom_enabled(), "precondition: bloom defaults ON");
        gpu.set_shimmer_phase_for_test(Some(0.33));
        gpu.set_pad(14);
        let pad = gpu.pad();
        let (cw, ch) = gpu.cell_size();
        let (rows, cols) = (10usize, 12usize);

        let frame = |bytes: &[u8], glow: Vec<GlowQuad>| {
            let mut term = Terminal::new(rows as u16, cols as u16);
            term.process(b"\x1b[?25l");
            term.process(bytes);
            let mut input = term.cell_frame(rows, cols);
            input.cursor_glow_add = glow;
            input
        };
        let quad = |row: usize, x: usize, y: usize, h: usize| GlowQuad {
            row: row as u16,
            x: (pad + x) as u16,
            y: (pad + row * ch + y) as u16,
            w: cw as u16,
            h: h as u16,
            color: 0x0030_50A0, // premultiplied ADDITIVE light (alpha 0)
            alpha: 0,
            color2: 0x0030_50A0,
            alpha2: 0,
        };
        let inner = |row: usize, dx: usize| quad(row, 2 * cw + dx, 2, ch - 4);
        let seq = [
            (None, frame(b"AAAA", Vec::new())),
            (None, frame(b"AAAA\r\nbbbb", vec![inner(4, 0)])),
            (
                Some(8.0),
                frame(b"AAAA\r\nbbbb", vec![inner(4, 3), inner(5, 1)]),
            ),
            (None, frame(b"AAAA\r\nbbbb", Vec::new())),
            (None, frame(b"AAAA\r\ncccc", vec![quad(0, cw, 1, ch - 2)])),
            (None, frame(b"AAAA\r\ncccc", Vec::new())),
        ];
        let mut win = WindowGpu::new();
        let mut scissored = 0;
        for (i, (radius, input)) in seq.iter().enumerate() {
            if let Some(radius) = radius {
                gpu.set_bloom_params(BLOOM_STRENGTH, *radius);
            }
            let before = gpu.scissor_taken();
            let got = gpu.present_input_readback(&mut win, input);
            scissored += gpu.scissor_taken() - before;
            let mut ref_win = WindowGpu::new();
            let want = gpu.render_input(&mut ref_win, input, None);
            assert_eq!(
                (got.width, got.height, got.pixels == want.pixels),
                (want.width, want.height, true),
                "frame {i}: the scissored bloom present must be byte-identical to \
                 a fresh full render (halo ghost, accumulation or old-radius fringe?)"
            );
        }
        assert_eq!(
            scissored as usize,
            seq.len() - 1,
            "every frame after the first must keep the scissored path, not force Full \
             (full_repaints={})",
            gpu.full_repaints(),
        );
    }

    /// A shrunk window keeps its GL bloom half texture, and the halo drawn through that roomy
    /// storage matches a never-resized window's out to the frame's edge.
    #[cfg(gl_arm)]
    #[test]
    fn a_bloom_through_kept_storage_matches_one_never_resized() {
        let mut gpu = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
        assert!(gpu.bloom_enabled(), "precondition: bloom defaults ON");
        let pad = gpu.pad();
        let (cw, ch) = gpu.cell_size();
        let frame = |rows: usize, cols: usize| {
            let mut term = Terminal::new(rows as u16, cols as u16);
            term.process(b"\x1b[?25lAAAA\r\nbbbb");
            let mut input = term.cell_frame(rows, cols);
            // In the last column, so the halo reaches the half texture's last texels in use.
            input.cursor_glow_add = vec![GlowQuad {
                row: 1,
                x: (pad + (cols - 1) * cw) as u16,
                y: (pad + ch + 2) as u16,
                w: cw as u16,
                h: (ch - 4) as u16,
                color: 0x0030_50A0,
                alpha: 0,
                color2: 0x0030_50A0,
                alpha2: 0,
            }];
            input
        };
        let (large, small) = (frame(14, 20), frame(12, 18));
        let mut shrunk = WindowGpu::new();
        let _ = gpu.present_input_readback(&mut shrunk, &large);
        let got = gpu.present_input_readback(&mut shrunk, &small);
        let half = shrunk
            .gl_bloom_half
            .as_ref()
            .expect("a bloom frame on the GL arm draws its half texture");
        assert!(
            half.roomy(),
            "the shrink kept the larger frame's storage: {half:?}"
        );
        let want = gpu.present_input_readback(&mut WindowGpu::new(), &small);
        assert_eq!((got.width, got.height), (want.width, want.height));
        assert_eq!(
            got.pixels, want.pixels,
            "the halo through kept storage must be the never-resized window's"
        );
    }

    #[test]
    fn tray_upload_skips_unchanged_card_bytes() {
        let mut gpu = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
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
                scrim: None,
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

        let swapped = TrayQuad {
            rgba: &green,
            pw: ph,
            ph: pw,
            dx: 4,
            dy: 4,
            scrim: None,
        };
        let _ = gpu.render_input(&mut win, &input, Some(swapped));
        assert_eq!(gpu.tray_uploads(), 3, "a recreate must always upload");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_tray_card_drawn_and_waited_inline_records_no_reader() {
        if crate::metal::ffi::Device::preferred().is_none() {
            aterm_spec::host::could_not_run("no Metal device");
        }
        let mut gpu = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
        gpu.arm_metal_for_test();
        let (rows, cols) = (4usize, 10usize);
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process(b"\x1b[?25lhello");
        let input = term.cell_frame(rows, cols);
        let mut win = WindowGpu::new();
        let (pw, ph) = (32u32, 16u32);
        let cards: Vec<Vec<u8>> = [[200, 40, 40, 255], [40, 200, 40, 255], [40, 40, 200, 255]]
            .iter()
            .map(|rgba| rgba.repeat((pw * ph) as usize))
            .collect();
        fn tray(rgba: &[u8], pw: u32, ph: u32) -> TrayQuad<'_> {
            TrayQuad {
                rgba,
                pw,
                ph,
                dx: 4,
                dy: 4,
                scrim: None,
            }
        }
        let readers = |win: &WindowGpu| {
            let card = win.metal_tray_card.as_ref().expect("the armed card");
            (0..crate::tray_card_ring::TRAY_CARD_SLOTS)
                .filter_map(|slot| card.ring.slot(slot))
                .map(|slot| slot.last_read)
                .collect::<Vec<_>>()
        };
        for (i, card) in cards.iter().cycle().take(4).enumerate() {
            let _ = gpu.render_input(&mut win, &input, Some(tray(card, pw, ph)));
            assert!(gpu.metal_render_armed(), "the frame took the Metal arm");
            assert!(
                readers(&win).iter().all(Option::is_none),
                "bake {i}: {:?}",
                readers(&win)
            );
        }
        assert_eq!(
            readers(&win).len(),
            2,
            "the ring rotated through both slots"
        );
        for (i, card) in cards.iter().take(2).enumerate() {
            assert!(gpu.present_virtual(&mut win, &input, false, None, Some(tray(card, pw, ph))));
            assert!(
                readers(&win).iter().all(Option::is_none),
                "virtual present {i}: {:?}",
                readers(&win)
            );
        }
    }

    #[test]
    fn fractional_scroll_keeps_tray_pixels_pinned() {
        let mut gpu = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
        let (rows, cols) = (6usize, 12usize);
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process(b"\x1b[?25lrow zero\r\nrow one\r\nrow two\r\nrow three");
        let mut input = term.cell_frame(rows, cols);
        input.grid_top_row = 1;
        input.grid_bot_row = rows - 1;
        let (cw, ch) = gpu.cell_size();
        let grid_top = gpu.cpu.grid_top();
        let (pw, ph) = ((cw * 3) as u32, ch.saturating_sub(4).max(4) as u32);
        let (dx, dy) = (cw as u32, (grid_top + ch * 2 + 2) as u32);
        let magenta: Vec<u8> = [220, 30, 180, 255].repeat((pw * ph) as usize);
        let tray = || TrayQuad {
            rgba: &magenta,
            pw,
            ph,
            dx,
            dy,
            scrim: None,
        };

        input.scroll_frac_px = 0;
        let mut zero_win = WindowGpu::new();
        let zero = gpu.render_input(&mut zero_win, &input, Some(tray()));
        input.scroll_frac_px = 3;
        let mut shifted_win = WindowGpu::new();
        let shifted = gpu.render_input(&mut shifted_win, &input, Some(tray()));
        assert_eq!((zero.width, zero.height), (shifted.width, shifted.height));

        for y in dy as usize..(dy + ph) as usize {
            let x0 = y * zero.width + dx as usize;
            let x1 = x0 + pw as usize;
            assert_eq!(
                &shifted.pixels[x0..x1],
                &zero.pixels[x0..x1],
                "tray row {y} moved with the terminal scroll band"
            );
        }
        assert_ne!(
            shifted.pixels, zero.pixels,
            "negative control: terminal pixels outside the pinned tray must move"
        );
    }

    #[test]
    #[cfg(wgpu_arm)]
    fn fractional_scroll_frames_keep_the_scissor_path() {
        let mut gpu = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
        #[cfg(target_os = "macos")]
        gpu.disarm_metal_for_test();
        #[cfg(gl_arm)]
        gpu.disarm_gl_for_oracle();
        let (rows, cols) = (6usize, 12usize);
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process(b"\x1b[?25lrow zero\r\nrow one\r\nrow two\r\nrow three");
        let mut input = term.cell_frame(rows, cols);
        input.grid_top_row = 1;
        input.grid_bot_row = rows - 1;
        let (fw, fh) = gpu.frame_size(rows, cols);
        let dest = (fw as u32 + 9, fh as u32 + 7);
        let mut win = WindowGpu::new();

        input.scroll_frac_px = 0;
        gpu.present_swapchain_standin_for_test(&mut win, &input, false, None, None, dest);
        let full_after_prime = gpu.full_repaints();
        let scissor_after_prime = gpu.scissor_taken();

        input.scroll_frac_px = 3;
        gpu.present_swapchain_standin_for_test(&mut win, &input, false, None, None, dest);
        assert_eq!(
            gpu.full_repaints(),
            full_after_prime,
            "a sub-row scroll frame over unchanged content must not force a Full repaint"
        );
        assert!(
            gpu.scissor_taken() > scissor_after_prime,
            "the frac frame must take the scissor/gate path (scissor_taken={}, full_repaints={})",
            gpu.scissor_taken(),
            gpu.full_repaints()
        );

        for frac in [7, 0] {
            input.scroll_frac_px = frac;
            let before = gpu.scissor_taken();
            gpu.present_swapchain_standin_for_test(&mut win, &input, false, None, None, dest);
            assert_eq!(
                gpu.full_repaints(),
                full_after_prime,
                "frac {frac}: no Full repaint"
            );
            assert!(gpu.scissor_taken() > before, "frac {frac}: scissor path");
        }

        input.scroll_frac_px = 0;
        let resident = gpu.present_input_readback(&mut win, &input);
        let mut fresh = WindowGpu::new();
        let oracle = gpu.render_input(&mut fresh, &input, None);
        assert_eq!(
            resident.pixels, oracle.pixels,
            "the scissor base must stay untranslated through a glide"
        );
        input.scroll_frac_px = 3;
        let translated = gpu.render_input(&mut fresh, &input, None);
        assert_ne!(
            translated.pixels, oracle.pixels,
            "frac 3 must move terminal pixels"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_failed_first_frame_warm_leaves_the_inline_mint_and_its_failure_route() {
        let dev = crate::metal::ffi::Device::preferred()
            .unwrap_or_else(|| aterm_spec::host::could_not_run("no Metal device"));
        static LATCH: crate::metal::warm::WarmLatch =
            crate::metal::warm::WarmLatch::new(crate::metal::warm::DEADLINE);
        let renderer = || {
            let mut g = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
            g.arm_metal_for_test();
            g.set_first_frame_latch_for_test(&LATCH);
            g
        };
        let mut gpu = renderer();
        let mut warm = LATCH.begin().expect("the latch's first warm");
        let failed = warm.run_on(
            &dev,
            crate::metal::warm::FirstFrameKnobs::default(),
            |_, _| Err("injected warm failure".to_owned()),
        );
        assert_eq!(failed, Err("injected warm failure".to_owned()));
        drop(warm);
        assert!(
            gpu.metal_arm_ready(),
            "the mint after a failed warm compiles inline: {}",
            gpu.metal_start_error()
        );
        assert!(gpu.metal_render_armed());

        let mut gpu = renderer();
        INJECTED_MINT_FAILURE.with(|f| f.set(Some("injected mint failure")));
        assert!(!gpu.metal_arm_ready(), "a failed mint is not ready");
        assert!(!gpu.metal_armed, "a failed mint disarms");
        assert_eq!(
            gpu.metal_start_error(),
            "the Metal renderer could not start: injected mint failure"
        );
    }

    /// A small text frame lit by one cursor-aurora quad: Unorm-law light for the route to place.
    #[cfg(target_os = "macos")]
    fn glow_over_text(gpu: &GpuRenderer) -> RenderInput {
        let (rows, cols) = (6usize, 24usize);
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process(b"glow over text");
        let mut input = term.cell_frame(rows, cols);
        let (cw, ch) = gpu.cpu.cell_size();
        input.cursor_glow_add.push(GlowQuad {
            row: 0,
            x: 0,
            y: 0,
            w: (4 * cw) as u16,
            h: ch as u16,
            color: 0x0040_6080,
            alpha: 0,
            color2: 0x0040_6080,
            alpha2: 0,
        });
        input
    }

    /// One [`glow_over_text`] frame on a fresh armed renderer: its shape and the fetch twins built.
    #[cfg(target_os = "macos")]
    fn glow_frame_on(setup: impl FnOnce(&mut GpuRenderer)) -> (FrameShape, usize) {
        let mut gpu = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
        gpu.arm_metal_for_test();
        gpu.set_bloom(false);
        gpu.set_shimmer(false);
        setup(&mut gpu);
        let input = glow_over_text(&gpu);
        let mut win = WindowGpu::new();
        let _ = gpu.render_input(&mut win, &input, None);
        assert!(gpu.metal_render_armed(), "the frame minted the Metal arm");
        let twins = gpu
            .metal_built_for_test()
            .into_iter()
            .filter(|item| matches!(item, crate::metal::warm::WarmItem::Twin(_)))
            .count();
        (gpu.frame_shape(), twins)
    }

    /// A device whose fetch verdict is anything but Passed draws its Unorm-law light on the split
    /// whatever route the renderer asks for, building no twin.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_device_that_did_not_pass_the_probe_draws_its_unorm_light_on_the_split() {
        let dev = crate::metal::ffi::Device::preferred()
            .unwrap_or_else(|| aterm_spec::host::could_not_run("no Metal device"));
        let failed = FetchProbe::Failed {
            stage: crate::FetchStage::InPass,
            channel: 2,
            want: 9,
            got: 10,
        };
        for verdict in [failed, FetchProbe::Refused, FetchProbe::Unsupported] {
            let (shape, twins) = glow_frame_on(|gpu| {
                gpu.set_unorm_route(UnormRoute::Fetch);
                gpu.set_fetch_probe_seam(Some(verdict));
            });
            assert_eq!(shape.route, UnormRoute::Split, "{verdict:?}");
            assert!(
                shape.unorm_draws > 0 && shape.passes > 1,
                "{verdict:?}: the light draws on the Unorm view, {} passes",
                shape.passes
            );
            assert_eq!(twins, 0, "{verdict:?}: no twin is built");
        }
        if !crate::metal::fetch::capable(&dev) {
            aterm_spec::host::could_not_run("the device has no framebuffer fetch");
        }
        let (shape, twins) = glow_frame_on(|gpu| {
            gpu.set_unorm_route(UnormRoute::Fetch);
            gpu.set_fetch_probe_seam(Some(FetchProbe::Passed));
        });
        assert_eq!(
            (shape.route, shape.passes),
            (UnormRoute::Fetch, 1),
            "the same frame under a passed verdict takes the fetch: the verdict kept the split"
        );
        assert!(twins > 0);
    }

    /// An unconfigured renderer routes Unorm-law light through fetch, every Apple GPU has fetch,
    /// and on a device that passes its probe the default glow frame is one pass.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_default_route_is_the_fetch_and_this_device_takes_it() {
        use std::ffi::OsStr;
        assert_eq!(UnormRoute::initial(None), UnormRoute::Fetch);
        assert_eq!(
            UnormRoute::initial(Some(OsStr::new("split"))),
            UnormRoute::Split
        );
        assert_eq!(
            UnormRoute::initial(Some(OsStr::new("fetch"))),
            UnormRoute::Fetch
        );
        let dev = crate::metal::ffi::Device::preferred()
            .unwrap_or_else(|| aterm_spec::host::could_not_run("no Metal device"));
        if cfg!(target_arch = "aarch64") {
            assert!(
                crate::metal::fetch::capable(&dev),
                "{}: an Apple GPU has framebuffer fetch",
                dev.name()
            );
        } else if !crate::metal::fetch::capable(&dev) {
            aterm_spec::host::could_not_run("the device has no framebuffer fetch");
        }
        if aterm_types::dev_seam!("ATERM_GPU_UNORM_ROUTE").is_some() {
            aterm_spec::host::could_not_run("ATERM_GPU_UNORM_ROUTE chooses the route here");
        }
        let (shape, twins) = glow_frame_on(|_| {});
        assert_eq!(
            (shape.route, shape.passes),
            (UnormRoute::Fetch, 1),
            "{}: the default glow frame",
            dev.name()
        );
        assert!(twins > 0, "it drew through a twin");
    }

    /// An armed renderer with every present pass on (bloom, shimmer, both crowns, a window on
    /// screen that may present f16), routing Unorm-law light through framebuffer fetch.
    #[cfg(target_os = "macos")]
    fn every_pass_on() -> GpuRenderer {
        let mut gpu = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
        gpu.arm_metal_for_test();
        gpu.metal_on_screen = true;
        gpu.set_unorm_route(UnormRoute::Fetch);
        gpu.set_bloom(true);
        gpu.set_bloom_params(0.85, 2.2);
        gpu.set_shimmer(true);
        gpu.set_hdr_glow(true);
        gpu.set_sdr_glow_boost(0.25);
        gpu
    }

    /// A default window's first frames on `gpu`, presented to a fresh `format` swapchain on a
    /// screen of `edr` headroom: a blank grid, the first prompt (cursor aurora and a Claude card's
    /// fx layer), then the prompt with an emoji.
    #[cfg(target_os = "macos")]
    fn present_first_frames(
        gpu: &mut GpuRenderer,
        format: crate::device_layer::TexelFormat,
        edr: f32,
    ) {
        let (rows, cols) = (24usize, 80usize);
        let mut win = WindowGpu::new();
        win.set_edr_max(edr);
        let blank = Terminal::new(rows as u16, cols as u16).cell_frame(rows, cols);
        let (cw, ch) = gpu.cpu.cell_size();
        let card = aterm_core::render::FxLayer::new(
            1,
            std::sync::Arc::new(aterm_core::render::FxContent::new(
                vec![aterm_core::render::FxItem::Rect(
                    aterm_core::render::FxRect {
                        x: 0,
                        y: (2 * ch) as i32,
                        w: (20 * cw) as u16,
                        h: 2,
                        ink: 0,
                    },
                )],
                vec![
                    aterm_core::render::FxInk::add(0x0020_4060),
                    aterm_core::render::FxInk::add(0x0060_4020),
                ],
            )),
            aterm_core::render::FxSlot::Under,
            aterm_core::render::FxSpace::Window,
            aterm_core::render::FxBlend::Exact,
        );
        let prompt = |text: &str| {
            let mut term = Terminal::new(rows as u16, cols as u16);
            term.process(text.as_bytes());
            let mut input = term.cell_frame(rows, cols);
            input.cursor_glow_add.push(aterm_render::GlowQuad {
                row: 0,
                x: (14 * cw) as u16,
                y: 0,
                w: (2 * cw) as u16,
                h: ch as u16,
                color: 0x0040_6080,
                alpha: 0,
                color2: 0x0040_6080,
                alpha2: 0,
            });
            input.fx_layers.push(card.clone());
            input
        };
        let (fw, fh) = gpu.frame_size(rows, cols);
        for input in [&blank, &prompt("user@host ~ % "), &prompt("\u{1f680} ~ % ")] {
            gpu.metal_present_standalone_for_test(
                &mut win,
                input,
                (fw as u32, fh as u32),
                (false, true),
                format,
                |_, _| Ok(()),
            )
            .expect("the armed present");
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_first_frame_warm_builds_what_a_default_windows_first_frames_build() {
        use crate::device_layer::TexelFormat;
        use crate::metal::warm::{FirstFrameKnobs, WarmItem, first_frame_rows, first_frame_twins};
        if crate::metal::ffi::Device::preferred().is_none() {
            aterm_spec::host::could_not_run("no Metal device");
        }
        let mut built: Vec<WarmItem> = Vec::new();
        let mut fetch = false;
        for (format, edr) in [
            (TexelFormat::Bgra8Unorm, 1.0),
            (TexelFormat::Rgba16Float, 2.0),
        ] {
            let mut gpu = every_pass_on();
            present_first_frames(&mut gpu, format, edr);
            for item in gpu.metal_built_for_test() {
                if !built.contains(&item) {
                    built.push(item);
                }
            }
            fetch = gpu.fetch_probe() == Some(FetchProbe::Passed);
        }
        let knobs = FirstFrameKnobs {
            aurora: true,
            bloom: true,
            hdr_glow: true,
            sdr_glow: true,
            fx: true,
        };
        let mut warmed: Vec<WarmItem> = Vec::new();
        for (row, present) in first_frame_rows(knobs, fetch) {
            for item in [
                WarmItem::Library(row.spec().library),
                WarmItem::pipeline(row, present),
            ] {
                if !warmed.contains(&item) {
                    warmed.push(item);
                }
            }
        }
        warmed.extend(
            first_frame_twins(knobs, fetch)
                .into_iter()
                .map(WarmItem::Twin),
        );
        let missed: Vec<&WarmItem> = built.iter().filter(|i| !warmed.contains(i)).collect();
        let idle: Vec<&WarmItem> = warmed.iter().filter(|i| !built.contains(i)).collect();
        assert!(
            missed.is_empty() && idle.is_empty(),
            "the first frames built {built:?}\nthe warm builds {warmed:?}\nmissed by the \
             warm: {missed:?}\nbuilt by the warm for nothing: {idle:?}"
        );
    }

    /// A warm asked for before the arm minted starts from the mint and hands over every row and
    /// twin, so a default window's first frames build no pipeline on the render thread; without
    /// the warm they build their own.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_warmed_arm_builds_no_pipeline_on_the_render_thread() {
        use crate::device_layer::TexelFormat;
        use crate::metal::warm::{PresentKnobs, bindable};
        if crate::metal::ffi::Device::preferred().is_none() {
            aterm_spec::host::could_not_run("no Metal device");
        }
        const HANG: std::time::Duration = std::time::Duration::from_secs(60);
        let on = PresentKnobs {
            bloom: true,
            shimmer: true,
            hdr_glow: true,
            sdr_glow: true,
        };
        for (format, edr) in [
            (TexelFormat::Bgra8Unorm, 1.0),
            (TexelFormat::Rgba16Float, 2.0),
        ] {
            let mut gpu = every_pass_on();
            gpu.warm_effect_pipelines();
            assert!(
                gpu.metal_arm_ready(),
                "the mint: {}",
                gpu.metal_start_error()
            );
            let twins = gpu.fetch_probe().is_none_or(|v| v == FetchProbe::Passed);
            assert_eq!(
                gpu.metal_built_for_test(),
                vec![crate::metal::warm::WarmItem::Library(ShaderLibrary::Cell)],
                "the mint built nothing"
            );
            assert_eq!(
                gpu.await_pipeline_warm_for_test(HANG),
                Vec::new(),
                "builds the warm failed"
            );
            present_first_frames(&mut gpu, format, edr);
            let held = gpu.metal_built_for_test();
            let untaken: Vec<_> = bindable(on, twins)
                .into_iter()
                .filter(|item| !held.contains(item))
                .collect();
            assert!(
                untaken.is_empty(),
                "the frames' first miss took every build the warm made: {untaken:?} untaken"
            );
            assert_eq!(
                gpu.metal_built_here_for_test(),
                Vec::new(),
                "{format:?}: built on the render thread after the warm"
            );

            let mut cold = every_pass_on();
            present_first_frames(&mut cold, format, edr);
            assert!(
                !cold.metal_built_here_for_test().is_empty(),
                "{format:?}: without the warm the frames build their own"
            );
        }
    }

    /// A pass whose switch was off when the warm ran is warmed when the switch turns it on, and
    /// only then.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_pass_switched_on_after_the_warm_is_warmed_then() {
        use crate::metal::ffi::PixelFormat;
        use crate::metal::warm::WarmItem;
        if crate::metal::ffi::Device::preferred().is_none() {
            aterm_spec::host::could_not_run("no Metal device");
        }
        const HANG: std::time::Duration = std::time::Duration::from_secs(60);
        let shimmer = WarmItem::pipeline(Pipeline::Shimmer, PixelFormat::Rgba8Unorm);
        let mut gpu = every_pass_on();
        gpu.set_shimmer(false);
        gpu.warm_effect_pipelines();
        assert!(
            gpu.metal_arm_ready(),
            "the mint: {}",
            gpu.metal_start_error()
        );
        assert!(
            !gpu.metal_warm_owed_for_test().is_empty()
                && !gpu.metal_warm_owed_for_test().contains(&shimmer),
            "the warm left the shimmer it found off"
        );
        gpu.set_shimmer(false);
        assert!(!gpu.metal_warm_owed_for_test().contains(&shimmer));
        gpu.set_shimmer(true);
        assert!(
            gpu.metal_warm_owed_for_test().contains(&shimmer),
            "turning the shimmer on warms it"
        );
        assert_eq!(gpu.await_pipeline_warm_for_test(HANG), Vec::new());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn armed_present_composes_the_stale_rect_and_matches_a_full_copy() {
        if crate::metal::ffi::Device::preferred().is_none() {
            aterm_spec::host::could_not_run("no Metal device");
        }
        let mut gpu = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
        gpu.arm_metal_for_test();
        let (rows, cols) = (6usize, 12usize);
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process(b"\x1b[?25lrow zero\r\nrow one\r\nrow two\r\nrow three");
        let glide_input = |term: &mut Terminal| {
            let mut input = term.cell_frame(rows, cols);
            input.grid_top_row = 1;
            input.grid_bot_row = rows - 1;
            input.scroll_frac_px = 3;
            input
        };
        let mut input = glide_input(&mut term);
        let mut win = WindowGpu::new();
        input.scroll_frac_px = 0;
        let first = gpu.render_input(&mut win, &input, None);
        assert!(
            gpu.metal_render_armed(),
            "the first frame must mint the arm"
        );
        let (fw, fh) = (first.width as u32, first.height as u32);
        let dest = (fw + 9, fh + 7);
        input.scroll_frac_px = 3;

        let primed = gpu
            .metal_present_bytes_with_scope_for_test(&mut win, &input, dest, false, true)
            .expect("armed present");
        assert_eq!(
            win.metal_last_compose_rect,
            Some([0, 0, fw, fh]),
            "the first sync into a fresh copy is the full frame"
        );
        let mut flat = input.clone();
        flat.scroll_frac_px = 0;
        let flat = gpu
            .metal_present_bytes_with_scope_for_test(
                &mut WindowGpu::new(),
                &flat,
                dest,
                false,
                true,
            )
            .expect("armed present");
        assert_ne!(
            primed, flat,
            "a frac 3 present must move terminal pixels on glass"
        );

        term.process(b"\x1b[3;1HX");
        let changed = glide_input(&mut term);
        let scissors_before = gpu.scissor_taken();
        let incremental = gpu
            .metal_present_bytes_with_scope_for_test(&mut win, &changed, dest, false, false)
            .expect("armed present");
        assert!(
            gpu.scissor_taken() > scissors_before,
            "non-vacuity: the second armed encode must take the scissor path"
        );
        let (y0, y1) = aterm_render::scroll_translate::grid_band_px(
            gpu.cpu.grid_top(),
            gpu.cell_size().1,
            1,
            rows - 1,
            fh as usize,
        );
        let band = [0, y0 as u32, fw, y1 as u32];
        assert!(
            band[3] - band[1] < fh,
            "fixture: the band {band:?} must be smaller than the {fw}x{fh} frame"
        );
        assert_eq!(
            win.metal_last_compose_rect,
            Some(band),
            "the compose must re-copy the stale union (the previous shift's band, \
             which contains the dirty row), never the whole {fw}x{fh} frame"
        );
        assert_ne!(
            incremental, primed,
            "negative control: the changed cell must reach the glass"
        );

        let mut fresh = WindowGpu::new();
        let oracle = gpu
            .metal_present_bytes_with_scope_for_test(&mut fresh, &changed, dest, false, true)
            .expect("armed present");
        assert_eq!(
            fresh.metal_last_compose_rect,
            Some([0, 0, fw, fh]),
            "fixture: the oracle is a full-frame compose"
        );
        assert_eq!(
            incremental, oracle,
            "the stale-rect compose must present the full-copy bytes"
        );

        assert_eq!(
            win.offscreen_dirty_since_sync,
            Some([0, 0, 0, 0]),
            "fixture: the compose just synced the copy"
        );
        gpu.shift_offscreen_rows(&mut win, 1, rows);
        assert_eq!(
            win.offscreen_dirty_since_sync, None,
            "the armed E7 shift must invalidate the incremental sync"
        );
    }

    #[test]
    #[cfg(wgpu_arm)]
    fn present_alpha_mode_is_opacity_and_caps_aware() {
        use super::GpuRenderer;
        use wgpu::CompositeAlphaMode::{Opaque, PostMultiplied};
        assert_eq!(GpuRenderer::present_alpha_mode(1.0, true), Opaque);
        assert_eq!(GpuRenderer::present_alpha_mode(1.5, true), Opaque);
        assert_eq!(GpuRenderer::present_alpha_mode(f32::NAN, true), Opaque);
        assert_eq!(GpuRenderer::present_alpha_mode(0.85, true), PostMultiplied);
        assert_eq!(GpuRenderer::present_alpha_mode(0.0, true), PostMultiplied);
        assert_eq!(GpuRenderer::present_alpha_mode(0.85, false), Opaque);
        assert_eq!(GpuRenderer::present_alpha_mode(0.0, false), Opaque);
    }

    #[test]
    fn hdr_upgrade_publishes_linear_capture_metadata_with_safe_defaults() {
        use crate::video_tap::CaptureColorSpace;

        let mut win = WindowGpu::new();
        win.set_capture_color_space(CaptureColorSpace::Srgb);
        win.set_sdr_white_scale(3.0);
        win.set_edr_max(4.0);
        win.apply_hdr_surface_upgrade();
        assert_eq!(
            win.capture_color_space(),
            CaptureColorSpace::ExtendedLinearSrgb
        );
        assert_eq!(win.sdr_white_scale(), 1.0);
        assert_eq!(win.edr_max(), 0.0);
    }

    #[cfg(wgpu_arm)]
    fn confirmed_hdr_window() -> WindowGpu {
        let mut win = WindowGpu::new();
        win.set_capture_color_space(crate::video_tap::CaptureColorSpace::ExtendedLinearSrgb);
        win.set_sdr_white_scale(2.5);
        win.set_edr_max(3.0);
        win
    }

    #[cfg(wgpu_arm)]
    fn project_reconfigure(
        before: &std::collections::BTreeMap<&'static str, i64>,
        projection: crate::format_plan::HdrReconfigureProjection,
    ) -> std::collections::BTreeMap<&'static str, i64> {
        let mut after = before.clone();
        after.insert("stage", i64::from(projection.stage));
        after.insert("retagged", i64::from(projection.retagged));
        after.insert("is_f16", i64::from(projection.is_f16));
        after.insert("capture_linear", i64::from(projection.capture_linear));
        after
    }

    #[cfg(wgpu_arm)]
    fn assert_reconfigure_transition(
        model: &aterm_spec::derive::Model,
        before: &std::collections::BTreeMap<&'static str, i64>,
        after: &std::collections::BTreeMap<&'static str, i64>,
        action: &'static str,
    ) {
        assert!(
            model.action_enabled(action, before),
            "{action} must be enabled for {before:?}"
        );
        assert_eq!(
            model.successors(action, before).as_slice(),
            std::slice::from_ref(after),
            "the shipping plan+apply projection must be the EXACT {action} successor"
        );
        let (conforms, evidence) = aterm_spec::verify::validate_transition_tiered(
            model,
            &[],
            before,
            after,
            Some(action),
            "HDR reconfigure plan/apply conformance",
        );
        assert!(conforms, "{action}: {evidence}");
        for invariant in &model.invariants {
            assert!(
                model.check_invariant(invariant.name, after),
                "{action} violates {}::{}: {after:?}",
                model.name,
                invariant.name
            );
        }
    }

    #[cfg(all(target_os = "macos", not(wgpu_arm)))]
    #[test]
    fn the_screen_upgrade_moves_the_layer_before_it_promotes_the_metadata() {
        use crate::device_layer::TexelFormat;
        use crate::video_tap::CaptureColorSpace;

        let _pool = crate::metal::ffi::AutoreleasePool::new();
        let parent = crate::metal::test_support::plain_calayer();
        let mut gpu =
            crate::renderer::gpu_renderer_for_test(None, 18.0, aterm_render::Theme::default());
        gpu.set_hdr_glow(true);
        let mut win = WindowGpu::new();
        let mut surf = gpu
            .attach_metal_window_surface(&parent, TexelFormat::Bgra8Unorm, 64, 48)
            .expect("an 8-bit attach under a stand-in parent layer");
        let linear =
            |win: &WindowGpu| win.capture_color_space == CaptureColorSpace::ExtendedLinearSrgb;
        let layer = |surf: &super::GpuSurface| {
            surf.metal
                .as_ref()
                .expect("the attach armed a swapchain")
                .config()
                .format
        };
        let held_8bit = |surf: &super::GpuSurface, why: &str| {
            assert_eq!(
                surf.neutral.format,
                TexelFormat::Bgra8Unorm,
                "{why}: retained"
            );
            assert_eq!(
                layer(surf),
                TexelFormat::Bgra8Unorm.metal(),
                "{why}: live layer"
            );
        };

        for potential in [Some(1.0), None] {
            assert!(
                !gpu.upgrade_surface_on_screen(&mut win, &mut surf, || potential),
                "{potential:?} is no evidence of an EDR screen"
            );
            held_8bit(&surf, "no EDR screen");
            assert!(!linear(&win), "no EDR screen promotes no metadata");
        }

        let arm = gpu.metal_arm.take();
        assert!(!gpu.upgrade_surface_on_screen(&mut win, &mut surf, || Some(2.0)));
        held_8bit(&surf, "a refused flip");
        assert!(!linear(&win), "a refused flip promotes no metadata");
        gpu.metal_arm = arm;

        assert!(
            gpu.upgrade_surface_on_screen(&mut win, &mut surf, || Some(2.0)),
            "an EDR screen upgrades an opted-in 8-bit window"
        );
        assert_eq!(surf.neutral.format, TexelFormat::Rgba16Float);
        assert_eq!(
            layer(&surf),
            TexelFormat::Rgba16Float.metal(),
            "the live layer itself moved, not only the retained desire"
        );
        assert!(linear(&win), "the metadata follows the flipped layer");

        let mut read = false;
        assert!(!gpu.upgrade_surface_on_screen(&mut win, &mut surf, || {
            read = true;
            Some(1.0)
        }));
        assert!(!read, "an f16 surface never reads the screen");
        assert_eq!(
            layer(&surf),
            TexelFormat::Rgba16Float.metal(),
            "one direction only"
        );
    }

    #[test]
    #[cfg(wgpu_arm)]
    fn hdr_reconfigure_apply_conforms_to_retag_model() {
        use crate::format_plan::{HdrReconfigurePlan, hdr_reconfigure_plan};
        use crate::video_tap::CaptureColorSpace;

        let model = aterm_models_render::hdr_reconfigure_retag_model();
        let initial = model.init_state();

        let mut downgrade_fallbacks = 0usize;
        for retagged in [false, true] {
            let action = if retagged {
                "RetagSucceeds"
            } else {
                "RetagFails"
            };
            let mut win = confirmed_hdr_window();
            let plan = hdr_reconfigure_plan(true, retagged);
            win.apply_hdr_reconfigure_plan(plan);
            let after = project_reconfigure(
                &initial,
                win.project_hdr_reconfigure_state(2, retagged, plan),
            );
            assert_reconfigure_transition(&model, &initial, &after, action);
            match plan {
                HdrReconfigurePlan::KeepHdr => {
                    assert_eq!(win.sdr_white_scale(), 2.5, "a kept HDR epoch is preserved");
                    assert_eq!(win.edr_max(), 3.0);
                }
                HdrReconfigurePlan::FallbackToSdr => {
                    downgrade_fallbacks += 1;
                    assert_eq!(win.sdr_white_scale(), 1.0);
                    assert_eq!(win.edr_max(), 0.0);
                }
                HdrReconfigurePlan::KeepSdr => {
                    panic!("an existing f16 surface cannot take the KeepSdr arm")
                }
            }
        }
        assert_eq!(
            downgrade_fallbacks, 1,
            "exactly the failed f16 re-tag must select SDR fallback"
        );

        let fallback_plan = hdr_reconfigure_plan(true, false);
        assert_eq!(fallback_plan, HdrReconfigurePlan::FallbackToSdr);
        let mut fallback_window = confirmed_hdr_window();
        fallback_window.apply_hdr_reconfigure_plan(fallback_plan);
        let sdr = project_reconfigure(
            &initial,
            fallback_window.project_hdr_reconfigure_state(1, false, fallback_plan),
        );
        assert_reconfigure_transition(&model, &initial, &sdr, "EnterSdrFallback");
        assert_eq!(sdr["stage"], 1);
        assert_eq!(sdr["is_f16"], 0);
        assert_eq!(sdr["capture_linear"], 0);

        let mut upgrade_fallbacks = 0usize;
        for retagged in [false, true] {
            let action = if retagged {
                "UpgradeSucceeds"
            } else {
                "UpgradeFails"
            };
            let mut win = confirmed_hdr_window();
            win.apply_hdr_reconfigure_plan(fallback_plan);
            let plan = hdr_reconfigure_plan(true, retagged);
            win.apply_hdr_reconfigure_plan(plan);
            match plan {
                HdrReconfigurePlan::KeepHdr => {
                    assert_eq!(
                        win.capture_color_space(),
                        CaptureColorSpace::Srgb,
                        "a successful plan alone must not publish the new encoding"
                    );
                    win.apply_hdr_surface_upgrade();
                }
                HdrReconfigurePlan::FallbackToSdr => {
                    upgrade_fallbacks += 1;
                }
                HdrReconfigurePlan::KeepSdr => {
                    panic!("an admitted f16 upgrade cannot take the KeepSdr arm")
                }
            }
            let after =
                project_reconfigure(&sdr, win.project_hdr_reconfigure_state(2, retagged, plan));
            assert_reconfigure_transition(&model, &sdr, &after, action);
        }
        assert_eq!(
            upgrade_fallbacks, 1,
            "a failed upgrade tag must restore exactly one SDR fallback"
        );

        for retagged in [false, true] {
            let plan = hdr_reconfigure_plan(false, retagged);
            assert_eq!(plan, HdrReconfigurePlan::KeepSdr);
            let mut p3 = WindowGpu::new();
            p3.set_capture_color_space(CaptureColorSpace::DisplayP3);
            p3.apply_hdr_reconfigure_plan(plan);
            assert_eq!(
                p3.capture_color_space(),
                CaptureColorSpace::DisplayP3,
                "KeepSdr must preserve confirmed non-sRGB SDR metadata"
            );
        }

        let mut ignored_failure = initial.clone();
        ignored_failure.insert("stage", 2);
        ignored_failure.insert("retagged", 0);
        ignored_failure.insert("is_f16", 1);
        ignored_failure.insert("capture_linear", 1);
        let (accepted, evidence) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &initial,
            &ignored_failure,
            Some("RetagFails"),
            "HDR ignored-retag-failure negative control",
        );
        assert!(
            !accepted,
            "ignored retag failure unexpectedly conformed: {evidence}"
        );

        let mut failed_upgrade = sdr.clone();
        failed_upgrade.insert("stage", 2);
        failed_upgrade.insert("retagged", 0);
        failed_upgrade.insert("is_f16", 1);
        failed_upgrade.insert("capture_linear", 0);
        let (accepted, evidence) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &sdr,
            &failed_upgrade,
            Some("UpgradeFails"),
            "HDR failed-upgrade negative control",
        );
        assert!(
            !accepted,
            "failed upgrade mutant unexpectedly conformed: {evidence}"
        );

        let unapplied = confirmed_hdr_window();
        let unescaped = project_reconfigure(
            &initial,
            unapplied.project_hdr_reconfigure_state(1, false, fallback_plan),
        );
        assert_eq!(unescaped["capture_linear"], 1);
        let (accepted, evidence) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &initial,
            &unescaped,
            Some("EnterSdrFallback"),
            "HDR unapplied-escape negative control",
        );
        assert!(
            !accepted,
            "unapplied SDR escape unexpectedly conformed: {evidence}"
        );
        assert!(!model.check_invariant("CaptureMatchesSurfaceEncoding", &unescaped));
    }

    #[test]
    #[cfg(wgpu_arm)]
    fn hdr_state_probe_is_immediate_then_throttled() {
        let now = aterm_time::Instant::now();
        assert!(hdr_color_space_probe_due(None, now));
        assert!(!hdr_color_space_probe_due(Some(now), now));

        let just_before = now + HDR_COLOR_SPACE_PROBE_INTERVAL - std::time::Duration::from_nanos(1);
        assert!(!hdr_color_space_probe_due(Some(now), just_before));

        let due = now + HDR_COLOR_SPACE_PROBE_INTERVAL;
        assert!(hdr_color_space_probe_due(Some(now), due));
    }
}

impl Rasterizer for GpuRenderer {
    fn cell_size(&self) -> (usize, usize) {
        GpuRenderer::cell_size(self)
    }
    fn render_input(&mut self, input: &RenderInput) -> Frame {
        let mut win = WindowGpu::new();
        GpuRenderer::render_input(self, &mut win, input, None)
    }
    fn set_cursor_blink_phase(&mut self, on: bool) {
        GpuRenderer::set_cursor_blink_phase(self, on)
    }
    fn set_cursor_style_override(&mut self, style: Option<CursorStyle>) {
        GpuRenderer::set_cursor_style_override(self, style)
    }
}

/// A window a GPU backend can attach a swapchain to.
#[cfg(target_os = "macos")]
pub trait WindowTarget: aterm_objc::HasNsView + Send + Sync + 'static {}

#[cfg(target_os = "macos")]
impl<T> WindowTarget for T where T: aterm_objc::HasNsView + Send + Sync + 'static {}

/// A window a GPU backend can attach a swapchain to.
#[cfg(target_os = "linux")]
pub trait WindowTarget:
    aterm_types::native_window::HasNativeWindow + Send + Sync + 'static
{
}

#[cfg(target_os = "linux")]
impl<T> WindowTarget for T where
    T: aterm_types::native_window::HasNativeWindow + Send + Sync + 'static
{
}

/// The frontend's own presenter for one window, which the GL arm posts frames through instead of
/// a window surface where the window system's GL stalls it (X11 on NVIDIA's GLX under a
/// compositor, where every resize of a `GLXWindow` stalls the X server and the GPU for every
/// client). Handed to [`GpuRenderer::create_window_surface_with_sink`].
#[cfg(target_os = "linux")]
pub trait FrameSink: Send {
    /// Get ready to post. `Err` says why it cannot, and the window is posted
    /// through its window surface instead.
    fn attach(&mut self) -> Result<(), String>;

    /// Post one `width` x `height` frame: size the store, wait until the window system has read
    /// the last post, have `draw` write a rectangle of the frame into the store (`0x00RRGGBB`
    /// words, rows top first, `width` words a row), and post that rectangle. `damage`
    /// (`[x0, y0, x1, y1]`, half-open; `None` for all of it) is the part that differs from the
    /// last post; the sink widens it to the whole frame wherever its store does not hold the last
    /// post (the first post, a new size) and hands `draw` the rectangle it settled on. An empty
    /// one posts nothing; `draw`'s `Err` posts nothing and is returned.
    fn post(
        &mut self,
        width: u32,
        height: u32,
        damage: Option<[u32; 4]>,
        draw: &mut FrameDraw<'_>,
    ) -> Result<(), String>;
}

/// A [`FrameSink::post`]'s draw: write the rectangle (`[x0, y0, x1, y1]`,
/// half-open) of the frame into the store's words.
#[cfg(target_os = "linux")]
pub type FrameDraw<'a> = dyn FnMut(&mut [u32], [u32; 4]) -> Result<(), String> + 'a;

/// A window a GPU backend can attach a swapchain to.
#[cfg(not(any(target_os = "macos", target_os = "linux", target_arch = "wasm32")))]
pub trait WindowTarget:
    raw_window_handle::HasWindowHandle + raw_window_handle::HasDisplayHandle + Send + Sync + 'static
{
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_arch = "wasm32")))]
impl<T> WindowTarget for T where
    T: raw_window_handle::HasWindowHandle
        + raw_window_handle::HasDisplayHandle
        + Send
        + Sync
        + 'static
{
}

#[cfg(all(target_os = "macos", wgpu_arm))]
struct AppKitSurfaceTarget<W>(W);

#[cfg(all(target_os = "macos", wgpu_arm))]
impl<W: aterm_objc::HasNsView> wgpu::rwh::HasWindowHandle for AppKitSurfaceTarget<W> {
    fn window_handle(&self) -> Result<wgpu::rwh::WindowHandle<'_>, wgpu::rwh::HandleError> {
        let view = std::ptr::NonNull::new(self.0.ns_view().as_ptr())
            .ok_or(wgpu::rwh::HandleError::Unavailable)?;
        let raw = wgpu::rwh::RawWindowHandle::AppKit(wgpu::rwh::AppKitWindowHandle::new(view));
        // SAFETY: `HasNsView` keeps the view alive for the lifetime of `self.0`.
        Ok(unsafe { wgpu::rwh::WindowHandle::borrow_raw(raw) })
    }
}

#[cfg(all(target_os = "macos", wgpu_arm))]
impl<W> wgpu::rwh::HasDisplayHandle for AppKitSurfaceTarget<W> {
    fn display_handle(&self) -> Result<wgpu::rwh::DisplayHandle<'_>, wgpu::rwh::HandleError> {
        Ok(wgpu::rwh::DisplayHandle::appkit())
    }
}

#[cfg(all(target_os = "linux", wgpu_arm))]
struct NativeSurfaceTarget<W>(W);

#[cfg(all(target_os = "linux", wgpu_arm))]
impl<W: aterm_types::native_window::HasNativeWindow> wgpu::rwh::HasWindowHandle
    for NativeSurfaceTarget<W>
{
    fn window_handle(&self) -> Result<wgpu::rwh::WindowHandle<'_>, wgpu::rwh::HandleError> {
        use aterm_types::native_window::NativeWindow;
        let raw = match self.0.native_window() {
            Some(NativeWindow::Xlib { window, visual, .. }) => {
                let mut h = wgpu::rwh::XlibWindowHandle::new(window.into());
                h.visual_id = visual.into();
                wgpu::rwh::RawWindowHandle::Xlib(h)
            }
            Some(NativeWindow::Wayland { surface, .. }) => {
                wgpu::rwh::RawWindowHandle::Wayland(wgpu::rwh::WaylandWindowHandle::new(surface))
            }
            None => return Err(wgpu::rwh::HandleError::Unavailable),
        };
        // SAFETY: `HasNativeWindow` keeps the handles alive for the lifetime of `self.0`.
        Ok(unsafe { wgpu::rwh::WindowHandle::borrow_raw(raw) })
    }
}

#[cfg(all(target_os = "linux", wgpu_arm))]
impl<W: aterm_types::native_window::HasNativeWindow> wgpu::rwh::HasDisplayHandle
    for NativeSurfaceTarget<W>
{
    fn display_handle(&self) -> Result<wgpu::rwh::DisplayHandle<'_>, wgpu::rwh::HandleError> {
        use aterm_types::native_window::NativeWindow;
        let raw = match self.0.native_window() {
            Some(NativeWindow::Xlib {
                display, screen, ..
            }) => wgpu::rwh::RawDisplayHandle::Xlib(wgpu::rwh::XlibDisplayHandle::new(
                Some(display),
                screen,
            )),
            Some(NativeWindow::Wayland { display, .. }) => {
                wgpu::rwh::RawDisplayHandle::Wayland(wgpu::rwh::WaylandDisplayHandle::new(display))
            }
            None => return Err(wgpu::rwh::HandleError::Unavailable),
        };
        // SAFETY: the display connection stays open for the lifetime of `self.0`.
        Ok(unsafe { wgpu::rwh::DisplayHandle::borrow_raw(raw) })
    }
}

#[cfg(test)]
mod device_lost_tests {

    #[cfg(target_os = "macos")]
    #[test]
    fn a_wired_metal_loss_latch_feeds_device_lost() {
        use crate::metal::loss::{CbOutcome, LossLatch};
        let gpu =
            crate::renderer::gpu_renderer_for_test(None, 18.0, aterm_render::Theme::default());
        assert!(!gpu.device_lost(), "a fresh context is healthy");
        let latch = gpu.ctx.metal_loss_latch_or_wire();
        assert!(
            !gpu.device_lost(),
            "a wired but healthy latch reports nothing"
        );
        latch.record(&CbOutcome::Lost {
            code: None,
            name: "injected PageFault stand-in",
        });
        assert!(
            gpu.device_lost(),
            "the injected Metal loss must reach the frontend's existing poll"
        );
        assert!(
            !gpu.ctx
                .wire_metal_loss_latch(std::sync::Arc::new(LossLatch::new())),
            "a second wire refuses — one loss domain per context lifetime"
        );
    }
}

#[cfg(all(target_os = "macos", test))]
mod atlas_upload_sync_tests {
    use super::{ArmAtlas, DrawAtlas, MetalArmLive};
    use crate::device_layer::TexelFormat;
    use crate::metal::ffi::{AutoreleasePool, PixelFormat, TEXTURE_USAGE_SHADER_READ};
    use crate::metal::loss::LossLatch;

    fn reader_count(live: &MetalArmLive) -> i64 {
        live.frame_slots
            .iter()
            .filter(|s| s.inflight.is_some())
            .count() as i64
    }

    fn atlas_state(
        live: &MetalArmLive,
        uploaded_h: i64,
        presented: i64,
    ) -> aterm_spec::interp::State {
        assert_eq!(super::ARM_FRAME_SLOTS, 3, "the model's MaxReaders");
        let busy = |i: usize| i64::from(live.frame_slots[i].inflight.is_some());
        [
            ("epoch", 1),
            ("tex_h", 3),
            ("occupied_h", 3),
            ("uploaded_h", uploaded_h),
            ("presented", presented),
            ("readers", reader_count(live)),
            ("upload_conflict", 0),
            ("fx_gen", 1),
            ("fx_regions", 1),
            ("fx_uploaded", 1),
            ("fx_rewrite", 0),
            ("region_race", 0),
            ("pal_conflict", 0),
            ("busy0", busy(0)),
            ("busy1", busy(1)),
            ("busy2", busy(2)),
            ("cur", ((live.cur_slot + 1) % super::ARM_FRAME_SLOTS) as i64),
        ]
        .into_iter()
        .collect()
    }

    fn fx_state(
        live: &MetalArmLive,
        fx_regions: i64,
        fx_uploaded: i64,
        presented: i64,
    ) -> aterm_spec::interp::State {
        let mut s = atlas_state(live, 3, presented);
        s.insert("fx_regions", fx_regions);
        s.insert("fx_uploaded", fx_uploaded);
        s
    }

    fn left_in_flight(mut state: aterm_spec::interp::State) -> aterm_spec::interp::State {
        state.insert("readers", 2);
        state.insert("busy0", 1);
        state.insert("busy2", 1);
        state
    }

    fn live_or_skip() -> MetalArmLive {
        match MetalArmLive::new(
            std::sync::Arc::new(LossLatch::new()),
            &crate::metal::warm::FIRST_FRAME,
        ) {
            Ok(live) => live,
            Err(error) => aterm_spec::host::could_not_run(format!("no Metal arm: {error}")),
        }
    }

    fn readers_in(
        live: &mut MetalArmLive,
        tex: &crate::metal::resources::SealedTexture,
        slots: &[usize],
    ) {
        readers_in_as(live, tex, PixelFormat::R8Unorm, slots);
    }

    fn readers_in_as(
        live: &mut MetalArmLive,
        tex: &crate::metal::resources::SealedTexture,
        format: PixelFormat,
        slots: &[usize],
    ) {
        let dest = live
            .mint
            .texture_2d(format, tex.width(), tex.height(), TEXTURE_USAGE_SHADER_READ)
            .expect("reader destination");
        for &index in slots {
            let mut cb = live.session.begin().expect("reader command buffer");
            cb.copy_texture_sub_rect(tex, (0, 0), &dest, (0, 0), 1, 1)
                .expect("reader");
            live.frame_slots[index].inflight = Some(cb.commit());
        }
    }

    fn two_readers(live: &mut MetalArmLive, tex: &crate::metal::resources::SealedTexture) {
        readers_in(live, tex, &[0, 2]);
        assert_eq!(reader_count(live), 2);
    }

    fn index_art(w: u16, h: u16, fill: u8) -> std::sync::Arc<aterm_core::render::FxArt> {
        std::sync::Arc::new(
            aterm_core::render::FxArt::new(
                w,
                h,
                aterm_core::render::FxTexels::Index8,
                vec![fill; usize::from(w) * usize::from(h)],
            )
            .expect("sized"),
        )
    }

    /// An fx region rewritten after an LRU eviction first settles both frames in flight that
    /// sample it; the texture is kept, only the new tile's texels upload, and the region reads back
    /// as the new tile.
    #[test]
    fn an_fx_region_rewrite_settles_every_frame_reader() {
        let _pool = AutoreleasePool::new();
        let mut live = live_or_skip();
        let mut fx = super::fx_gpu::FxGpu::default();
        let tile = |rgb: [u8; 3]| {
            let texel = [rgb[0], rgb[1], rgb[2], 0xff];
            std::sync::Arc::new(
                aterm_core::render::FxArt::new(
                    512,
                    512,
                    aterm_core::render::FxTexels::Rgba8Straight,
                    texel.repeat(512 * 512),
                )
                .expect("sized"),
            )
        };
        let cap = super::fx_gpu::FX_ATLAS_MIN;
        fx.admit(&[], &[tile([0x11, 0x22, 0x33])], cap);
        live.sync_fx(&fx.atlas, false).expect("first mint");
        let rgba = super::fx_gpu::FxAtlasKind::Rgba as usize;
        let tex_id = live.fx_tex[rgba].as_ref().unwrap().tex.metal().obj().id();
        let tex = live.fx_tex[rgba]
            .as_ref()
            .unwrap()
            .tex
            .metal()
            .clone_handle();
        readers_in_as(&mut live, &tex, PixelFormat::Rgba8Unorm, &[0, 2]);
        assert_eq!(reader_count(&live), 2);
        fx.admit(&[], &[tile([0x44, 0x55, 0x66])], cap);
        let side = fx.atlas.side(super::fx_gpu::FxAtlasKind::Rgba);
        assert_eq!(
            (side.generation(), side.evictions(), side.len()),
            (1, 1, 1),
            "the new tile evicts the old one into the same generation"
        );
        let mut before = fx_state(&live, 1, 1, 0);
        before.insert("fx_rewrite", 1);
        let bytes = live.sync_fx(&fx.atlas, false).expect("rewrite");
        assert_eq!(bytes, 512 * 512 * 4, "only the new tile's texels upload");
        assert_eq!(reader_count(&live), 0, "the rewrite settled both readers");
        assert_eq!(live.fx_hazard.settles, 1);
        assert_eq!(
            live.fx_reader_wait.0, 2,
            "the frame shape's reader waits count both readers"
        );
        let kept = live.fx_tex[rgba].as_ref().unwrap();
        assert_eq!(
            kept.tex.metal().obj().id(),
            tex_id,
            "a rewrite keeps the texture"
        );
        let frame = crate::device_layer::metal_try_read_back(
            &live.session,
            &live.mint,
            kept.tex.metal(),
            512,
            512,
        )
        .expect("rewritten atlas readback");
        assert!(
            frame.pixels.iter().all(|&p| p == 0x0044_5566),
            "the new tile"
        );
        let after = fx_state(&live, 1, 1, 1);
        let model = aterm_models_render::metal_atlas_residency_model();
        assert_eq!(
            aterm_spec::interp::admits(&model, &before, &after),
            Some("SyncAndPresent")
        );
        assert_eq!(
            aterm_spec::interp::admits(&model, &before, &left_in_flight(after.clone())),
            None,
            "negative control: a rewrite under readers in flight is refused"
        );
        let mut raced = after;
        raced.insert("region_race", 1);
        assert_eq!(
            aterm_spec::interp::admits(&model, &before, &raced),
            None,
            "negative control: the region race is refused"
        );
    }

    #[test]
    fn an_fx_append_settles_every_frame_reader() {
        let _pool = AutoreleasePool::new();
        let mut live = live_or_skip();
        let mut fx = super::fx_gpu::FxGpu::default();
        let first = vec![index_art(5, 3, 1)];
        fx.admit(&[], &first, super::fx_gpu::FX_ATLAS_CAP);
        live.sync_fx(&fx.atlas, false).expect("first mint");
        let r8 = super::fx_gpu::FxAtlasKind::R8 as usize;
        let tex_id = live.fx_tex[r8].as_ref().unwrap().tex.metal().obj().id();
        assert_eq!(live.fx_hazard.settles, 0, "a fresh texture has no reader");
        let tex = live.fx_tex[r8].as_ref().unwrap().tex.metal().clone_handle();
        two_readers(&mut live, &tex);
        assert_eq!(live.sync_fx(&fx.atlas, false).expect("unchanged"), 0);
        assert_eq!(reader_count(&live), 2);
        let second = vec![index_art(5, 3, 1), index_art(7, 2, 1)];
        fx.admit(&[], &second, super::fx_gpu::FX_ATLAS_CAP);
        let before = fx_state(&live, 2, 1, 0);
        let bytes = live.sync_fx(&fx.atlas, false).expect("resident append");
        assert_eq!(bytes, 14, "only the appended art's texels upload");
        assert_eq!(reader_count(&live), 0, "the append settled both readers");
        assert_eq!(live.fx_hazard.settles, 1);
        assert_eq!(
            live.fx_tex[r8].as_ref().unwrap().tex.metal().obj().id(),
            tex_id,
            "an append keeps the texture"
        );
        let after = fx_state(&live, 2, 2, 1);
        let model = aterm_models_render::metal_atlas_residency_model();
        assert_eq!(
            aterm_spec::interp::admits(&model, &before, &after),
            Some("SyncAndPresent")
        );
        let unsettled = left_in_flight(after.clone());
        assert_eq!(
            aterm_spec::interp::admits(&model, &before, &unsettled),
            None,
            "negative control: an append under readers in flight is refused"
        );
        let mut raced = after;
        raced.insert("region_race", 1);
        assert_eq!(
            aterm_spec::interp::admits(&model, &before, &raced),
            None,
            "negative control: the region race is refused"
        );
    }

    #[test]
    fn a_frame_palette_never_lands_under_a_reader() {
        let _pool = AutoreleasePool::new();
        let mut live = live_or_skip();
        let mut fx = super::fx_gpu::FxGpu::default();
        fx.admit(&[], &[index_art(2, 2, 1)], super::fx_gpu::FX_ATLAS_CAP);
        live.sync_fx(&fx.atlas, false).expect("mint");
        let tex = live.fx_tex[0].as_ref().unwrap().tex.metal().clone_handle();
        two_readers(&mut live, &tex);
        live.cur_slot = 0;
        let before = atlas_state(&live, 3, 1);
        live.cur_slot = 1;
        let words = [0x0102_0304u32, 0x0506_0708];
        let buf = live.upload_fx_palette(&words).expect("staged");
        assert_eq!(
            reader_count(&live),
            2,
            "staging the palette settles nothing"
        );
        assert_eq!(live.fx_hazard.settles, 0);
        assert_eq!(
            buf.id(),
            live.frame_slots[1]
                .fx_palette
                .as_ref()
                .unwrap()
                .buf
                .metal()
                .id(),
            "the palette is slot 1's own stream"
        );
        assert!(
            live.frame_slots[0].fx_palette.is_none() && live.frame_slots[2].fx_palette.is_none(),
            "no other slot's palette is touched"
        );
        readers_in(&mut live, &tex, &[1]);
        let after = atlas_state(&live, 3, 1);
        assert_eq!(after["readers"], 3);
        let model = aterm_models_render::metal_atlas_residency_model();
        assert_eq!(
            aterm_spec::interp::admits(&model, &before, &after),
            Some("SubmitFrame")
        );
        let mut conflict = after.clone();
        conflict.insert("pal_conflict", 1);
        assert_eq!(
            aterm_spec::interp::admits(&model, &before, &conflict),
            None,
            "negative control: a palette written under a reader is refused"
        );
        let mut taken = before.clone();
        taken.insert("busy1", 1);
        taken.insert("busy2", 0);
        let mut staged = taken.clone();
        staged.insert("readers", 3);
        staged.insert("cur", 2);
        staged.insert("pal_conflict", 1);
        assert_eq!(
            aterm_spec::interp::admits(&aterm_spec::interp::with_buggy(&model, 4), &taken, &staged),
            Some("SubmitFrame"),
            "the mutant stages into the busy slot"
        );
        for pal_conflict in [0, 1] {
            staged.insert("pal_conflict", pal_conflict);
            assert_eq!(
                aterm_spec::interp::admits(&model, &taken, &staged),
                None,
                "negative control: no frame stages into a slot in flight"
            );
        }
        live.write_shared_palette(&words).expect("mint");
        readers_in(&mut live, &tex, &[0, 2]);
        assert_eq!(reader_count(&live), 3);
        live.write_shared_palette(&words).expect("rewrite");
        assert_eq!(
            reader_count(&live),
            0,
            "a shared palette settles every reader"
        );
        assert_eq!(live.fx_hazard.settles, 1);
    }

    #[test]
    fn an_in_place_atlas_upload_settles_every_frame_reader() {
        let _pool = AutoreleasePool::new();
        let mut live = match MetalArmLive::new(
            std::sync::Arc::new(LossLatch::new()),
            &crate::metal::warm::FIRST_FRAME,
        ) {
            Ok(live) => live,
            Err(error) => {
                aterm_spec::host::could_not_run(format!("no Metal arm: {error}"));
            }
        };
        let mut atlas: Option<ArmAtlas> = None;
        let key = (4, 3, 1);
        let first = [0x11, 0x22, 0x33, 0xff].repeat(8);
        MetalArmLive::ensure_atlas(
            crate::device_layer::DeviceHandle::Metal(&live.mint),
            &mut atlas,
            DrawAtlas::Color,
            key,
            0,
            0,
            TexelFormat::Rgba8Unorm,
            &first,
            &mut live.frame_slots,
        )
        .expect("initial atlas upload");
        let texture_id = atlas.as_ref().unwrap().tex.metal().obj().id();

        let dest = live
            .mint
            .texture_2d(PixelFormat::Rgba8Unorm, 4, 3, TEXTURE_USAGE_SHADER_READ)
            .expect("reader destination");
        for index in [0, 2] {
            let mut cb = live.session.begin().expect("reader command buffer");
            cb.copy_texture_sub_rect(
                atlas.as_ref().unwrap().tex.metal(),
                (0, 0),
                &dest,
                (0, 0),
                4,
                2,
            )
            .expect("atlas reader");
            live.frame_slots[index].inflight = Some(cb.commit());
        }
        assert_eq!(reader_count(&live), 2);

        assert_eq!(
            MetalArmLive::ensure_atlas(
                crate::device_layer::DeviceHandle::Metal(&live.mint),
                &mut atlas,
                DrawAtlas::Color,
                key,
                0,
                0,
                TexelFormat::Rgba8Unorm,
                &first,
                &mut live.frame_slots,
            )
            .expect("unchanged atlas"),
            (0, 0)
        );
        assert_eq!(reader_count(&live), 2);

        let mut appended = first;
        appended.extend_from_slice(&[0x44, 0x55, 0x66, 0xff].repeat(4));
        let before = atlas_state(&live, 2, 0);
        let (consumed, _) = MetalArmLive::ensure_atlas(
            crate::device_layer::DeviceHandle::Metal(&live.mint),
            &mut atlas,
            DrawAtlas::Color,
            key,
            1,
            0,
            TexelFormat::Rgba8Unorm,
            &appended,
            &mut live.frame_slots,
        )
        .expect("resident append");
        assert_eq!(consumed, 2);
        let resident = atlas.as_ref().unwrap();
        assert_eq!(
            resident.tex.metal().obj().id(),
            texture_id,
            "append keeps the texture"
        );
        assert_eq!(resident.write_gen, 1);
        let after = atlas_state(&live, 3, 1);
        let model = aterm_models_render::metal_atlas_residency_model();
        assert_eq!(
            aterm_spec::interp::admits(&model, &before, &after),
            Some("SyncAndPresent")
        );
        let overlapping = left_in_flight(after);
        assert_eq!(
            aterm_spec::interp::admits(&model, &before, &overlapping),
            None,
            "negative control: uploading while another frame owns the atlas is rejected"
        );

        let frame = crate::device_layer::metal_try_read_back(
            &live.session,
            &live.mint,
            resident.tex.metal(),
            4,
            3,
        )
        .expect("uploaded atlas readback");
        assert_eq!(&frame.pixels[..8], &[0x112233; 8]);
        assert_eq!(&frame.pixels[8..], &[0x445566; 4]);
    }
}

#[cfg(all(test, wgpu_arm))]
mod fx_wgsl_tests {
    use super::*;

    fn validation_error(device: &wgpu::Device, f: impl FnOnce()) -> Option<String> {
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        f();
        crate::block_on::block_on(scope.pop()).map(|e| e.to_string())
    }

    #[test]
    fn the_wgsl_twin_validates_with_its_fx_entry_points() {
        let mut gpu = gpu_renderer_for_test(None, 18.0, Theme::default());
        let device = gpu.ctx.device.clone();
        let module = |src: &str| {
            validation_error(&device, || {
                drop(device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("fx wgsl validation"),
                    source: wgpu::ShaderSource::Wgsl(src.into()),
                }));
            })
        };
        assert_eq!(module(SHADER), None, "the cell SHADER validates");
        gpu.effects.ensure(&device, EffectPipeline::Fx);
        gpu.effects.ensure(&device, EffectPipeline::FxLinear);
        assert_eq!(
            gpu.effects.fx_layouts(&device).err(),
            None,
            "the fx pipelines build without a validation error"
        );
        let resident = gpu.effects.resident();
        assert!(
            resident[EffectPipeline::Fx as usize] && resident[EffectPipeline::FxLinear as usize],
            "both fx pipelines are resident"
        );
        let broken = SHADER.replace("textureLoad(fx_r8, t, 0)", "textureLoad(fx_r8, t)");
        assert_ne!(broken, SHADER, "the control edits the fx read");
        assert!(module(&broken).is_some(), "the control is refused");
    }

    #[test]
    fn a_refused_fx_row_fails_fx_closed() {
        use aterm_core::render::{
            FxBlend, FxContent, FxInk, FxItem, FxLayer, FxRect, FxSlot, FxSpace,
        };
        let mut input = aterm_core::terminal::Terminal::new(4, 16).cell_frame(4, 16);
        input.fx_layers = vec![FxLayer::new(
            1,
            Arc::new(FxContent::new(
                vec![FxItem::Rect(FxRect {
                    x: 4,
                    y: 4,
                    w: 40,
                    h: 12,
                    ink: 0,
                })],
                vec![FxInk::add(0x0030_2010); 2],
            )),
            FxSlot::Over,
            FxSpace::Window,
            FxBlend::Exact,
        )];
        let frames = |shader: Option<String>| {
            let mut gpu = gpu_renderer_for_test(None, 18.0, Theme::default());
            #[cfg(target_os = "macos")]
            gpu.disarm_metal_for_test();
            #[cfg(gl_arm)]
            gpu.disarm_gl_for_oracle();
            let device = gpu.ctx.device.clone();
            if let Some(src) = shader {
                gpu.effects.shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("fx refusal"),
                    source: wgpu::ShaderSource::Wgsl(src.into()),
                });
            }
            let mut win = WindowGpu::new();
            let drawn: Vec<(bool, bool)> = (0..2)
                .map(|frame| {
                    gpu.try_render_input(&mut win, &input, None)
                        .unwrap_or_else(|e| panic!("frame {frame}: {e}"));
                    (
                        gpu.inst.fx_instances() > 0,
                        gpu.frame_plan.iter().any(|i| i.stream.is_fx()),
                    )
                })
                .collect();
            (gpu, device, drawn)
        };
        let (_, _, drawn) = frames(None);
        assert_eq!(
            drawn,
            [(true, true), (true, true)],
            "the control slices and plans its fx stream"
        );

        let renamed = SHADER.replace("fn fs_fx(", "fn fs_fx_renamed(");
        assert_ne!(renamed, SHADER, "the refusal renames the Exact entry point");
        let (mut gpu, device, drawn) = frames(Some(renamed));
        assert_eq!(
            drawn,
            [(true, false), (false, false)],
            "the first frame slices but plans no fx stream; the second slices none"
        );
        let why = gpu
            .effects
            .fx_layouts(&device)
            .err()
            .expect("the refusal closes fx")
            .to_owned();
        assert!(why.contains("refused the fx pipeline"), "{why}");
        gpu.effects.ensure(&device, EffectPipeline::FxLinear);
        let resident = gpu.effects.resident();
        assert!(
            !resident[EffectPipeline::Fx as usize] && !resident[EffectPipeline::FxLinear as usize],
            "neither fx row is resident"
        );
    }
}

#[cfg(all(test, target_os = "macos"))]
mod fx_hazard_tests {
    use super::*;
    use aterm_core::terminal::Terminal;

    fn episode(shared: bool) -> ((u64, u64, u64), Vec<usize>) {
        let mut gpu = gpu_renderer_for_test(None, 18.0, Theme::default());
        gpu.arm_metal_for_test();
        gpu.set_fx_palette_shared_for_test(shared);
        let mut win = WindowGpu::new();
        let (rows, cols) = (8usize, 24usize);
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process(b"\x1b[?25l");
        let art = Arc::new(
            aterm_core::render::FxArt::new(
                4,
                2,
                aterm_core::render::FxTexels::Index8,
                vec![1u8, 2, 1, 2, 2, 1, 2, 1],
            )
            .expect("sized"),
        );
        let mut before = None;
        let mut bound = Vec::new();
        for n in 0..186u32 {
            let palette = vec![
                aterm_core::render::FxInk::add(0x0010_0000 | (n & 0xff) << 8),
                aterm_core::render::FxInk::add(0x0000_2000 | (n * 3) & 0xff),
            ];
            let mut input = term.cell_frame(rows, cols);
            input.fx_layers = vec![aterm_core::render::FxLayer::new(
                1,
                Arc::new(aterm_core::render::FxContent::new(
                    vec![
                        aterm_core::render::FxItem::Sprite(aterm_core::render::FxSprite {
                            x: 8,
                            y: 8,
                            dw: 40,
                            dh: 20,
                            flip_x: false,
                            art: Arc::clone(&art),
                            ink: 0,
                            clip: aterm_core::render::PxRect::EVERYWHERE,
                        }),
                        aterm_core::render::FxItem::Rect(aterm_core::render::FxRect {
                            x: 60,
                            y: 30,
                            w: 90,
                            h: 50,
                            ink: 0,
                        }),
                    ],
                    palette,
                )),
                aterm_core::render::FxSlot::Glow,
                aterm_core::render::FxSpace::Window,
                aterm_core::render::FxBlend::Exact,
            )];
            gpu.encode_present_frame(&mut win, &input);
            assert!(gpu.last_frame_arm_metal, "frame {n} encoded on Metal");
            assert!(gpu.frame_shape().fx_instances > 0, "frame {n} draws fx");
            bound.push(gpu.metal_fx_hazard().palette);
            if n == 0 {
                before = Some(gpu.metal_fx_hazard());
            }
        }
        let (a, b) = (before.expect("frame 0 ran"), gpu.metal_fx_hazard());
        (
            (
                b.settles - a.settles,
                b.settle_ns - a.settle_ns,
                b.park_ns - a.park_ns,
            ),
            bound,
        )
    }

    fn assert_ring_palettes(bound: &[usize], what: &str) {
        assert!(
            bound.iter().all(|&p| p != 0),
            "{what}: every frame bound one"
        );
        for (n, &p) in bound.iter().enumerate() {
            for back in 1..ARM_FRAME_SLOTS.min(n + 1) {
                assert_ne!(
                    p,
                    bound[n - back],
                    "{what}: frame {n} binds frame {}'s palette buffer",
                    n - back
                );
            }
        }
    }

    #[test]
    fn animating_fx_frames_settle_no_reader() {
        if crate::metal::ffi::Device::preferred().is_none() {
            aterm_spec::host::could_not_run("no Metal device");
        }
        let ((settles, settle_ns, park_ns), bound) = episode(false);
        let ((c_settles, c_settle_ns, c_park_ns), c_bound) = episode(true);
        assert_ring_palettes(&bound, "the ring");
        assert_ring_palettes(&c_bound, "the control's ring");
        eprintln!(
            "fx palette over 185 pipelined frames: ring {settles} settles, {settle_ns} ns \
             settled, {park_ns} ns parked in all; shared texture {c_settles} settles, \
             {c_settle_ns} ns settled, {c_park_ns} ns parked in all"
        );
        assert_eq!(settles, 0, "the per-slot palette settles no reader");
        assert_eq!(
            c_settles, 185,
            "the control: a shared palette texture settles its readers every frame"
        );
    }
}

#[cfg(test)]
mod acquire_wait_failure_tests {
    use super::{SurfacePresentFailure, WindowGpu};

    #[test]
    fn a_refused_acquire_is_measured_and_published_once() {
        let mut window = WindowGpu::new();
        window.begin_surface_present();
        let mut invoked = false;
        let result = window.measure_surface_acquire(|| {
            invoked = true;
            Err::<(), _>(SurfacePresentFailure::Timeout)
        });
        assert!(invoked);
        assert!(matches!(result, Err(SurfacePresentFailure::Timeout)));
        assert_eq!(
            window.take_acquire_wait_sample_ns(),
            Some(window.last_acquire_wait_ns())
        );
        assert_eq!(
            window.take_acquire_wait_sample_ns(),
            None,
            "no duplicate publication"
        );

        window.begin_surface_present();
        assert_eq!(window.take_acquire_wait_sample_ns(), None);
        assert!(matches!(
            window.measure_surface_acquire(|| Ok::<_, SurfacePresentFailure>(7)),
            Ok(7)
        ));
        assert!(window.take_acquire_wait_sample_ns().is_some());
    }

    #[test]
    fn a_new_attempt_discards_an_unconsumed_old_sample() {
        let mut window = WindowGpu::new();
        window.begin_surface_present();
        window.measure_surface_acquire(|| ());
        window.begin_surface_present();
        assert_eq!(window.take_acquire_wait_sample_ns(), None);
    }
}

#[cfg(all(target_os = "macos", test))]
mod gpu_park_accounting_tests {
    use super::WindowGpu;

    #[test]
    fn a_gpu_park_accumulates_per_attempt_and_publishes_exactly_once() {
        let mut window = WindowGpu::new();
        window.begin_surface_present();
        assert_eq!(
            window.take_gpu_park_sample_ns(),
            None,
            "an attempt that never parked must publish no sample"
        );

        window.begin_surface_present();
        window.add_gpu_park(3_000_000);
        window.add_gpu_park(7_000_000);
        assert_eq!(window.last_gpu_park_ns(), 10_000_000);
        assert_eq!(window.take_gpu_park_sample_ns(), Some(10_000_000));
        assert_eq!(
            window.take_gpu_park_sample_ns(),
            None,
            "no duplicate publication"
        );

        window.begin_surface_present();
        assert_eq!(window.take_gpu_park_sample_ns(), None);
        window.add_gpu_park(250_000);
        assert_eq!(window.take_gpu_park_sample_ns(), Some(250_000));
    }
}

#[cfg(all(test, wgpu_arm))]
mod surface_format_tests {
    use super::GpuRenderer;
    use wgpu::TextureFormat::{Bgra8Unorm, Bgra8UnormSrgb, Rgb10a2Unorm, Rgba8UnormSrgb};

    fn caps(formats: &[wgpu::TextureFormat]) -> wgpu::SurfaceCapabilities {
        wgpu::SurfaceCapabilities {
            formats: formats.to_vec(),
            ..Default::default()
        }
    }

    #[test]
    fn no_formats_is_an_adapter_that_cannot_present() {
        let none = GpuRenderer::pick_surface_format(&caps(&[])).expect_err("refused");
        assert!(none.contains("cannot present"), "{none}");
        let srgb_only = GpuRenderer::pick_surface_format(&caps(&[Bgra8UnormSrgb, Rgba8UnormSrgb]))
            .expect_err("refused");
        assert_ne!(
            none, srgb_only,
            "an sRGB-only surface is a different refusal"
        );
        assert_eq!(
            GpuRenderer::pick_surface_format(&caps(&[Bgra8UnormSrgb, Rgb10a2Unorm, Bgra8Unorm])),
            Ok(Bgra8Unorm)
        );
    }
}

#[cfg(all(test, any(wgpu_arm, target_os = "macos")))]
#[path = "window_atlas_tests.rs"]
mod window_atlas_tests;

#[cfg(gl_arm)]
#[path = "gl_arm.rs"]
mod gl_arm;
#[cfg(gl_arm)]
#[path = "gl_present.rs"]
mod gl_present;

#[cfg(all(gl_arm, wgpu_arm, test))]
#[path = "gl_replay_tests.rs"]
mod gl_replay_tests;

#[path = "fx_gpu.rs"]
pub(crate) mod fx_gpu;
