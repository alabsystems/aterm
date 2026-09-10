#include <metal_stdlib>
using namespace metal;

struct VsOut {
    float4 pos [[position]];
    float2 uv;
};

// Oversized triangle covering the whole clip rect; UVs map the framebuffer 1:1.
//
// NAMED `vs_fs`, not `vs_fs_bloom`: this is the twin of `renderer.rs::BLOOM_SHADER`'s
// `vs_fs`, and THE PIPELINE TABLE (`Pipeline::Bloom`) names ONE entry point for both
// backends. The MSL used to rename it, `shaders::libraries`'s roster listed the NEW
// name, and the roster test was self-consistent with the rename — so nothing could
// see that `renderer.rs` was asking for an entry point the MSL did not define. The
// roster is derived from the table now, and the name is the WGSL's.
vertex VsOut vs_fs(uint vi [[vertex_id]]) {
    const float2 uv[3] = { float2(0.0, 0.0), float2(2.0, 0.0), float2(0.0, 2.0) };
    VsOut o;
    o.uv = uv[vi];
    o.pos = float4(o.uv.x * 2.0 - 1.0, 1.0 - o.uv.y * 2.0, 0.0, 1.0);
    return o;
}

struct BloomU {
    float2 texel;
    float strength;
    float radius;        // the WHITE part's radius, half-res texels
    float chroma_radius; // the COLOURED part's, always the shorter of the two
    float _pad[3];
};

// The bloom's twin of `hdr_glow.metal::white_part` — the smallest channel is
// the white light in a premultiplied colour, the rest is its chroma.
static inline float white_part(float3 c) {
    return min(min(c.r, c.g), c.b);
}

// WHITE IS HOT (design §L5), the halo's half: ONE 25-tap kernel, two radii.
// The white part of the source spreads at `radius`, the chroma at the shorter
// `chroma_radius`, and the two are summed back into one halo — so a meteor
// reads as an over-white streak dying into a coloured train, and less coloured
// light smears sideways into the glyphs beside it. The taps, their gaussian
// weights and the normalization are the kernel that was here before; only the
// offset each half samples at differs.
fragment float4 fs_bloom(VsOut in [[stage_in]],
                         texture2d<float> bloom_src [[texture(0)]],
                         sampler bloom_samp [[sampler(0)]],
                         constant BloomU& bu [[buffer(2)]]) {
    float white = 0.0;
    float3 chroma = float3(0.0, 0.0, 0.0);
    float wsum = 0.0;
    for (int j = -2; j <= 2; j = j + 1) {
        for (int i = -2; i <= 2; i = i + 1) {
            float2 tap = float2(float(i), float(j)) * bu.texel;
            float d2 = float(i * i + j * j);
            float w = exp(-d2 / 4.0);
            float3 sw = bloom_src.sample(bloom_samp, in.uv + tap * bu.radius).rgb;
            white = white + white_part(sw) * w;
            float3 sc = bloom_src.sample(bloom_samp, in.uv + tap * bu.chroma_radius).rgb;
            chroma = chroma + (sc - float3(white_part(sc))) * w;
            wsum = wsum + w;
        }
    }
    float3 sum = float3(white) + chroma;
    return float4(sum / wsum * bu.strength, 1.0);
}
