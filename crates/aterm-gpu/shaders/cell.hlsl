// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// HLSL (Shader Model 5.0) twin of renderer.rs SHADER, shaders/cell.metal and
// shaders/cell.glsl, for the D3D11 arm (src/d3d11,
// docs/AUDIT-third-party-2026-10-15.md GW.5). Every function is the WGSL's op
// for op, in the order naga 29's HLSL writer emits it for wgpu's DX12
// backend: the D3D11 arm is held byte-identical to wgpu-dx12 on the same
// adapter (src/d3d11_replay_tests.rs, oracle/warp-frames.tsv), so float
// expressions keep the WGSL's order and constants (naga folds `1.0 / 2.4` to
// 0.41666666, written out here) and the conversions and divisions keep
// naga's guards (`f2u`, `f2i`, `div_u`, `div_i`: WGSL defines a float to
// integer conversion as saturating and a division by zero as the dividend).
// Integer arithmetic wraps on both sides, so naga's `asint(asuint(...))`
// spellings are written as plain arithmetic.
//
// ONE FILE, MANY ENTRY POINTS: src/d3d11 compiles each row's vertex and pixel
// entry point from this file by name (D3DCompile's entry argument). Each
// vertex entry point's output struct is its pixel entry points' input, so the
// two signatures match by construction.
//
// BINDINGS (the cell rows' BindSpec slots): the `Uniforms` buffer is b0 (bound
// to both stages; fs_glyph reads text_blend), the atlas t0 with its sampler
// s0. The fx rows read the Index8 atlas at t0, the RGBA atlas at t1 and the
// frame palette at t2, an RGBA8 texture 1024 entries wide (entry j at
// (j % 1024, j / 1024)), as the GL arm's palette ring holds it. The glow
// items row's vertex stage reads the bloom's item list at its own t0
// (BindSpec::GLOW_ITEMS), a texture of the same shape.

cbuffer Uniforms : register(b0) {
    float2 u_screen;
    // != 0.0 => fs_glyph applies the W2 linear-corrected coverage remap.
    float u_text_blend;
    float u_pad;
};

Texture2D<float4> atlas_tex : register(t0);
SamplerState atlas_samp : register(s0);
Texture2D<float4> fx_rgba : register(t1);
Texture2D<float4> fx_pal : register(t2);

// naga's guards: WGSL's saturating float->integer conversions (the bounds
// are the largest floats below 2^32 and inside i32) and its division by zero,
// which yields the dividend (and INT_MIN / -1, which yields INT_MIN).
uint f2u(float v) {
    return uint(clamp(v, 0.0, 4294967000.0));
}
uint4 f2u4(float4 v) {
    return uint4(clamp(v, 0.0, 4294967000.0));
}
int f2i(float v) {
    return int(clamp(v, -2147483600.0, 2147483500.0));
}
int2 f2i2(float2 v) {
    return int2(clamp(v, -2147483600.0, 2147483500.0));
}
uint div_u(uint a, uint b) {
    return a / (b == 0u ? 1u : b);
}
uint4 div_u4(uint4 a, uint4 b) {
    return a / (b == 0u ? 1u : b);
}
int div_i(int a, int b) {
    return a / (((a == int(-2147483647 - 1) & b == -1) | (b == 0)) ? 1 : b);
}
int abs_i(int v) {
    return v >= 0 ? v : -v;
}

// Unit quad corner for vertex index 0..6 (two CCW triangles).
float2 corner(uint vi) {
    float2 c[6] = {
        float2(0.0, 0.0), float2(1.0, 0.0), float2(0.0, 1.0),
        float2(1.0, 0.0), float2(1.0, 1.0), float2(0.0, 1.0)
    };
    return c[min(vi, 5u)];
}

// Pixel coords (top-left origin, y down) -> clip space (y up).
float2 to_ndc(float2 px) {
    return float2(((2.0 * px.x) / u_screen.x) - 1.0, 1.0 - ((2.0 * px.y) / u_screen.y));
}

// sRGB-encoded channel -> linear-light (the proper piecewise curve). Alpha and
// coverage are not sRGB and pass through.
float3 s2l(float3 c) {
    float3 lo = c / (12.92).xxx;
    float3 hi = pow((c + (0.055).xxx) / (1.055).xxx, (2.4).xxx);
    return (c > (0.04045).xxx) ? hi : lo;
}

// Scalar sRGB decode/encode for the W2 corrected-alpha remap: the CPU
// srgb_to_linear / linear_to_srgb curves and thresholds.
float s2l_s(float c) {
    if (c <= 0.04045) {
        return c / 12.92;
    }
    return pow((c + 0.055) / 1.055, 2.4);
}
float l2s_s(float l) {
    if (l <= 0.0031308) {
        return 12.92 * l;
    }
    return (1.055 * pow(l, 0.41666666)) - 0.055;
}

// ---------------------------------------------------------------- BG
// BG_LAYOUT: LOC0 => Uint16x4 (rect), LOC1 => Unorm8x4 (colour).
struct BgVsOut {
    float4 pos : SV_Position;
    float4 color : LOC0;
};

BgVsOut vs_bg(uint vi : SV_VertexID, uint4 rect_u : LOC0, float4 color : LOC1) {
    BgVsOut o;
    // Integer pixel coords -> exact f32 (no loss).
    float4 rect = float4(rect_u);
    float2 k = corner(vi);
    float2 px = rect.xy + (k * rect.zw);
    o.pos = float4(to_ndc(px), 0.0, 1.0);
    o.color = color;
    return o;
}

float4 fs_bg(BgVsOut i) : SV_Target0 {
    return float4(s2l(i.color.xyz), i.color.w);
}

// ---------------------------------------------------------------- GLOW
// GLOW_LAYOUT: LOC0 => Uint16x4 (rect), LOC1 => Uint8x4 (left colour), LOC2
// => Uint8x4 (right colour), both raw integer bytes. The pixel stage ramps
// the pair per column with aterm_render::glow_lerp's integer law and emits
// c / 255 raw. `lx` is the fragment's x inside the quad, interpolated
// linearly (the WGSL's `@interpolate(linear)`).
struct GlowVsOut {
    float4 pos : SV_Position;
    nointerpolation float2 span : LOC0;
    nointerpolation uint4 c0 : LOC1;
    nointerpolation uint4 c1 : LOC2;
    noperspective float lx : LOC3;
};

GlowVsOut vs_glow(uint vi : SV_VertexID, uint4 rect_u : LOC0, uint4 c0 : LOC1, uint4 c1 : LOC2) {
    GlowVsOut o;
    float4 rect = float4(rect_u);
    float2 k = corner(vi);
    float2 px = rect.xy + (k * rect.zw);
    o.pos = float4(to_ndc(px), 0.0, 1.0);
    o.span = rect.xz;
    o.lx = k.x * rect.z;
    o.c0 = c0;
    o.c1 = c1;
    return o;
}

// The fragment's 0-based column inside its quad (see the WGSL twin).
uint glow_column(float lx, float w) {
    return f2u(clamp(floor(lx), 0.0, max(w - 1.0, 0.0)));
}

// aterm_render::glow_lerp, per channel and for alpha alike, at column i of a
// w-wide quad (w >= 1).
uint4 glow_lerp4(uint4 c0, uint4 c1, uint i, uint w) {
    uint m = (2u * min(i, w - 1u)) + 1u;
    return div_u4(((c0 * ((2u * w) - m)) + (c1 * m)) + (w).xxxx, (2u * w).xxxx);
}

float4 fs_glow(GlowVsOut i) : SV_Target0 {
    uint w = max(f2u(i.span.y), 1u);
    uint4 c = glow_lerp4(i.c0, i.c1, glow_column(i.lx, i.span.y), w);
    return float4(c) / (255.0).xxxx;
}

// ---------------------------------------------------------------- GLOW ITEMS
// The bloom's lights as an item list (aterm_render::bloom::BloomItems, whose
// `light` is this reader's twin; shaders/cell.metal's GLOW ITEMS spells out
// the layout), read by instance id, the WGSL `glow_item` op for op. The list
// is cell.glsl's RGBA8 texture 1024 words wide (word j at (j % 1024,
// j / 1024), its bytes little-endian, recovered with round), read at the
// vertex stage's t0 (BindSpec::GLOW_ITEMS); the pixel stage's t0 is the
// atlas, a slot of another stage.
Texture2D<float4> glow_items_tex : register(t0);

uint glow_item_word(uint j) {
    uint4 b = f2u4(round(glow_items_tex.Load(int3(int(j % 1024u), int(j / 1024u), 0)) * 255.0));
    return ((b.x | (b.y << 8u)) | (b.z << 16u)) | (b.w << 24u);
}

struct GlowItem {
    uint4 rect;
    uint4 c0;
    uint4 c1;
};

// An end as bytes (r, g, b, a), its colour times num / den, rounded.
uint4 glow_item_end(uint e, uint num, uint den) {
    uint3 c = (((uint3(e >> 16u, e >> 8u, e) & (255u).xxx) * num) + (den / 2u).xxx)
        / (den == 0u ? 1u : den);
    return uint4(c, e >> 24u);
}

GlowItem glow_item(uint i, bool widen) {
    uint lo = 0u;
    uint hi = glow_item_word(0u) - 1u;
    [loop]
    while (lo < hi) {
        uint mid = ((lo + hi) + 1u) >> 1u;
        if (glow_item_word(4u + mid) <= i) {
            lo = mid;
        } else {
            hi = mid - 1u;
        }
    }
    uint s = i - glow_item_word(4u + lo);
    uint r = glow_item_word(1u) + (4u * lo);
    uint xy = glow_item_word(r);
    uint join = glow_item_word(r + 2u) & 65535u;
    uint joined = glow_item_word(r + 2u) >> 16u;
    uint e = (glow_item_word(2u) + glow_item_word(r + 3u)) + (2u * s);
    uint e0 = glow_item_word(e);
    uint e1 = glow_item_word(e + 1u);
    uint qw = glow_item_word(r + 1u) & 65535u;
    uint qh = (s == join) ? (joined + 1u) : 1u;
    uint num = 1u;
    uint den = 1u;
    if (widen) {
        if (qw > 0u && qw < 2u) {
            num = num * qw;
            den = den * 2u;
            qw = 2u;
        }
        if (qh < 2u) {
            num = num * qh;
            den = den * 2u;
            qh = 2u;
        }
    }
    GlowItem it;
    it.rect = ((e0 | e1) == 0u)
        ? (0u).xxxx
        : uint4(xy & 65535u, ((xy >> 16u) + s) + ((s > join) ? joined : 0u), qw, qh);
    it.c0 = glow_item_end(e0, num, den);
    it.c1 = glow_item_end(e1, num, den);
    return it;
}

// vs_glow over the item list, widened: the bloom extract's lights.
GlowVsOut vs_glow_items(uint vi : SV_VertexID, uint ii : SV_InstanceID) {
    GlowItem it = glow_item(ii, true);
    GlowVsOut o;
    float4 rect = float4(it.rect);
    float2 k = corner(vi);
    float2 px = rect.xy + (k * rect.zw);
    o.pos = float4(to_ndc(px), 0.0, 1.0);
    o.span = rect.xz;
    o.lx = k.x * rect.z;
    o.c0 = it.c0;
    o.c1 = it.c1;
    return o;
}

// ---------------------------------------------------------------- RAIN HALO
// RAIN_GLOW_LAYOUT: LOC0 => Uint16x4, LOC1 => Unorm8x4, LOC2 => Uint16x4.
struct RainVsOut {
    float4 pos : SV_Position;
    float4 color : LOC0;
    // cx, cy, rx, ry (window px)
    nointerpolation uint4 fall : LOC1;
};

RainVsOut vs_rain_glow(uint vi : SV_VertexID, uint4 rect_u : LOC0, float4 color : LOC1,
                       uint4 fall_u : LOC2) {
    RainVsOut o;
    float4 rect = float4(rect_u);
    float2 k = corner(vi);
    float2 px = rect.xy + (k * rect.zw);
    o.pos = float4(to_ndc(px), 0.0, 1.0);
    o.color = color;
    o.fall = fall_u;
    return o;
}

// The elliptical integer falloff weight shared by the Add and Over halos: pure
// int math, byte-for-byte the CPU draw_rain_add. The fragment position is the
// pixel centre (px + 0.5); floor gives the integer pixel index the CPU walks.
int rain_weight(float4 pos, uint4 fall) {
    int px = f2i(floor(pos.x));
    int py = f2i(floor(pos.y));
    int cx = int(fall.x);
    int cy = int(fall.y);
    int rx = int(fall.z);
    int ry = int(fall.w);
    int dx = px - cx;
    int dy = py - cy;
    int nsq = div_i((dx * dx) * 256, rx * rx) + div_i((dy * dy) * 256, ry * ry);
    int wt = 256 - nsq;
    if (wt < 0) {
        wt = 0;
    }
    wt = div_i(wt * wt, 256);
    if (wt > 255) {
        wt = 255;
    }
    return wt;
}

float4 fs_rain_glow(RainVsOut i) : SV_Target0 {
    int wt = rain_weight(i.pos, i.fall);
    // mul8(premul_channel, wt) == (c*wt + 127)/255, the CPU premul_rgb.
    int cr = f2i(round(i.color.x * 255.0));
    int cg = f2i(round(i.color.y * 255.0));
    int cb = f2i(round(i.color.z * 255.0));
    float r = float(div_i((cr * wt) + 127, 255)) / 255.0;
    float g = float(div_i((cg * wt) + 127, 255)) / 255.0;
    float b = float(div_i((cb * wt) + 127, 255)) / 255.0;
    return float4(r, g, b, 1.0);
}

// HaloMode::Over radial veil: the same integer weight, emitted as the straight
// colour with the weight as the per-pixel alpha into the source-over blend.
float4 fs_rain_glow_over(RainVsOut i) : SV_Target0 {
    int wt = rain_weight(i.pos, i.fall);
    // Per-pixel alpha ceiling (the CPU wt.min(halo_over_cap)): the cap rides
    // the colour's alpha byte; 0 means uncapped (255).
    int cap = f2i(round(i.color.w * 255.0));
    if (cap == 0) {
        cap = 255;
    }
    if (wt > cap) {
        wt = cap;
    }
    return float4(i.color.xyz, float(wt) / 255.0);
}

// ========================== EMBERFORGE FIRE ==========================
// The twin of aterm_render::fire_field, op for op: pure int/uint math with
// wrapping uint semantics and arithmetic `>>` on int.

uint fire_hash(uint x, uint y, uint seed) {
    uint h = ((x * 2654435761u) + (y * 2246822519u)) + (seed * 3266489917u);
    h = h ^ (h >> 15u);
    h = h * 739982445u;
    h = h ^ (h >> 12u);
    h = h * 695872825u;
    h = h ^ (h >> 15u);
    return h;
}

int fire_fade(int t) {
    return ((t * t) * (768 - (2 * t))) >> 16u;
}

int fire_vnoise(uint x, uint y, uint ymask, uint seed) {
    uint ix = x >> 8u;
    uint iy0 = (y >> 8u) & ymask;
    uint iy1 = (iy0 + 1u) & ymask;
    uint ix1 = ix + 1u;
    int fx = int(x & 255u);
    int fy = int(y & 255u);
    int n00 = int(fire_hash(ix, iy0, seed) >> 24u);
    int n10 = int(fire_hash(ix1, iy0, seed) >> 24u);
    int n01 = int(fire_hash(ix, iy1, seed) >> 24u);
    int n11 = int(fire_hash(ix1, iy1, seed) >> 24u);
    int ux = fire_fade(fx);
    int uy = fire_fade(fy);
    int a = (n00 * (256 - ux)) + (n10 * ux);
    int b = (n01 * (256 - ux)) + (n11 * ux);
    return ((a * (256 - uy)) + (b * uy)) >> 16u;
}

struct FireCoreOut {
    int idx;
    int q;
    int edge;
    int body;
    int root;
    int rim;
};

FireCoreOut fire_core(int px, int py, int base_y, int peak_h, uint phase, int temp,
                      int strength, int lean, int cell_h) {
    FireCoreOut o;
    o.idx = 0;
    o.q = 256;
    o.edge = 0;
    o.body = 0;
    o.root = 0;
    o.rim = 0;
    int ch = max(cell_h, 2);
    uint chu = uint(ch);
    int peak = clamp(peak_h, 1, 2048);
    // ROOT SKIRT: below the root the envelope mirrors, compressed 5x.
    int v = base_y - py;
    if (v < 0) {
        v = (-v) * 5;
    }
    int vn = min(div_i(v * 256, peak), 512);
    int shear = div_i(lean * vn, 4);
    uint xq = asuint(((px * 256) + shear) + 4194304);
    uint sx = div_u(xq * 4u, 5u * chu);
    uint ts = uint(300 + (temp * 2));
    uint tr = (phase * ts) >> 10u;
    int n0 = fire_vnoise(sx, tr, 16383u, 1374725047u);
    int n1 = fire_vnoise((sx * 2u) + 12799u, (tr * 2u) + 37199u, 32767u, 2636928640u);
    int n = ((n0 * 9) + (n1 * 7)) >> 4u;
    int ridge = 255 - abs_i((2 * n) - 255);
    int hs2 = (ridge * ridge) >> 8u;
    int hshape = (hs2 * (256 + ridge)) >> 9u;
    int hq = 30 + ((hshape * 260) >> 8u);
    int hcol = div_i((peak * hq) * strength, 255);
    int vv = v * 256;
    int d = hcol - vv;
    if (d <= 0) {
        return o;
    }
    int aa = max(div_i(ch, 4), 3) * 256;
    int edge = min(div_i(d * 256, aa), 256);
    int rim = clamp(div_i(((2 * aa) - d) * 256, aa), 0, 256);
    int q = div_i(vv * 256, hcol);
    uint rs = uint(350 + temp);
    uint offr = (phase * rs) >> 10u;
    uint sy = div_u(uint(vv) * 2u, 3u * chu);
    uint by = sy - offr;
    int m0 = fire_vnoise(div_u(sx * 3u, 2u) + 5023u, by, 16383u, 3039394381u);
    int m1 = fire_vnoise((sx * 3u) + 9531u, (by * 2u) + 15913u, 32767u, 1759714724u);
    int m2 = fire_vnoise((sx * 6u) + 26251u, (by * 4u) + 37633u, 65535u, 458671337u);
    int body0 = (((m0 * 4) + (m1 * 3)) + m2) >> 3u;
    int body = 128 + (((body0 - 128) * edge) >> 8u);
    int heat = ((256 - q) * (112 + ((temp * 120) >> 8u))) >> 6u;
    int idx0 = clamp((heat * (150 + ((body * 212) >> 8u))) >> 8u, 0, 850);
    int root = min(div_i(v * 1536, ch), 256);
    o.idx = (idx0 * (192 + (root >> 2u))) >> 8u;
    o.q = q;
    o.edge = edge;
    o.body = body;
    o.root = root;
    o.rim = rim;
    return o;
}

// The five-stop palettes, each segment lerped in 8.8 fixed point.
int3 fire_pal_lerp(int3 a, int3 b, int f) {
    return int3(((a.x * (256 - f)) + (b.x * f)) >> 8u,
                ((a.y * (256 - f)) + (b.y * f)) >> 8u,
                ((a.z * (256 - f)) + (b.z * f)) >> 8u);
}

int3 fire_pal_add(int idx_in) {
    int3 pal[5] = {
        int3(42, 0, 0), int3(139, 26, 0), int3(224, 74, 0),
        int3(255, 176, 32), int3(255, 240, 192)
    };
    // == FIRE_ADD_IDX_MAX: additive fire stops a breath past amber.
    int idx = min(idx_in, 780);
    int seg = clamp(idx >> 8u, 0, 3);
    int f = idx - (seg << 8u);
    return fire_pal_lerp(pal[min(uint(seg), 4u)], pal[min(uint(seg + 1), 4u)], f);
}

int3 fire_pal_over(int idx) {
    int3 pal[5] = {
        int3(102, 24, 4), int3(168, 40, 0), int3(208, 70, 0),
        int3(238, 110, 0), int3(255, 166, 30)
    };
    int seg = clamp(idx >> 8u, 0, 3);
    int f = idx - (seg << 8u);
    return fire_pal_lerp(pal[min(uint(seg), 4u)], pal[min(uint(seg + 1), 4u)], f);
}

// FIRE_LAYOUT: LOC0 => Uint16x4 (rect), LOC1 => Uint16x4 (geo), LOC2 =>
// Uint32 (phase), LOC3 => Uint8x4 (tsl).
struct FireVsOut {
    float4 pos : SV_Position;
    // base_y, peak_h, cell_h, grid_top
    nointerpolation uint4 geo : LOC0;
    nointerpolation uint phase : LOC1;
    // temp, strength, cov_cap, lean bits
    nointerpolation uint4 tsl : LOC2;
};

FireVsOut vs_fire(uint vi : SV_VertexID, uint4 rect_u : LOC0, uint4 geo : LOC1,
                  uint phase : LOC2, uint4 tsl : LOC3) {
    FireVsOut o;
    float4 rect = float4(rect_u);
    float2 k = corner(vi);
    float2 px = rect.xy + (k * rect.zw);
    o.pos = float4(to_ndc(px), 0.0, 1.0);
    o.geo = geo;
    o.phase = phase;
    o.tsl = tsl;
    return o;
}

// The field at this fragment; `lean` is the i8 lean byte sign-extended out
// of its u8 slot.
FireCoreOut fire_at(FireVsOut i, out int py) {
    int px = f2i(floor(i.pos.x));
    py = f2i(floor(i.pos.y));
    int lean = asint(i.tsl.w << 24u) >> 24u;
    return fire_core(px, py, int(i.geo.x), int(i.geo.y), i.phase, int(i.tsl.x),
                     int(i.tsl.y), lean, int(i.geo.z));
}

// TOP-EDGE FADE == CPU fire_top_fade: dissolve toward the grid top.
int fire_top_fade(FireVsOut i, int py) {
    int fade_px = max(int(i.geo.z) * 2, 2);
    int ttop = clamp(div_i((py - int(i.geo.w)) * 255, fade_px), 0, 255);
    return fire_fade(ttop);
}

// FireMode::Add: premultiplied field light; (palette*cov + 127)/255 per
// channel is the single rounding point (the CPU fire_shade_add bytes).
float4 fs_fire_add(FireVsOut i) : SV_Target0 {
    int py;
    FireCoreOut c = fire_at(i, py);
    int temp = int(i.tsl.x);
    // Coverage law == fire_cov_add: AA edge x temp density x pockets x root.
    int dens = 150 + ((temp * 106) >> 8u);
    int bodyc = 110 + ((c.body * 146) >> 8u);
    int cov = min((((((c.edge * dens) >> 8u) * bodyc) >> 8u) * c.root) >> 8u, int(i.tsl.z));
    int covf = div_i(cov * fire_top_fade(i, py), 255);
    // Rim cooling: the outline drops toward deep red, the core stays hot.
    int idx = (c.idx * (256 - ((c.rim * 112) >> 8u))) >> 8u;
    int3 rgb = fire_pal_add(idx);
    float r = float(div_i((rgb.x * covf) + 127, 255)) / 255.0;
    float g = float(div_i((rgb.y * covf) + 127, 255)) / 255.0;
    float b = float(div_i((rgb.z * covf) + 127, 255)) / 255.0;
    return float4(r, g, b, 1.0);
}

// FireMode::Over: straight ink plus field alpha through the source-over blend.
float4 fs_fire_over(FireVsOut i) : SV_Target0 {
    int py;
    FireCoreOut c = fire_at(i, py);
    // Alpha law == fire_alpha_over: pooled rims, dense root, wispy tips.
    int bodyc = 130 + ((c.body * 166) >> 8u);
    int tipf = 120 + (((256 - c.q) * 136) >> 8u);
    int pool = 256 + ((c.rim * 96) >> 8u);
    int a = min((((((((c.edge * bodyc) >> 8u) * c.root) >> 8u) * tipf) >> 8u) * pool) >> 8u,
                int(i.tsl.z));
    int af = div_i(a * fire_top_fade(i, py), 255);
    // Rim darkening: ink pools at the outline.
    int idx = (c.idx * (256 - ((c.rim * 128) >> 8u))) >> 8u;
    int3 rgb = fire_pal_over(idx);
    return float4(float(rgb.x) / 255.0, float(rgb.y) / 255.0, float(rgb.z) / 255.0,
                  float(af) / 255.0);
}

// ---------------------------------------------------------------- GLYPHS
// GLYPH_LAYOUT: LOC0 => Float32x4 (rect), LOC1 => Float32x4 (uv), LOC2 =>
// Unorm8x4 (colour), LOC3 => Unorm8x4 (bg).
struct GlyphVsOut {
    float4 pos : SV_Position;
    float2 uv : LOC0;
    float4 color : LOC1;
    float4 bg : LOC2;
};

GlyphVsOut vs_glyph(uint vi : SV_VertexID, float4 rect : LOC0, float4 uv : LOC1,
                    float4 color : LOC2, float4 bg : LOC3) {
    GlyphVsOut o;
    float2 k = corner(vi);
    float2 px = rect.xy + (k * rect.zw);
    o.pos = float4(to_ndc(px), 0.0, 1.0);
    o.uv = uv.xy + (k * uv.zw);
    o.color = color;
    o.bg = bg;
    return o;
}

// TEXT glyphs, with the W2 linear-corrected coverage remap gated exactly like
// the CPU correction_applies (interior coverage, a non-degenerate luminance
// gap: 0.001 == aterm_render::TEXT_BLEND_EPS).
float4 fs_glyph(GlyphVsOut i) : SV_Target0 {
    float cov = atlas_tex.Sample(atlas_samp, i.uv).x;
    float3 fg_lin = s2l(i.color.xyz);
    if (u_text_blend != 0.0 && cov > 0.0 && cov < 1.0) {
        float3 lum = float3(0.2126, 0.7152, 0.0722);
        float fg_l = dot(fg_lin, lum);
        float bg_l = dot(s2l(i.bg.xyz), lum);
        if (abs(fg_l - bg_l) >= 0.001) {
            float blend_l = s2l_s((l2s_s(fg_l) * cov) + (l2s_s(bg_l) * (1.0 - cov)));
            cov = clamp((blend_l - bg_l) / (fg_l - bg_l), 0.0, 1.0);
        }
    }
    return float4(fg_lin, cov);
}

// Colour-emoji glyphs: the atlas holds the CPU renderer's final RGBA pixels.
float4 fs_glyph_color(GlyphVsOut i) : SV_Target0 {
    float4 c = atlas_tex.Sample(atlas_samp, i.uv);
    return float4(s2l(c.xyz), c.w);
}

// Sparkle-word decorations, OVER: the coverage quantized to the CPU's byte
// lattice, round(cov*a*255)/255 == the CPU (cov*alpha + 127)/255.
float4 fs_deco_over(GlyphVsOut i) : SV_Target0 {
    float cov = atlas_tex.Sample(atlas_samp, i.uv).x;
    float a = round((cov * i.color.w) * 255.0) / 255.0;
    return float4(s2l(i.color.xyz), a);
}

// ADD (the profanity sparkle): premultiplied (rgb*a, a) into One/One.
float4 fs_deco_add(GlyphVsOut i) : SV_Target0 {
    float cov = atlas_tex.Sample(atlas_samp, i.uv).x;
    float a = round((cov * i.color.w) * 255.0) / 255.0;
    return float4(i.color.xyz * a, a);
}

// RGBA8 sprites: the tint and opacity multiplies quantized to 8 bits before
// the sRGB decode, reproducing the CPU stamp's intermediate byte.
float4 fs_sprite_over(GlyphVsOut i) : SV_Target0 {
    float4 c = atlas_tex.Sample(atlas_samp, i.uv);
    float3 tinted = round((c.xyz * i.color.xyz) * 255.0) / (255.0).xxx;
    float a = round((c.w * i.color.w) * 255.0) / 255.0;
    return float4(s2l(tinted), a);
}

// ---------------------------------------------------------------- FX LAYERS
// FX_LAYOUT: LOC0 => Uint16x4 (the slice rect), LOC1 => Sint16x2 (the item
// origin mod 2^16), LOC2 => Uint16x2 (a sprite's dw, dh), LOC3 => Uint16x4
// (the atlas region; [w, 0, 0, 0] for Rect and Ramps), LOC4 => Uint32 (the
// absolute palette entry), LOC5 => Uint32 (flags), all integers. Pixel law F,
// the WGSL fs_fx / fs_fx_linear / fs_fx_store twins op for op: flat operands, the
// fragment's own pixel and the item's unclamped origin, texels read by
// integer address and their bytes recovered with round.
struct FxVsOut {
    float4 pos : SV_Position;
    nointerpolation int2 origin : LOC0;
    nointerpolation uint2 dst : LOC1;
    nointerpolation uint4 src : LOC2;
    nointerpolation uint pal : LOC3;
    nointerpolation uint flags : LOC4;
};

FxVsOut vs_fx(uint vi : SV_VertexID, uint4 rect_u : LOC0, int2 origin : LOC1, uint2 dst : LOC2,
              uint4 src : LOC3, uint pal : LOC4, uint flags : LOC5) {
    FxVsOut o;
    float4 rect = float4(rect_u);
    float2 k = corner(vi);
    float2 px = rect.xy + (k * rect.zw);
    o.pos = float4(to_ndc(px), 0.0, 1.0);
    o.origin = origin;
    o.dst = dst;
    o.src = src;
    o.pal = pal;
    o.flags = flags;
    return o;
}

// The stored byte of each Unorm8 channel.
uint4 fx_byte4(float4 v) {
    return f2u4(round(v * 255.0));
}

// Palette entry j as bytes (r, g, b, a).
uint4 fx_ink(uint j) {
    return fx_byte4(fx_pal.Load(int3(int(j % 1024u), int(j / 1024u), 0)));
}

// The item-local pixel l = (p - O) mod 2^16.
uint2 fx_local(float4 pos, int2 origin) {
    int2 p = f2i2(floor(pos.xy));
    return uint2(p - origin) & (65535u).xx;
}

// The sprite texel under l: t = (l.x*w / dw, l.y*h / dh), mirrored under
// flip_x, offset to the art's atlas region.
int2 fx_texel(uint2 l, uint2 dst, uint4 src, uint flags) {
    uint2 t = uint2(div_u(l.x * src.z, dst.x), div_u(l.y * src.w, dst.y));
    if ((flags & 4u) != 0u) {
        t.x = (src.z - 1u) - t.x;
    }
    return int2(src.xy + t);
}

// A Ramps item's palette row for its row y: the rows src.y + 1 ..= src.y +
// src.z repeat row src.y's ends (a run, one light as tall as its rows), which
// the palette holds once.
uint fx_ramp_row(uint y, uint4 src) {
    return y - min(y - min(y, src.y), src.z);
}

// The Exact law: (c, a) / 255 raw into src + dst*(1 - a) on the Unorm view.
float4 fs_fx(FxVsOut i) : SV_Target0 {
    uint2 l = fx_local(i.pos, i.origin);
    uint kind = i.flags & 3u;
    uint4 c;
    if (kind == 2u) {
        int2 t = fx_texel(l, i.dst, i.src, i.flags);
        if (((i.flags >> 3u) & 3u) == 0u) {
            // Index8: 0 writes nothing; k names entry ink + k - 1, and an
            // index past the palette writes nothing.
            uint k = fx_byte4(atlas_tex.Load(int3(t, 0))).x;
            uint reach = (i.flags >> 8u) & 255u;
            if (k == 0u || k > reach) {
                discard;
            }
            c = fx_ink((i.pal + clamp(k, 1u, reach)) - 1u);
        } else {
            // Rgba8Premul: the texel is its own (c, a); a = 0 is light.
            c = fx_byte4(fx_rgba.Load(int3(t, 0)));
        }
    } else {
        // Rect (row 0) and Ramps (row l.y's palette row).
        uint j = i.pal + (2u * ((kind == 1u) ? fx_ramp_row(l.y, i.src) : 0u));
        c = glow_lerp4(fx_ink(j), fx_ink(j + 1u), l.x, max(i.src.x, 1u));
    }
    return float4(c) / (255.0).xxxx;
}

// The Exact law where every pixel the layer draws is opaque (route (1) of the
// pass economy): the entry's colour, stored on the sRGB view with no blend
// under the Color mask.
float4 fs_fx_store(FxVsOut i) : SV_Target0 {
    uint2 l = fx_local(i.pos, i.origin);
    uint kind = i.flags & 3u;
    uint4 c;
    if (kind == 2u) {
        int2 t = fx_texel(l, i.dst, i.src, i.flags);
        if (((i.flags >> 3u) & 3u) == 0u) {
            uint k = fx_byte4(atlas_tex.Load(int3(t, 0))).x;
            uint reach = (i.flags >> 8u) & 255u;
            if (k == 0u || k > reach) {
                discard;
            }
            c = fx_ink((i.pal + clamp(k, 1u, reach)) - 1u);
        } else {
            c = fx_byte4(fx_rgba.Load(int3(t, 0)));
        }
    } else {
        uint j = i.pal + (2u * ((kind == 1u) ? fx_ramp_row(l.y, i.src) : 0u));
        c = glow_lerp4(fx_ink(j), fx_ink(j + 1u), l.x, max(i.src.x, 1u));
    }
    return float4(s2l(float3(c.xyz) / (255.0).xxx), 1.0);
}

// The Linear law (FreeSprite's), on the sRGB view.
float4 fs_fx_linear(FxVsOut i) : SV_Target0 {
    uint2 l = fx_local(i.pos, i.origin);
    uint4 v = fx_byte4(fx_rgba.Load(int3(fx_texel(l, i.dst, i.src, i.flags), 0)));
    uint4 tint = fx_ink(i.pal);
    uint4 p = div_u4((v * tint) + (127u).xxxx, (255u).xxxx);
    if (p.w == 0u) {
        discard;
    }
    return float4(s2l(float3(p.xyz) / (255.0).xxx), float(p.w) / 255.0);
}
