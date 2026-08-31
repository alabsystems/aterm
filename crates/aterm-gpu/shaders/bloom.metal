#include <metal_stdlib>
using namespace metal;

struct VsOut {
    float4 pos [[position]];
    float2 uv;
};

// Oversized triangle covering the whole clip rect; UVs map the framebuffer 1:1.
vertex VsOut vs_fs_bloom(uint vi [[vertex_id]]) {
    const float2 uv[3] = { float2(0.0, 0.0), float2(2.0, 0.0), float2(0.0, 2.0) };
    VsOut o;
    o.uv = uv[vi];
    o.pos = float4(o.uv.x * 2.0 - 1.0, 1.0 - o.uv.y * 2.0, 0.0, 1.0);
    return o;
}

struct BloomU { float2 texel; float strength; float radius; };

fragment float4 fs_bloom(VsOut in [[stage_in]],
                         texture2d<float> bloom_src [[texture(0)]],
                         sampler bloom_samp [[sampler(0)]],
                         constant BloomU& bu [[buffer(2)]]) {
    float3 sum = float3(0.0, 0.0, 0.0);
    float wsum = 0.0;
    for (int j = -2; j <= 2; j = j + 1) {
        for (int i = -2; i <= 2; i = i + 1) {
            float2 off = float2(float(i), float(j)) * bu.texel * bu.radius;
            float d2 = float(i * i + j * j);
            float w = exp(-d2 / 4.0);
            sum = sum + bloom_src.sample(bloom_samp, in.uv + off).rgb * w;
            wsum = wsum + w;
        }
    }
    return float4(sum / wsum * bu.strength, 1.0);
}
