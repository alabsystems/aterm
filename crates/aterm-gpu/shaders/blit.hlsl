// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// HLSL (Shader Model 5.0) twin of renderer.rs BLIT_SHADER, shaders/blit.metal
// and shaders/blit.glsl: the present blit, a vertex-bufferless fullscreen
// triangle that fetches the offscreen 1:1. Compiled per entry point by
// src/d3d11 as shaders/cell.hlsl describes. Bindings: the `Blit` buffer is
// b2, the frame t0 (its sampler s0 is bound and unread: the fetch is by
// texel address).

// Matching the Rust `BlitUniform` byte for byte (112 bytes).
cbuffer Blit : register(b2) {
    // bell-flash invert
    uint b_flag;
    // drop-target highlight enabled
    uint b_overlay;
    // inset border thickness, device px
    float b_border_px;
    // != 0: re-encode linear->sRGB (downlevel blit)
    float b_encode_srgb;
    // overlay accent rgb, 0..1
    float3 b_accent;
    // end of the host-chrome source rows
    float b_chrome_y1;
    // offscreen frame width, height in px
    float2 b_dims;
    // interior wash alpha
    float b_wash_a;
    // border alpha
    float b_border_a;
    // remainder-band colour (live terminal bg)
    float4 b_band;
    // frame top-left inside the destination, px
    float2 b_content_off;
    // != 0: decode sRGB->linear, clamp <= 1
    float b_hdr;
    // != 0: emit the offscreen/band alpha
    float b_translucent;
    float b_sdr_white_scale;
    // first source row exposed by the crop
    float b_visible_y;
    // exposed source height
    float b_visible_h;
    // != 0: multiply rgb by the emitted alpha
    float b_premult;
    // first host-chrome source row
    float b_chrome_y0;
    float b_chrome_pad0;
    float b_chrome_pad1;
    float b_chrome_pad2;
};

Texture2D<float4> src_tex : register(t0);

struct VsOut {
    float4 pos : SV_Position;
    float2 uv : LOC0;
};

// Oversized triangle covering the whole clip rect; uvs map the target 1:1:
// uv (0..2, y down) -> clip (x: -1..3, y: 1..-3).
VsOut vs_blit(uint vi : SV_VertexID) {
    float2 uv[3] = { float2(0.0, 0.0), float2(2.0, 0.0), float2(0.0, 2.0) };
    VsOut o;
    o.uv = uv[min(vi, 2u)];
    o.pos = float4((o.uv.x * 2.0) - 1.0, 1.0 - (o.uv.y * 2.0), 0.0, 1.0);
    return o;
}

int2 f2i2(float2 v) {
    return int2(clamp(v, -2147483600.0, 2147483500.0));
}

// Linear-light channel -> sRGB (the standard piecewise encode), for the
// downlevel blit that samples an sRGB-typed offscreen.
float l2s(float c) {
    float cc = clamp(c, 0.0, 1.0);
    if (cc <= 0.0031308) {
        return 12.92 * cc;
    }
    return (1.055 * pow(cc, 0.41666666)) - 0.055;
}

// sRGB-encoded rgb -> linear light, clamped to [0, 1] (the grid clamp law).
float3 hdr_grid_encode3(float3 c) {
    float3 cc = clamp(c, (0.0).xxx, (1.0).xxx);
    float3 lo = cc / (12.92).xxx;
    float3 hi = pow((cc + (0.055).xxx) / (1.055).xxx, (2.4).xxx);
    return clamp((cc > (0.04045).xxx) ? hi : lo, (0.0).xxx, (1.0).xxx);
}

// The WGSL's branches in its order: the remainder bands (with the chrome-edge
// continuation) are painted the band colour, the frame is a 1:1 texel fetch
// with the bell invert, the drop overlay, the downlevel encode, the EDR
// decode, the translucent alpha and the premultiply.
float4 fs_blit(VsOut i) : SV_Target0 {
    float2 p = i.pos.xy - b_content_off;
    float visible_y1 = b_visible_y + b_visible_h;
    bool chrome_edge = false;
    if ((p.x < 0.0 || p.x >= b_dims.x) && p.y >= max(b_visible_y, b_chrome_y0)
            && p.y < min(visible_y1, b_chrome_y1)) {
        p.x = clamp(p.x, 0.0, b_dims.x - 1.0);
        chrome_edge = true;
    }
    if (p.x < 0.0 || p.y < b_visible_y || p.x >= b_dims.x || p.y >= visible_y1) {
        float band_a = (b_translucent != 0.0) ? b_band.w : 1.0;
        float3 band_rgb = b_band.xyz;
        if (b_hdr != 0.0) {
            band_rgb = hdr_grid_encode3(band_rgb) * b_sdr_white_scale;
        }
        if (b_premult != 0.0) {
            band_rgb = band_rgb * band_a;
        }
        return float4(band_rgb, band_a);
    }
    float4 c = src_tex.Load(int3(f2i2(p), 0));
    float3 rgb = c.xyz;
    if (b_flag != 0u && !chrome_edge) {
        rgb = (1.0).xxx - rgb;
    }
    if (b_overlay != 0u && !chrome_edge) {
        float2 visible_p = float2(p.x, p.y - b_visible_y);
        float edge = min(min(visible_p.x, b_dims.x - visible_p.x),
                         min(visible_p.y, b_visible_h - visible_p.y));
        float a = b_wash_a;
        if (edge < b_border_px) {
            a = b_border_a;
        }
        rgb = lerp(rgb, b_accent.xyz, a);
    }
    if (b_encode_srgb != 0.0) {
        rgb = float3(l2s(rgb.x), l2s(rgb.y), l2s(rgb.z));
    }
    if (b_hdr != 0.0) {
        rgb = hdr_grid_encode3(rgb) * b_sdr_white_scale;
    }
    float out_a = (b_translucent != 0.0) ? c.w : 1.0;
    if (b_premult != 0.0) {
        rgb = rgb * out_a;
    }
    return float4(rgb, out_a);
}
