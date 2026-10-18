#version 300 es
// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// GLSL ES 3.00 twin of renderer.rs BLIT_SHADER and shaders/blit.metal: the
// present blit, a vertex-bufferless fullscreen triangle that fetches the
// offscreen 1:1. Compiled per stage by src/gl as shaders/cell.glsl describes.
// Bindings: the `Blit` uniform is `aterm_b2`, the frame `aterm_t0`.
precision highp float;
precision highp int;

// std140, matching the Rust `BlitUniform` byte for byte (112 bytes).
layout(std140) uniform aterm_b2 {
    uint b_flag;            // bell-flash invert
    uint b_overlay;         // drop-target highlight enabled
    float b_border_px;      // inset border thickness, device px
    float b_encode_srgb;    // !=0: re-encode linear->sRGB (downlevel blit)
    vec3 b_accent;          // overlay accent rgb, 0..1
    float b_chrome_y1;      // end of the host-chrome source rows
    vec2 b_dims;            // offscreen frame width, height in px
    float b_wash_a;         // interior wash alpha
    float b_border_a;       // border alpha
    vec4 b_band;            // remainder-band colour (live terminal bg)
    vec2 b_content_off;     // frame top-left inside the destination, px
    float b_hdr;            // !=0: decode sRGB->linear, clamp <= 1
    float b_translucent;    // !=0: emit the offscreen/band alpha
    float b_sdr_white_scale;
    float b_visible_y;      // first source row exposed by the crop
    float b_visible_h;      // exposed source height
    float b_premult;        // !=0: multiply rgb by the emitted alpha
    float b_chrome_y0;      // first host-chrome source row
    float b_chrome_pad0;
    float b_chrome_pad1;
    float b_chrome_pad2;
};
uniform highp sampler2D aterm_t0;

#ifdef ATERM_VERTEX
#define ATERM_VARYING out
#else
#define ATERM_VARYING in
layout(location = 0) out vec4 o_frag;
#endif

ATERM_VARYING vec2 v_uv;

#if defined(ATERM_VERTEX) && defined(ATERM_VS_vs_blit)
// Oversized triangle covering the whole clip rect; uvs map the target 1:1.
void vs_blit() {
    vec2 uv[3] = vec2[3](vec2(0.0, 0.0), vec2(2.0, 0.0), vec2(0.0, 2.0));
    v_uv = uv[uint(gl_VertexID)];
    // uv (0..2, y down) -> clip (x: -1..3, y: 1..-3).
    gl_Position = vec4(v_uv.x * 2.0 - 1.0, 1.0 - v_uv.y * 2.0, 0.0, 1.0);
}
#endif

// Linear-light channel -> sRGB (the standard piecewise encode), for the
// downlevel blit that samples an sRGB-typed offscreen.
float l2s(float c) {
    float cc = clamp(c, 0.0, 1.0);
    if (cc <= 0.0031308) { return 12.92 * cc; }
    return 1.055 * pow(cc, 0.41666666) - 0.055;
}

// sRGB-encoded rgb -> linear light, clamped to [0, 1] (the grid clamp law).
vec3 hdr_grid_encode3(vec3 c) {
    vec3 cc = clamp(c, vec3(0.0), vec3(1.0));
    vec3 lo = cc / vec3(12.92);
    vec3 hi = pow((cc + vec3(0.055)) / vec3(1.055), vec3(2.4));
    return clamp(mix(lo, hi, greaterThan(cc, vec3(0.04045))), vec3(0.0), vec3(1.0));
}

#if defined(ATERM_FRAGMENT) && defined(ATERM_FS_fs_blit)
// The WGSL's branches in its order: the remainder bands (with the chrome-edge
// continuation) are painted the band colour, the frame is a 1:1 texel fetch
// with the bell invert, the drop overlay, the downlevel encode, the EDR
// decode, the translucent alpha and the premultiply.
void fs_blit() {
    vec2 p = gl_FragCoord.xy - b_content_off;
    float visible_y1 = b_visible_y + b_visible_h;
    bool chrome_edge = false;
    if ((p.x < 0.0 || p.x >= b_dims.x) && p.y >= max(b_visible_y, b_chrome_y0)
            && p.y < min(visible_y1, b_chrome_y1)) {
        p.x = clamp(p.x, 0.0, b_dims.x - 1.0);
        chrome_edge = true;
    }
    if (p.x < 0.0 || p.y < b_visible_y || p.x >= b_dims.x || p.y >= visible_y1) {
        float band_a = b_translucent != 0.0 ? b_band.a : 1.0;
        vec3 band_rgb = b_band.rgb;
        if (b_hdr != 0.0) {
            band_rgb = hdr_grid_encode3(band_rgb) * b_sdr_white_scale;
        }
        if (b_premult != 0.0) {
            band_rgb = band_rgb * band_a;
        }
        o_frag = vec4(band_rgb, band_a);
        return;
    }
    // The WGSL's `textureLoad`, as a NEAREST sample at the texel's centre:
    // GL's `texelFetch` decodes an sRGB texture whatever its
    // TEXTURE_SRGB_DECODE_EXT (measured on Mesa 25.2), and the native plan
    // reads the frame raw (the Unorm view), which only a filtered sample
    // honours. At a texel centre NEAREST returns exactly that texel.
    vec4 c = texture(aterm_t0, (floor(p) + vec2(0.5)) / vec2(textureSize(aterm_t0, 0)));
    vec3 rgb = c.rgb;
    if (b_flag != 0u && !chrome_edge) {
        rgb = vec3(1.0) - rgb;
    }
    if (b_overlay != 0u && !chrome_edge) {
        vec2 visible_p = vec2(p.x, p.y - b_visible_y);
        float edge = min(
            min(visible_p.x, b_dims.x - visible_p.x),
            min(visible_p.y, b_visible_h - visible_p.y)
        );
        float a = b_wash_a;
        if (edge < b_border_px) { a = b_border_a; }
        rgb = mix(rgb, b_accent.rgb, a);
    }
    if (b_encode_srgb != 0.0) {
        rgb = vec3(l2s(rgb.r), l2s(rgb.g), l2s(rgb.b));
    }
    if (b_hdr != 0.0) {
        rgb = hdr_grid_encode3(rgb) * b_sdr_white_scale;
    }
    float out_a = b_translucent != 0.0 ? c.a : 1.0;
    if (b_premult != 0.0) {
        rgb = rgb * out_a;
    }
    o_frag = vec4(rgb, out_a);
}
#endif
