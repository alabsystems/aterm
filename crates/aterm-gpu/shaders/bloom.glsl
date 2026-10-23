#version 300 es
// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// GLSL ES 3.00 twin of renderer.rs BLOOM_SHADER and shaders/bloom.metal: the
// cursor-comet bloom composite, a 25-tap gaussian over the half-res comet
// target with two radii (white part and chroma). Compiled per stage by src/gl
// as shaders/cell.glsl describes. Bindings: `BloomU` is `aterm_b2`, the
// half-res source `aterm_t0`.
precision highp float;
precision highp int;

layout(std140) uniform aterm_b2 {
    vec2 bu_texel;
    float bu_strength;
    float bu_radius;        // the white part's radius, half-res texels
    float bu_chroma_radius; // the coloured part's, the shorter of the two
    float bu_extent_x;      // the share of the half texture's storage in use: 1 unless roomy
    float bu_extent_y;
};
uniform highp sampler2D aterm_t0;

#ifdef ATERM_VERTEX
#define ATERM_VARYING out
#else
#define ATERM_VARYING in
layout(location = 0) out vec4 o_frag;
#endif

ATERM_VARYING vec2 v_uv;

#if defined(ATERM_VERTEX) && defined(ATERM_VS_vs_fs)
void vs_fs() {
    vec2 uv[3] = vec2[3](vec2(0.0, 0.0), vec2(2.0, 0.0), vec2(0.0, 2.0));
    v_uv = uv[uint(gl_VertexID)];
    gl_Position = vec4(v_uv.x * 2.0 - 1.0, 1.0 - v_uv.y * 2.0, 0.0, 1.0);
}
#endif

// The smallest channel is the white light in a premultiplied colour.
float white_part(vec3 c) {
    return min(min(c.r, c.g), c.b);
}

#if defined(ATERM_FRAGMENT) && defined(ATERM_FS_fs_bloom)
// The half texture at `uv` over its texels in use, held to their outer
// centres as the sampler's edge clamp holds an exact texture, then placed in
// its storage (the GL arm keeps that storage roomy across resizes; an exact
// texture's extent is 1, so this is the edge clamp alone).
vec3 half_rgb(vec2 uv) {
    vec2 held = clamp(uv, 0.5 * bu_texel, vec2(1.0) - 0.5 * bu_texel);
    return texture(aterm_t0, held * vec2(bu_extent_x, bu_extent_y)).rgb;
}

// One 25-tap kernel, two radii: the white part spreads at `radius`, the chroma
// at the shorter `chroma_radius`, summed back into one halo.
void fs_bloom() {
    float white = 0.0;
    vec3 chroma = vec3(0.0, 0.0, 0.0);
    float wsum = 0.0;
    for (int j = -2; j <= 2; j = j + 1) {
        for (int i = -2; i <= 2; i = i + 1) {
            vec2 tap = vec2(float(i), float(j)) * bu_texel;
            float d2 = float(i * i + j * j);
            float w = exp(-d2 / 4.0);
            vec3 sw = half_rgb(v_uv + tap * bu_radius);
            white = white + white_part(sw) * w;
            vec3 sc = half_rgb(v_uv + tap * bu_chroma_radius);
            chroma = chroma + (sc - vec3(white_part(sc))) * w;
            wsum = wsum + w;
        }
    }
    vec3 sum = vec3(white) + chroma;
    o_frag = vec4(sum / vec3(wsum) * bu_strength, 1.0);
}
#endif
