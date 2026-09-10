#include <metal_stdlib>
using namespace metal;

struct HdrU {
    float2 screen;      // SWAPCHAIN width,height in px (the NDC divisor)
    float2 content_off; // W1 frame top-left inside the swapchain, device px
    float boost;        // linear emission boost (aterm_render::hdr::HDR_GLOW_BOOST)
    float headroom;     // sanitize(edr_max) - 1.0, >= 0 (proven CPU-side)
    float2 _pad;
};

struct HdrVsOut {
    float4 pos [[position]];
    float4 color;
};

// Uint16x4 arrives as ushort4; widened to uint4 to mirror the WGSL vec4<u32>.
//
// BINDING LAW: vs_hdr_glow's uniform says [[buffer(0)]]; safe ONLY because the
// instance stream for this [[stage_in]] sits at vertex-buffer index 30
// (ffi.rs::INSTANCE_STREAM_SLOT) — see cell.metal's fuller statement.
struct HdrVsIn {
    ushort4 rect_u [[attribute(0)]];
    float4 color   [[attribute(1)]];
};

static inline float2 hdr_corner(uint vi) {
    const float2 c[6] = {
        float2(0.0, 0.0), float2(1.0, 0.0), float2(0.0, 1.0),
        float2(1.0, 0.0), float2(1.0, 1.0), float2(0.0, 1.0)
    };
    return c[vi];
}

vertex HdrVsOut vs_hdr_glow(uint vi [[vertex_id]],
                            HdrVsIn vin [[stage_in]],
                            constant HdrU& hu [[buffer(0)]]) {
    float4 rect = float4(vin.rect_u);
    float2 k = hdr_corner(vi);
    // Offscreen px -> swapchain px (the W1 band placement) -> NDC.
    float2 px = rect.xy + k * rect.zw + hu.content_off;
    HdrVsOut o;
    o.pos = float4(2.0 * px.x / hu.screen.x - 1.0, 1.0 - 2.0 * px.y / hu.screen.y, 0.0, 1.0);
    o.color = vin.color;
    return o;
}

// WHITE IS HOT (design §L5). The WHITE PART of a premultiplied sRGB colour is
// its smallest channel: a spectrum stop (`#FF0000`, `#FFFF00`, ...) has a zero
// channel and therefore NO white part, however bright it is, while `#FFFFFF`
// at coverage `k` is white all the way to `k`. So `min(r, g, b)` reads the
// luminance hierarchy off the colour itself, with no side channel and no
// per-quad tagging.
static inline float white_part(float3 c) {
    return min(min(c.r, c.g), c.b);
}

// Decode the premultiplied sRGB-space aurora colour to linear (same piecewise
// s2l as everywhere), boost, clamp to the headroom (never negative), and emit
// into the One/One add. COLOR write-mask: the blit's alpha stays 1.0.
//
// THE BOOST IS PER-FRAGMENT (§L5, "in EDR, `out` transients <= 460 ms may
// exceed reference white"): `1 + 2*smoothstep(0.35, 0.60, white_part)`, capped
// at 3x linear and then clamped to the panel's real headroom exactly as
// before. Below the 0.35 knee the factor is EXACTLY 1.0 and a multiply by 1.0
// is exact in IEEE-754, so every coloured mark emits the same bits it did
// before this pass learned about white. MSL smoothstep(edge0, edge1, x) is
// WGSL smoothstep(low, high, x), argument for argument.
fragment float4 fs_hdr_glow(HdrVsOut in [[stage_in]],
                            constant HdrU& hu [[buffer(0)]]) {
    float3 c = clamp(in.color.rgb, float3(0.0), float3(1.0));
    float3 lo = c / 12.92;
    float3 hi = pow((c + float3(0.055)) / 1.055, float3(2.4));
    // MSL select(a,b,cond) == cond ? b : a — the SAME argument order as WGSL's
    // select(false_val, true_val, cond). Verified against the WGSL line-for-line.
    float3 lin = select(lo, hi, c > float3(0.04045));
    float hot = min(1.0 + 2.0 * smoothstep(0.35, 0.60, white_part(c)), 3.0);
    float bound = max(hu.headroom, 0.0);
    float3 add = max(min(lin * hu.boost * hot, float3(bound)), float3(0.0));
    return float4(add, 0.0);
}

// SDR twin of the boost (the swapchain-side crown on a NON-f16 present): scale
// the aurora colour by the BUDGET and emit it RAW — the SDR swapchain is a
// non-sRGB Unorm view, so blending works in code values (no s2l decode).
fragment float4 fs_sdr_glow(HdrVsOut in [[stage_in]],
                            constant HdrU& hu [[buffer(0)]]) {
    float3 c = clamp(in.color.rgb, float3(0.0), float3(1.0));
    float bound = max(hu.headroom, 0.0);
    return float4(c * bound * max(hu.boost, 0.0), 0.0);
}
