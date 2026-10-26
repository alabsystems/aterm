#include <metal_stdlib>
using namespace metal;

// text_blend != 0.0 => fs_glyph applies the linear-corrected coverage remap.
//
// Vertex functions here take uniforms at [[buffer(0)]], safe only because the
// instance stream sits at vertex-buffer index 30 (ffi.rs::INSTANCE_STREAM_SLOT,
// wgpu-hal's own deconfliction): [[stage_in]] attributes and constant buffers share
// one vertex argument table, and a stream at 0 overwrites this block so the draw
// paints nothing. pipelines.rs's slot-scan test enforces it.
struct Uniforms { float2 screen; float text_blend; float pad; };

// Unit quad corner for vertex index 0..6 (two CCW triangles).
static inline float2 corner(uint vi) {
    const float2 c[6] = {
        float2(0.0, 0.0), float2(1.0, 0.0), float2(0.0, 1.0),
        float2(1.0, 0.0), float2(1.0, 1.0), float2(0.0, 1.0)
    };
    return c[vi];
}

// Pixel coords (top-left origin, y down) -> clip space (y up, row 0 at top).
static inline float2 to_ndc(float2 px, constant Uniforms& u) {
    return float2(2.0 * px.x / u.screen.x - 1.0, 1.0 - 2.0 * px.y / u.screen.y);
}

// sRGB-encoded channel -> linear light. The base (OVER/REPLACE) passes render into
// an sRGB view that re-encodes on store, so emitting linear makes fixed-function
// blending composite in linear light like the CPU `blend`. Alpha and coverage pass
// through; the additive passes render to the Unorm view and must not decode.
static inline float3 s2l(float3 c) {
    float3 lo = c / 12.92;
    float3 hi = pow((c + float3(0.055)) / 1.055, float3(2.4));
    return select(lo, hi, c > float3(0.04045));
}

// Scalar sRGB decode/encode for the text_blend coverage remap: the same curves and
// thresholds as the CPU srgb_to_linear/linear_to_srgb.
static inline float s2l_s(float c) {
    if (c <= 0.04045) { return c / 12.92; }
    return pow((c + 0.055) / 1.055, 2.4);
}
static inline float l2s_s(float l) {
    if (l <= 0.0031308) { return 12.92 * l; }
    return 1.055 * pow(l, 1.0 / 2.4) - 0.055;
}

// ---------------------------------------------------------------- BG
// BG_ATTRS: 0 => Uint16x4 (rect), 1 => Unorm8x4 (colour, hardware-normalized).
struct BgVsIn {
    ushort4 rect_u [[attribute(0)]];
    float4  color  [[attribute(1)]];
};

struct BgVsOut {
    float4 pos [[position]];
    float4 color;
};

vertex BgVsOut vs_bg(uint vi [[vertex_id]],
                     BgVsIn vin [[stage_in]],
                     constant Uniforms& u [[buffer(0)]]) {
    // Uint16x4 arrives as ushort4; integer pixel coords -> exact f32 (no loss).
    float4 rect = float4(vin.rect_u);
    float2 k = corner(vi);
    float2 px = rect.xy + k * rect.zw;
    BgVsOut o;
    o.pos = float4(to_ndc(px, u), 0.0, 1.0);
    o.color = vin.color;
    return o;
}

fragment float4 fs_bg(BgVsOut in [[stage_in]]) {
    return float4(s2l(in.color.rgb), in.color.a);
}

// ---------------------------------------------------------------- GLOW
// GLOW_ATTRS: 0 => Uint16x4 (rect), 1 => Uint8x4 (left colour), 2 => Uint8x4
// (right colour); both colours are raw bytes, not normalized.
//
// A GlowInstance is the bg rect plus a colour pair for the quad's left and right
// edges (aterm_render::GlowQuad::color2). The fragment ramps between them per
// column with the CPU's integer law (aterm_render::glow_lerp), alpha included:
//
//     c(i) = (c0·(2w − m) + c1·m + w) / (2w),   m = 2i + 1
//
// A flat pair is the identity for every i. The rasterizer's float interpolant is
// not used: its rounding is not the CPU's, and byte parity is the contract. The
// fragment emits c / 255.0 raw (no sRGB decode) into the glow blend
// src + dst·(1 − src_a), which rounds once on store.
struct GlowVsIn {
    ushort4 rect_u [[attribute(0)]];
    uchar4  c0     [[attribute(1)]];
    uchar4  c1     [[attribute(2)]];
};

struct GlowVsOut {
    float4 pos [[position]];
    // The quad's left edge and width in device px, as whole numbers.
    float2 span [[flat]];
    // The fragment's x inside the quad, in the quad's own px (0 at its left edge,
    // its width at its right). Not `pos.x - span.x`: the same pipeline draws the
    // bloom extract into the half-res target under the full-res uniforms, where
    // `pos.x` is a half-res pixel and `span.x` a full-res one.
    float lx [[center_no_perspective]];
    // The colour pair as exact bytes (r, g, b, a): premultiplied, a the mode.
    uint4 c0 [[flat]];
    uint4 c1 [[flat]];
};

vertex GlowVsOut vs_glow(uint vi [[vertex_id]],
                         GlowVsIn vin [[stage_in]],
                         constant Uniforms& u [[buffer(0)]]) {
    float4 rect = float4(vin.rect_u);
    float2 k = corner(vi);
    float2 px = rect.xy + k * rect.zw;
    GlowVsOut o;
    o.pos = float4(to_ndc(px, u), 0.0, 1.0);
    o.span = rect.xz;
    o.lx = k.x * rect.z;
    o.c0 = uint4(vin.c0);
    o.c1 = uint4(vin.c1);
    return o;
}

// The fragment's 0-based column inside its quad, from its x in the quad's own
// px. At full res the pixel centre interpolates to i + 0.5, so the floor is i
// with half a pixel of margin either side; at half res each fragment reads the
// quad's own column under its centre. The clamp only guards the edges.
static inline uint glow_column(float lx, float w) {
    return uint(clamp(floor(lx), 0.0, max(w - 1.0, 0.0)));
}

// floor(n / d) for d >= 1, as the GPU's emulated integer divide gives it but
// cheaper: a float estimate within one of the quotient, corrected once each
// way in integers. Exact for every divisor up to 2^17 and quotient up to 4096,
// the domain of its two callers (the parity kernel k_udiv_parity sweeps it).
static inline uint udiv_small(uint n, uint d) {
    uint q = uint(float(n) * (1.0 / float(d)));
    q -= uint(q * d > n);
    q += uint((q + 1u) * d <= n);
    return q;
}

// aterm_render::glow_lerp, per channel and for alpha alike, at column i of a
// w-wide quad (w >= 1: a zero-width quad rasterizes no fragment). Each
// channel's quotient is at most 255 and its numerator below 511 * 2^16.
static inline uint4 glow_lerp4(uint4 c0, uint4 c1, uint i, uint w) {
    uint m = 2u * min(i, w - 1u) + 1u;
    uint4 n = c0 * (2u * w - m) + c1 * m + uint4(w);
    uint d = 2u * w;
    return uint4(udiv_small(n.x, d), udiv_small(n.y, d), udiv_small(n.z, d), udiv_small(n.w, d));
}

// The quad's (c, a) at this fragment, as bytes.
static inline uint4 glow_src(GlowVsOut in) {
    uint w = max(uint(in.span.y), 1u);
    return glow_lerp4(in.c0, in.c1, glow_column(in.lx, in.span.y), w);
}

fragment float4 fs_glow(GlowVsOut in [[stage_in]]) {
    return float4(glow_src(in)) / 255.0;
}

// ---------------------------------------------------------------- GLOW ITEMS
// The bloom's lights as an item list (aterm_render::bloom::BloomItems, whose
// `light` is this reader's twin), so a ramp reaches the extract whole: 4 header
// words (the item count, the word the records start at, the word the ends start
// at, the instance count), each item's first instance, each item's record
// (x | y << 16, w | h << 16, join | joined << 16, its first end counted from the
// ends' start), then the ends (0x00RRGGBB | alpha << 24). Instance i draws row
// s = i - first of the last item whose first instance is at most i: the quad
// (x, y + s, w, 1), its run (s == join) joined + 1 rows tall and the rows after
// it joined lower, between ends 2s and 2s + 1, and nothing where both are zero.
// `widen` lifts a side under the half-res texel (BLOOM_DOWNSCALE, 2) to it and
// scales the colour by the share it covered, rounded, as the extract's copies of
// thin lights are. The list sits where an instance stream would
// (ffi.rs::INSTANCE_STREAM_SLOT), read by instance id.
struct GlowItem {
    uint4 rect;
    uint4 c0;
    uint4 c1;
};

// An end as bytes (r, g, b, a), its colour times num / den, rounded.
static inline uint4 glow_item_end(uint e, uint num, uint den) {
    uint3 c = ((uint3(e >> 16u, e >> 8u, e) & 0xFFu) * num + den / 2u) / den;
    return uint4(c, e >> 24u);
}

static inline GlowItem glow_item(device const uint* w, uint i, bool widen) {
    uint lo = 0u;
    uint hi = w[0] - 1u;
    while (lo < hi) {
        uint mid = (lo + hi + 1u) >> 1u;
        if (w[4u + mid] <= i) {
            lo = mid;
        } else {
            hi = mid - 1u;
        }
    }
    uint s = i - w[4u + lo];
    uint r = w[1] + 4u * lo;
    uint xy = w[r];
    uint join = w[r + 2u] & 0xFFFFu;
    uint joined = w[r + 2u] >> 16u;
    uint e = w[2] + w[r + 3u] + 2u * s;
    uint e0 = w[e];
    uint e1 = w[e + 1u];
    uint qw = w[r + 1u] & 0xFFFFu;
    uint qh = s == join ? joined + 1u : 1u;
    uint num = 1u;
    uint den = 1u;
    if (widen) {
        if (qw > 0u && qw < 2u) {
            num *= qw;
            den *= 2u;
            qw = 2u;
        }
        if (qh < 2u) {
            num *= qh;
            den *= 2u;
            qh = 2u;
        }
    }
    GlowItem it;
    it.rect = (e0 | e1) == 0u
        ? uint4(0u)
        : uint4(xy & 0xFFFFu, (xy >> 16u) + s + (s > join ? joined : 0u), qw, qh);
    it.c0 = glow_item_end(e0, num, den);
    it.c1 = glow_item_end(e1, num, den);
    return it;
}

// vs_glow over the item list, widened: the bloom extract's lights.
vertex GlowVsOut vs_glow_items(uint vi [[vertex_id]],
                               uint ii [[instance_id]],
                               device const uint* items [[buffer(30)]],
                               constant Uniforms& u [[buffer(0)]]) {
    GlowItem it = glow_item(items, ii, true);
    float4 rect = float4(it.rect);
    float2 k = corner(vi);
    float2 px = rect.xy + k * rect.zw;
    GlowVsOut o;
    o.pos = float4(to_ndc(px, u), 0.0, 1.0);
    o.span = rect.xz;
    o.lx = k.x * rect.z;
    o.c0 = it.c0;
    o.c1 = it.c1;
    return o;
}

// ---------------------------------------------------------------- RAIN HALO
// RAIN_GLOW_ATTRS: 0 => Uint16x4, 1 => Unorm8x4, 2 => Uint16x4.
struct RainVsIn {
    ushort4 rect_u [[attribute(0)]];
    float4  color  [[attribute(1)]];
    ushort4 fall_u [[attribute(2)]];
};

struct RainVsOut {
    float4 pos [[position]];
    float4 color;
    // cx, cy, rx, ry (window px). Integers in stage_in must be flat in MSL (the
    // WGSL @interpolate(flat)); the value is per instance, so the provoking
    // vertex never matters.
    uint4  fall [[flat]];
};

vertex RainVsOut vs_rain_glow(uint vi [[vertex_id]],
                              RainVsIn vin [[stage_in]],
                              constant Uniforms& u [[buffer(0)]]) {
    float4 rect = float4(vin.rect_u);
    float2 k = corner(vi);
    float2 px = rect.xy + k * rect.zw;
    RainVsOut o;
    o.pos = float4(to_ndc(px, u), 0.0, 1.0);
    o.color = vin.color;
    o.fall = uint4(vin.fall_u);
    return o;
}

// The elliptical integer falloff weight, shared by the Add and Over halos.
// Pure i32 math so it is byte-for-byte identical to the CPU draw_rain_add.
static inline int rain_weight(float2 fragpos, uint4 fall) {
    // Frag pos is the pixel centre (px + 0.5); floor -> the integer pixel index
    // the CPU walks.
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

// The Add halo's light at this fragment, as bytes.
static inline uint3 rain_add_src(RainVsOut in) {
    int wt = rain_weight(in.pos.xy, in.fall);
    // mul8(premul_channel, wt) == (c*wt + 127)/255 — matches CPU premul_rgb.
    int cr = int(round(in.color.r * 255.0));
    int cg = int(round(in.color.g * 255.0));
    int cb = int(round(in.color.b * 255.0));
    return uint3(uint((cr * wt + 127) / 255), uint((cg * wt + 127) / 255),
                 uint((cb * wt + 127) / 255));
}

fragment float4 fs_rain_glow(RainVsOut in [[stage_in]]) {
    return float4(float3(rain_add_src(in)) / 255.0, 1.0);
}

// The Over veil's alpha byte at this fragment.
static inline uint rain_over_alpha(RainVsOut in) {
    int wt = rain_weight(in.pos.xy, in.fall);
    // Per-pixel alpha ceiling (== CPU wt.min(halo_over_cap)), carried in the
    // instance colour's alpha byte; 0 means uncapped (255).
    int cap = int(round(in.color.a * 255.0));
    if (cap == 0) { cap = 255; }
    if (wt > cap) { wt = cap; }
    return uint(wt);
}

// HaloMode::Over veil: the same falloff weight, as the alpha of the straight
// colour, into the deco source-over blend.
fragment float4 fs_rain_glow_over(RainVsOut in [[stage_in]]) {
    return float4(in.color.rgb, float(rain_over_alpha(in)) / 255.0);
}

// ---------------------------------------------------------------- FIRE
// The MSL twin of aterm_render::fire_field, op for op (pure int/uint math,
// wrapping uint), so the CPU pixel and this fragment's byte agree everywhere.
// Several lines shift a possibly negative int right (`(body0 - 128) * edge >> 8`);
// WGSL and MSL both define signed >> as arithmetic, so that case matches too.

static inline uint fire_hash(uint x, uint y, uint seed) {
    uint h = x * 0x9E3779B1u + y * 0x85EBCA77u + seed * 0xC2B2AE3Du;
    h ^= h >> 15u;
    h = h * 0x2C1B3C6Du;
    h ^= h >> 12u;
    h = h * 0x297A2D39u;
    h ^= h >> 15u;
    return h;
}

static inline int fire_fade(int t) {
    return (t * t * (768 - 2 * t)) >> 16;
}

static inline int fire_vnoise(uint x, uint y, uint ymask, uint seed) {
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
    return (a * (256 - uy) + b * uy) >> 16;
}

struct FireCoreOut {
    int idx;
    int q;
    int edge;
    int body;
    int root;
    int rim;
};

static inline FireCoreOut fire_core(int px, int py, int base_y, int peak_h, uint phase,
                                    int temp, int strength, int lean, int cell_h) {
    FireCoreOut o;
    o.idx = 0; o.q = 256; o.edge = 0; o.body = 0; o.root = 0; o.rim = 0;
    int ch = max(cell_h, 2);
    uint chu = uint(ch);
    int peak = clamp(peak_h, 1, 2048);
    // Root skirt: below the root the envelope mirrors, compressed 5x.
    int v = base_y - py;
    if (v < 0) { v = -v * 5; }
    int vn = min((v * 256) / peak, 512);
    int shear = lean * vn / 4;
    uint xq = as_type<uint>(px * 256 + shear + 4194304);
    uint sx = (xq * 4u) / (5u * chu);
    uint ts = uint(300 + temp * 2);
    uint tr = (phase * ts) >> 10u;
    int n0 = fire_vnoise(sx, tr, 0x3FFFu, 0x51F0A3B7u);
    int n1 = fire_vnoise(sx * 2u + 12799u, tr * 2u + 37199u, 0x7FFFu, 0x9D2C5680u);
    int n = (n0 * 9 + n1 * 7) >> 4;
    int ridge = 255 - abs(2 * n - 255);
    int hs2 = (ridge * ridge) >> 8;
    int hshape = (hs2 * (256 + ridge)) >> 9;
    int hq = 30 + ((hshape * 260) >> 8);
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
    int body0 = (m0 * 4 + m1 * 3 + m2) >> 3;
    int body = 128 + (((body0 - 128) * edge) >> 8);
    int heat = ((256 - q) * (112 + ((temp * 120) >> 8))) >> 6;
    int idx0 = clamp((heat * (150 + ((body * 212) >> 8))) >> 8, 0, 850);
    int root = min((v * 1536) / ch, 256);
    o.idx = (idx0 * (192 + (root >> 2))) >> 8;
    o.q = q;
    o.edge = edge;
    o.body = body;
    o.root = root;
    o.rim = rim;
    return o;
}

static inline int3 fire_pal_add(int idx_in) {
    const int3 pal[5] = {
        int3(42, 0, 0), int3(139, 26, 0), int3(224, 74, 0),
        int3(255, 176, 32), int3(255, 240, 192)
    };
    // == FIRE_ADD_IDX_MAX: additive fire stops a breath past amber.
    int idx = min(idx_in, 780);
    int seg = clamp(idx >> 8, 0, 3);
    int f = idx - (seg << 8);
    int3 a = pal[seg];
    int3 b = pal[seg + 1];
    return int3((a.x * (256 - f) + b.x * f) >> 8,
                (a.y * (256 - f) + b.y * f) >> 8,
                (a.z * (256 - f) + b.z * f) >> 8);
}

static inline int3 fire_pal_over(int idx) {
    const int3 pal[5] = {
        int3(102, 24, 4), int3(168, 40, 0), int3(208, 70, 0),
        int3(238, 110, 0), int3(255, 166, 30)
    };
    int seg = clamp(idx >> 8, 0, 3);
    int f = idx - (seg << 8);
    int3 a = pal[seg];
    int3 b = pal[seg + 1];
    return int3((a.x * (256 - f) + b.x * f) >> 8,
                (a.y * (256 - f) + b.y * f) >> 8,
                (a.z * (256 - f) + b.z * f) >> 8);
}

// FIRE_ATTRS: 0 => Uint16x4 (rect), 1 => Uint16x4 (geo), 2 => Uint32 (phase),
// 3 => Uint8x4 (tsl).
struct FireVsIn {
    ushort4 rect_u [[attribute(0)]];
    ushort4 geo    [[attribute(1)]];
    uint    phase  [[attribute(2)]];
    uchar4  tsl    [[attribute(3)]];
};

struct FireVsOut {
    float4 pos [[position]];
    uint4 geo   [[flat]];   // base_y, peak_h, cell_h, grid_top
    uint  phase [[flat]];
    uint4 tsl   [[flat]];   // temp, strength, cov_cap, lean-bits
};

vertex FireVsOut vs_fire(uint vi [[vertex_id]],
                         FireVsIn vin [[stage_in]],
                         constant Uniforms& u [[buffer(0)]]) {
    float4 rect = float4(vin.rect_u);
    float2 k = corner(vi);
    float2 px = rect.xy + k * rect.zw;
    FireVsOut o;
    o.pos = float4(to_ndc(px, u), 0.0, 1.0);
    o.geo = uint4(vin.geo);
    o.phase = vin.phase;
    o.tsl = uint4(vin.tsl);
    return o;
}

// Shared shading tail == CPU fire_shade_add: coverage law, top fade, rim cool,
// palette, and the single (ch*cov + 127)/255 rounding. Returns packed 0xRRGGBB so
// the parity differential compares the exact CPU u32.
static inline uint fire_shade_add_px(FireCoreOut c, int py, int temp, int strength,
                                     int cov_cap, int cell_h, int top_fade_y) {
    (void)strength;
    // Coverage law == fire_cov_add: AA edge x temp density x pockets x root.
    int dens = 150 + ((temp * 106) >> 8);
    int bodyc = 110 + ((c.body * 146) >> 8);
    int cov0 = min((((((c.edge * dens) >> 8) * bodyc) >> 8) * c.root) >> 8, cov_cap);
    // Top-edge fade == CPU fire_top_fade, whose tf == 255 short-circuit this
    // unconditional form matches (fire_fade(255) == 255).
    int fade_px = max(cell_h * 2, 2);
    int ttop = clamp(((py - top_fade_y) * 255) / fade_px, 0, 255);
    int cov = (cov0 * fire_fade(ttop)) / 255;
    if (cov <= 0) { return 0u; }
    // Rim cooling: outline drops toward deep red, the core stays hot.
    int idx = (c.idx * (256 - ((c.rim * 112) >> 8))) >> 8;
    int3 rgb = fire_pal_add(idx);
    uint r = uint((rgb.x * cov + 127) / 255);
    uint g = uint((rgb.y * cov + 127) / 255);
    uint b = uint((rgb.z * cov + 127) / 255);
    return (r << 16) | (g << 8) | b;
}

// The Add field's light at this fragment, packed 0xRRGGBB.
static inline uint fire_add_src(FireVsOut in) {
    int px = int(floor(in.pos.x));
    int py = int(floor(in.pos.y));
    // Sign-extend the i8 lean byte out of its u8 slot.
    int lean = as_type<int>(in.tsl.w << 24u) >> 24;
    int temp = int(in.tsl.x);
    FireCoreOut c = fire_core(px, py, int(in.geo.x), int(in.geo.y), in.phase,
                              temp, int(in.tsl.y), lean, int(in.geo.z));
    return fire_shade_add_px(c, py, temp, int(in.tsl.y), int(in.tsl.z),
                             int(in.geo.z), int(in.geo.w));
}

// FireMode::Add — premultiplied field light on the One/One Unorm view.
fragment float4 fs_fire_add(FireVsOut in [[stage_in]]) {
    uint packed = fire_add_src(in);
    return float4(float((packed >> 16) & 0xFFu) / 255.0,
                  float((packed >> 8) & 0xFFu) / 255.0,
                  float(packed & 0xFFu) / 255.0, 1.0);
}

// Shared shading tail == CPU fire_shade_over. Returns (packed 0xRRGGBB ink,
// alpha 0..255) so the parity differential compares both exactly.
static inline uint2 fire_shade_over_px(FireCoreOut c, int py, int temp, int cov_cap,
                                       int cell_h, int top_fade_y) {
    (void)temp;
    // Alpha law == fire_alpha_over: pooled rims, dense root, wispy tips.
    int bodyc = 130 + ((c.body * 166) >> 8);
    int tipf = 120 + (((256 - c.q) * 136) >> 8);
    int pool = 256 + ((c.rim * 96) >> 8);
    int a0 = min((((((((c.edge * bodyc) >> 8) * c.root) >> 8) * tipf) >> 8) * pool) >> 8,
                 cov_cap);
    int fade_px = max(cell_h * 2, 2);
    int ttop = clamp(((py - top_fade_y) * 255) / fade_px, 0, 255);
    int a = (a0 * fire_fade(ttop)) / 255;
    if (a <= 0) { return uint2(0u, 0u); }
    // Rim darkening: ink pools at the outline (the watercolor edge law).
    int idx = (c.idx * (256 - ((c.rim * 128) >> 8))) >> 8;
    int3 rgb = fire_pal_over(idx);
    uint ink = (uint(rgb.x) << 16) | (uint(rgb.y) << 8) | uint(rgb.z);
    return uint2(ink, uint(clamp(a, 0, 255)));
}

// The Over field's (packed 0xRRGGBB ink, alpha) at this fragment.
static inline uint2 fire_over_src(FireVsOut in) {
    int px = int(floor(in.pos.x));
    int py = int(floor(in.pos.y));
    int lean = as_type<int>(in.tsl.w << 24u) >> 24;
    int temp = int(in.tsl.x);
    FireCoreOut c = fire_core(px, py, int(in.geo.x), int(in.geo.y), in.phase,
                              temp, int(in.tsl.y), lean, int(in.geo.z));
    return fire_shade_over_px(c, py, temp, int(in.tsl.z), int(in.geo.z), int(in.geo.w));
}

fragment float4 fs_fire_over(FireVsOut in [[stage_in]]) {
    uint2 io = fire_over_src(in);
    return float4(float((io.x >> 16) & 0xFFu) / 255.0,
                  float((io.x >> 8) & 0xFFu) / 255.0,
                  float(io.x & 0xFFu) / 255.0,
                  float(io.y) / 255.0);
}

// ---------------------------------------------------------------- GLYPHS
// GLYPH_ATTRS: 0 => Float32x4 (rect), 1 => Float32x4 (uv), 2 => Unorm8x4
// (colour), 3 => Unorm8x4 (bg).
struct GlyphVsIn {
    float4 rect  [[attribute(0)]];
    float4 uv    [[attribute(1)]];
    float4 color [[attribute(2)]];
    float4 bg    [[attribute(3)]];
};

struct GlyphVsOut {
    float4 pos [[position]];
    float2 uv;
    float4 color;
    float4 bg;
};

vertex GlyphVsOut vs_glyph(uint vi [[vertex_id]],
                           GlyphVsIn vin [[stage_in]],
                           constant Uniforms& u [[buffer(0)]]) {
    float2 k = corner(vi);
    float2 px = vin.rect.xy + k * vin.rect.zw;
    GlyphVsOut o;
    o.pos = float4(to_ndc(px, u), 0.0, 1.0);
    o.uv = vin.uv.xy + k * vin.uv.zw;
    o.color = vin.color;
    o.bg = vin.bg;
    return o;
}

// TEXT glyphs. W2 (u.text_blend != 0.0, the linear-corrected default): remap the
// coverage BEFORE the fixed-function sRGB-view blend so the blended LUMINANCE
// lands where a gamma-space blend would put it. Gated EXACTLY like the CPU
// blend_text: interior coverage only, non-degenerate luminance gap only
// (0.001 == aterm_render::TEXT_BLEND_EPS).
fragment float4 fs_glyph(GlyphVsOut in [[stage_in]],
                         texture2d<float> atlas_tex [[texture(0)]],
                         sampler atlas_samp [[sampler(0)]],
                         constant Uniforms& u [[buffer(0)]]) {
    float cov = atlas_tex.sample(atlas_samp, in.uv).r;
    float3 fg_lin = s2l(in.color.rgb);
    if (u.text_blend != 0.0 && cov > 0.0 && cov < 1.0) {
        float3 lum = float3(0.2126, 0.7152, 0.0722);
        float fg_l = dot(fg_lin, lum);
        float bg_l = dot(s2l(in.bg.rgb), lum);
        if (abs(fg_l - bg_l) >= 0.001) {
            float blend_l = s2l_s(l2s_s(fg_l) * cov + l2s_s(bg_l) * (1.0 - cov));
            cov = clamp((blend_l - bg_l) / (fg_l - bg_l), 0.0, 1.0);
        }
    }
    return float4(fg_lin, cov);
}

// Colour-emoji glyphs: the atlas holds the CPU renderer's final cell-sized RGBA,
// passed straight through.
fragment float4 fs_glyph_color(GlyphVsOut in [[stage_in]],
                               texture2d<float> atlas_tex [[texture(0)]],
                               sampler atlas_samp [[sampler(0)]]) {
    float4 c = atlas_tex.sample(atlas_samp, in.uv);
    return float4(s2l(c.rgb), c.a);
}

// Sparkle-word decorations, OVER (the feline cat-paw).
fragment float4 fs_deco_over(GlyphVsOut in [[stage_in]],
                             texture2d<float> atlas_tex [[texture(0)]],
                             sampler atlas_samp [[sampler(0)]]) {
    float cov = atlas_tex.sample(atlas_samp, in.uv).r;
    // Quantized to the CPU's byte lattice: round(cov*a*255)/255 == the CPU
    // (cov*alpha+127)/255, as in the WGSL fs_deco_over.
    float a = rint(cov * in.color.a * 255.0) / 255.0;
    return float4(s2l(in.color.rgb), a);
}

// ADD (the profanity sparkle): emit premultiplied (rgb*a, a) into the One/One
// pipeline, so dst + rgb*a == the CPU add_sat(dst, premul_rgb(color, a)).
// The sparkle's coverage times its alpha, on the CPU's intermediate byte
// lattice (see fs_deco_over), as a byte.
static inline uint deco_add_alpha(GlyphVsOut in, texture2d<float> atlas_tex,
                                  sampler atlas_samp) {
    float cov = atlas_tex.sample(atlas_samp, in.uv).r;
    return uint(rint(cov * in.color.a * 255.0));
}

fragment float4 fs_deco_add(GlyphVsOut in [[stage_in]],
                            texture2d<float> atlas_tex [[texture(0)]],
                            sampler atlas_samp [[sampler(0)]]) {
    float a = float(deco_add_alpha(in, atlas_tex, atlas_samp)) / 255.0;
    return float4(in.color.rgb * a, a);
}

// RGBA8 sprites: the per-instance tint and opacity multiplies are each quantized
// to 8 bits (round-half) before the sRGB decode, reproducing the CPU stamp's byte.
fragment float4 fs_sprite_over(GlyphVsOut in [[stage_in]],
                               texture2d<float> atlas_tex [[texture(0)]],
                               sampler atlas_samp [[sampler(0)]]) {
    float4 c = atlas_tex.sample(atlas_samp, in.uv);
    float3 tinted = round(c.rgb * in.color.rgb * 255.0) / 255.0;
    float a = round(c.a * in.color.a * 255.0) / 255.0;
    return float4(s2l(tinted), a);
}

// ---------------------------------------------------------------- FX LAYERS
// FX_LAYOUT: 0 => Uint16x4 (the slice rect), 1 => Sint16x2 (the item origin
// mod 2^16), 2 => Uint16x2 (a sprite's dw, dh), 3 => Uint16x4 (the atlas
// region; [w, 0, 0, 0] for Rect and Ramps), 4 => Uint32 (the absolute
// palette entry), 5 => Uint32 (flags: bits 0-1 kind 0 Rect / 1 Ramps / 2
// Sprite, bit 2 flip_x, bits 3-4 texels 0 Index8 / 1 Rgba8Premul / 2
// Rgba8Straight, bits 8-15 an Index8 sprite's reach).
//
// Pixel law F (design §4.2), byte for byte the WGSL `fs_fx` twin's (each
// divide by udiv_small): every operand reaches the fragment [[flat]], the
// fragment addresses from its own pixel p = floor(pos.xy) and the item's
// UNCLAMPED origin, so a slice cut anywhere draws the bytes the CPU stamps.
// Texels are read by integer address (no sampler, no float UV) and their
// bytes recovered with rint (a stored byte k reads k/255). The origin is
// stored mod 2^16 and every slice lies inside its item's box, whose sides are
// below 2^16, so (p - O) mod 2^16 is the item-local pixel. Every palette and
// texel address is in range by construction (renderer fx_gpu.rs), including
// after a discard.
struct FxVsIn {
    ushort4 rect_u [[attribute(0)]];
    short2  origin [[attribute(1)]];
    ushort2 dst    [[attribute(2)]];
    ushort4 src    [[attribute(3)]];
    uint    pal    [[attribute(4)]];
    uint    flags  [[attribute(5)]];
};

struct FxVsOut {
    float4 pos [[position]];
    int2  origin [[flat]];
    uint2 dst    [[flat]];
    uint4 src    [[flat]];
    uint  pal    [[flat]];
    uint  flags  [[flat]];
};

vertex FxVsOut vs_fx(uint vi [[vertex_id]],
                     FxVsIn vin [[stage_in]],
                     constant Uniforms& u [[buffer(0)]]) {
    float4 rect = float4(vin.rect_u);
    float2 k = corner(vi);
    float2 px = rect.xy + k * rect.zw;
    FxVsOut o;
    o.pos = float4(to_ndc(px, u), 0.0, 1.0);
    o.origin = int2(vin.origin);
    o.dst = uint2(vin.dst);
    o.src = uint4(vin.src);
    o.pal = vin.pal;
    o.flags = vin.flags;
    return o;
}

// Palette entry j as bytes (r, g, b, a): each entry's RGBA bytes read
// little-endian. The palette is the frame slot's own stream (never a shared
// texture), bound at fragment [[buffer(2)]] (BindSpec::FX).
static inline uint4 fx_ink(device const uint* pal, uint j) {
    uint e = pal[j];
    return uint4(e & 255u, (e >> 8u) & 255u, (e >> 16u) & 255u, e >> 24u);
}

// The stored byte of each Unorm8 channel.
static inline uint4 fx_byte4(float4 v) {
    return uint4(rint(v * 255.0));
}

// The item-local pixel l = (p - O) mod 2^16.
static inline uint2 fx_local(float4 pos, int2 origin) {
    int2 p = int2(floor(pos.xy));
    return uint2(p - origin) & uint2(65535u);
}

// The sprite texel under l: t = (l.x*w / dw, l.y*h / dh), mirrored under
// flip_x, offset to the art's atlas region. l < (dw, dh) and an art side is
// at most the fx atlas's 4096, so each quotient is below the art's side and
// each numerator below 2^28.
static inline uint2 fx_texel(uint2 l, uint2 dst, uint4 src, uint flags) {
    uint2 t = uint2(udiv_small(l.x * src.z, dst.x), udiv_small(l.y * src.w, dst.y));
    if ((flags & 4u) != 0u) {
        t.x = src.z - 1u - t.x;
    }
    return src.xy + t;
}

// A Ramps item's palette row for its row y: the rows src.y + 1 ..= src.y +
// src.z repeat row src.y's ends (a run, one light as tall as its rows), which
// the palette holds once.
static inline uint fx_ramp_row(uint y, uint4 src) {
    return y - min(y - min(y, src.y), src.z);
}

// Law F's (c, a) at this fragment, as bytes, or false where the item writes
// nothing.
static inline bool fx_paint(FxVsOut in, texture2d<float> fx_r8,
                            texture2d<float> fx_rgba, device const uint* pal,
                            thread uint4& c) {
    uint2 l = fx_local(in.pos, in.origin);
    uint kind = in.flags & 3u;
    if (kind == 2u) {
        uint2 t = fx_texel(l, in.dst, in.src, in.flags);
        if (((in.flags >> 3u) & 3u) == 0u) {
            // Index8: 0 writes nothing; k names entry ink + k - 1, and an
            // index past the palette writes nothing.
            uint k = fx_byte4(fx_r8.read(t)).x;
            uint reach = (in.flags >> 8u) & 255u;
            if (k == 0u || k > reach) {
                return false;
            }
            c = fx_ink(pal, in.pal + k - 1u);
        } else {
            // Rgba8Premul: the texel is its own (c, a); a = 0 is light.
            c = fx_byte4(fx_rgba.read(t));
        }
    } else {
        // Rect (row 0) and Ramps (row l.y's palette row): the GlowQuad ramp
        // over entries j, j + 1 at column l.x of the item's width.
        uint j = in.pal + 2u * (kind == 1u ? fx_ramp_row(l.y, in.src) : 0u);
        c = glow_lerp4(fx_ink(pal, j), fx_ink(pal, j + 1u), l.x, max(in.src.x, 1u));
    }
    return true;
}

// The Exact law: (c, a) / 255 raw into src + dst*(1 - a) on the Unorm view,
// fs_glow's state: over_premul, add_sat at a = 0.
fragment float4 fs_fx(FxVsOut in [[stage_in]],
                      texture2d<float> fx_r8 [[texture(0)]],
                      texture2d<float> fx_rgba [[texture(1)]],
                      device const uint* pal [[buffer(2)]]) {
    uint4 c = uint4(0u);
    if (!fx_paint(in, fx_r8, fx_rgba, pal, c)) {
        discard_fragment();
    }
    return float4(c) / 255.0;
}

// The Linear law (FreeSprite's): a straight texel under the tint entry,
// (mul8(rgb, tint.rgb), mul8(A, tint.a)) in exact integers, decoded and
// blended in linear light on the sRGB view (ALPHA_BLENDING).
fragment float4 fs_fx_linear(FxVsOut in [[stage_in]],
                             texture2d<float> fx_rgba [[texture(1)]],
                             device const uint* pal [[buffer(2)]]) {
    uint2 l = fx_local(in.pos, in.origin);
    uint4 v = fx_byte4(fx_rgba.read(fx_texel(l, in.dst, in.src, in.flags)));
    uint4 tint = fx_ink(pal, in.pal);
    uint4 p = (v * tint + uint4(127u)) / 255u;
    if (p.w == 0u) {
        discard_fragment();
    }
    return float4(s2l(float3(p.xyz) / 255.0), float(p.w) / 255.0);
}

// The Exact law where every pixel the layer draws is opaque (FxContent's
// draws_opaque, route (1) of the pass economy): over_premul at a = 255 is the
// entry's colour, so the draw stores it through the sRGB view with no blend,
// as fs_bg stores a cell's background, and the Color mask keeps alpha.
fragment float4 fs_fx_store(FxVsOut in [[stage_in]],
                            texture2d<float> fx_r8 [[texture(0)]],
                            texture2d<float> fx_rgba [[texture(1)]],
                            device const uint* pal [[buffer(2)]]) {
    uint4 c = uint4(0u);
    if (!fx_paint(in, fx_r8, fx_rgba, pal, c)) {
        discard_fragment();
    }
    return float4(s2l(float3(c.rgb) / 255.0), 1.0);
}

// ---------------------------------------------------------------- FRAMEBUFFER FETCH
// Compiled only for a device with framebuffer fetch (an Apple GPU), which
// metal/fetch.rs names by prepending `#define ATERM_FETCH 1`.
//
// The fetch twins draw a Unorm-law row inside the sRGB passes (design §5, lane
// B6), so a frame's additive and glow groups open no Unorm-view pass of their
// own. The attachment is the offscreen's sRGB view: [[color(0)]] reads the
// stored byte b decoded to linear light, fetch_bytes recovers b, the twin
// applies its row's fixed-function blend as the exact integer law the CPU
// rasterizer and the Unorm view compute (over_premul, add_sat, over_rgb), and
// fetch_store hands back the linear value whose sRGB store is that byte. Each
// twin runs with blending off and the row's Color write mask, so alpha is the
// stored alpha, as on the Unorm view. That both directions return every one of
// the 256 bytes, and that a twin fetching what an earlier fixed-function blend
// of its own pass left reads the byte the pass stores, are properties of the
// device's sRGB store and fetch, which metal/fetch.rs probes once per device
// (fs_fetch_probe_*) before any frame routes a draw here.
#if ATERM_FETCH

// linear -> sRGB, the CPU linear_to_srgb's curve per channel.
static inline float3 fetch_l2s(float3 l) {
    float3 lo = 12.92 * l;
    float3 hi = 1.055 * pow(l, float3(1.0 / 2.4)) - 0.055;
    return select(hi, lo, l <= float3(0.0031308));
}

// The stored bytes under a fetched linear colour.
static inline uint3 fetch_bytes(float3 lin) {
    return uint3(rint(saturate(fetch_l2s(saturate(lin))) * 255.0));
}

// The linear colour whose sRGB store is the bytes b.
static inline float3 fetch_store(uint3 b) {
    return s2l(float3(b) / 255.0);
}

// GLOW_OVER (One, OneMinusSrcAlpha) on the Unorm view: over_premul, which is
// add_sat at a = 0 and a store at a = 255.
static inline float4 fetch_over_premul(float4 dst, uint4 c) {
    uint3 d = fetch_bytes(dst.rgb);
    uint3 o = min(c.rgb + (d * (255u - c.a) + 127u) / 255u, uint3(255u));
    return float4(fetch_store(o), dst.a);
}

// ADDITIVE (One, One) on the Unorm view: add_sat.
static inline float4 fetch_add(float4 dst, uint3 c) {
    uint3 o = min(fetch_bytes(dst.rgb) + c, uint3(255u));
    return float4(fetch_store(o), dst.a);
}

// ALPHA_BLENDING's colour half (SrcAlpha, OneMinusSrcAlpha) on the Unorm
// view: over_rgb of the straight colour c at alpha a.
static inline float4 fetch_over_rgb(float4 dst, uint3 c, uint a) {
    uint3 d = fetch_bytes(dst.rgb);
    uint3 o = (c * a + d * (255u - a) + 127u) / 255u;
    return float4(fetch_store(o), dst.a);
}

fragment float4 fs_glow_fetch(GlowVsOut in [[stage_in]],
                              float4 dst [[color(0)]]) {
    return fetch_over_premul(dst, glow_src(in));
}

fragment float4 fs_rain_glow_fetch(RainVsOut in [[stage_in]],
                                   float4 dst [[color(0)]]) {
    return fetch_add(dst, rain_add_src(in));
}

fragment float4 fs_rain_glow_over_fetch(RainVsOut in [[stage_in]],
                                        float4 dst [[color(0)]]) {
    return fetch_over_rgb(dst, uint3(rint(in.color.rgb * 255.0)), rain_over_alpha(in));
}

fragment float4 fs_fire_add_fetch(FireVsOut in [[stage_in]],
                                  float4 dst [[color(0)]]) {
    uint p = fire_add_src(in);
    return fetch_add(dst, uint3((p >> 16) & 0xFFu, (p >> 8) & 0xFFu, p & 0xFFu));
}

fragment float4 fs_fire_over_fetch(FireVsOut in [[stage_in]],
                                   float4 dst [[color(0)]]) {
    uint2 io = fire_over_src(in);
    uint3 ink = uint3((io.x >> 16) & 0xFFu, (io.x >> 8) & 0xFFu, io.x & 0xFFu);
    return fetch_over_rgb(dst, ink, io.y);
}

// The sparkle's light is premul_rgb(colour, a): (c*a + 127)/255 per channel.
fragment float4 fs_deco_add_fetch(GlyphVsOut in [[stage_in]],
                                  texture2d<float> atlas_tex [[texture(0)]],
                                  sampler atlas_samp [[sampler(0)]],
                                  float4 dst [[color(0)]]) {
    uint a = deco_add_alpha(in, atlas_tex, atlas_samp);
    uint3 c = uint3(rint(in.color.rgb * 255.0));
    return fetch_add(dst, (c * a + 127u) / 255u);
}

fragment float4 fs_fx_fetch(FxVsOut in [[stage_in]],
                            texture2d<float> fx_r8 [[texture(0)]],
                            texture2d<float> fx_rgba [[texture(1)]],
                            device const uint* pal [[buffer(2)]],
                            float4 dst [[color(0)]]) {
    uint4 c = uint4(0u);
    if (!fx_paint(in, fx_r8, fx_rgba, pal, c)) {
        discard_fragment();
    }
    return fetch_over_premul(dst, c);
}

// The device probe's stages, over rows of 256 pixels drawn by vs_bg (Stages in
// metal/fetch.rs): pixel x carries byte x in red, 255 - x in green and
// x ^ 0x5A in blue, so each channel meets all 256 values.
static inline uint3 fetch_probe_bytes(float4 pos) {
    uint x = uint(floor(pos.x)) & 255u;
    return uint3(x, 255u - x, x ^ 0x5Au);
}

// Store: every byte's linear value stores as that byte.
fragment float4 fs_fetch_probe_store(BgVsOut in [[stage_in]]) {
    return float4(fetch_store(fetch_probe_bytes(in.pos)), 1.0);
}

// Round trip: every stored byte, fetched and stored again, is itself.
fragment float4 fs_fetch_probe_trip(BgVsOut in [[stage_in]],
                                    float4 dst [[color(0)]]) {
    return float4(fetch_store(fetch_bytes(dst.rgb)), dst.a);
}

// Blend: a colour per pixel over the row's bytes at an odd multiple of 1/256
// alpha (never 0 or 1), through the row's fixed-function straight-alpha over,
// so the tile holds values between the byte lattice's until the pass stores
// them. A trip after it in the same pass must fetch the byte the store gives.
fragment float4 fs_fetch_probe_blend(BgVsOut in [[stage_in]]) {
    uint x = uint(floor(in.pos.x)) & 255u;
    uint3 c = (uint3(x) * uint3(7u, 31u, 113u) + uint3(13u, 101u, 57u)) & 255u;
    float a = float(((x * 89u) & 254u) + 1u) / 256.0;
    return float4(fetch_store(c), a);
}

#endif
