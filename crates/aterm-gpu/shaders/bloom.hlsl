// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// HLSL (Shader Model 5.0) twin of renderer.rs BLOOM_SHADER,
// shaders/bloom.metal and shaders/bloom.glsl: the cursor-comet bloom
// composite, a 25-tap gaussian over the half-res comet target with two radii
// (white part and chroma). Compiled per entry point by src/d3d11 as
// shaders/cell.hlsl describes. Bindings: `BloomU` is b2, the half-res source
// t0 with its sampler s0.

cbuffer BloomU : register(b2) {
    float2 bu_texel;
    float bu_strength;
    // the white part's radius, half-res texels
    float bu_radius;
    // the coloured part's, the shorter of the two
    float bu_chroma_radius;
    float bu_pad0;
    float bu_pad1;
};

Texture2D<float4> bloom_src : register(t0);
SamplerState bloom_samp : register(s0);

struct VsOut {
    float4 pos : SV_Position;
    float2 uv : LOC0;
};

// A vertex-bufferless fullscreen triangle.
VsOut vs_fs(uint vi : SV_VertexID) {
    float2 uv[3] = { float2(0.0, 0.0), float2(2.0, 0.0), float2(0.0, 2.0) };
    VsOut o;
    o.uv = uv[min(vi, 2u)];
    o.pos = float4((o.uv.x * 2.0) - 1.0, 1.0 - (o.uv.y * 2.0), 0.0, 1.0);
    return o;
}

// The smallest channel is the white light in a premultiplied colour, the
// rest is its chroma.
float white_part(float3 c) {
    return min(min(c.x, c.y), c.z);
}

// One 25-tap kernel, two radii: the white part spreads at `radius`, the
// chroma at the shorter `chroma_radius`, summed back into one halo.
float4 fs_bloom(VsOut i) : SV_Target0 {
    float white = 0.0;
    float3 chroma = float3(0.0, 0.0, 0.0);
    float wsum = 0.0;
    for (int j = -2; j <= 2; j = j + 1) {
        for (int k = -2; k <= 2; k = k + 1) {
            float2 tap = float2(float(k), float(j)) * bu_texel;
            float d2 = float((k * k) + (j * j));
            float w = exp((-d2) / 4.0);
            float3 sw = bloom_src.Sample(bloom_samp, i.uv + (tap * bu_radius)).xyz;
            white = white + (white_part(sw) * w);
            float3 sc = bloom_src.Sample(bloom_samp, i.uv + (tap * bu_chroma_radius)).xyz;
            chroma = chroma + ((sc - (white_part(sc)).xxx) * w);
            wsum = wsum + w;
        }
    }
    float3 sum = (white).xxx + chroma;
    return float4((sum / (wsum).xxx) * bu_strength, 1.0);
}
