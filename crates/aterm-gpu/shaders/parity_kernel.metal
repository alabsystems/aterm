// ===================== VERIFICATION-ONLY COMPUTE KERNELS =====================
// NOT part of the shipped renderer — aterm asks the GPU for ZERO compute (see
// crates/aterm-gpu/src/lib.rs required_features == empty). These kernels exist
// so the parity test can evaluate the SAME fire_core / fire_shade_*_px /
// rain_weight functions the fragment shaders run, at arbitrary coordinates, and
// diff them against aterm_render::fire_field and aterm_render::halo_weight.
//
// This file is CONCATENATED onto cell.metal by the test, so the math under test
// is literally the shipped math — it cannot drift from what fs_fire_add and
// fs_rain_glow use.

struct FireParityParams {
    int base_y;
    int peak_h;
    uint phase;
    int temp;
    int strength;
    int lean;
    int cov_cap;
    int cell_h;
    int top_fade_y;
    int x0;
    int y0;
    int w;
};

kernel void k_fire_parity(constant FireParityParams& p [[buffer(0)]],
                          device uint* out_add [[buffer(1)]],
                          device uint* out_ink [[buffer(2)]],
                          device uint* out_alpha [[buffer(3)]],
                          uint2 gid [[thread_position_in_grid]]) {
    int px = p.x0 + int(gid.x);
    int py = p.y0 + int(gid.y);
    uint i = gid.y * uint(p.w) + gid.x;
    FireCoreOut c = fire_core(px, py, p.base_y, p.peak_h, p.phase,
                              p.temp, p.strength, p.lean, p.cell_h);
    out_add[i] = fire_shade_add_px(c, py, p.temp, p.strength, p.cov_cap,
                                   p.cell_h, p.top_fade_y);
    uint2 io = fire_shade_over_px(c, py, p.temp, p.cov_cap, p.cell_h, p.top_fade_y);
    out_ink[i] = io.x;
    out_alpha[i] = io.y;
}

struct RainParityParams { int cx, cy, rx, ry, cr, cg, cb, x0, y0, w; };

kernel void k_rain_parity(constant RainParityParams& p [[buffer(0)]],
                          device uint* out_wt [[buffer(1)]],
                          device uint* out_add [[buffer(2)]],
                          uint2 gid [[thread_position_in_grid]]) {
    int px = p.x0 + int(gid.x);
    int py = p.y0 + int(gid.y);
    uint i = gid.y * uint(p.w) + gid.x;
    // rain_weight() takes the fragment CENTRE, so feed px+0.5 exactly as the
    // rasterizer hands it to fs_rain_glow.
    int wt = rain_weight(float2(float(px) + 0.5, float(py) + 0.5),
                         uint4(uint(p.cx), uint(p.cy), uint(p.rx), uint(p.ry)));
    out_wt[i] = uint(wt);
    // The Add path's premultiplied byte: mul8(c, wt) == (c*wt + 127)/255.
    out_add[i] = (uint((p.cr * wt + 127) / 255) << 16)
               | (uint((p.cg * wt + 127) / 255) << 8)
               |  uint((p.cb * wt + 127) / 255);
}
