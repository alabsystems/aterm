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

// aterm_render::glow_lerp, per channel and for alpha alike, at column i of a
// w-wide quad (w >= 1: a zero-width quad rasterizes no fragment).
static inline uint4 glow_lerp4(uint4 c0, uint4 c1, uint i, uint w) {
    uint m = 2u * min(i, w - 1u) + 1u;
    return (c0 * (2u * w - m) + c1 * m + uint4(w)) / (2u * w);
}

fragment float4 fs_glow(GlowVsOut in [[stage_in]]) {
    uint w = max(uint(in.span.y), 1u);
    uint4 c = glow_lerp4(in.c0, in.c1, glow_column(in.lx, in.span.y), w);
    return float4(c) / 255.0;
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

fragment float4 fs_rain_glow(RainVsOut in [[stage_in]]) {
    int wt = rain_weight(in.pos.xy, in.fall);
    // mul8(premul_channel, wt) == (c*wt + 127)/255 — matches CPU premul_rgb.
    int cr = int(round(in.color.r * 255.0));
    int cg = int(round(in.color.g * 255.0));
    int cb = int(round(in.color.b * 255.0));
    float r = float((cr * wt + 127) / 255) / 255.0;
    float g = float((cg * wt + 127) / 255) / 255.0;
    float b = float((cb * wt + 127) / 255) / 255.0;
    return float4(r, g, b, 1.0);
}

// HaloMode::Over veil: the same falloff weight, as the alpha of the straight
// colour, into the deco source-over blend.
fragment float4 fs_rain_glow_over(RainVsOut in [[stage_in]]) {
    int wt = rain_weight(in.pos.xy, in.fall);
    // Per-pixel alpha ceiling (== CPU wt.min(halo_over_cap)), carried in the
    // instance colour's alpha byte; 0 means uncapped (255).
    int cap = int(round(in.color.a * 255.0));
    if (cap == 0) { cap = 255; }
    if (wt > cap) { wt = cap; }
    return float4(in.color.rgb, float(wt) / 255.0);
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

static inline int3 fire_pal_add(int idx) {
    const int3 pal[5] = {
        int3(42, 0, 0), int3(139, 26, 0), int3(224, 74, 0),
        int3(255, 176, 32), int3(255, 240, 192)
    };
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

// FireMode::Add — premultiplied field light on the One/One Unorm view.
fragment float4 fs_fire_add(FireVsOut in [[stage_in]]) {
    int px = int(floor(in.pos.x));
    int py = int(floor(in.pos.y));
    // Sign-extend the i8 lean byte out of its u8 slot.
    int lean = as_type<int>(in.tsl.w << 24u) >> 24;
    int temp = int(in.tsl.x);
    FireCoreOut c = fire_core(px, py, int(in.geo.x), int(in.geo.y), in.phase,
                              temp, int(in.tsl.y), lean, int(in.geo.z));
    uint packed = fire_shade_add_px(c, py, temp, int(in.tsl.y), int(in.tsl.z),
                                    int(in.geo.z), int(in.geo.w));
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

fragment float4 fs_fire_over(FireVsOut in [[stage_in]]) {
    int px = int(floor(in.pos.x));
    int py = int(floor(in.pos.y));
    int lean = as_type<int>(in.tsl.w << 24u) >> 24;
    int temp = int(in.tsl.x);
    FireCoreOut c = fire_core(px, py, int(in.geo.x), int(in.geo.y), in.phase,
                              temp, int(in.tsl.y), lean, int(in.geo.z));
    uint2 io = fire_shade_over_px(c, py, temp, int(in.tsl.z), int(in.geo.z), int(in.geo.w));
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
fragment float4 fs_deco_add(GlyphVsOut in [[stage_in]],
                            texture2d<float> atlas_tex [[texture(0)]],
                            sampler atlas_samp [[sampler(0)]]) {
    float cov = atlas_tex.sample(atlas_samp, in.uv).r;
    // Quantized to the CPU's intermediate byte lattice — see fs_deco_over.
    float a = rint(cov * in.color.a * 255.0) / 255.0;
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
// Pixel law F (design §4.2), the WGSL `fs_fx` twin op for op: every operand
// reaches the fragment [[flat]], the fragment addresses from its own pixel
// p = floor(pos.xy) and the item's UNCLAMPED origin, so a slice cut anywhere
// draws the bytes the CPU stamps. Texels are read by integer address (no
// sampler, no float UV) and their bytes recovered with rint (a stored byte k
// reads k/255). The origin is stored mod 2^16 and every slice lies inside its
// item's box, whose sides are below 2^16, so (p - O) mod 2^16 is the
// item-local pixel. Every palette and texel address is in range by
// construction (renderer fx_gpu.rs), including after a discard.
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
// flip_x, offset to the art's atlas region.
static inline uint2 fx_texel(uint2 l, uint2 dst, uint4 src, uint flags) {
    uint2 t = uint2(l.x * src.z / dst.x, l.y * src.w / dst.y);
    if ((flags & 4u) != 0u) {
        t.x = src.z - 1u - t.x;
    }
    return src.xy + t;
}

// The Exact law: (c, a) / 255 raw into src + dst*(1 - a) on the Unorm view,
// fs_glow's state: over_premul, add_sat at a = 0.
fragment float4 fs_fx(FxVsOut in [[stage_in]],
                      texture2d<float> fx_r8 [[texture(0)]],
                      texture2d<float> fx_rgba [[texture(1)]],
                      device const uint* pal [[buffer(2)]]) {
    uint2 l = fx_local(in.pos, in.origin);
    uint kind = in.flags & 3u;
    uint4 c;
    if (kind == 2u) {
        uint2 t = fx_texel(l, in.dst, in.src, in.flags);
        if (((in.flags >> 3u) & 3u) == 0u) {
            // Index8: 0 writes nothing; k names entry ink + k - 1, and an
            // index past the palette writes nothing.
            uint k = fx_byte4(fx_r8.read(t)).x;
            uint reach = (in.flags >> 8u) & 255u;
            if (k == 0u || k > reach) {
                discard_fragment();
            }
            c = fx_ink(pal, in.pal + clamp(k, 1u, reach) - 1u);
        } else {
            // Rgba8Premul: the texel is its own (c, a); a = 0 is light.
            c = fx_byte4(fx_rgba.read(t));
        }
    } else {
        // Rect (row 0) and Ramps (row l.y): the GlowQuad ramp over entries
        // j, j + 1 at column l.x of the item's width.
        uint j = in.pal + 2u * (kind == 1u ? l.y : 0u);
        c = glow_lerp4(fx_ink(pal, j), fx_ink(pal, j + 1u), l.x, max(in.src.x, 1u));
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
