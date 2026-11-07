#version 300 es
// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// GLSL ES 3.00 twin of renderer.rs BLOOM_SHADER and shaders/bloom.metal: the
// cursor-comet bloom composite, a 25- or 49-tap gaussian over the half-res
// comet target with two radii (white part and chroma). Compiled per stage by src/gl
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
    float bu_pad0;
    vec2 bu_screen;         // the frame, px: a fragment lies at `uv * screen`
    // The frame's letter mask (aterm_render::bloom::LetterMask): the cells the bloom keeps off.
    vec2 bu_origin;         // the grid's top-left, px
    vec2 bu_cell;           // a cell, px
    vec2 bu_grow;           // kept off entirely this far past a kept cell's box, px
    vec2 bu_feather;        // easing back to whole over this far past that, px
    float bu_mask_on;       // 0: the bloom is laid whole
    float bu_pad2;
    uvec4 bu_window;        // the cells the bits cover: first column, first row, columns, rows
    uvec4 bu_bits[256];     // one bit a cell, row-major: bit i & 31 of word i >> 5
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

// Whether cell (c, r) is kept off (`LetterMask::kept`).
bool kept_cell(int c, int r) {
    int lc = c - int(bu_window.x);
    int lr = r - int(bu_window.y);
    if (lc < 0 || lr < 0 || lc >= int(bu_window.z) || lr >= int(bu_window.w)) {
        return false;
    }
    uint i = uint(lr) * bu_window.z + uint(lc);
    uint word = bu_bits[int(i >> 7u)][int((i >> 5u) & 3u)];
    return ((word >> (i & 31u)) & 1u) != 0u;
}

// The share of the bloom laid at frame point p (`LetterMask::weight`, its twin): 0 within a
// kept cell's box grown by `grow`, rising to 1 over `feather` past it.
float letter_mask(vec2 p) {
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
            float e2 = dx * dx + dy * dy;
            if (e2 >= 1.0) {
                continue;
            }
            float e = sqrt(e2);
            m = min(m, e * e * (3.0 - 2.0 * e));
        }
    }
    return m;
}

// One kernel, two radii: the white part spreads at `radius`, the chroma at the
// shorter `chroma_radius`, summed back into one halo. 25 taps to a radius of 6,
// 49 past it.
void fs_bloom() {
    float white = 0.0;
    vec3 chroma = vec3(0.0, 0.0, 0.0);
    float wsum = 0.0;
    // `taps_per_side` (aterm-render's bloom.rs): two taps a side to a radius of 6, three
    // past it, so no two taps stand wider apart than 6 texels.
    int n = clamp(int(ceil(bu_radius / 3.0)), 2, 3);
    float step = 2.0 / float(n);
    float spread = float(n * n);
    for (int j = -n; j <= n; j = j + 1) {
        for (int i = -n; i <= n; i = i + 1) {
            vec2 tap = vec2(float(i), float(j)) * step * bu_texel;
            float d2 = float(i * i + j * j);
            float w = exp(-d2 / spread);
            vec3 sw = half_rgb(v_uv + tap * bu_radius);
            white = white + white_part(sw) * w;
            vec3 sc = half_rgb(v_uv + tap * bu_chroma_radius);
            chroma = chroma + (sc - vec3(white_part(sc))) * w;
            wsum = wsum + w;
        }
    }
    vec3 sum = vec3(white) + chroma;
    float kept = letter_mask(v_uv * bu_screen);
    o_frag = vec4(sum / vec3(wsum) * bu_strength * kept, 1.0);
}
#endif
