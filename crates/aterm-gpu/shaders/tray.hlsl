// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// HLSL (Shader Model 5.0) twin of renderer.rs TRAY_SHADER, shaders/tray.metal
// and shaders/tray.glsl: the settings-card tray, one device-px straight-alpha
// quad drawn as a four-vertex triangle strip. Compiled per entry point by
// src/d3d11 as shaders/cell.hlsl describes. Bindings: `Tray` is b2 (read by
// the vertex stage), the card t0 with its sampler s0.

// Matching the Rust `TrayUniform` (32 bytes).
cbuffer Tray : register(b2) {
    // device-px x, y, w, h
    float4 t_rect;
    // destination width, height in px
    float2 t_fb;
    float2 t_pad;
};

Texture2D<float4> tray_tex : register(t0);
SamplerState tray_samp : register(s0);

struct TrayOut {
    float4 pos : SV_Position;
    float2 uv : LOC0;
};

TrayOut vs_tray(uint vi : SV_VertexID) {
    float2 corner[4] = { float2(0.0, 0.0), float2(1.0, 0.0), float2(0.0, 1.0), float2(1.0, 1.0) };
    TrayOut o;
    float2 k = corner[min(vi, 3u)];
    float2 px = t_rect.xy + (k * t_rect.zw);
    float2 ndc = float2(((2.0 * px.x) / t_fb.x) - 1.0, 1.0 - ((2.0 * px.y) / t_fb.y));
    o.pos = float4(ndc, 0.0, 1.0);
    o.uv = k;
    return o;
}

float4 fs_tray(TrayOut i) : SV_Target0 {
    return tray_tex.Sample(tray_samp, i.uv);
}
