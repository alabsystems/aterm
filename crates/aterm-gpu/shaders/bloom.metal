#include <metal_stdlib>
using namespace metal;

struct VsOut {
    float4 pos [[position]];
    float2 uv;
};

// Oversized triangle covering the whole clip rect; UVs map the framebuffer 1:1.
// Named `vs_fs` like the WGSL: `Pipeline::Bloom` names one entry point for both
// backends.
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
    float radius;        // the white part's radius, half-res texels
    float chroma_radius; // the chroma's, always the shorter of the two
    float _pad[3];
    float2 screen;       // the frame, px: a fragment lies at `uv * screen`
    // The frame's letter mask (aterm_render::bloom::LetterMask): the cells the bloom keeps off.
    float2 origin;       // the grid's top-left, px
    float2 cell;         // a cell, px
    float2 grow;         // kept off entirely this far past a kept cell's box, px
    float2 feather;      // easing back to whole over this far past that, px
    float mask_on;       // 0: the bloom is laid whole
    float _pad2;
    uint4 window;        // the cells the bits cover: first column, first row, columns, rows
    uint4 bits[256];     // one bit a cell, row-major: bit i & 31 of word i >> 5
};

// Whether cell (c, r) is kept off (`LetterMask::kept`).
static inline bool kept_cell(constant BloomU& bu, int c, int r) {
    int lc = c - int(bu.window.x);
    int lr = r - int(bu.window.y);
    if (lc < 0 || lr < 0 || lc >= int(bu.window.z) || lr >= int(bu.window.w)) {
        return false;
    }
    uint i = uint(lr) * bu.window.z + uint(lc);
    uint word = bu.bits[i >> 7u][(i >> 5u) & 3u];
    return ((word >> (i & 31u)) & 1u) != 0u;
}

// The share of the bloom laid at frame point p (`LetterMask::weight`, its twin): 0 within a
// kept cell's box grown by `grow`, rising to 1 over `feather` past it.
static inline float letter_mask(constant BloomU& bu, float2 p) {
    if (bu.mask_on == 0.0) {
        return 1.0;
    }
    int c = int(floor((p.x - bu.origin.x) / bu.cell.x));
    int r = int(floor((p.y - bu.origin.y) / bu.cell.y));
    float m = 1.0;
    for (int dr = -2; dr <= 2; dr = dr + 1) {
        for (int dc = -1; dc <= 1; dc = dc + 1) {
            int cc = c + dc;
            int rr = r + dr;
            if (!kept_cell(bu, cc, rr)) {
                continue;
            }
            float bx0 = bu.origin.x + float(cc) * bu.cell.x - bu.grow.x;
            float by0 = bu.origin.y + float(rr) * bu.cell.y - bu.grow.y;
            float bx1 = bx0 + bu.cell.x + 2.0 * bu.grow.x;
            float by1 = by0 + bu.cell.y + 2.0 * bu.grow.y;
            float dx = max(max(bx0 - p.x, p.x - bx1), 0.0) / bu.feather.x;
            float dy = max(max(by0 - p.y, p.y - by1), 0.0) / bu.feather.y;
            float e2 = dx * dx + dy * dy;
            if (e2 >= 1.0) {
                continue;
            }
            float e = sqrt(e2);
            m = min(m, e * e * (3.0 - 2.0 * e));
        }
    }
    return m;
}

// Twin of `hdr_glow.metal`'s `white_part`: the smallest channel is the white light
// in a premultiplied colour, the rest is its chroma.
static inline float white_part(float3 c) {
    return min(min(c.r, c.g), c.b);
}

// One 25-tap gaussian kernel, two radii (design §L5): the source's white part
// spreads at `radius`, its chroma at the shorter `chroma_radius`, and the two sum
// into one halo, so a meteor reads as an over-white streak dying into a coloured
// train and less colour smears into the glyphs beside it.
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
    float kept = letter_mask(bu, in.uv * bu.screen);
    return float4(sum / wsum * bu.strength * kept, 1.0);
}
