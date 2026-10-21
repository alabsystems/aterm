#include <metal_stdlib>
using namespace metal;

struct VsOut {
    float4 pos [[position]];
    float2 uv;
};

// Oversized triangle covering the whole clip rect; UVs map the framebuffer 1:1.
vertex VsOut vs_blit(uint vi [[vertex_id]]) {
    const float2 uv[3] = { float2(0.0, 0.0), float2(2.0, 0.0), float2(0.0, 2.0) };
    VsOut o;
    o.uv = uv[vi];
    // uv (0..2, y down) -> clip (x: -1..3, y: 1..-3).
    o.pos = float4(o.uv.x * 2.0 - 1.0, 1.0 - o.uv.y * 2.0, 0.0, 1.0);
    return o;
}

// std140 twin of the Rust `BlitUniform`, 112 bytes, member for member:
//   flag 0, overlay 4, border_px 8, encode_srgb 12, accent 16, chrome_y1 28,
//   dims 32, wash_a 40, border_a 44, band 48, content_off 64, hdr 72,
//   translucent 76, sdr_white_scale 80, visible_y 84, visible_h 88, premult 92,
//   chrome_y0 96, three pads 100..112.
// Every float4 is 16-byte aligned, so MSL's natural layout equals std140. `accent`
// is a `packed_float3` (12 bytes, 4-aligned) so `chrome_y1` lands at 28, as after
// the WGSL `vec3<f32>`.
struct Blit {
    uint flag;          // bell-flash invert
    uint overlay;       // drop-target highlight enabled
    float border_px;    // inset border thickness, device px
    float encode_srgb;  // !=0: re-encode linear->sRGB (downlevel WebGL2 blit)
    packed_float3 accent; // overlay accent rgb, normalized 0..1
    float chrome_y1;    // host-chrome rows are [chrome_y0, chrome_y1): bands extend edge pixels
    float2 dims;        // offscreen frame width,height in px
    float wash_a;       // interior wash alpha 0..1
    float border_a;     // border alpha 0..1
    float4 band;        // remainder-band colour (live terminal bg; a unused)
    float2 content_off; // frame top-left inside the swapchain, device px
    float hdr;          // !=0: decode sRGB->linear, clamp <=1 (f16 EDR swapchain)
    float translucent;  // !=0: emit the offscreen/band alpha (translucent window)
    float sdr_white_scale; // Windows scRGB reference-white scale; 1.0 on macOS/SDR
    float visible_y;    // first source row exposed by the frontend crop
    float visible_h;    // exposed source height; rows outside are remainder bands
    float premult;      // !=0: multiply output rgb by the emitted alpha
    float chrome_y0;    // first host-chrome source row (rows above it keep flat bands)
    float chrome_pad0;  // pad to 112 bytes, matching the Rust/WGSL twins
    float chrome_pad1;
    float chrome_pad2;
};

// Linear-light channel -> sRGB (the standard piecewise encode).
static inline float l2s(float c) {
    float cc = clamp(c, 0.0, 1.0);
    if (cc <= 0.0031308) { return 12.92 * cc; }
    return 1.055 * pow(cc, 1.0 / 2.4) - 0.055;
}

// EDR present: sRGB-encoded rgb -> linear light, clamped to [0,1]. Same piecewise
// constants as the cell shader's s2l.
static inline float3 hdr_grid_encode3(float3 c) {
    float3 cc = clamp(c, float3(0.0), float3(1.0));
    float3 lo = cc / 12.92;
    float3 hi = pow((cc + float3(0.055)) / 1.055, float3(2.4));
    return clamp(select(lo, hi, cc > float3(0.04045)), float3(0.0), float3(1.0));
}

fragment float4 fs_blit(VsOut in [[stage_in]],
                        texture2d<float> src_tex [[texture(0)]],
                        sampler src_samp [[sampler(0)]],
                        constant Blit& b [[buffer(2)]]) {
    // `in.pos.xy` is the swapchain pixel centre, so `p` floors to exact frame
    // texels: `read` is a 1:1 fetch, never scaled.
    float2 p = in.pos.xy - b.content_off;
    float visible_y1 = b.visible_y + b.visible_h;
    // Beside a host-chrome source row the horizontal bands continue the frame's
    // edge pixel (still a band pixel: never inverted or washed). Rows above
    // `chrome_y0` keep the flat band.
    bool chrome_edge = false;
    if ((p.x < 0.0 || p.x >= b.dims.x) && p.y >= max(b.visible_y, b.chrome_y0) && p.y < min(visible_y1, b.chrome_y1)) {
        p.x = clamp(p.x, 0.0, b.dims.x - 1.0);
        chrome_edge = true;
    }
    if (p.x < 0.0 || p.y < b.visible_y || p.x >= b.dims.x || p.y >= visible_y1) {
        // On a translucent window the bands take the bg alpha; opaque forces 1.0.
        float band_a = select(1.0, b.band.a, b.translucent != 0.0);
        float3 band_rgb = b.band.rgb;
        if (b.hdr != 0.0) {
            // The bands take the grid's encode.
            band_rgb = hdr_grid_encode3(band_rgb) * b.sdr_white_scale;
        }
        // A premultiplied destination wants rgb*a.
        if (b.premult != 0.0) {
            band_rgb = band_rgb * band_a;
        }
        return float4(band_rgb, band_a);
    }
    // `p` is non-negative here, so the uint2 truncation matches the WGSL
    // vec2<i32> conversion (both truncate toward zero).
    float4 c = src_tex.read(uint2(p), 0);
    float3 rgb = c.rgb;
    if (b.flag != 0u && !chrome_edge) {
        rgb = float3(1.0) - rgb;
    }
    // Drop-target highlight: accent wash plus inset border, relative to the
    // visible content frame.
    if (b.overlay != 0u && !chrome_edge) {
        float2 visible_p = float2(p.x, p.y - b.visible_y);
        float edge = min(
            min(visible_p.x, b.dims.x - visible_p.x),
            min(visible_p.y, b.visible_h - visible_p.y)
        );
        float a = b.wash_a;
        if (edge < b.border_px) { a = b.border_a; }
        rgb = mix(rgb, float3(b.accent), a);
    }
    // Downlevel: re-encode linear->sRGB for the non-sRGB swapchain.
    if (b.encode_srgb != 0.0) {
        rgb = float3(l2s(rgb.r), l2s(rgb.g), l2s(rgb.b));
    }
    // EDR present: native-only, so never combined with encode_srgb.
    if (b.hdr != 0.0) {
        rgb = hdr_grid_encode3(rgb) * b.sdr_white_scale;
    }
    // A translucent window emits the offscreen alpha; opaque forces 1.0.
    float out_a = select(1.0, c.a, b.translucent != 0.0);
    // Premultiplied composition (Windows Mica/Acrylic) wants rgb*a.
    if (b.premult != 0.0) {
        rgb = rgb * out_a;
    }
    return float4(rgb, out_a);
}
