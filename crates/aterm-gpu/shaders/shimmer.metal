#include <metal_stdlib>
using namespace metal;

struct VsOut {
    float4 pos [[position]];
};

// NAMED `vs_fs`, not `vs_fs_shimmer` — the twin of `renderer.rs::SHIMMER_SHADER`'s
// `vs_fs`. See the same note in `bloom.metal`.
vertex VsOut vs_fs(uint vi [[vertex_id]]) {
    const float2 xy[3] = { float2(-1.0, -1.0), float2(3.0, -1.0), float2(-1.0, 3.0) };
    VsOut o;
    o.pos = float4(xy[vi], 0.0, 1.0);
    return o;
}

// std140 twin of the Rust `ShimmerU`. `heat` is float4[16] (NOT float[64]) for
// exactly the reason the WGSL used array<vec4<f32>,16>: a uniform array of
// scalars takes a 16-byte stride under std140, so the vec4 form is the layout
// that is already byte-identical on both sides. MSL gives float4[16] the same
// 16-byte stride, so the Rust struct is UNCHANGED.
struct ShimmerU {
    float2 frame;       // full frame dims, device px
    float2 region_min;  // pass rect min (== scissor origin)
    float2 region_max;  // pass rect max
    float hot_top;      // top edge of the hot band (px)
    float rise;         // haze height above the hot band (px)
    float amp;          // max displacement, device px (<= 1.5)
    float period;       // spatial ripple period (~cell height, px)
    float phase;        // present-time phase, seconds (wrapped at 100 s)
    float band_x0;      // heat-band strip origin (px)
    float band_w;       // heat-band width (px)
    float rolloff;      // horizontal edge rolloff width (px)
    float2 _pad;
    float4 heat[16];    // 64 per-column-band heat proxies, 0..1
};

constant float TAU = 6.28318530718;

static inline float heat_at(constant ShimmerU& su, uint i) {
    return su.heat[i / 4u][i % 4u];
}

fragment float4 fs_shimmer(VsOut in [[stage_in]],
                           texture2d<float> shimmer_src [[texture(0)]],
                           sampler shimmer_samp [[sampler(0)]],
                           constant ShimmerU& su [[buffer(2)]]) {
    float2 p = in.pos.xy;
    // Per-column heat, linearly interpolated between adjacent bands so the
    // haze strength follows the flame's silhouette without band steps.
    float xb = clamp((p.x - su.band_x0) / max(su.band_w, 1e-3) - 0.5, 0.0, 63.0);
    uint i0 = uint(floor(xb));
    float heat = mix(heat_at(su, i0), heat_at(su, min(i0 + 1u, 63u)), fract(xb));
    // Height envelope: full strength at the hot band's top edge, smoothstep
    // fading to zero `rise` px above it (haze thins with altitude).
    float a = clamp((su.hot_top - p.y) / max(su.rise, 1.0), 0.0, 1.0);
    float vfade = 1.0 - a * a * (3.0 - 2.0 * a);
    // Horizontal rolloff: zero AT the region edges, easing in over `rolloff`
    // px, so the hard scissor bound is met by an already-zero displacement.
    float xr = min(p.x - su.region_min.x, su.region_max.x - p.x);
    float e = clamp(xr / max(su.rolloff, 1.0), 0.0, 1.0);
    float xfade = e * e * (3.0 - 2.0 * e);
    // The ripple field: two incommensurate plane waves RISING with time plus a
    // slow columnar wobble that breaks straight-line coherence.
    float ky = TAU / max(su.period, 4.0);
    float col = p.x * ky;
    float w1 = sin(ky * p.y + TAU * (0.55 * su.phase) + 1.7 * sin(0.37 * col + TAU * (0.09 * su.phase)));
    float w2 = sin(1.93 * ky * p.y + TAU * (0.83 * su.phase) + 0.61 * col);
    float wx = sin(1.31 * ky * p.y + TAU * (0.70 * su.phase) + 1.11 * col);
    float env = su.amp * clamp(heat, 0.0, 1.0) * vfade * xfade;
    float2 d = float2(0.30 * wx, 0.94 * (0.62 * w1 + 0.38 * w2)) * env;
    // Belt-and-braces displacement cap: |d| <= amp by construction
    // (sqrt(0.94^2 + 0.30^2) < 1); re-clamp so no future retune can break it.
    float m = length(d);
    if (m > su.amp) {
        d = d * (su.amp / m);
    }
    // Never sample outside the frame: clamp the displaced point to the texel
    // interior (the ClampToEdge sampler is the second fence).
    float2 sp = clamp(p + d, float2(0.5, 0.5), su.frame - float2(0.5, 0.5));
    float4 c = shimmer_src.sample(shimmer_samp, sp / su.frame, level(0.0));
    return float4(c.rgb, 1.0);
}
