// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// HLSL (Shader Model 5.0) twin of renderer.rs BLOOM_SHADER,
// shaders/bloom.metal and shaders/bloom.glsl: the cursor-comet bloom
// composite, a 25- or 49-tap gaussian over the half-res comet target with two radii
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
    float bu_pad2;
    // the frame, px: a fragment lies at `uv * screen`
    float2 bu_screen;
    // The frame's letter mask (aterm_render::bloom::LetterMask): the cells the bloom keeps off.
    // the grid's top-left, px
    float2 bu_origin;
    // a cell, px
    float2 bu_cell;
    // kept off entirely this far past a kept cell's box, px
    float2 bu_grow;
    // easing back to whole over this far past that, px
    float2 bu_feather;
    // 0: the bloom is laid whole
    float bu_mask_on;
    float bu_pad3;
    // the cells the bits cover: first column, first row, columns, rows
    uint4 bu_window;
    // one bit a cell, row-major: bit i & 31 of word i >> 5
    uint4 bu_bits[256];
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

// Whether cell (c, r) is kept off (`LetterMask::kept`).
bool kept_cell(int c, int r) {
    int lc = c - int(bu_window.x);
    int lr = r - int(bu_window.y);
    if (lc < 0 || lr < 0 || lc >= int(bu_window.z) || lr >= int(bu_window.w)) {
        return false;
    }
    uint i = uint(lr) * bu_window.z + uint(lc);
    uint word = bu_bits[i >> 7u][(i >> 5u) & 3u];
    return ((word >> (i & 31u)) & 1u) != 0u;
}

// The share of the bloom laid at frame point p (`LetterMask::weight`, its twin): 0 within a
// kept cell's box grown by `grow`, rising to 1 over `feather` past it.
float letter_mask(float2 p) {
    if (bu_mask_on == 0.0) {
        return 1.0;
    }
    int c = int(floor((p.x - bu_origin.x) / bu_cell.x));
    int r = int(floor((p.y - bu_origin.y) / bu_cell.y));
    float m = 1.0;
    for (int dr = -2; dr <= 2; dr = dr + 1) {
        for (int dc = -1; dc <= 1; dc = dc + 1) {
            int cc = c + dc;
            int rr = r + dr;
            if (!kept_cell(cc, rr)) {
                continue;
            }
            float bx0 = bu_origin.x + float(cc) * bu_cell.x - bu_grow.x;
            float by0 = bu_origin.y + float(rr) * bu_cell.y - bu_grow.y;
            float bx1 = bx0 + bu_cell.x + 2.0 * bu_grow.x;
            float by1 = by0 + bu_cell.y + 2.0 * bu_grow.y;
            float dx = max(max(bx0 - p.x, p.x - bx1), 0.0) / bu_feather.x;
            float dy = max(max(by0 - p.y, p.y - by1), 0.0) / bu_feather.y;
            float e2 = (dx * dx) + (dy * dy);
            if (e2 >= 1.0) {
                continue;
            }
            float e = sqrt(e2);
            m = min(m, e * e * (3.0 - (2.0 * e)));
        }
    }
    return m;
}

// One kernel, two radii: the white part spreads at `radius`, the chroma at
// the shorter `chroma_radius`, summed back into one halo. 25 taps to a radius
// of 6, 49 past it.
float4 fs_bloom(VsOut i) : SV_Target0 {
    float white = 0.0;
    float3 chroma = float3(0.0, 0.0, 0.0);
    float wsum = 0.0;
    // `taps_per_side` (aterm-render's bloom.rs): two taps a side to a radius of 6,
    // three past it, so no two taps stand wider apart than 6 texels. The loop's
    // bound is the uniform's, so its samples name their level (the half-res source
    // has one): FXC takes no gradient in a loop it cannot unroll.
    int n = clamp(int(ceil(bu_radius / 3.0)), 2, 3);
    float step = 2.0 / float(n);
    float spread = float(n * n);
    for (int j = -n; j <= n; j = j + 1) {
        for (int k = -n; k <= n; k = k + 1) {
            float2 tap = float2(float(k), float(j)) * (step * bu_texel);
            float d2 = float((k * k) + (j * j));
            float w = exp((-d2) / spread);
            float3 sw = bloom_src.SampleLevel(bloom_samp, i.uv + (tap * bu_radius), 0.0).xyz;
            white = white + (white_part(sw) * w);
            float3 sc = bloom_src.SampleLevel(bloom_samp, i.uv + (tap * bu_chroma_radius), 0.0).xyz;
            chroma = chroma + ((sc - (white_part(sc)).xxx) * w);
            wsum = wsum + w;
        }
    }
    float3 sum = (white).xxx + chroma;
    float kept = letter_mask(i.uv * bu_screen);
    return float4((sum / (wsum).xxx) * bu_strength * kept, 1.0);
}
