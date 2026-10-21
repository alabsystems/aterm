#include <metal_stdlib>
using namespace metal;

// VERIFICATION ONLY. No shipping pipeline compiles this file; it exists so the
// PIPELINE STATE in `src/metal/ffi.rs` — the colour write mask and the
// sampler's filters — can be proven with a GPU readback instead of asserted
// from the selector name. A selector that is declared but never honoured is
// exactly the defect these probes catch: Metal's default write mask is ALL and
// its default filter is nearest, so both setters are INVISIBLE to a test that
// only checks the pass ran. (Scissor and viewport are proven by the scissored
// and letterboxed Metal-vs-wgpu differentials.)
//
// Kept beside the shipped `.metal` files rather than inline in Rust for the
// reason `shaders.rs` gives for all of them: a `.metal` file is what an editor
// and `xcrun metal` can read.

struct ProbeOut {
    float4 pos [[position]];
    float2 uv;
};

// The SAME oversized triangle `vs_blit` uses, so the probes rasterize exactly
// the geometry the shipped fullscreen pass does.
vertex ProbeOut vs_probe(uint vi [[vertex_id]]) {
    const float2 uv[3] = { float2(0.0, 0.0), float2(2.0, 0.0), float2(0.0, 2.0) };
    ProbeOut o;
    o.uv = uv[vi];
    // uv (0..2, y down) -> clip (x: -1..3, y: 1..-3).
    o.pos = float4(o.uv.x * 2.0 - 1.0, 1.0 - o.uv.y * 2.0, 0.0, 1.0);
    return o;
}

struct Probe {
    float4 color;  // what fs_probe_const emits, straight
    float2 dst;    // destination size in px, for fs_probe_sample's uv
    float2 pad;    // 32 bytes total; every member <=16B and naturally aligned
};

// Emits `u.color` regardless of position, so the destination depends only on the
// state under test: blend, write mask, and whether the fragment rasterized
// (scissor, viewport).
fragment float4 fs_probe_const(ProbeOut in [[stage_in]],
                               constant Probe& u [[buffer(2)]]) {
    return u.color;
}

// Samples texture 0 at the destination pixel's centre. A destination larger than
// the source puts that sample off-centre in a source texel, the only case where
// linear and nearest filters disagree.
fragment float4 fs_probe_sample(ProbeOut in [[stage_in]],
                                constant Probe& u [[buffer(2)]],
                                texture2d<float> src [[texture(0)]],
                                sampler samp [[sampler(0)]]) {
    return src.sample(samp, in.pos.xy / u.dst);
}
