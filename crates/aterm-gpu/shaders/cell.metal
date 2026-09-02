#include <metal_stdlib>
using namespace metal;

// text_blend != 0.0 => fs_glyph applies the W2 linear-corrected coverage remap.
//
// BINDING LAW: uniform blocks in vertex functions here say [[buffer(0)]], and
// that is safe ONLY because the instance stream is laid out at vertex-buffer
// index 30 (ffi.rs::INSTANCE_STREAM_SLOT, wgpu-hal's own deconfliction).
// [[stage_in]] attributes and constant buffers share ONE vertex argument
// table; a stream at 0 lands on top of this block and the draw completes
// having painted nothing. pipelines.rs's slot-scan test holds this file to it.
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

// sRGB-encoded channel -> linear-light (the proper piecewise curve). The base
// (OVER/REPLACE) passes render into an sRGB-typed target that re-encodes on
// store, so emitting linear here makes fixed-function blending composite in
// linear light, matching the CPU `blend`. Alpha/coverage are NOT sRGB and pass
// through. (The ADDITIVE glow/deco-add passes render to the Unorm view and must
// NOT decode.)
static inline float3 s2l(float3 c) {
    float3 lo = c / 12.92;
    float3 hi = pow((c + float3(0.055)) / 1.055, float3(2.4));
    return select(lo, hi, c > float3(0.04045));
}

// Scalar sRGB decode/encode for the W2 corrected-alpha remap — the SAME
// piecewise curves (and thresholds) as the CPU srgb_to_linear/linear_to_srgb.
static inline float s2l_s(float c) {
    if (c <= 0.04045) { return c / 12.92; }
    return pow((c + 0.055) / 1.055, 2.4);
}
static inline float l2s_s(float l) {
    if (l <= 0.0031308) { return 12.92 * l; }
    return 1.055 * pow(l, 1.0 / 2.4) - 0.055;
}

// ---------------------------------------------------------------- BG / GLOW
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

// Glow (LUMEN aurora, One/One additive) emits its premultiplied colour RAW (no
// sRGB decode). Same vertex path as fs_bg, different fragment.
fragment float4 fs_glow(BgVsOut in [[stage_in]]) {
    return in.color;
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
    // cx, cy, rx, ry (window px). Integers in stage_in MUST be flat in MSL,
    // which is exactly the WGSL @interpolate(flat). The value is per-INSTANCE
    // (identical across all 6 quad vertices), so the provoking-vertex rule
    // never enters into it.
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
    // Frag pos is the pixel CENTRE (px + 0.5); floor -> the integer pixel index
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

// HaloMode::Over radial VEIL: the SAME integer falloff weight, emitted as the
// STRAIGHT colour with the weight as the per-pixel ALPHA into the deco
// source-over blend state.
fragment float4 fs_rain_glow_over(RainVsOut in [[stage_in]]) {
    int wt = rain_weight(in.pos.xy, in.fall);
    // Per-pixel alpha CEILING (== CPU wt.min(halo_over_cap)): the veil's cap
    // rides the instance colour's ALPHA byte; 0 == uncapped (255), so every
    // historical veil (packed a == 0) is byte-identical.
    int cap = int(round(in.color.a * 255.0));
    if (cap == 0) { cap = 255; }
    if (wt > cap) { wt = cap; }
    return float4(in.color.rgb, float(wt) / 255.0);
}

// ========================== EMBERFORGE FIRE ==========================
// The MSL twin of aterm_render::fire_field — OP-FOR-OP: every add, mul, shift,
// division and clamp below mirrors the Rust module exactly (pure int/uint math,
// wrapping uint semantics), so the CPU rasterizer's pixel and this fragment's
// byte agree everywhere (the fire-parity contract).
//
// SHIFT DISCIPLINE: several of these shift a possibly-NEGATIVE int right
// (notably `(body0 - 128) * edge >> 8`). WGSL defines i32 >> as ARITHMETIC
// (sign-replicating), and so does MSL — Metal's integers are two's-complement
// and >> on a signed type is an arithmetic shift. That equality is what keeps
// the negative-body case identical; it is NOT the C++ implementation-defined
// case, because MSL pins it.

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
    // ROOT SKIRT: below the root the envelope mirrors, compressed 5x.
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

// FireMode::Add — premultiplied field light on the One/One Unorm view:
// (palette*cov + 127)/255 per channel is the SINGLE rounding point.
// Shared shading tail == CPU fire_shade_add: coverage law, top fade, rim cool,
// palette, and the single (ch*cov + 127)/255 rounding. Returns packed 0xRRGGBB
// so the parity differential can compare the exact CPU u32.
static inline uint fire_shade_add_px(FireCoreOut c, int py, int temp, int strength,
                                     int cov_cap, int cell_h, int top_fade_y) {
    (void)strength;
    // Coverage law == fire_cov_add: AA edge x temp density x pockets x root.
    int dens = 150 + ((temp * 106) >> 8);
    int bodyc = 110 + ((c.body * 146) >> 8);
    int cov0 = min((((((c.edge * dens) >> 8) * bodyc) >> 8) * c.root) >> 8, cov_cap);
    // TOP-EDGE FADE == CPU fire_top_fade. The CPU short-circuits tf == 255 to
    // cov0; (cov0 * 255) / 255 == cov0 exactly, so this unconditional form is
    // byte-identical (fire_fade(255) == 255).
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

// FireMode::Over — straight ink + field alpha through the deco source-over
// blend state on the same Unorm view.
// Shared shading tail == CPU fire_shade_over. Returns (packed 0xRRGGBB ink,
// alpha 0..255) in a uint2 so the parity differential compares both exactly.
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

// FireMode::Over — straight ink + field alpha through the deco source-over
// blend state on the same Unorm view.
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
// correction_applies: interior coverage only, non-degenerate luminance gap only
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

// Colour-emoji glyphs: the atlas already holds the CPU renderer's final,
// cell-sized RGBA pixels, so we blit them straight through.
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
    // Quantized to the CPU's intermediate byte lattice — the wgsl twin's law
    // (fs_deco_over): round(cov*a*255)/255 == the CPU (cov*alpha+127)/255.
    float a = rint(cov * in.color.a * 255.0) / 255.0;
    return float4(s2l(in.color.rgb), a);
}

// ADD (the profanity sparkle): output PREMULTIPLIED (rgb*a, a) into the One/One
// additive pipeline => dst + rgb*a == the CPU add_sat(dst, premul_rgb(color, a)).
fragment float4 fs_deco_add(GlyphVsOut in [[stage_in]],
                            texture2d<float> atlas_tex [[texture(0)]],
                            sampler atlas_samp [[sampler(0)]]) {
    float cov = atlas_tex.sample(atlas_samp, in.uv).r;
    // Quantized to the CPU's intermediate byte lattice — see fs_deco_over.
    float a = rint(cov * in.color.a * 255.0) / 255.0;
    return float4(in.color.rgb * a, a);
}

// RGBA8 sprites use a per-instance multiply tint and opacity. The tint multiply
// and the opacity multiply are QUANTIZED to 8 bits with round-half BEFORE the
// sRGB decode, exactly reproducing the CPU stamp's intermediate byte.
fragment float4 fs_sprite_over(GlyphVsOut in [[stage_in]],
                               texture2d<float> atlas_tex [[texture(0)]],
                               sampler atlas_samp [[sampler(0)]]) {
    float4 c = atlas_tex.sample(atlas_samp, in.uv);
    float3 tinted = round(c.rgb * in.color.rgb * 255.0) / 255.0;
    float a = round(c.a * in.color.a * 255.0) / 255.0;
    return float4(s2l(tinted), a);
}
