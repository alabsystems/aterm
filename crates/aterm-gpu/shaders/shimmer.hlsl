// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// HLSL (Shader Model 5.0) twin of renderer.rs SHIMMER_SHADER,
// shaders/shimmer.metal and shaders/shimmer.glsl: the fire's heat-haze
// refraction, a fullscreen triangle scissored to the hot region that samples
// a stage copy of the frame at a displaced point. Compiled per entry point by
// src/d3d11 as shaders/cell.hlsl describes. Bindings: `ShimmerU` is b2, the
// stage copy t0 with its sampler s0.

// Matching the Rust `ShimmerUniform` (320 bytes).
cbuffer ShimmerU : register(b2) {
    // full frame dims, device px
    float2 su_frame;
    // pass rect min (== scissor origin)
    float2 su_region_min;
    // pass rect max
    float2 su_region_max;
    // top edge of the hot band (px)
    float su_hot_top;
    // haze height above the hot band (px)
    float su_rise;
    // max displacement, device px (<= 1.5)
    float su_amp;
    // spatial ripple period (~cell height, px)
    float su_period;
    // present-time phase, seconds (wrapped at 100 s)
    float su_phase;
    // heat-band strip origin (px)
    float su_band_x0;
    // heat-band width (px)
    float su_band_w;
    // horizontal edge rolloff width (px)
    float su_rolloff;
    float2 su_pad;
    // 64 per-column-band heat proxies, 0..1
    float4 su_heat[16];
};

Texture2D<float4> shimmer_src : register(t0);
SamplerState shimmer_samp : register(s0);

// The WGSL's 6.28318530718 as f32.
static const float TAU = 6.2831855;

struct VsOut {
    float4 pos : SV_Position;
};

// A vertex-bufferless fullscreen triangle.
VsOut vs_fs(uint vi : SV_VertexID) {
    float2 xy[3] = { float2(-1.0, -1.0), float2(3.0, -1.0), float2(-1.0, 3.0) };
    VsOut o;
    o.pos = float4(xy[min(vi, 2u)], 0.0, 1.0);
    return o;
}

uint div_u(uint a, uint b) {
    return a / (b == 0u ? 1u : b);
}
uint mod_u(uint a, uint b) {
    return a % (b == 0u ? 1u : b);
}
uint f2u(float v) {
    return uint(clamp(v, 0.0, 4294967000.0));
}

float heat_at(uint i) {
    return su_heat[div_u(i, 4u)][mod_u(i, 4u)];
}

float4 fs_shimmer(VsOut i) : SV_Target0 {
    float2 p = i.pos.xy;
    // Per-column heat, linearly interpolated between adjacent bands.
    float xb = clamp(((p.x - su_band_x0) / max(su_band_w, 0.001)) - 0.5, 0.0, 63.0);
    uint i0 = f2u(floor(xb));
    float heat = lerp(heat_at(i0), heat_at(min(i0 + 1u, 63u)), frac(xb));
    // Height envelope: full strength at the hot band's top edge, smoothstep
    // fading to zero `rise` px above it.
    float a = clamp((su_hot_top - p.y) / max(su_rise, 1.0), 0.0, 1.0);
    float vfade = 1.0 - ((a * a) * (3.0 - (2.0 * a)));
    // Horizontal rolloff: zero at the region edges, easing in over `rolloff`.
    float xr = min(p.x - su_region_min.x, su_region_max.x - p.x);
    float e = clamp(xr / max(su_rolloff, 1.0), 0.0, 1.0);
    float xfade = (e * e) * (3.0 - (2.0 * e));
    // The ripple field: two incommensurate plane waves rising with time plus
    // a slow columnar wobble.
    float ky = TAU / max(su_period, 4.0);
    float col = p.x * ky;
    float w1 = sin(((ky * p.y) + (TAU * (0.55 * su_phase)))
                   + (1.7 * sin((0.37 * col) + (TAU * (0.09 * su_phase)))));
    float w2 = sin((((1.93 * ky) * p.y) + (TAU * (0.83 * su_phase))) + (0.61 * col));
    float wx = sin((((1.31 * ky) * p.y) + (TAU * (0.7 * su_phase))) + (1.11 * col));
    float env = ((su_amp * clamp(heat, 0.0, 1.0)) * vfade) * xfade;
    float2 d = float2(0.3 * wx, 0.94 * ((0.62 * w1) + (0.38 * w2))) * env;
    // The displacement cap: |d| <= amp.
    float m = length(d);
    if (m > su_amp) {
        d = d * (su_amp / m);
    }
    // Never sample outside the frame: clamp to the texel interior.
    float2 sp = clamp(p + d, float2(0.5, 0.5), su_frame - float2(0.5, 0.5));
    // The whole displaced sample, alpha included, rounded to the 8-bit grid
    // to nearest even. The pass writes the offscreen's Unorm view unblended,
    // and the API lets a device round a float-to-UNORM tie either way (0.6
    // ULP): WARP's D3D11 path rounds the bilinear sample's exact ties (two
    // texels a code apart, weight 1/2) down where its D3D12 path, and so
    // wgpu-dx12, rounds them to even; 332 texels of the heat fixture. A value
    // already on the grid converts exactly on every device.
    float4 c = shimmer_src.SampleLevel(shimmer_samp, sp / su_frame, 0.0);
    return round(c * 255.0) / 255.0;
}
