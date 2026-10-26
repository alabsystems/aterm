// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// HLSL (Shader Model 5.0) twin of renderer.rs HDR_GLOW_SHADER,
// shaders/hdr_glow.metal and shaders/hdr_glow.glsl: the crown pair (the
// aurora on an scRGB swapchain, the SDR crown through the screen blend).
// Quads are in offscreen px; the vertex stage adds `content_off`. Compiled per
// entry point by src/d3d11 as shaders/cell.hlsl describes. Bindings: `HdrU`
// is b0 (both stages); the crown samples no texture, and its item rows'
// vertex stage reads the bloom's item list at t0 (BindSpec::CROWN_ITEMS).

cbuffer HdrU : register(b0) {
    // destination width, height in px (the NDC divisor)
    float2 hu_screen;
    // frame top-left inside the destination, px
    float2 hu_content_off;
    // linear emission boost
    float hu_boost;
    // sanitize(edr_max) - 1.0, >= 0
    float hu_headroom;
    float2 hu_pad;
};

// GLOW_LAYOUT: LOC0 => Uint16x4 (rect), LOC1 and LOC2 => Uint8x4 (the colour
// pair). The quad's left edge in destination px and its width ride `span`.
struct HdrVsOut {
    float4 pos : SV_Position;
    nointerpolation float2 span : LOC0;
    nointerpolation uint4 c0 : LOC1;
    nointerpolation uint4 c1 : LOC2;
};

uint f2u(float v) {
    return uint(clamp(v, 0.0, 4294967000.0));
}
uint4 div_u4(uint4 a, uint4 b) {
    return a / (b == 0u ? 1u : b);
}

// The fragment's 0-based column inside its quad.
uint glow_column(float pos_x, float2 span) {
    return f2u(clamp(floor(pos_x - span.x), 0.0, max(span.y - 1.0, 0.0)));
}

// aterm_render::glow_lerp at column i of a w-wide quad (w >= 1).
uint4 glow_lerp4(uint4 c0, uint4 c1, uint i, uint w) {
    uint m = (2u * min(i, w - 1u)) + 1u;
    return div_u4(((c0 * ((2u * w) - m)) + (c1 * m)) + (w).xxxx, (2u * w).xxxx);
}

float3 crown_color(HdrVsOut i) {
    uint w = max(f2u(i.span.y), 1u);
    uint4 c = glow_lerp4(i.c0, i.c1, glow_column(i.pos.x, i.span), w);
    return float3(c.xyz) / (255.0).xxx;
}

float2 hdr_corner(uint vi) {
    float2 c[6] = {
        float2(0.0, 0.0), float2(1.0, 0.0), float2(0.0, 1.0),
        float2(1.0, 0.0), float2(1.0, 1.0), float2(0.0, 1.0)
    };
    return c[min(vi, 5u)];
}

float white_part(float3 c) {
    return min(min(c.x, c.y), c.z);
}

HdrVsOut vs_hdr_glow(uint vi : SV_VertexID, uint4 rect_u : LOC0, uint4 c0 : LOC1,
                     uint4 c1 : LOC2) {
    HdrVsOut o;
    float4 rect = float4(rect_u);
    float2 k = hdr_corner(vi);
    float2 px = (rect.xy + (k * rect.zw)) + hu_content_off;
    o.pos = float4(((2.0 * px.x) / hu_screen.x) - 1.0, 1.0 - ((2.0 * px.y) / hu_screen.y), 0.0,
                   1.0);
    o.span = float2(rect.x + hu_content_off.x, rect.z);
    o.c0 = c0;
    o.c1 = c1;
    return o;
}

// vs_hdr_glow over the bloom's item list, unwidened: cell.hlsl's GLOW ITEMS
// reader (its RGBA8 word texture at the vertex stage's t0,
// BindSpec::CROWN_ITEMS), so the crown reads a ramp whole as the extract does.
Texture2D<float4> glow_items_tex : register(t0);

uint4 f2u4(float4 v) {
    return uint4(clamp(v, 0.0, 4294967000.0));
}

uint glow_item_word(uint j) {
    uint4 b = f2u4(round(glow_items_tex.Load(int3(int(j % 1024u), int(j / 1024u), 0)) * 255.0));
    return ((b.x | (b.y << 8u)) | (b.z << 16u)) | (b.w << 24u);
}

// An end as bytes (r, g, b, a): the unwidened reader's num / den is 1 / 1.
uint4 glow_item_end(uint e) {
    return uint4(uint3(e >> 16u, e >> 8u, e) & (255u).xxx, e >> 24u);
}

HdrVsOut vs_hdr_glow_items(uint vi : SV_VertexID, uint ii : SV_InstanceID) {
    uint lo = 0u;
    uint hi = glow_item_word(0u) - 1u;
    [loop]
    while (lo < hi) {
        uint mid = ((lo + hi) + 1u) >> 1u;
        if (glow_item_word(4u + mid) <= ii) {
            lo = mid;
        } else {
            hi = mid - 1u;
        }
    }
    uint s = ii - glow_item_word(4u + lo);
    uint r = glow_item_word(1u) + (4u * lo);
    uint xy = glow_item_word(r);
    uint join = glow_item_word(r + 2u) & 65535u;
    uint joined = glow_item_word(r + 2u) >> 16u;
    uint e = (glow_item_word(2u) + glow_item_word(r + 3u)) + (2u * s);
    uint e0 = glow_item_word(e);
    uint e1 = glow_item_word(e + 1u);
    uint4 rect_u = ((e0 | e1) == 0u)
        ? (0u).xxxx
        : uint4(xy & 65535u, ((xy >> 16u) + s) + ((s > join) ? joined : 0u),
                glow_item_word(r + 1u) & 65535u, (s == join) ? (joined + 1u) : 1u);
    HdrVsOut o;
    float4 rect = float4(rect_u);
    float2 k = hdr_corner(vi);
    float2 px = (rect.xy + (k * rect.zw)) + hu_content_off;
    o.pos = float4(((2.0 * px.x) / hu_screen.x) - 1.0, 1.0 - ((2.0 * px.y) / hu_screen.y), 0.0,
                   1.0);
    o.span = float2(rect.x + hu_content_off.x, rect.z);
    o.c0 = glow_item_end(e0);
    o.c1 = glow_item_end(e1);
    return o;
}

// The aurora: the crown colour decoded to linear light, white made hot, the
// boost bounded by the output's headroom, added onto the scRGB frame.
float4 fs_hdr_glow(HdrVsOut i) : SV_Target0 {
    float3 c = clamp(crown_color(i), (0.0).xxx, (1.0).xxx);
    float3 lo = c / (12.92).xxx;
    float3 hi = pow((c + (0.055).xxx) / (1.055).xxx, (2.4).xxx);
    float3 lin = (c > (0.04045).xxx) ? hi : lo;
    float hot = min(1.0 + (2.0 * smoothstep(0.35, 0.6, white_part(c))), 3.0);
    float bound = max(hu_headroom, 0.0);
    float3 add = max(min((lin * hu_boost) * hot, (bound).xxx), (0.0).xxx);
    return float4(add, 0.0);
}

// The SDR crown: the colour itself, scaled, through the screen blend.
float4 fs_sdr_glow(HdrVsOut i) : SV_Target0 {
    float3 c = clamp(crown_color(i), (0.0).xxx, (1.0).xxx);
    float bound = max(hu_headroom, 0.0);
    return float4((c * bound) * max(hu_boost, 0.0), 0.0);
}
