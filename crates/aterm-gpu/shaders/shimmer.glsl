#version 300 es
// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// GLSL ES 3.00 twin of renderer.rs SHIMMER_SHADER and shaders/shimmer.metal:
// the present-time heat shimmer, a bounded per-pixel uv displacement of the
// finished frame above burning cells. Compiled per stage by src/gl as
// shaders/cell.glsl describes. Bindings: `ShimmerU` is `aterm_b2`, the frame
// `aterm_t0`.
precision highp float;
precision highp int;

// std140, matching the Rust `ShimmerUniform` (320 bytes).
layout(std140) uniform aterm_b2 {
    vec2 su_frame;       // full frame dims, device px
    vec2 su_region_min;  // pass rect min (== scissor origin)
    vec2 su_region_max;  // pass rect max
    float su_hot_top;    // top edge of the hot band (px)
    float su_rise;       // haze height above the hot band (px)
    float su_amp;        // max displacement, device px
    float su_period;     // spatial ripple period (px)
    float su_phase;      // present-time phase, seconds (wrapped at 100 s)
    float su_band_x0;    // heat-band strip origin (px)
    float su_band_w;     // heat-band width (px)
    float su_rolloff;    // horizontal edge rolloff width (px)
    vec2 su_pad;
    vec4 su_heat[16];    // 64 per-column-band heat proxies, 0..1
};
uniform highp sampler2D aterm_t0;

#ifndef ATERM_VERTEX
layout(location = 0) out vec4 o_frag;
#endif

#if defined(ATERM_VERTEX) && defined(ATERM_VS_vs_fs)
void vs_fs() {
    vec2 xy[3] = vec2[3](vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    gl_Position = vec4(xy[uint(gl_VertexID)], 0.0, 1.0);
}
#endif

// naga's f32 spelling of the WGSL's 6.28318530718.
const float TAU = 6.2831855;

float heat_at(uint i) {
    return su_heat[i / 4u][i % 4u];
}

#if defined(ATERM_FRAGMENT) && defined(ATERM_FS_fs_shimmer)
// Per-column heat, the height and edge envelopes, two rising plane waves and a
// columnar wobble; the displacement is capped at `amp` and the sample clamped
// to the frame interior. The whole displaced sample, alpha included.
void fs_shimmer() {
    vec2 p = gl_FragCoord.xy;
    float xb = clamp((p.x - su_band_x0) / max(su_band_w, 0.001) - 0.5, 0.0, 63.0);
    uint i0 = uint(floor(xb));
    float heat = mix(heat_at(i0), heat_at(min(i0 + 1u, 63u)), fract(xb));
    float a = clamp((su_hot_top - p.y) / max(su_rise, 1.0), 0.0, 1.0);
    float vfade = 1.0 - a * a * (3.0 - 2.0 * a);
    float xr = min(p.x - su_region_min.x, su_region_max.x - p.x);
    float e = clamp(xr / max(su_rolloff, 1.0), 0.0, 1.0);
    float xfade = e * e * (3.0 - 2.0 * e);
    float ky = TAU / max(su_period, 4.0);
    float col = p.x * ky;
    float w1 = sin(ky * p.y + TAU * (0.55 * su_phase) + 1.7 * sin(0.37 * col + TAU * (0.09 * su_phase)));
    float w2 = sin(1.93 * ky * p.y + TAU * (0.83 * su_phase) + 0.61 * col);
    float wx = sin(1.31 * ky * p.y + TAU * (0.7 * su_phase) + 1.11 * col);
    float env = su_amp * clamp(heat, 0.0, 1.0) * vfade * xfade;
    vec2 d = vec2(0.3 * wx, 0.94 * (0.62 * w1 + 0.38 * w2)) * env;
    float m = length(d);
    if (m > su_amp) {
        d = d * (su_amp / m);
    }
    vec2 sp = clamp(p + d, vec2(0.5, 0.5), su_frame - vec2(0.5, 0.5));
    o_frag = textureLod(aterm_t0, sp / su_frame, 0.0);
}
#endif
