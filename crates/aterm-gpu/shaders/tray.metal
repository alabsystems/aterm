#include <metal_stdlib>
using namespace metal;

// std140 twin of the Rust `TrayUniform` (32 bytes: float4 @0, float2 @16,
// float2 pad @24). MSL's natural constant-buffer layout matches std140 here
// because every member is <=16B and naturally aligned, so the Rust struct is
// UNCHANGED from the WGSL era.
struct Tray {
    float4 rect;  // device-px x, y, w, h
    float2 fb;    // framebuffer width, height in px
    float2 pad;
};

struct TrayOut {
    float4 pos [[position]];
    float2 uv;
};

// Unit quad as a 4-vert triangle-strip: (0,0) (1,0) (0,1) (1,1).
vertex TrayOut vs_tray(uint vi [[vertex_id]],
                       constant Tray& t [[buffer(2)]]) {
    const float2 corner[4] = {
        float2(0.0, 0.0), float2(1.0, 0.0),
        float2(0.0, 1.0), float2(1.0, 1.0)
    };
    float2 k = corner[vi];
    float2 px = t.rect.xy + k * t.rect.zw;
    // y-down device px -> y-up NDC.
    float2 ndc = float2(2.0 * px.x / t.fb.x - 1.0, 1.0 - 2.0 * px.y / t.fb.y);
    TrayOut o;
    o.pos = float4(ndc, 0.0, 1.0);
    o.uv = k; // full-texture sample
    return o;
}

fragment float4 fs_tray(TrayOut in [[stage_in]],
                        texture2d<float> tray_tex [[texture(0)]],
                        sampler tray_samp [[sampler(0)]]) {
    // Straight RGBA passthrough; ALPHA_BLENDING does the src-over composite.
    return tray_tex.sample(tray_samp, in.uv);
}
