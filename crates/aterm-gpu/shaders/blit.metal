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

// std140 twin of the Rust `BlitUniform` — 112 bytes, member for member:
//   flag 0, overlay 4, border_px 8, encode_srgb 12, accent 16, chrome_y1 28,
//   dims 32, wash_a 40, border_a 44, band 48, content_off 64, hdr 72,
//   translucent 76, sdr_white_scale 80, visible_y 84, visible_h 88, premult 92,
//   chrome_y0 96, three pads 100..112.
// Every float4 lands on a 16-byte boundary in BOTH layouts, so MSL's natural
// constant layout is byte-identical to std140 here and the Rust struct is
// UNCHANGED from the WGSL era. The accent is a `packed_float3` (12 bytes,
// 4-aligned) so `chrome_y1` takes the slot the old float4's unused alpha held —
// the WGSL `vec3<f32>` + `f32` pair lands at the same 16 and 28.
struct Blit {
    uint flag;          // bell-flash invert
    uint overlay;       // drop-target highlight enabled
    float border_px;    // inset border thickness, device px
    float encode_srgb;  // !=0: re-encode linear->sRGB (downlevel WebGL2 blit)
    packed_float3 accent; // overlay accent rgb, normalized 0..1
    float chrome_y1;    // source rows [chrome_y0, chrome_y1) are host chrome: their bands continue the frame's edge pixels
    float2 dims;        // OFFSCREEN frame width,height in px
    float wash_a;       // interior wash alpha 0..1
    float border_a;     // border alpha 0..1
    float4 band;        // W1 remainder-band colour (live terminal bg; a unused)
    float2 content_off; // W1: frame top-left inside the swapchain, device px
    float hdr;          // M3: !=0: decode sRGB->linear, clamp <=1 (f16 EDR swapchain)
    float translucent;  // M5: !=0: emit the offscreen/band ALPHA (translucent glass)
    float sdr_white_scale; // M3 (Windows scRGB): reference-white scale; 1.0 on macOS/SDR
    float visible_y;    // first source row exposed by the frontend crop
    float visible_h;    // exposed source height; rows outside are remainder bands
    float premult;      // H1: !=0: multiply output rgb by the emitted alpha
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

// M3 (EDR present): sRGB-encoded rgb -> linear light, CLAMPED to [0,1] — the
// GRID CLAMP LAW. Same piecewise constants as the cell shader's s2l.
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
    // W1: `in.pos.xy` is the swapchain pixel CENTER (x+0.5, y+0.5), so `p`
    // floors to exact frame texel coords: `read` is a 1:1 texel fetch — zero
    // scaling ever.
    float2 p = in.pos.xy - b.content_off;
    float visible_y1 = b.visible_y + b.visible_h;
    // CHROME REACHES THE WINDOW EDGE (the WGSL twin's words): beside a
    // host-chrome source row the horizontal remainder bands continue the
    // frame's own edge pixel; still a band pixel — never inverted or washed.
    // Rows above `chrome_y0` (a surfaceless strip on the padding) keep the band.
    bool chrome_edge = false;
    if ((p.x < 0.0 || p.x >= b.dims.x) && p.y >= max(b.visible_y, b.chrome_y0) && p.y < min(visible_y1, b.chrome_y1)) {
        p.x = clamp(p.x, 0.0, b.dims.x - 1.0);
        chrome_edge = true;
    }
    if (p.x < 0.0 || p.y < b.visible_y || p.x >= b.dims.x || p.y >= visible_y1) {
        // M5: on a translucent window the remainder bands take the bg-quad
        // alpha too; the opaque default forces 1.0 (byte-identical chrome).
        float band_a = select(1.0, b.band.a, b.translucent != 0.0);
        float3 band_rgb = b.band.rgb;
        if (b.hdr != 0.0) {
            // The bands are the same sheet as the grid, so they take the same
            // encode; sdr_white_scale is 1.0 on macOS / SDR.
            band_rgb = hdr_grid_encode3(band_rgb) * b.sdr_white_scale;
        }
        // H1: a premultiplied destination wants rgb*a. Identity when opaque.
        if (b.premult != 0.0) {
            band_rgb = band_rgb * band_a;
        }
        return float4(band_rgb, band_a);
    }
    // `p` is bounds-checked non-negative above, so the uint2 truncation is the
    // exact twin of the WGSL vec2<i32> conversion (both truncate toward zero).
    float4 c = src_tex.read(uint2(p), 0);
    float3 rgb = c.rgb;
    if (b.flag != 0u && !chrome_edge) {
        rgb = float3(1.0) - rgb;
    }
    // Drag-and-drop drop-target highlight: faint accent wash + inset accent
    // border, relative to the CONTENT frame.
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
    // M3 (EDR present, native-only so mutually exclusive with encode_srgb).
    if (b.hdr != 0.0) {
        rgb = hdr_grid_encode3(rgb) * b.sdr_white_scale;
    }
    // M5 true vibrancy: emit the offscreen alpha on a translucent window; the
    // opaque default forces 1.0 — the byte-identical solid present.
    float out_a = select(1.0, c.a, b.translucent != 0.0);
    // H1 (Windows Mica/Acrylic): premultiplied composition wants rgb*a.
    if (b.premult != 0.0) {
        rgb = rgb * out_a;
    }
    return float4(rgb, out_a);
}
