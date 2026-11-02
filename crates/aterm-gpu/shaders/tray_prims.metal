// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// GPU tray prims (docs/DESIGN-stats-deck-gpu-motion-2026-10-26.md §3.2): the vector subset of
// the chrome's `DrawPrim`s, each instance a quad over the device pixels it may ink, its
// coverage the CPU raster's own f32 formula (crates/aterm-gui/src/tray_raster.rs), and its
// composite that raster's exact law (`Canvas::composite_at`): straight sRGB bytes in, the
// linear-light source-over with the CPU's own decode table and quantizer thresholds, straight
// sRGB bytes out, rounded after each prim. The target is a Rgba8Unorm card read back through
// framebuffer fetch ([[color(0)]], Apple GPUs), so the stored bytes are the CPU's bytes and
// prims of one draw composite in submission order. Compiled with fast math off
// (metal/pipelines.rs), and every expression keeps the CPU's order of operations.

#include <metal_stdlib>
using namespace metal;

// The Rust `TrayPrim` (crates/aterm-gpu/src/tray_prims.rs), 64 bytes: every member naturally
// aligned at its Rust offset (short4 at 8, the floats from 16, the colour at 56).
struct TrayPrim {
    uint kind;
    uint offset;
    short4 rect;      // device px x0, y0, x1, y1 (half-open): the pixels the prim may ink,
                      // its offset's travel included
    float g[10];      // the kind's geometry, device px (tray_prims.rs `kind`)
    uchar4 color;     // straight RGBA8, sRGB-encoded
    uint pad;
};

// The Rust `TrayPrimsUniform`: the target's size and its top-left in device px, and the
// per-frame offsets an instance's `offset` selects (index 0 is none).
struct TrayPrimsU {
    float2 target;
    int2 origin;
    float2 offsets[8];
};

// The CPU's composite tables (tray_raster.rs `srgb_to_linear_lut`, `f32::from(b) / 255.0`, and
// `linear_to_srgb_u8`'s thresholds, padded with +inf to 256).
struct TrayPrimLaw {
    float to_linear[256];
    float unit[256];
    float thresholds[256];
};

constant uint KIND_ROUND_RECT = 1u;
constant uint KIND_RING_ARC = 2u;
constant uint KIND_ARC_BAND = 3u;
constant uint KIND_SEGMENT = 4u;
constant uint KIND_DISC = 5u;
constant uint KIND_STROKE = 6u;
constant uint KIND_FILL = 7u;

// `std::f32::consts::TAU`.
constant float TAU_F = 6.28318548202514648438f;

struct TrayPrimOut {
    float4 pos [[position]];
    uint index [[flat]];
};

// Unit quad as a 4-vertex strip over the instance's rect.
vertex TrayPrimOut vs_tray_prim(uint vi [[vertex_id]],
                                uint ii [[instance_id]],
                                constant TrayPrimsU& u [[buffer(2)]],
                                const device TrayPrim* prims [[buffer(30)]]) {
    const float2 corner[4] = {
        float2(0.0, 0.0), float2(1.0, 0.0),
        float2(0.0, 1.0), float2(1.0, 1.0)
    };
    TrayPrim p = prims[ii];
    float2 lo = float2(int2(p.rect.xy) - u.origin);
    float2 hi = float2(int2(p.rect.zw) - u.origin);
    hi = max(hi, lo);
    float2 px = lo + corner[vi] * (hi - lo);
    TrayPrimOut o;
    o.pos = float4(2.0 * px.x / u.target.x - 1.0, 1.0 - 2.0 * px.y / u.target.y, 0.0, 1.0);
    o.index = ii;
    return o;
}

#pragma clang fp contract(off)

// `cov`: a pixel's coverage from its signed distance to the edge.
static inline float tp_cov(float d) {
    return clamp(0.5f - d, 0.0f, 1.0f);
}

// `sd_round_box`.
static inline float tp_sd_round_box(float px, float py, float cx, float cy, float hx, float hy,
                                    float r) {
    float qx = abs(px - cx) - (hx - r);
    float qy = abs(py - cy) - (hy - r);
    float ax = max(qx, 0.0f);
    float ay = max(qy, 0.0f);
    return (precise::sqrt(ax * ax + ay * ay) + min(max(qx, qy), 0.0f)) - r;
}

// `f32::rem_euclid`.
static inline float tp_rem_euclid(float x, float m) {
    float r = precise::fmod(x, m);
    return r < 0.0f ? r + abs(m) : r;
}

// `f32::hypot`, as the sum's square root.
static inline float tp_hypot(float x, float y) {
    return precise::sqrt(x * x + y * y);
}

// `Canvas::round_rect_under` at pixel (ix, iy): 1 inside the inset rect's spans, else the rim's
// distance-field coverage.
static inline float tp_round_rect(thread const float* g, int ix, int iy) {
    float x = g[0], y = g[1], w = g[2], h = g[3];
    float cx = x + w / 2.0f, cy = y + h / 2.0f, hx = w / 2.0f, hy = h / 2.0f;
    float r = min(min(g[4], hx), hy);
    float inner_w = w - 1.0f * 2.0f;
    float inner_h = h - 1.0f * 2.0f;
    float inner_r = min(min(max(r - 1.0f, 0.0f), inner_w / 2.0f), inner_h / 2.0f);
    float inner_x = x + 1.0f;
    float inner_y = y + 1.0f;
    float pyc = float(iy) + 0.5f;
    if (inner_w > 0.0f && inner_h > 0.0f && pyc >= inner_y && pyc <= inner_y + inner_h) {
        float left, right;
        if (inner_r <= 0.0f || (pyc >= inner_y + inner_r && pyc <= inner_y + inner_h - inner_r)) {
            left = inner_x;
            right = inner_x + inner_w;
        } else {
            float corner_y = pyc < inner_y + inner_r ? inner_y + inner_r
                                                     : inner_y + inner_h - inner_r;
            float dy = abs(pyc - corner_y);
            float extent = precise::sqrt(max(inner_r * inner_r - dy * dy, 0.0f));
            left = inner_x + inner_r - extent;
            right = inner_x + inner_w - inner_r + extent;
        }
        int start = int(ceil(left - 0.5f));
        int end = int(floor(right - 0.5f)) + 1;
        if (start < end && ix >= start && ix < end) {
            return 1.0f;
        }
    }
    return tp_cov(tp_sd_round_box(float(ix) + 0.5f, pyc, cx, cy, hx, hy, r));
}

// `Canvas::arc` (a `Ring`'s track or fill) at pixel (ix, iy).
static inline float tp_ring_arc(thread const float* g, int ix, int iy) {
    float cx = g[0], cy = g[1], r_mid = g[2], thickness = g[3];
    float frac = clamp(g[4], 0.0f, 1.0f);
    float start = g[5];
    float dy = float(iy) + 0.5f - cy;
    float dx = float(ix) + 0.5f - cx;
    float rr = precise::sqrt(dx * dx + dy * dy);
    float radial = abs(rr - r_mid) - thickness / 2.0f;
    float cov_r = tp_cov(radial);
    if (cov_r <= 0.0f) {
        return 0.0f;
    }
    float cov_a = 1.0f;
    if (frac < 1.0f) {
        // Turns clockwise from `start`, as the CPU's `(a / TAU - start).rem_euclid(1.0)`.
        float a = tp_rem_euclid(tp_rem_euclid(precise::atan2(dx, -dy), TAU_F) / TAU_F - start, 1.0f);
        float d_frac = a <= frac ? -(min(a, frac - a)) : min(a - frac, 1.0f - a);
        float d_px = d_frac * TAU_F * max(rr, 1.0f);
        cov_a = tp_cov(d_px);
    }
    return cov_r * cov_a;
}

// `Canvas::arc_band` at pixel (ix, iy); the ends are the CPU's.
static inline float tp_arc_band(thread const float* g, int ix, int iy) {
    float cx = g[0], cy = g[1], r = g[2], half_w = g[3], from = g[4], span = g[5];
    float sx = float(ix) + 0.5f, sy = float(iy) + 0.5f;
    float dy = float(iy) + 0.5f - cy;
    float dx = sx - cx;
    float radial = abs(tp_hypot(dx, dy) - r);
    if (radial - half_w >= 0.5f) {
        return 0.0f;
    }
    float a = tp_rem_euclid(precise::atan2(dx, -dy) / TAU_F - from, 1.0f);
    float d;
    if (span >= 1.0f || a <= span) {
        d = radial;
    } else {
        d = min(min(FLT_MAX, tp_hypot(sx - g[6], sy - g[7])), tp_hypot(sx - g[8], sy - g[9]));
    }
    return tp_cov(d - half_w);
}

// `Canvas::line_segment` at pixel (ix, iy) (a zero-length one is a disc).
static inline float tp_segment(thread const float* g, int ix, int iy) {
    float x1 = g[0], y1 = g[1], vx = g[2], vy = g[3], length_squared = g[4], radius = g[5];
    float sample_x = float(ix) + 0.5f;
    float sample_y = float(iy) + 0.5f;
    float projection =
        clamp(fma(sample_x - x1, vx, (sample_y - y1) * vy) / length_squared, 0.0f, 1.0f);
    float nearest_x = x1 + projection * vx;
    float nearest_y = y1 + projection * vy;
    float dx = sample_x - nearest_x;
    float dy = sample_y - nearest_y;
    return tp_cov(precise::sqrt(fma(dx, dx, dy * dy)) - radius);
}

// `Canvas::disc` at pixel (ix, iy).
static inline float tp_disc(thread const float* g, int ix, int iy) {
    float dx = float(ix) + 0.5f - g[0];
    float dy = float(iy) + 0.5f - g[1];
    float d = precise::sqrt(dx * dx + dy * dy) - g[2];
    return tp_cov(d);
}

// `Canvas::stroke_rect`'s rounded arm at pixel (ix, iy).
static inline float tp_stroke(thread const float* g, int ix, int iy) {
    float d = abs(tp_sd_round_box(float(ix) + 0.5f, float(iy) + 0.5f, g[0], g[1], g[2], g[3],
                                  g[4])) - g[5];
    return tp_cov(d);
}

// `linear_to_srgb_u8`: how many thresholds `l` reaches.
static inline uint tp_quantize(constant TrayPrimLaw& law, float l) {
    l = clamp(l, 0.0f, 1.0f);
    uint lo = 0u, hi = 255u;
    while (lo < hi) {
        uint mid = (lo + hi) / 2u;
        if (l >= law.thresholds[mid]) {
            lo = mid + 1u;
        } else {
            hi = mid;
        }
    }
    return lo;
}

// `(v * 255.0).round().clamp(0.0, 255.0) as u8`.
static inline uint tp_alpha_byte(float v) {
    return uint(clamp(round(v * 255.0f), 0.0f, 255.0f));
}

// `Canvas::blend` then `Canvas::composite_at` of colour `c` at coverage `cov` over the stored
// bytes `d`.
static inline uint4 tp_composite(constant TrayPrimLaw& law, uint4 d, uint4 c, float cov) {
    cov = clamp(cov, 0.0f, 1.0f);
    float a = law.unit[c.a] * cov;
    if (a <= 0.0f) {
        return d;
    }
    if (c.a == 255u && cov >= 1.0f) {
        return c;
    }
    float da = law.unit[d.a];
    if (cov >= 1.0f && all(d.rgb == c.rgb)) {
        float out_a = a + da * (1.0f - a);
        return uint4(d.rgb, tp_alpha_byte(out_a));
    }
    float out_a = a + da * (1.0f - a);
    uint4 o = d;
    if (out_a > 0.0f) {
        for (uint k = 0u; k < 3u; k++) {
            float s_lin = law.to_linear[c[k]];
            float d_lin = law.to_linear[d[k]];
            float out_lin = fma(s_lin, a, d_lin * da * (1.0f - a)) / out_a;
            o[k] = tp_quantize(law, out_lin);
        }
    }
    o.a = tp_alpha_byte(out_a);
    return o;
}

// The stored bytes under a fetched Unorm colour.
static inline uint4 tp_bytes(float4 v) {
    return uint4(rint(saturate(v) * 255.0f));
}

fragment float4 fs_tray_prim(TrayPrimOut in [[stage_in]],
                             constant TrayPrimsU& u [[buffer(2)]],
                             constant TrayPrimLaw& law [[buffer(1)]],
                             const device TrayPrim* prims [[buffer(3)]],
                             float4 dst [[color(0)]]) {
    TrayPrim p = prims[in.index];
    uint4 d = tp_bytes(dst);
    int2 at = int2(floor(in.pos.xy)) + u.origin;
    float2 off = u.offsets[min(p.offset, 7u)];
    // A moved prim inks the pixel its geometry covers `off` back.
    float g[10];
    for (uint k = 0u; k < 10u; k++) {
        g[k] = p.g[k];
    }
    if (p.offset != 0u) {
        // Every kind's first two values are a point; the band and segment ends follow.
        g[0] += off.x;
        g[1] += off.y;
        if (p.kind == KIND_FILL) {
            g[2] += off.x;
            g[3] += off.y;
        } else if (p.kind == KIND_ARC_BAND) {
            g[6] += off.x;
            g[7] += off.y;
            g[8] += off.x;
            g[9] += off.y;
        }
    }
    float c = 0.0f;
    switch (p.kind) {
        case KIND_ROUND_RECT: c = tp_round_rect(g, at.x, at.y); break;
        case KIND_RING_ARC: c = tp_ring_arc(g, at.x, at.y); break;
        case KIND_ARC_BAND: c = tp_arc_band(g, at.x, at.y); break;
        case KIND_SEGMENT: c = tp_segment(g, at.x, at.y); break;
        case KIND_DISC: c = tp_disc(g, at.x, at.y); break;
        case KIND_STROKE: c = tp_stroke(g, at.x, at.y); break;
        case KIND_FILL: {
            float2 at_c = float2(at) + 0.5f;
            c = (at_c.x >= g[0] && at_c.x < g[2] && at_c.y >= g[1] && at_c.y < g[3]) ? 1.0f : 0.0f;
            break;
        }
        default: c = 0.0f; break;
    }
    uint4 o = tp_composite(law, d, uint4(p.color), c);
    return float4(o) / 255.0f;
}

// ---------------------------------------------------------------- THE TRAY STACK
// (docs/DESIGN-stats-deck-gpu-motion-2026-10-26.md §3.3) One card of a window's tray stack folded
// into the stack's own Rgba8Unorm card: each pixel the card covers composites its texel over the
// fetched stack byte by the same law at coverage `opacity`, which at 1 is the CPU fold's
// (`tray_raster::composite_rgba_surface`: a clear texel leaves the byte, an opaque one replaces
// it, the rest composite).

// The Rust `TrayLayerUniform` (`metal/tray_prims.rs`), 32 bytes.
struct TrayLayerU {
    float2 target;   // the stack card's size in px
    int2 origin;     // the card's top-left in the stack card's px
    int2 size;       // the card's size in px
    float opacity;
    float pad;
};

vertex TrayPrimOut vs_tray_layer(uint vi [[vertex_id]],
                                 constant TrayLayerU& u [[buffer(2)]]) {
    const float2 corner[4] = {
        float2(0.0, 0.0), float2(1.0, 0.0),
        float2(0.0, 1.0), float2(1.0, 1.0)
    };
    float2 lo = float2(u.origin);
    float2 px = lo + corner[vi] * float2(u.size);
    TrayPrimOut o;
    o.pos = float4(2.0 * px.x / u.target.x - 1.0, 1.0 - 2.0 * px.y / u.target.y, 0.0, 1.0);
    o.index = 0u;
    return o;
}

fragment float4 fs_tray_layer(TrayPrimOut in [[stage_in]],
                              constant TrayLayerU& u [[buffer(2)]],
                              constant TrayPrimLaw& law [[buffer(1)]],
                              texture2d<float, access::read> card [[texture(0)]],
                              float4 dst [[color(0)]]) {
    int2 at = int2(floor(in.pos.xy)) - u.origin;
    uint4 c = tp_bytes(card.read(uint2(clamp(at, int2(0), u.size - 1))));
    uint4 o = tp_composite(law, tp_bytes(dst), c, u.opacity);
    return float4(o) / 255.0f;
}
