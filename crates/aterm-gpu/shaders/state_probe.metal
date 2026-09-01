#include <metal_stdlib>
using namespace metal;

// VERIFICATION ONLY. No shipping pipeline compiles this file; it exists so the
// PIPELINE AND ENCODER STATE in `src/metal/ffi.rs` — the colour write mask, the
// scissor rect, the viewport and the sampler's filters — can be proven with a
// GPU readback instead of asserted from the selector name. A selector that is
// declared but never honoured is exactly the defect these probes catch: Metal's
// default write mask is ALL, its default scissor is the whole attachment and
// its default filter is nearest, so every one of those setters is INVISIBLE to
// a test that only checks the pass ran.
//
// Kept beside the shipped `.metal` files rather than inline in Rust for the
// reason `shaders.rs` gives for all of them: a `.metal` file is what an editor
// and `xcrun metal` can read.

struct ProbeOut {
    float4 pos [[position]];
    float2 uv;
};

// The SAME oversized triangle `vs_blit` uses, so the probes rasterize exactly
// the geometry the shipped fullscreen pass does — including how it interacts
// with a viewport, which is the point of the viewport probe.
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

// Emits `u.color` verbatim, with no dependence on position. What lands in the
// destination is then purely a function of the fixed-function state under test:
// the blend factors, the write mask, and whether the fragment was rasterized at
// all (scissor and viewport).
fragment float4 fs_probe_const(ProbeOut in [[stage_in]],
                               constant Probe& u [[buffer(2)]]) {
    return u.color;
}

// Samples texture 0 through sampler 0 at the destination pixel's CENTRE. With a
// destination larger than the source the sample lands off-centre inside a
// source texel, which is the only condition under which a linear filter and a
// nearest filter can disagree — `shimmer` samples at displaced sub-texel
// positions for exactly this reason.
fragment float4 fs_probe_sample(ProbeOut in [[stage_in]],
                                constant Probe& u [[buffer(2)]],
                                texture2d<float> src [[texture(0)]],
                                sampler samp [[sampler(0)]]) {
    return src.sample(samp, in.pos.xy / u.dst);
}
