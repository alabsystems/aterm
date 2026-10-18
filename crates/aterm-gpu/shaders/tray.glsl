#version 300 es
// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// GLSL ES 3.00 twin of renderer.rs TRAY_SHADER and shaders/tray.metal: the
// settings-card tray, one device-px straight-alpha quad drawn as a
// four-vertex triangle strip. Compiled per stage by src/gl as
// shaders/cell.glsl describes. Bindings: `Tray` is `aterm_b2` (read by the
// vertex stage), the card `aterm_t0`.
precision highp float;
precision highp int;

// std140, matching the Rust `TrayUniform` (32 bytes).
layout(std140) uniform aterm_b2 {
    vec4 t_rect;  // device-px x, y, w, h
    vec2 t_fb;    // destination width, height in px
    vec2 t_pad;
};
uniform highp sampler2D aterm_t0;

#ifdef ATERM_VERTEX
#define ATERM_VARYING out
#else
#define ATERM_VARYING in
layout(location = 0) out vec4 o_frag;
#endif

ATERM_VARYING vec2 v_uv;

#if defined(ATERM_VERTEX) && defined(ATERM_VS_vs_tray)
// Unit quad as a 4-vertex strip: (0,0) (1,0) (0,1) (1,1).
void vs_tray() {
    vec2 corner[4] = vec2[4](vec2(0.0, 0.0), vec2(1.0, 0.0), vec2(0.0, 1.0), vec2(1.0, 1.0));
    vec2 k = corner[uint(gl_VertexID)];
    vec2 px = t_rect.xy + k * t_rect.zw;
    // y-down device px -> y-up NDC.
    vec2 ndc = vec2(2.0 * px.x / t_fb.x - 1.0, 1.0 - 2.0 * px.y / t_fb.y);
    gl_Position = vec4(ndc, 0.0, 1.0);
    v_uv = k;
}
#endif

#if defined(ATERM_FRAGMENT) && defined(ATERM_FS_fs_tray)
// Straight RGBA passthrough; the blend state does the src-over composite.
void fs_tray() {
    o_frag = texture(aterm_t0, v_uv);
}
#endif
