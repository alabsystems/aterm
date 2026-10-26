#version 300 es
// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// GLSL ES 3.00 twin of renderer.rs HDR_GLOW_SHADER and shaders/hdr_glow.metal:
// the crown pair (the EDR aurora on an f16 swapchain, the SDR crown through
// the screen blend). Quads are in offscreen px; the vertex stage adds
// `content_off`. Compiled per stage by src/gl as shaders/cell.glsl describes.
// Bindings: `HdrU` is `aterm_b0`; the crown samples no texture.
precision highp float;
precision highp int;

layout(std140) uniform aterm_b0 {
    vec2 hu_screen;       // destination width, height in px (the NDC divisor)
    vec2 hu_content_off;  // frame top-left inside the destination, px
    float hu_boost;       // linear emission boost
    float hu_headroom;    // sanitize(edr_max) - 1.0, >= 0
    vec2 hu_pad;
};

#ifdef ATERM_VERTEX
#define ATERM_VARYING out
#else
#define ATERM_VARYING in
layout(location = 0) out vec4 o_frag;
#endif

// The quad's left edge in destination px and its width, and the colour pair.
flat ATERM_VARYING vec2 v_span;
flat ATERM_VARYING uvec4 v_c0;
flat ATERM_VARYING uvec4 v_c1;

uint glow_column(float pos_x, vec2 span) {
    return uint(clamp(floor(pos_x - span.x), 0.0, max(span.y - 1.0, 0.0)));
}

uvec4 glow_lerp4(uvec4 c0, uvec4 c1, uint i, uint w) {
    uint m = 2u * min(i, w - 1u) + 1u;
    return (c0 * (2u * w - m) + c1 * m + uvec4(w)) / uvec4(2u * w);
}

#ifdef ATERM_FRAGMENT
// The fragment's premultiplied sRGB-space colour, ramped, as 0..1 floats.
vec3 crown_color() {
    uint w = max(uint(v_span.y), 1u);
    uvec4 c = glow_lerp4(v_c0, v_c1, glow_column(gl_FragCoord.x, v_span), w);
    return vec3(c.rgb) / vec3(255.0);
}
#endif

vec2 hdr_corner(uint vi) {
    vec2 c[6] = vec2[6](
        vec2(0.0, 0.0), vec2(1.0, 0.0), vec2(0.0, 1.0),
        vec2(1.0, 0.0), vec2(1.0, 1.0), vec2(0.0, 1.0)
    );
    return c[vi];
}

#if defined(ATERM_VERTEX) && defined(ATERM_VS_vs_hdr_glow)
layout(location = 0) in uvec4 a_rect;
layout(location = 1) in uvec4 a_c0;
layout(location = 2) in uvec4 a_c1;
void vs_hdr_glow() {
    vec4 rect = vec4(a_rect);
    vec2 k = hdr_corner(uint(gl_VertexID));
    // Offscreen px -> destination px (the W1 band placement) -> NDC.
    vec2 px = rect.xy + k * rect.zw + hu_content_off;
    gl_Position = vec4(2.0 * px.x / hu_screen.x - 1.0, 1.0 - 2.0 * px.y / hu_screen.y, 0.0, 1.0);
    v_span = vec2(rect.x + hu_content_off.x, rect.z);
    v_c0 = a_c0;
    v_c1 = a_c1;
}
#endif

// vs_hdr_glow over the bloom's item list, unwidened: cell.glsl's GLOW ITEMS
// reader (its RGBA8 word texture at the vertex stage's unit 0,
// BindSpec::CROWN_ITEMS), so the crown reads a ramp whole as the extract does.
#if defined(ATERM_VERTEX) && defined(ATERM_VS_vs_hdr_glow_items)
uniform highp sampler2D aterm_t0;

uint glow_item_word(uint j) {
    uvec4 b = uvec4(roundEven(texelFetch(aterm_t0, ivec2(int(j % 1024u), int(j / 1024u)), 0)
                              * 255.0));
    return b.r | (b.g << 8u) | (b.b << 16u) | (b.a << 24u);
}

uvec4 glow_item_end(uint e) {
    return uvec4(uvec3(e >> 16u, e >> 8u, e) & uvec3(255u), e >> 24u);
}

void vs_hdr_glow_items() {
    uint i = uint(gl_InstanceID);
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
    uvec4 rect_u = (e0 | e1) == 0u
        ? uvec4(0u)
        : uvec4(xy & 0xFFFFu, (xy >> 16u) + s + (s > join ? joined : 0u),
                glow_item_word(r + 1u) & 0xFFFFu, s == join ? joined + 1u : 1u);
    vec4 rect = vec4(rect_u);
    vec2 k = hdr_corner(uint(gl_VertexID));
    vec2 px = rect.xy + k * rect.zw + hu_content_off;
    gl_Position = vec4(2.0 * px.x / hu_screen.x - 1.0, 1.0 - 2.0 * px.y / hu_screen.y, 0.0, 1.0);
    v_span = vec2(rect.x + hu_content_off.x, rect.z);
    v_c0 = glow_item_end(e0);
    v_c1 = glow_item_end(e1);
}
#endif

// The white part of a premultiplied colour is its smallest channel.
float white_part(vec3 c) {
    return min(min(c.r, c.g), c.b);
}

#if defined(ATERM_FRAGMENT) && defined(ATERM_FS_fs_hdr_glow)
// Decode to linear, boost per fragment (whiter cores climb), clamp to the
// headroom, emit into the One/One add.
void fs_hdr_glow() {
    vec3 c = clamp(crown_color(), vec3(0.0), vec3(1.0));
    vec3 lo = c / vec3(12.92);
    vec3 hi = pow((c + vec3(0.055)) / vec3(1.055), vec3(2.4));
    vec3 lin = mix(lo, hi, greaterThan(c, vec3(0.04045)));
    float hot = min(1.0 + 2.0 * smoothstep(0.35, 0.6, white_part(c)), 3.0);
    float bound = max(hu_headroom, 0.0);
    vec3 add = max(min(lin * hu_boost * hot, vec3(bound)), vec3(0.0));
    o_frag = vec4(add, 0.0);
}
#endif

#if defined(ATERM_FRAGMENT) && defined(ATERM_FS_fs_sdr_glow)
// The SDR crown: the colour scaled by the budget, raw, into the screen blend.
void fs_sdr_glow() {
    vec3 c = clamp(crown_color(), vec3(0.0), vec3(1.0));
    float bound = max(hu_headroom, 0.0);
    o_frag = vec4(c * bound * max(hu_boost, 0.0), 0.0);
}
#endif
