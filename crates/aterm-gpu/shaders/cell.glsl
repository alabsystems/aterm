#version 300 es
// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// GLSL ES 3.00 twin of renderer.rs SHADER and shaders/cell.metal, for the GL
// arm (src/gl). Every function is the WGSL's op for op: the GL arm is held
// byte-identical to wgpu's GLES backend on the same driver, so float
// expressions keep the WGSL's order and constants (naga folds `1.0 / 2.4` to
// 0.41666666, written out here), `round` is `roundEven` (WGSL rounds half to
// even), and integer `>>` on `int` is arithmetic, as WGSL defines it.
//
// ONE FILE, MANY ENTRY POINTS. GLSL has one `main` per stage, so src/gl
// compiles this file once per stage of a (vs, fs) row with
//   #define ATERM_VERTEX or ATERM_FRAGMENT
//   #define ATERM_VS_<vs> and ATERM_FS_<fs> (both stages see both)
// prepended after the #version line, and appends a `main` that calls the
// entry point (the vertex `main` then flips clip-space y and remaps z, as
// naga's ADJUST_COORDINATE_SPACE does; see src/gl/pipelines.rs). Each vertex
// entry's attributes and varyings live under its `ATERM_VS_` guard, so one
// file serves every interface without two `in`s sharing a location.
//
// BINDINGS (src/gl/shaders.rs checks them): the `Uniforms` block is `aterm_b0`
// (binding point 0), the atlas is `aterm_t0` (texture unit 0 with its sampler
// object at unit 0) — the cell rows' BindSpec slots.
precision highp float;
precision highp int;

// text_blend != 0.0 => fs_glyph applies the W2 linear-corrected coverage remap.
layout(std140) uniform aterm_b0 {
    vec2 u_screen;
    float u_text_blend;
    float u_pad;
};
uniform highp sampler2D aterm_t0;

#ifdef ATERM_VERTEX
#define ATERM_VARYING out
#else
#define ATERM_VARYING in
layout(location = 0) out vec4 o_frag;
#endif

// Unit quad corner for vertex index 0..6 (two CCW triangles).
vec2 corner(uint vi) {
    vec2 c[6] = vec2[6](
        vec2(0.0, 0.0), vec2(1.0, 0.0), vec2(0.0, 1.0),
        vec2(1.0, 0.0), vec2(1.0, 1.0), vec2(0.0, 1.0)
    );
    return c[vi];
}

// Pixel coords (top-left origin, y down) -> clip space (y up, row 0 at top).
vec2 to_ndc(vec2 px) {
    return vec2(2.0 * px.x / u_screen.x - 1.0, 1.0 - 2.0 * px.y / u_screen.y);
}

// sRGB-encoded channel -> linear-light (the proper piecewise curve). Alpha and
// coverage are not sRGB and pass through.
vec3 s2l(vec3 c) {
    vec3 lo = c / vec3(12.92);
    vec3 hi = pow((c + vec3(0.055)) / vec3(1.055), vec3(2.4));
    return mix(lo, hi, greaterThan(c, vec3(0.04045)));
}

// Scalar sRGB decode/encode for the W2 corrected-alpha remap: the CPU
// srgb_to_linear / linear_to_srgb curves and thresholds.
float s2l_s(float c) {
    if (c <= 0.04045) { return c / 12.92; }
    return pow((c + 0.055) / 1.055, 2.4);
}
float l2s_s(float l) {
    if (l <= 0.0031308) { return 12.92 * l; }
    return 1.055 * pow(l, 0.41666666) - 0.055;
}

// ---------------------------------------------------------------- BG
// BG_LAYOUT: 0 => Uint16x4 (rect), 1 => Unorm8x4 (colour).
#if defined(ATERM_VS_vs_bg)
ATERM_VARYING vec4 v_color;
#endif

#if defined(ATERM_VERTEX) && defined(ATERM_VS_vs_bg)
layout(location = 0) in uvec4 a_rect;
layout(location = 1) in vec4 a_color;
void vs_bg() {
    // Uint16x4 arrives as uvec4; integer pixel coords -> exact f32 (no loss).
    vec4 rect = vec4(a_rect);
    vec2 k = corner(uint(gl_VertexID));
    vec2 px = rect.xy + k * rect.zw;
    gl_Position = vec4(to_ndc(px), 0.0, 1.0);
    v_color = a_color;
}
#endif

#if defined(ATERM_FRAGMENT) && defined(ATERM_FS_fs_bg)
void fs_bg() {
    o_frag = vec4(s2l(v_color.rgb), v_color.a);
}
#endif

// ---------------------------------------------------------------- GLOW
// GLOW_LAYOUT: 0 => Uint16x4 (rect), 1 => Uint8x4 (left colour), 2 => Uint8x4
// (right colour), both raw integer bytes. The fragment ramps the pair per
// column with aterm_render::glow_lerp's integer law and emits c / 255 raw.
//
// `v_lx` is the fragment's x inside the quad, interpolated across it. The
// WGSL asks for `@interpolate(linear)`, which GLSL ES 3.00 cannot spell (naga
// refuses to translate it for any GLES target, so wgpu's GL backend cannot
// build this row at all); every vertex has w == 1, so the default smooth
// interpolation computes the same value.
#if defined(ATERM_VS_vs_glow) || defined(ATERM_VS_vs_glow_items)
flat ATERM_VARYING vec2 v_span;
flat ATERM_VARYING uvec4 v_c0;
flat ATERM_VARYING uvec4 v_c1;
ATERM_VARYING float v_lx;
#endif

#if defined(ATERM_VERTEX) && defined(ATERM_VS_vs_glow)
layout(location = 0) in uvec4 a_rect;
layout(location = 1) in uvec4 a_c0;
layout(location = 2) in uvec4 a_c1;
void vs_glow() {
    vec4 rect = vec4(a_rect);
    vec2 k = corner(uint(gl_VertexID));
    vec2 px = rect.xy + k * rect.zw;
    gl_Position = vec4(to_ndc(px), 0.0, 1.0);
    v_span = rect.xz;
    v_lx = k.x * rect.z;
    v_c0 = a_c0;
    v_c1 = a_c1;
}
#endif

// The fragment's 0-based column inside its quad (see the WGSL twin).
uint glow_column(float lx, float w) {
    return uint(clamp(floor(lx), 0.0, max(w - 1.0, 0.0)));
}

// aterm_render::glow_lerp, per channel and for alpha alike, at column i of a
// w-wide quad (w >= 1).
uvec4 glow_lerp4(uvec4 c0, uvec4 c1, uint i, uint w) {
    uint m = 2u * min(i, w - 1u) + 1u;
    return (c0 * (2u * w - m) + c1 * m + uvec4(w)) / uvec4(2u * w);
}

#if defined(ATERM_FRAGMENT) && defined(ATERM_FS_fs_glow)
void fs_glow() {
    uint w = max(uint(v_span.y), 1u);
    uvec4 c = glow_lerp4(v_c0, v_c1, glow_column(v_lx, v_span.y), w);
    o_frag = vec4(c) / vec4(255.0);
}
#endif

// ---------------------------------------------------------------- GLOW ITEMS
// The bloom's lights as an item list (aterm_render::bloom::BloomItems, whose
// `light` is this reader's twin; shaders/cell.metal's GLOW ITEMS spells out the
// layout), read by instance id, the WGSL `glow_item` op for op. GLSL ES 3.00
// has no storage buffer, so the list is an RGBA8 texture 1024 words wide (word
// j at (j % 1024, j / 1024), its bytes little-endian, recovered with roundEven)
// at the vertex stage's unit 0 (BindSpec::GLOW_ITEMS).
#if defined(ATERM_VERTEX) && defined(ATERM_VS_vs_glow_items)
uint glow_item_word(uint j) {
    uvec4 b = uvec4(roundEven(texelFetch(aterm_t0, ivec2(int(j % 1024u), int(j / 1024u)), 0)
                              * 255.0));
    return b.r | (b.g << 8u) | (b.b << 16u) | (b.a << 24u);
}

// An end as bytes (r, g, b, a), its colour times num / den, rounded.
uvec4 glow_item_end(uint e, uint num, uint den) {
    uvec3 c = ((uvec3(e >> 16u, e >> 8u, e) & uvec3(255u)) * num + uvec3(den / 2u)) / uvec3(den);
    return uvec4(c, e >> 24u);
}

void glow_item(uint i, bool widen, out uvec4 rect, out uvec4 c0, out uvec4 c1) {
    uint lo = 0u;
    uint hi = glow_item_word(0u) - 1u;
    while (lo < hi) {
        uint mid = (lo + hi + 1u) >> 1u;
        if (glow_item_word(4u + mid) <= i) {
            lo = mid;
        } else {
            hi = mid - 1u;
        }
    }
    uint s = i - glow_item_word(4u + lo);
    uint r = glow_item_word(1u) + 4u * lo;
    uint xy = glow_item_word(r);
    uint jj = glow_item_word(r + 2u);
    uint join = jj & 0xFFFFu;
    uint joined = jj >> 16u;
    uint e = glow_item_word(2u) + glow_item_word(r + 3u) + 2u * s;
    uint e0 = glow_item_word(e);
    uint e1 = glow_item_word(e + 1u);
    uint qw = glow_item_word(r + 1u) & 0xFFFFu;
    uint qh = s == join ? joined + 1u : 1u;
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
    rect = (e0 | e1) == 0u
        ? uvec4(0u)
        : uvec4(xy & 0xFFFFu, (xy >> 16u) + s + (s > join ? joined : 0u), qw, qh);
    c0 = glow_item_end(e0, num, den);
    c1 = glow_item_end(e1, num, den);
}

// vs_glow over the item list, widened: the bloom extract's lights.
void vs_glow_items() {
    uvec4 r;
    uvec4 c0;
    uvec4 c1;
    glow_item(uint(gl_InstanceID), true, r, c0, c1);
    vec4 rect = vec4(r);
    vec2 k = corner(uint(gl_VertexID));
    vec2 px = rect.xy + k * rect.zw;
    gl_Position = vec4(to_ndc(px), 0.0, 1.0);
    v_span = rect.xz;
    v_lx = k.x * rect.z;
    v_c0 = c0;
    v_c1 = c1;
}
#endif

// ---------------------------------------------------------------- RAIN HALO
// RAIN_GLOW_LAYOUT: 0 => Uint16x4, 1 => Unorm8x4, 2 => Uint16x4.
#if defined(ATERM_VS_vs_rain_glow)
ATERM_VARYING vec4 v_color;
flat ATERM_VARYING uvec4 v_fall; // cx, cy, rx, ry (window px)
#endif

#if defined(ATERM_VERTEX) && defined(ATERM_VS_vs_rain_glow)
layout(location = 0) in uvec4 a_rect;
layout(location = 1) in vec4 a_color;
layout(location = 2) in uvec4 a_fall;
void vs_rain_glow() {
    vec4 rect = vec4(a_rect);
    vec2 k = corner(uint(gl_VertexID));
    vec2 px = rect.xy + k * rect.zw;
    gl_Position = vec4(to_ndc(px), 0.0, 1.0);
    v_color = a_color;
    v_fall = a_fall;
}
#endif

// The elliptical integer falloff weight shared by the Add and Over halos: pure
// int math, byte-for-byte the CPU draw_rain_add.
int rain_weight(vec2 fragpos, uvec4 fall) {
    // The fragment position is the pixel centre (px + 0.5); floor gives the
    // integer pixel index the CPU walks.
    int px = int(floor(fragpos.x));
    int py = int(floor(fragpos.y));
    int cx = int(fall.x);
    int cy = int(fall.y);
    int rx = int(fall.z);
    int ry = int(fall.w);
    int dx = px - cx;
    int dy = py - cy;
    int nsq = (dx * dx * 256) / (rx * rx) + (dy * dy * 256) / (ry * ry);
    int wt = 256 - nsq;
    if (wt < 0) { wt = 0; }
    wt = (wt * wt) / 256;
    if (wt > 255) { wt = 255; }
    return wt;
}

#if defined(ATERM_FRAGMENT) && defined(ATERM_FS_fs_rain_glow)
void fs_rain_glow() {
    int wt = rain_weight(gl_FragCoord.xy, v_fall);
    // mul8(premul_channel, wt) == (c*wt + 127)/255, the CPU premul_rgb.
    int cr = int(roundEven(v_color.r * 255.0));
    int cg = int(roundEven(v_color.g * 255.0));
    int cb = int(roundEven(v_color.b * 255.0));
    float r = float((cr * wt + 127) / 255) / 255.0;
    float g = float((cg * wt + 127) / 255) / 255.0;
    float b = float((cb * wt + 127) / 255) / 255.0;
    o_frag = vec4(r, g, b, 1.0);
}
#endif

// HaloMode::Over radial veil: the same integer weight, emitted as the straight
// colour with the weight as the per-pixel alpha into the source-over blend.
#if defined(ATERM_FRAGMENT) && defined(ATERM_FS_fs_rain_glow_over)
void fs_rain_glow_over() {
    int wt = rain_weight(gl_FragCoord.xy, v_fall);
    // Per-pixel alpha ceiling (the CPU wt.min(halo_over_cap)): the cap rides
    // the colour's alpha byte; 0 means uncapped (255).
    int cap = int(roundEven(v_color.a * 255.0));
    if (cap == 0) { cap = 255; }
    if (wt > cap) { wt = cap; }
    o_frag = vec4(v_color.rgb, float(wt) / 255.0);
}
#endif

// ========================== EMBERFORGE FIRE ==========================
// The twin of aterm_render::fire_field, op for op: pure int/uint math with
// wrapping uint semantics and arithmetic `>>` on int.

uint fire_hash(uint x, uint y, uint seed) {
    uint h = x * 0x9E3779B1u + y * 0x85EBCA77u + seed * 0xC2B2AE3Du;
    h ^= h >> 15u;
    h = h * 0x2C1B3C6Du;
    h ^= h >> 12u;
    h = h * 0x297A2D39u;
    h ^= h >> 15u;
    return h;
}

int fire_fade(int t) {
    return (t * t * (768 - 2 * t)) >> 16u;
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
    int a = n00 * (256 - ux) + n10 * ux;
    int b = n01 * (256 - ux) + n11 * ux;
    return (a * (256 - uy) + b * uy) >> 16u;
}

struct FireCoreOut {
    int idx;
    int q;
    int edge;
    int body;
    int root;
    int rim;
};

FireCoreOut fire_core(int px, int py, int base_y, int peak_h, uint phase,
                      int temp, int strength, int lean, int cell_h) {
    FireCoreOut o = FireCoreOut(0, 256, 0, 0, 0, 0);
    int ch = max(cell_h, 2);
    uint chu = uint(ch);
    int peak = clamp(peak_h, 1, 2048);
    // ROOT SKIRT: below the root the envelope mirrors, compressed 5x.
    int v = base_y - py;
    if (v < 0) { v = -v * 5; }
    int vn = min((v * 256) / peak, 512);
    int shear = lean * vn / 4;
    uint xq = uint(px * 256 + shear + 4194304);
    uint sx = (xq * 4u) / (5u * chu);
    uint ts = uint(300 + temp * 2);
    uint tr = (phase * ts) >> 10u;
    int n0 = fire_vnoise(sx, tr, 0x3FFFu, 0x51F0A3B7u);
    int n1 = fire_vnoise(sx * 2u + 12799u, tr * 2u + 37199u, 0x7FFFu, 0x9D2C5680u);
    int n = (n0 * 9 + n1 * 7) >> 4u;
    int ridge = 255 - abs(2 * n - 255);
    int hs2 = (ridge * ridge) >> 8u;
    int hshape = (hs2 * (256 + ridge)) >> 9u;
    int hq = 30 + ((hshape * 260) >> 8u);
    int hcol = (peak * hq * strength) / 255;
    int vv = v * 256;
    int d = hcol - vv;
    if (d <= 0) { return o; }
    int aa = max(ch / 4, 3) * 256;
    int edge = min((d * 256) / aa, 256);
    int rim = clamp((2 * aa - d) * 256 / aa, 0, 256);
    int q = (vv * 256) / hcol;
    uint rs = uint(350 + temp);
    uint offr = (phase * rs) >> 10u;
    uint sy = (uint(vv) * 2u) / (3u * chu);
    uint by = sy - offr;
    int m0 = fire_vnoise(sx * 3u / 2u + 5023u, by, 0x3FFFu, 0xB5297A4Du);
    int m1 = fire_vnoise(sx * 3u + 9531u, by * 2u + 15913u, 0x7FFFu, 0x68E31DA4u);
    int m2 = fire_vnoise(sx * 6u + 26251u, by * 4u + 37633u, 0xFFFFu, 0x1B56C4E9u);
    int body0 = (m0 * 4 + m1 * 3 + m2) >> 3u;
    int body = 128 + (((body0 - 128) * edge) >> 8u);
    int heat = ((256 - q) * (112 + ((temp * 120) >> 8u))) >> 6u;
    int idx0 = clamp((heat * (150 + ((body * 212) >> 8u))) >> 8u, 0, 850);
    int root = min((v * 1536) / ch, 256);
    o.idx = (idx0 * (192 + (root >> 2u))) >> 8u;
    o.q = q;
    o.edge = edge;
    o.body = body;
    o.root = root;
    o.rim = rim;
    return o;
}

ivec3 fire_pal_add(int idx_in) {
    ivec3 pal[5] = ivec3[5](
        ivec3(42, 0, 0), ivec3(139, 26, 0), ivec3(224, 74, 0),
        ivec3(255, 176, 32), ivec3(255, 240, 192));
    // == FIRE_ADD_IDX_MAX: additive fire stops a breath past amber.
    int idx = min(idx_in, 780);
    int seg = clamp(idx >> 8u, 0, 3);
    int f = idx - (seg << 8u);
    ivec3 a = pal[seg];
    ivec3 b = pal[seg + 1];
    return ivec3((a.x * (256 - f) + b.x * f) >> 8u,
                 (a.y * (256 - f) + b.y * f) >> 8u,
                 (a.z * (256 - f) + b.z * f) >> 8u);
}

ivec3 fire_pal_over(int idx) {
    ivec3 pal[5] = ivec3[5](
        ivec3(102, 24, 4), ivec3(168, 40, 0), ivec3(208, 70, 0),
        ivec3(238, 110, 0), ivec3(255, 166, 30));
    int seg = clamp(idx >> 8u, 0, 3);
    int f = idx - (seg << 8u);
    ivec3 a = pal[seg];
    ivec3 b = pal[seg + 1];
    return ivec3((a.x * (256 - f) + b.x * f) >> 8u,
                 (a.y * (256 - f) + b.y * f) >> 8u,
                 (a.z * (256 - f) + b.z * f) >> 8u);
}

// FIRE_LAYOUT: 0 => Uint16x4 (rect), 1 => Uint16x4 (geo), 2 => Uint32
// (phase), 3 => Uint8x4 (tsl).
#if defined(ATERM_VS_vs_fire)
flat ATERM_VARYING uvec4 v_geo;   // base_y, peak_h, cell_h, grid_top
flat ATERM_VARYING uint v_phase;
flat ATERM_VARYING uvec4 v_tsl;   // temp, strength, cov_cap, lean bits
#endif

#if defined(ATERM_VERTEX) && defined(ATERM_VS_vs_fire)
layout(location = 0) in uvec4 a_rect;
layout(location = 1) in uvec4 a_geo;
layout(location = 2) in uint a_phase;
layout(location = 3) in uvec4 a_tsl;
void vs_fire() {
    vec4 rect = vec4(a_rect);
    vec2 k = corner(uint(gl_VertexID));
    vec2 px = rect.xy + k * rect.zw;
    gl_Position = vec4(to_ndc(px), 0.0, 1.0);
    v_geo = a_geo;
    v_phase = a_phase;
    v_tsl = a_tsl;
}
#endif

// FireMode::Add: premultiplied field light; (palette*cov + 127)/255 per
// channel is the single rounding point (the CPU fire_shade_add bytes).
#if defined(ATERM_FRAGMENT) && defined(ATERM_FS_fs_fire_add)
void fs_fire_add() {
    int px = int(floor(gl_FragCoord.x));
    int py = int(floor(gl_FragCoord.y));
    // Sign-extend the i8 lean byte out of its u8 slot.
    int lean = int(v_tsl.w << 24u) >> 24u;
    int temp = int(v_tsl.x);
    FireCoreOut c = fire_core(px, py, int(v_geo.x), int(v_geo.y), v_phase,
                              temp, int(v_tsl.y), lean, int(v_geo.z));
    // Coverage law == fire_cov_add: AA edge x temp density x pockets x root.
    int dens = 150 + ((temp * 106) >> 8u);
    int bodyc = 110 + ((c.body * 146) >> 8u);
    int cov = min((((((c.edge * dens) >> 8u) * bodyc) >> 8u) * c.root) >> 8u,
                  int(v_tsl.z));
    // TOP-EDGE FADE == CPU fire_top_fade: dissolve toward the grid top.
    int fade_px = max(int(v_geo.z) * 2, 2);
    int ttop = clamp(((py - int(v_geo.w)) * 255) / fade_px, 0, 255);
    int covf = (cov * fire_fade(ttop)) / 255;
    // Rim cooling: the outline drops toward deep red, the core stays hot.
    int idx = (c.idx * (256 - ((c.rim * 112) >> 8u))) >> 8u;
    ivec3 rgb = fire_pal_add(idx);
    float r = float((rgb.x * covf + 127) / 255) / 255.0;
    float g = float((rgb.y * covf + 127) / 255) / 255.0;
    float b = float((rgb.z * covf + 127) / 255) / 255.0;
    o_frag = vec4(r, g, b, 1.0);
}
#endif

// FireMode::Over: straight ink plus field alpha through the source-over blend.
#if defined(ATERM_FRAGMENT) && defined(ATERM_FS_fs_fire_over)
void fs_fire_over() {
    int px = int(floor(gl_FragCoord.x));
    int py = int(floor(gl_FragCoord.y));
    int lean = int(v_tsl.w << 24u) >> 24u;
    int temp = int(v_tsl.x);
    FireCoreOut c = fire_core(px, py, int(v_geo.x), int(v_geo.y), v_phase,
                              temp, int(v_tsl.y), lean, int(v_geo.z));
    // Alpha law == fire_alpha_over: pooled rims, dense root, wispy tips.
    int bodyc = 130 + ((c.body * 166) >> 8u);
    int tipf = 120 + (((256 - c.q) * 136) >> 8u);
    int pool = 256 + ((c.rim * 96) >> 8u);
    int a = min((((((((c.edge * bodyc) >> 8u) * c.root) >> 8u) * tipf) >> 8u) * pool) >> 8u,
                int(v_tsl.z));
    int fade_px = max(int(v_geo.z) * 2, 2);
    int ttop = clamp(((py - int(v_geo.w)) * 255) / fade_px, 0, 255);
    int af = (a * fire_fade(ttop)) / 255;
    // Rim darkening: ink pools at the outline.
    int idx = (c.idx * (256 - ((c.rim * 128) >> 8u))) >> 8u;
    ivec3 rgb = fire_pal_over(idx);
    o_frag = vec4(float(rgb.x) / 255.0, float(rgb.y) / 255.0, float(rgb.z) / 255.0,
                  float(af) / 255.0);
}
#endif

// ---------------------------------------------------------------- GLYPHS
// GLYPH_LAYOUT: 0 => Float32x4 (rect), 1 => Float32x4 (uv), 2 => Unorm8x4
// (colour), 3 => Unorm8x4 (bg).
#if defined(ATERM_VS_vs_glyph)
ATERM_VARYING vec2 v_uv;
ATERM_VARYING vec4 v_color;
ATERM_VARYING vec4 v_bg;
#endif

#if defined(ATERM_VERTEX) && defined(ATERM_VS_vs_glyph)
layout(location = 0) in vec4 a_rect;
layout(location = 1) in vec4 a_uv;
layout(location = 2) in vec4 a_color;
layout(location = 3) in vec4 a_bg;
void vs_glyph() {
    vec2 k = corner(uint(gl_VertexID));
    vec2 px = a_rect.xy + k * a_rect.zw;
    gl_Position = vec4(to_ndc(px), 0.0, 1.0);
    v_uv = a_uv.xy + k * a_uv.zw;
    v_color = a_color;
    v_bg = a_bg;
}
#endif

// TEXT glyphs, with the W2 linear-corrected coverage remap gated exactly like
// the CPU correction_applies (interior coverage, a non-degenerate luminance
// gap: 0.001 == aterm_render::TEXT_BLEND_EPS).
#if defined(ATERM_FRAGMENT) && defined(ATERM_FS_fs_glyph)
void fs_glyph() {
    float cov = texture(aterm_t0, v_uv).r;
    vec3 fg_lin = s2l(v_color.rgb);
    if (u_text_blend != 0.0 && cov > 0.0 && cov < 1.0) {
        vec3 lum = vec3(0.2126, 0.7152, 0.0722);
        float fg_l = dot(fg_lin, lum);
        float bg_l = dot(s2l(v_bg.rgb), lum);
        if (abs(fg_l - bg_l) >= 0.001) {
            float blend_l = s2l_s(l2s_s(fg_l) * cov + l2s_s(bg_l) * (1.0 - cov));
            cov = clamp((blend_l - bg_l) / (fg_l - bg_l), 0.0, 1.0);
        }
    }
    o_frag = vec4(fg_lin, cov);
}
#endif

// Colour-emoji glyphs: the atlas holds the CPU renderer's final RGBA pixels.
#if defined(ATERM_FRAGMENT) && defined(ATERM_FS_fs_glyph_color)
void fs_glyph_color() {
    vec4 c = texture(aterm_t0, v_uv);
    o_frag = vec4(s2l(c.rgb), c.a);
}
#endif

// Sparkle-word decorations, OVER: the coverage quantized to the CPU's byte
// lattice, round(cov*a*255)/255 == the CPU (cov*alpha + 127)/255.
#if defined(ATERM_FRAGMENT) && defined(ATERM_FS_fs_deco_over)
void fs_deco_over() {
    float cov = texture(aterm_t0, v_uv).r;
    float a = roundEven(cov * v_color.a * 255.0) / 255.0;
    o_frag = vec4(s2l(v_color.rgb), a);
}
#endif

// ADD (the profanity sparkle): premultiplied (rgb*a, a) into One/One.
#if defined(ATERM_FRAGMENT) && defined(ATERM_FS_fs_deco_add)
void fs_deco_add() {
    float cov = texture(aterm_t0, v_uv).r;
    float a = roundEven(cov * v_color.a * 255.0) / 255.0;
    o_frag = vec4(v_color.rgb * a, a);
}
#endif

// RGBA8 sprites: the tint and opacity multiplies quantized to 8 bits before
// the sRGB decode, reproducing the CPU stamp's intermediate byte.
#if defined(ATERM_FRAGMENT) && defined(ATERM_FS_fs_sprite_over)
void fs_sprite_over() {
    vec4 c = texture(aterm_t0, v_uv);
    vec3 tinted = roundEven(c.rgb * v_color.rgb * 255.0) / vec3(255.0);
    float a = roundEven(c.a * v_color.a * 255.0) / 255.0;
    o_frag = vec4(s2l(tinted), a);
}
#endif

// ---------------------------------------------------------------- FX LAYERS
// FX_LAYOUT: 0 => Uint16x4 (the slice rect), 1 => Sint16x2 (the item origin
// mod 2^16), 2 => Uint16x2 (a sprite's dw, dh), 3 => Uint16x4 (the atlas
// region; [w, 0, 0, 0] for Rect and Ramps), 4 => Uint32 (the absolute
// palette entry), 5 => Uint32 (flags), all integers. Pixel law F, the WGSL
// fs_fx / fs_fx_linear / fs_fx_store twins op for op (see their comment):
// flat operands, the fragment's own pixel and the item's unclamped origin,
// texels read with texelFetch and their bytes recovered with roundEven.
//
// BINDINGS (BindSpec::FX): the Index8 atlas is aterm_t0, the RGBA atlas
// aterm_t1, and the frame palette aterm_t2: GLSL ES 3.00 has no storage
// buffer, so the palette is an RGBA8 texture 1024 entries wide (entry j at
// (j % 1024, j / 1024)) from the arm's ring of three, one per frame.
#if defined(ATERM_VS_vs_fx)
flat ATERM_VARYING ivec2 v_origin;
flat ATERM_VARYING uvec2 v_dst;
flat ATERM_VARYING uvec4 v_src;
flat ATERM_VARYING uint v_pal;
flat ATERM_VARYING uint v_flags;
#endif

#if defined(ATERM_VERTEX) && defined(ATERM_VS_vs_fx)
layout(location = 0) in uvec4 a_rect;
layout(location = 1) in ivec2 a_origin;
layout(location = 2) in uvec2 a_dst;
layout(location = 3) in uvec4 a_src;
layout(location = 4) in uint a_pal;
layout(location = 5) in uint a_flags;
void vs_fx() {
    vec4 rect = vec4(a_rect);
    vec2 k = corner(uint(gl_VertexID));
    vec2 px = rect.xy + k * rect.zw;
    gl_Position = vec4(to_ndc(px), 0.0, 1.0);
    v_origin = a_origin;
    v_dst = a_dst;
    v_src = a_src;
    v_pal = a_pal;
    v_flags = a_flags;
}
#endif

#if defined(ATERM_FRAGMENT) && (defined(ATERM_FS_fs_fx) || defined(ATERM_FS_fs_fx_linear) || defined(ATERM_FS_fs_fx_store))
uniform highp sampler2D aterm_t1;
uniform highp sampler2D aterm_t2;

// Palette entry j as bytes (r, g, b, a).
uvec4 fx_ink(uint j) {
    return uvec4(roundEven(texelFetch(aterm_t2, ivec2(int(j % 1024u), int(j / 1024u)), 0)
                           * 255.0));
}

// The stored byte of each Unorm8 channel.
uvec4 fx_byte4(vec4 v) {
    return uvec4(roundEven(v * 255.0));
}

// The item-local pixel l = (p - O) mod 2^16.
uvec2 fx_local() {
    ivec2 p = ivec2(floor(gl_FragCoord.xy));
    return uvec2(p - v_origin) & uvec2(65535u);
}

// The sprite texel under l: t = (l.x*w / dw, l.y*h / dh), mirrored under
// flip_x, offset to the art's atlas region.
ivec2 fx_texel(uvec2 l) {
    uvec2 t = uvec2(l.x * v_src.z / v_dst.x, l.y * v_src.w / v_dst.y);
    if ((v_flags & 4u) != 0u) {
        t.x = v_src.z - 1u - t.x;
    }
    return ivec2(v_src.xy + t);
}

// A Ramps item's palette row for its row y: the rows src.y + 1 ..= src.y +
// src.z repeat row src.y's ends (a run, one light as tall as its rows), which
// the palette holds once.
uint fx_ramp_row(uint y, uvec4 src) {
    return y - min(y - min(y, src.y), src.z);
}
#endif

// The Exact law: (c, a) / 255 raw into src + dst*(1 - a) on the Unorm view.
#if defined(ATERM_FRAGMENT) && defined(ATERM_FS_fs_fx)
void fs_fx() {
    uvec2 l = fx_local();
    uint kind = v_flags & 3u;
    uvec4 c;
    if (kind == 2u) {
        ivec2 t = fx_texel(l);
        if (((v_flags >> 3u) & 3u) == 0u) {
            // Index8: 0 writes nothing; k names entry ink + k - 1, and an
            // index past the palette writes nothing.
            uint k = fx_byte4(texelFetch(aterm_t0, t, 0)).x;
            uint reach = (v_flags >> 8u) & 255u;
            if (k == 0u || k > reach) {
                discard;
            }
            c = fx_ink(v_pal + clamp(k, 1u, reach) - 1u);
        } else {
            // Rgba8Premul: the texel is its own (c, a); a = 0 is light.
            c = fx_byte4(texelFetch(aterm_t1, t, 0));
        }
    } else {
        // Rect (row 0) and Ramps (row l.y's palette row).
        uint j = v_pal + 2u * (kind == 1u ? fx_ramp_row(l.y, v_src) : 0u);
        c = glow_lerp4(fx_ink(j), fx_ink(j + 1u), l.x, max(v_src.x, 1u));
    }
    o_frag = vec4(c) / vec4(255.0);
}
#endif

// The Exact law where every pixel the layer draws is opaque (route (1) of the
// pass economy): the entry's colour, stored on the sRGB view with no blend
// under the Color mask.
#if defined(ATERM_FRAGMENT) && defined(ATERM_FS_fs_fx_store)
void fs_fx_store() {
    uvec2 l = fx_local();
    uint kind = v_flags & 3u;
    uvec4 c;
    if (kind == 2u) {
        ivec2 t = fx_texel(l);
        if (((v_flags >> 3u) & 3u) == 0u) {
            uint k = fx_byte4(texelFetch(aterm_t0, t, 0)).x;
            uint reach = (v_flags >> 8u) & 255u;
            if (k == 0u || k > reach) {
                discard;
            }
            c = fx_ink(v_pal + clamp(k, 1u, reach) - 1u);
        } else {
            c = fx_byte4(texelFetch(aterm_t1, t, 0));
        }
    } else {
        uint j = v_pal + 2u * (kind == 1u ? fx_ramp_row(l.y, v_src) : 0u);
        c = glow_lerp4(fx_ink(j), fx_ink(j + 1u), l.x, max(v_src.x, 1u));
    }
    o_frag = vec4(s2l(vec3(c.rgb) / vec3(255.0)), 1.0);
}
#endif

// The Linear law (FreeSprite's), on the sRGB view.
#if defined(ATERM_FRAGMENT) && defined(ATERM_FS_fs_fx_linear)
void fs_fx_linear() {
    uvec2 l = fx_local();
    uvec4 v = fx_byte4(texelFetch(aterm_t1, fx_texel(l), 0));
    uvec4 tint = fx_ink(v_pal);
    uvec4 p = (v * tint + uvec4(127u)) / uvec4(255u);
    if (p.w == 0u) {
        discard;
    }
    o_frag = vec4(s2l(vec3(p.xyz) / vec3(255.0)), float(p.w) / 255.0);
}
#endif
