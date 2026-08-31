// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The macOS Metal backend — FOUNDATION ONLY, not yet a renderer.
//!
//! # What this is, precisely
//!
//! THE ROW retires `wgpu` and `softbuffer` on the macOS cell behind first-party
//! code. This module lands the pieces of that work that can be built AND
//! PROVEN today:
//!
//! 1. [`ffi`] — the hand-written Objective-C binding to Metal and QuartzCore.
//! 2. [`shaders`] — all six of aterm's shaders ported from WGSL to MSL, so that
//!    `naga` (212,320 of the lines this row exists to remove) is not needed to
//!    reach Metal.
//! 3. [`blit`] — the present-path blit, end to end on Metal: pipeline, sampler,
//!    uniform, render pass and readback. This is the ONE experiment
//!    `docs/measured/wgpu-metal-decision-2026-08-30.md` §8 makes the whole row
//!    conditional on, and it is GREEN — see below.
//!
//! # THE §8 GATE: PASSED
//!
//! The decision document names a single falsifiable experiment and a refusal
//! condition: port `build_blit_resources` + `BLIT_SHADER` to Metal, hold the
//! byte-identical blit assertions green, or REFUSE the row.
//!
//! [`tests::blit_matches_wgpu_byte_for_byte`] is that experiment, run as a
//! DIFFERENTIAL rather than as a property check: the same offscreen texels and
//! the same 96 `BlitUniform` bytes go through the shipped `wgpu` blit and
//! through the first-party Metal one, and every byte of both outputs must
//! agree. Measured on an Apple M5 Max, over the four control-flow arms of
//! `fs_blit` — 156,226 pixels, **0 differing**:
//!
//! | arm | destination | pixels | differing |
//! |---|---|---:|---:|
//! | exact fit, passthrough | 264x126 | 33,264 | 0 |
//! | exact fit, bell invert | 264x126 | 33,264 | 0 |
//! | oversized, W1 bands | 301x149 | 44,849 | 0 |
//! | cropped + drop overlay | 301x149 | 44,849 | 0 |
//!
//! The gate is FALSIFIABLE, and that was checked rather than assumed. Five
//! mutations were planted and the results recorded:
//!
//! | mutation | caught | first divergence |
//! |---|---|---|
//! | uniform bound at `[[buffer(1)]]` instead of `(2)` | YES, arm 1 | 24,752/33,264 |
//! | destination `Rgba8UnormSrgb` (the §4.1 gamma double-encode) | YES, arm 1 | 24,752/33,264 |
//! | `1.0 - rgb` → `1.0 - rgb * 0.99` (1 LSB) | YES, arm 2 only | 2,679/33,264 |
//! | band colour scaled 0.9 | YES, arms 3-4 only | 11,585/44,849 |
//! | overlay `mix` alpha halved | YES, arm 4 only | 32,472/44,849 |
//! | crop origin ignored (`p.y < 0.0`) | YES, arm 4 only | 792/44,849 |
//!
//! Two of those matter beyond "the test works". The **sRGB destination** is the
//! exact defect class §4.1 predicts a port would ship and records as invisible
//! to all 274 existing `aterm-gpu` tests; this gate moves 74% of the frame on
//! it. And each mutation fails ONLY the arms that reach it, so the four cases
//! are independently non-vacuous rather than one assertion counted four times.
//!
//! One planted mutation was NOT caught and is recorded because it is evidence
//! about the shader, not about the gate: `edge < border_px` → `edge <=` changes
//! nothing, because `edge` is a distance between half-pixel centres and an
//! integer thickness and is therefore never exactly equal to it. That is an
//! inert edit, not a hole.
//!
//! # What this is NOT
//!
//! It is **not** wired into [`crate::renderer`], and nothing outside this
//! module calls it. `GpuRenderer` is still `wgpu` on every cell, including
//! macOS; `crates/aterm-gpu/Cargo.toml` still names `wgpu` for macOS; and the
//! `forge survey` figure for the mac-arm cell is unmoved at 91 packages /
//! 1,275,882 lines. The renderer is 18,194 lines holding 1,073 `wgpu::`
//! references across 95 distinct items, 129 `GpuRenderer` fields, 17 pipelines,
//! 10 render passes and 7 bind-group layouts; translating that is a separate,
//! much larger phase. What the §8 gate above establishes is that the phase is
//! WORTH STARTING — not that it has started. Landing this module first means the shader port — the
//! part with a real correctness contract — is reviewable and testable on its
//! own, before any of that plumbing moves.
//!
//! There is also no seam to hide behind yet: `docs/measured/gpu-seam-2026-08-30.md`
//! DESIGNED one (three traits plus a cfg-selected module) but no `seam.rs` was
//! ever written, so "gate the new backend behind the seam" has nothing to gate
//! against. This module is therefore gated the only way it currently can be —
//! `#[cfg(target_os = "macos")]` at its declaration in `lib.rs` — and every
//! other cell keeps the wgpu/softbuffer paths byte-for-byte untouched.
//!
//! # The parity contract, and how it was checked
//!
//! Two of aterm's fragment shaders are not free-floating art: `fs_rain_glow`
//! and `fs_fire_add`/`fs_fire_over` reproduce `aterm_render`'s pure-integer
//! CPU field math OP-FOR-OP, and a differential is the referee. Those ports
//! were verified against the CPU by evaluating the SHIPPED MSL functions in a
//! verification-only compute kernel and diffing against
//! `aterm_render::fire_field` / `aterm_render::halo_weight`:
//!
//! * fire: 10 parameter sets, 85,120 pixels (36,462 with non-zero output),
//!   covering zero and maximum temp/strength, both lean extremes, `phase ==
//!   u32::MAX`, minimum `cell_h`, maximum `peak_h`, negative pixel coordinates
//!   and the top-fade band — **0 differing pixels**.
//! * rain halo: 6 parameter sets, 34,000 pixels, including the centre clamp,
//!   degenerate ellipses and negative coordinates — **0 differing pixels**.
//!
//! The one construct that made this delicate is the arithmetic right shift.
//! `fire_core` shifts a possibly-NEGATIVE `int` right (`(body0 - 128) * edge >>
//! 8`), and the parity depends on that being sign-replicating. WGSL defines
//! `i32 >>` as arithmetic and MSL pins the same for signed types, so the twins
//! agree; this is NOT the C++ implementation-defined case.

// This module is deliberately not reachable from `renderer` yet (see the docs
// above), so every binding in it is "dead" until the plumbing phase wires it
// up. The alternative — deleting the unused half of the FFI — would mean the
// pipeline/format tests below could not exist, and those tests are the entire
// reason the shader port is trustworthy. Scoped to this module only.
#![allow(dead_code)]

pub(crate) mod blit;
pub(crate) mod ffi;
pub(crate) mod shaders;

#[cfg(test)]
mod tests {
    use super::ffi::{
        BlendFactor, Device, PixelFormat, RenderPipelineDescriptor,
        TEXTURE_USAGE_PIXEL_FORMAT_VIEW, TEXTURE_USAGE_RENDER_TARGET, TEXTURE_USAGE_SHADER_READ,
        VertexDescriptor, VertexFormat,
    };
    use super::{ffi, shaders};

    /// Every test here needs a GPU. A machine without one (a VM, a sandbox that
    /// denies the device) should SKIP rather than fail, but a machine WITH one
    /// must actually run the checks — so the skip is loud.
    fn device() -> Option<Device> {
        let d = Device::system_default();
        if d.is_none() {
            eprintln!("SKIP: no Metal device on this machine");
        }
        d
    }

    /// The load-bearing test: every shader compiles and every entry point the
    /// renderer will ask for resolves. This is what replaces `naga`.
    #[test]
    fn all_shaders_compile_and_expose_their_entry_points() {
        let Some(dev) = device() else { return };
        for (name, src, entries) in shaders::LIBRARIES {
            let lib = dev
                .new_library(src)
                .unwrap_or_else(|e| panic!("{name}.metal failed to compile:\n{e}"));
            for e in *entries {
                assert!(
                    lib.function(e).is_some(),
                    "{name}.metal is missing entry point `{e}`"
                );
            }
        }
    }

    /// The four formats the renderer names must all be real, and the offscreen
    /// pair must be view-compatible — that pairing is what carries the sRGB
    /// encode law (see [`super::shaders`]).
    #[test]
    fn the_four_renderer_formats_exist() {
        let Some(dev) = device() else { return };
        for f in [
            PixelFormat::Bgra8Unorm,
            PixelFormat::Rgba8Unorm,
            PixelFormat::R8Unorm,
            PixelFormat::Rgba16Float,
        ] {
            assert!(
                dev.new_texture_2d(f, 16, 16, TEXTURE_USAGE_SHADER_READ)
                    .is_some(),
                "{f:?} texture creation failed"
            );
        }
        // The offscreen: a Unorm texture that a sRGB-typed view can alias.
        assert!(
            dev.new_texture_2d(
                PixelFormat::Bgra8Unorm,
                16,
                16,
                TEXTURE_USAGE_RENDER_TARGET
                    | TEXTURE_USAGE_SHADER_READ
                    | TEXTURE_USAGE_PIXEL_FORMAT_VIEW,
            )
            .is_some(),
            "view-capable offscreen creation failed"
        );
    }

    /// Build a real `MTLRenderPipelineState` for the background pass on the
    /// non-sRGB swapchain format, with the BG instance layout and the OVER
    /// blend state. This validates the vertex descriptor, the attribute format
    /// constants and the blend-factor constants together — none of which the
    /// shader compile alone can check.
    #[test]
    fn bg_pipeline_builds_with_the_real_vertex_layout() {
        let Some(dev) = device() else { return };
        let lib = dev.new_library(shaders::CELL).expect("cell.metal compiles");
        let vs = lib.function("vs_bg").expect("vs_bg");
        let fs = lib.function("fs_bg").expect("fs_bg");

        // BgInstance is `[u16;4]` + `[u8;4]` == 12 bytes, matching the
        // `size_of` assertion in renderer.rs.
        let vd = VertexDescriptor::new().expect("vertex descriptor");
        vd.attribute(0, VertexFormat::UShort4, 0, 0);
        vd.attribute(1, VertexFormat::UChar4Normalized, 8, 0);
        vd.layout_per_instance(0, 12);

        let desc = RenderPipelineDescriptor::new().expect("pipeline descriptor");
        desc.set_vertex_function(&vs);
        desc.set_fragment_function(&fs);
        desc.set_vertex_descriptor(&vd);
        desc.set_color_attachment(
            PixelFormat::Bgra8Unorm,
            Some((
                BlendFactor::SourceAlpha,
                BlendFactor::OneMinusSourceAlpha,
                BlendFactor::One,
                BlendFactor::OneMinusSourceAlpha,
            )),
        );
        dev.new_render_pipeline(&desc)
            .expect("bg pipeline state builds");
    }

    /// The fire pipeline exercises the widest instance layout: `[u16;4]` +
    /// `[u16;4]` + `u32` + `[u8;4]` == 24 bytes, four attributes, three
    /// distinct integer vertex formats.
    #[test]
    fn fire_pipeline_builds_with_the_real_vertex_layout() {
        let Some(dev) = device() else { return };
        let lib = dev.new_library(shaders::CELL).expect("cell.metal compiles");
        let vs = lib.function("vs_fire").expect("vs_fire");
        let fs = lib.function("fs_fire_add").expect("fs_fire_add");

        let vd = VertexDescriptor::new().expect("vertex descriptor");
        vd.attribute(0, VertexFormat::UShort4, 0, 0);
        vd.attribute(1, VertexFormat::UShort4, 8, 0);
        vd.attribute(2, VertexFormat::UInt, 16, 0);
        vd.attribute(3, VertexFormat::UChar4, 20, 0);
        vd.layout_per_instance(0, 24);

        let desc = RenderPipelineDescriptor::new().expect("pipeline descriptor");
        desc.set_vertex_function(&vs);
        desc.set_fragment_function(&fs);
        desc.set_vertex_descriptor(&vd);
        // FireMode::Add composites One/One on the Unorm view.
        desc.set_color_attachment(
            PixelFormat::Bgra8Unorm,
            Some((
                BlendFactor::One,
                BlendFactor::One,
                BlendFactor::One,
                BlendFactor::One,
            )),
        );
        dev.new_render_pipeline(&desc)
            .expect("fire pipeline state builds");
    }

    /// The glyph pipeline runs on the sRGB-typed VIEW format and samples the
    /// R8 atlas, so it pins the other half of the format law.
    #[test]
    fn glyph_pipeline_builds_on_the_srgb_view_format() {
        let Some(dev) = device() else { return };
        let lib = dev.new_library(shaders::CELL).expect("cell.metal compiles");
        let vs = lib.function("vs_glyph").expect("vs_glyph");
        let fs = lib.function("fs_glyph").expect("fs_glyph");

        // GlyphInstance is f32x4 + f32x4 + u8x4 + u8x4 == 40 bytes.
        let vd = VertexDescriptor::new().expect("vertex descriptor");
        vd.attribute(0, VertexFormat::Float4, 0, 0);
        vd.attribute(1, VertexFormat::Float4, 16, 0);
        vd.attribute(2, VertexFormat::UChar4Normalized, 32, 0);
        vd.attribute(3, VertexFormat::UChar4Normalized, 36, 0);
        vd.layout_per_instance(0, 40);

        let desc = RenderPipelineDescriptor::new().expect("pipeline descriptor");
        desc.set_vertex_function(&vs);
        desc.set_fragment_function(&fs);
        desc.set_vertex_descriptor(&vd);
        desc.set_color_attachment(
            PixelFormat::Bgra8UnormSrgb,
            Some((
                BlendFactor::SourceAlpha,
                BlendFactor::OneMinusSourceAlpha,
                BlendFactor::One,
                BlendFactor::OneMinusSourceAlpha,
            )),
        );
        dev.new_render_pipeline(&desc)
            .expect("glyph pipeline state builds");
    }

    /// The EDR arm: the aurora crown pipeline on the `Rgba16Float` swapchain.
    #[test]
    fn hdr_glow_pipeline_builds_on_rgba16float() {
        let Some(dev) = device() else { return };
        let lib = dev
            .new_library(shaders::HDR_GLOW)
            .expect("hdr_glow.metal compiles");
        let vs = lib.function("vs_hdr_glow").expect("vs_hdr_glow");
        let fs = lib.function("fs_hdr_glow").expect("fs_hdr_glow");

        let vd = VertexDescriptor::new().expect("vertex descriptor");
        vd.attribute(0, VertexFormat::UShort4, 0, 0);
        vd.attribute(1, VertexFormat::UChar4Normalized, 8, 0);
        vd.layout_per_instance(0, 12);

        let desc = RenderPipelineDescriptor::new().expect("pipeline descriptor");
        desc.set_vertex_function(&vs);
        desc.set_fragment_function(&fs);
        desc.set_vertex_descriptor(&vd);
        desc.set_color_attachment(
            PixelFormat::Rgba16Float,
            Some((
                BlendFactor::One,
                BlendFactor::One,
                BlendFactor::Zero,
                BlendFactor::One,
            )),
        );
        dev.new_render_pipeline(&desc)
            .expect("hdr glow pipeline state builds");
    }

    // -----------------------------------------------------------------
    // THE PARITY DIFFERENTIAL.
    //
    // `fs_fire_*` and `fs_rain_glow*` are not free art: they reproduce
    // `aterm_render`'s pure-integer CPU field math OP-FOR-OP so the CPU and GPU
    // rasterizers agree byte-for-byte. These tests are the referee, and they
    // run the SHIPPED MSL functions (the kernels are concatenated onto
    // `cell.metal`) against the SHIPPED CPU functions.
    // -----------------------------------------------------------------

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct FireParityParams {
        base_y: i32,
        peak_h: i32,
        phase: u32,
        temp: i32,
        strength: i32,
        lean: i32,
        cov_cap: i32,
        cell_h: i32,
        top_fade_y: i32,
        x0: i32,
        y0: i32,
        w: i32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct RainParityParams {
        cx: i32,
        cy: i32,
        rx: i32,
        ry: i32,
        cr: i32,
        cg: i32,
        cb: i32,
        x0: i32,
        y0: i32,
        w: i32,
    }

    /// # Safety
    /// `T` must be `repr(C)` and free of padding-sensitive invariants; it is
    /// copied verbatim into a GPU-visible buffer.
    unsafe fn as_bytes<T: Copy>(v: &T) -> &[u8] {
        // SAFETY: `T: Copy` and `repr(C)`; the slice is read-only and lives
        // only for the copy into the Metal buffer.
        unsafe { std::slice::from_raw_parts(std::ptr::from_ref(v).cast::<u8>(), size_of::<T>()) }
    }

    /// EMBERFORGE: the MSL fire field must equal `aterm_render::fire_field`
    /// exactly, at every pixel, for every parameter extreme.
    #[test]
    fn fire_field_matches_the_cpu_bit_for_bit() {
        use aterm_render::fire_field::{FireFieldParams, fire_field_add, fire_field_over};

        let Some(dev) = device() else { return };
        let lib = dev
            .new_library(&shaders::cell_with_parity_kernels())
            .expect("cell.metal + parity kernels compile");
        let kf = lib.function("k_fire_parity").expect("k_fire_parity");
        let pso = dev.new_compute_pipeline(&kf).expect("fire compute pso");
        let queue = dev.new_command_queue().expect("queue");

        // A case is the GPU param block plus the row count. Spelled through a
        // constructor rather than a bare 13-tuple so each column is named once
        // and the table below stays one line per case.
        #[expect(clippy::too_many_arguments, reason = "one named column per field")]
        const fn case(
            base_y: i32,
            peak_h: i32,
            phase: u32,
            temp: i32,
            strength: i32,
            lean: i32,
            cov_cap: i32,
            cell_h: i32,
            top_fade_y: i32,
            x0: i32,
            y0: i32,
            w: i32,
            h: i32,
        ) -> (FireParityParams, i32) {
            (
                FireParityParams {
                    base_y,
                    peak_h,
                    phase,
                    temp,
                    strength,
                    lean,
                    cov_cap,
                    cell_h,
                    top_fade_y,
                    x0,
                    y0,
                    w,
                },
                h,
            )
        }

        // Covers: the baseline, zero temp/strength, both lean extremes at
        // maximum heat, `phase == u32::MAX`, the top-fade band, minimum cell
        // height with maximum peak, a degenerate peak, negative pixel
        // coordinates, and an offset region.
        let cases = [
            case(200, 120, 12345, 180, 220, -37, 255, 18, 40, 0, 60, 64, 160),
            case(200, 120, 0, 0, 0, 0, 255, 18, 40, 0, 60, 48, 160),
            case(
                200, 120, 999_999, 255, 255, 127, 255, 18, 40, 0, 60, 48, 160,
            ),
            case(200, 120, 7777, 255, 255, -128, 255, 18, 40, 0, 60, 48, 160),
            case(200, 120, u32::MAX, 128, 128, 0, 255, 18, 40, 0, 60, 48, 160),
            case(60, 120, 5150, 200, 200, 40, 255, 18, 40, 0, 0, 48, 120),
            case(200, 2048, 31337, 200, 255, -60, 255, 2, 0, 0, 40, 48, 200),
            case(200, 1, 31337, 200, 255, 60, 64, 64, 40, 0, 150, 48, 100),
            case(200, 120, 88, 90, 140, 12, 200, 12, 40, -40, 60, 80, 140),
            case(
                512, 300, 271_828, 220, 240, -90, 255, 24, 100, 200, 200, 64, 200,
            ),
        ];

        let mut total = 0usize;
        let mut nonzero = 0usize;
        for (p, h) in cases {
            let (w, x0, y0) = (p.w, p.x0, p.y0);
            let n = (w * h) as usize;
            let pbuf = dev
                .new_buffer(size_of::<FireParityParams>())
                .expect("params");
            // SAFETY: `FireParityParams` is `repr(C)` and the buffer was just
            // created at exactly its size; no GPU work is in flight.
            unsafe { ffi::buffer_write(&pbuf, as_bytes(&p)) };
            let b_add = dev.new_buffer(n * 4).expect("add");
            let b_ink = dev.new_buffer(n * 4).expect("ink");
            let b_alpha = dev.new_buffer(n * 4).expect("alpha");
            ffi::dispatch_compute(
                &queue,
                &pso,
                &[&pbuf, &b_add, &b_ink, &b_alpha],
                ffi::MtlSize {
                    width: w as usize,
                    height: h as usize,
                    depth: 1,
                },
            );
            // SAFETY: `dispatch_compute` blocked on `waitUntilCompleted`, so
            // every GPU write to these shared buffers has landed.
            let (g_add, g_ink, g_alpha) = unsafe {
                (
                    ffi::buffer_u32s(&b_add, n),
                    ffi::buffer_u32s(&b_ink, n),
                    ffi::buffer_u32s(&b_alpha, n),
                )
            };

            let cpu = FireFieldParams {
                base_y: p.base_y,
                peak_h: p.peak_h,
                phase: p.phase,
                temp: p.temp,
                strength: p.strength,
                lean: p.lean,
                cov_cap: p.cov_cap,
                cell_h: p.cell_h,
                top_fade_y: p.top_fade_y,
            };
            for y in 0..h {
                for x in 0..w {
                    let i = (y * w + x) as usize;
                    let (px, py) = (x0 + x, y0 + y);
                    let add = fire_field_add(px, py, &cpu);
                    let (ink, alpha) = fire_field_over(px, py, &cpu);
                    assert_eq!(g_add[i], add, "fire ADD mismatch at ({px},{py}) {cpu:?}");
                    assert_eq!(g_ink[i], ink, "fire INK mismatch at ({px},{py}) {cpu:?}");
                    assert_eq!(
                        g_alpha[i],
                        u32::from(alpha),
                        "fire ALPHA mismatch at ({px},{py}) {cpu:?}"
                    );
                    total += 1;
                    if add != 0 || alpha != 0 {
                        nonzero += 1;
                    }
                }
            }
        }
        // A differential that only ever compared zeros would pass vacuously.
        assert!(
            nonzero * 4 > total,
            "parity sweep is too sparse to be meaningful: {nonzero}/{total} non-zero"
        );
    }

    /// PHOSPHOR: the MSL rain halo falloff must equal `aterm_render`'s.
    #[test]
    fn rain_halo_matches_the_cpu_bit_for_bit() {
        use aterm_render::{halo_row_ny, halo_weight};

        let Some(dev) = device() else { return };
        let lib = dev
            .new_library(&shaders::cell_with_parity_kernels())
            .expect("cell.metal + parity kernels compile");
        let kf = lib.function("k_rain_parity").expect("k_rain_parity");
        let pso = dev.new_compute_pipeline(&kf).expect("rain compute pso");
        let queue = dev.new_command_queue().expect("queue");

        // Covers the centre clamp, both degenerate ellipse orientations,
        // negative coordinates and the dim-colour rounding edge.
        #[expect(clippy::too_many_arguments, reason = "one named column per field")]
        const fn case(
            cx: i32,
            cy: i32,
            rx: i32,
            ry: i32,
            cr: i32,
            cg: i32,
            cb: i32,
            x0: i32,
            y0: i32,
            w: i32,
            h: i32,
        ) -> (RainParityParams, i32) {
            (
                RainParityParams {
                    cx,
                    cy,
                    rx,
                    ry,
                    cr,
                    cg,
                    cb,
                    x0,
                    y0,
                    w,
                },
                h,
            )
        }

        let cases = [
            case(100, 100, 20, 30, 255, 200, 150, 60, 50, 80, 100),
            case(100, 100, 8, 8, 255, 255, 255, 80, 80, 40, 40),
            case(100, 100, 64, 12, 200, 100, 50, 20, 80, 160, 40),
            case(100, 100, 12, 64, 60, 220, 255, 80, 20, 40, 160),
            case(0, 0, 16, 16, 255, 0, 128, -20, -20, 40, 40),
            case(500, 400, 40, 40, 1, 2, 3, 450, 350, 100, 100),
        ];

        let mut checked = 0usize;
        for (p, h) in cases {
            let (cx, cy, rx, ry) = (p.cx, p.cy, p.rx, p.ry);
            let (cr, cg, cb) = (p.cr, p.cg, p.cb);
            let (w, x0, y0) = (p.w, p.x0, p.y0);
            let n = (w * h) as usize;
            let pbuf = dev
                .new_buffer(size_of::<RainParityParams>())
                .expect("params");
            // SAFETY: `repr(C)` struct into an exactly-sized fresh buffer.
            unsafe { ffi::buffer_write(&pbuf, as_bytes(&p)) };
            let b_wt = dev.new_buffer(n * 4).expect("wt");
            let b_add = dev.new_buffer(n * 4).expect("add");
            ffi::dispatch_compute(
                &queue,
                &pso,
                &[&pbuf, &b_wt, &b_add],
                ffi::MtlSize {
                    width: w as usize,
                    height: h as usize,
                    depth: 1,
                },
            );
            // SAFETY: the dispatch blocked until completion.
            let (g_wt, g_add) =
                unsafe { (ffi::buffer_u32s(&b_wt, n), ffi::buffer_u32s(&b_add, n)) };

            for y in 0..h {
                for x in 0..w {
                    let i = (y * w + x) as usize;
                    let (px, py) = (x0 + x, y0 + y);
                    let ny = halo_row_ny(py - cy, ry * ry);
                    // The shader clamps the live weight to 255; the CPU leaves
                    // that to its callers (`draw_rain_add`), so clamp here to
                    // compare the same quantity.
                    let wt = halo_weight(px - cx, ny, rx * rx).min(255);
                    let m = |c: i32| ((c * wt + 127) / 255) as u32;
                    assert_eq!(g_wt[i], wt as u32, "halo WEIGHT mismatch at ({px},{py})");
                    assert_eq!(
                        g_add[i],
                        (m(cr) << 16) | (m(cg) << 8) | m(cb),
                        "halo ADD mismatch at ({px},{py})"
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked > 30_000, "halo sweep unexpectedly small: {checked}");
    }

    /// Buffers, samplers and a command queue — the remaining objects a frame
    /// needs, so every selector this module sends is exercised at least once.
    #[test]
    fn device_objects_allocate() {
        let Some(dev) = device() else { return };
        assert!(!dev.name().is_empty(), "device reports a name");
        assert!(dev.new_buffer(4096).is_some(), "buffer");
        assert!(dev.new_sampler().is_some(), "sampler");
        assert!(dev.new_command_queue().is_some(), "command queue");
    }

    // -----------------------------------------------------------------------
    // THE GATE. `wgpu-metal-decision-2026-08-30.md` §8 makes the whole row
    // conditional on ONE experiment: port `build_blit_resources` + `BLIT_SHADER`
    // to Metal and hold the byte-identical blit assertions green, or REFUSE.
    // Everything below is that experiment.
    // -----------------------------------------------------------------------

    use aterm_core::terminal::Terminal;
    use aterm_render::{Frame, RenderInput, Theme};

    use super::blit::MetalBlit;
    use crate::{DropOverlay, GpuRenderer, PresentCrop, WindowGpu};

    const ROWS: usize = 6;
    const COLS: usize = 24;

    /// The same representative frame `tests/blit_invert.rs` uses: a prompt, a
    /// glyph, and saturated red/green/blue SGR runs, so the differential runs
    /// over real glyph coverage and real colour, not a synthetic gradient.
    fn representative_input() -> RenderInput {
        let mut term = Terminal::new(ROWS as u16, COLS as u16);
        term.process(b"$ blit check >_\r\n");
        term.process(b"\x1b[31mRED\x1b[0m \x1b[32mGREEN\x1b[0m \x1b[34mBLUE\x1b[0m\r\n");
        term.process(b"\x1b[1mbold\x1b[0m plain 0123456789");
        term.cell_frame(ROWS, COLS)
    }

    /// The readback packs `((255 - a) << 24) | (r << 16) | (g << 8) | b`
    /// (`lib.rs::try_read_back`). Undo it to recover the texel bytes the blit
    /// actually sampled — every channel is recoverable, so this is lossless and
    /// the Metal arm samples the SAME texels the wgpu arm did.
    fn frame_to_rgba8(f: &Frame) -> Vec<u8> {
        let mut out = Vec::with_capacity(f.pixels.len() * 4);
        for &p in &f.pixels {
            out.push(((p >> 16) & 0xff) as u8);
            out.push(((p >> 8) & 0xff) as u8);
            out.push((p & 0xff) as u8);
            out.push((255 - ((p >> 24) & 0xff)) as u8);
        }
        out
    }

    /// Pack tightly-packed `Rgba8Unorm` bytes the way the wgpu readback packs
    /// them, so both sides are compared in one representation.
    fn rgba8_to_packed(bytes: &[u8]) -> Vec<u32> {
        bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| {
                ((255 - u32::from(c[3])) << 24)
                    | (u32::from(c[0]) << 16)
                    | (u32::from(c[1]) << 8)
                    | u32::from(c[2])
            })
            .collect()
    }

    /// THE FALSIFIABLE GATE: the first-party Metal blit must be BYTE-IDENTICAL
    /// to the shipped wgpu blit, on the same source texels and the same 96
    /// uniform bytes, across all four present geometries `fs_blit` branches on.
    ///
    /// This is deliberately stronger than `tests/blit_invert.rs` and
    /// `tests/blit_bands.rs`. Those assert PROPERTIES of one backend's output
    /// (`out == src`, `out == 255 - src`, "the bands are the theme background"),
    /// which a second backend can satisfy while still differing: a half-texel
    /// offset in the fullscreen triangle, an sRGB-typed destination, a filtered
    /// fetch instead of `read()`, or a swapped binding slot all preserve those
    /// properties on the arms they cover. Pinning byte equality against the
    /// shipped backend over real glyph pixels leaves none of that anywhere to
    /// hide — measured: an `Rgba8UnormSrgb` destination (the gamma
    /// double-encode of `wgpu-metal-decision-2026-08-30.md` §4.1, which that
    /// document records as invisible to all 274 existing tests) moves 24,752 of
    /// 33,264 pixels here.
    ///
    /// The four cases are the four control-flow arms of `fs_blit`:
    ///   1. exact fit, no invert   — every pixel in-bounds, the passthrough
    ///   2. exact fit, inverted    — the `flag != 0` bell arm
    ///   3. oversized destination  — the W1 remainder-band arm + `content_off`
    ///   4. cropped + drop overlay — `visible_y`/`visible_h` plus the wash and
    ///      the inset border (`edge < border_px`), the only arm where the two
    ///      shaders' `min`/`mix` chains must agree in floating point rather
    ///      than trivially.
    #[test]
    fn blit_matches_wgpu_byte_for_byte() {
        if device().is_none() {
            return;
        }
        let mut gpu = match GpuRenderer::new(18.0, Theme::default()) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("SKIP: no wgpu renderer/font to differentiate against: {e}");
                return;
            }
        };
        let mut win = WindowGpu::new();

        // ONE repaint. Every case below blits the SAME resident offscreen, so
        // the source texels are fixed and any divergence is the blit's.
        let source = gpu.render_input(&mut win, &representative_input(), None);
        let (sw, sh) = (source.width as u32, source.height as u32);
        assert!(sw > 0 && sh > 3, "renderer produced an empty offscreen");
        let src_rgba = frame_to_rgba8(&source);

        let mb = MetalBlit::new(ffi::PixelFormat::Rgba8Unorm)
            .expect("the Metal blit pipeline must build on a machine with a Metal device");
        eprintln!(
            "blit differential on {} — source {sw}x{sh}",
            mb.device_name()
        );

        /// Which wgpu test helper produces this case's expectation. `extra_*`
        /// widens the destination past the frame, which is what turns the W1
        /// band branch on.
        enum Arm {
            /// `blit_to_offscreen_for_test` — exact fit, `content_off == 0`.
            ExactFit { invert: bool },
            /// `blit_to_sized_for_test` — oversized destination, bands.
            Sized,
            /// `blit_to_sized_cropped_for_test` — bands + crop + drop overlay.
            CroppedOverlay,
        }
        const OVERLAY: DropOverlay = DropOverlay {
            accent: 0x0033_88ff,
            wash_a: 40,
            border_a: 200,
        };

        for (name, extra_w, extra_h, arm) in [
            (
                "exact fit, passthrough",
                0,
                0,
                Arm::ExactFit { invert: false },
            ),
            (
                "exact fit, bell invert",
                0,
                0,
                Arm::ExactFit { invert: true },
            ),
            ("oversized, W1 bands", 37, 23, Arm::Sized),
            ("cropped + drop overlay", 37, 23, Arm::CroppedOverlay),
        ] {
            let (dw, dh) = (sw + extra_w, sh + extra_h);
            let expected = match arm {
                Arm::ExactFit { invert } => gpu.blit_to_offscreen_for_test(&mut win, invert),
                Arm::Sized => gpu.blit_to_sized_for_test(&mut win, false, dw, dh),
                Arm::CroppedOverlay => gpu.blit_to_sized_cropped_for_test(
                    &mut win,
                    false,
                    Some(OVERLAY),
                    PresentCrop {
                        source_y: 3,
                        height: sh - 3,
                    },
                    dw,
                    dh,
                ),
            };
            let uniform = gpu
                .last_blit_uniform_bytes()
                .expect("the wgpu blit records the uniform it wrote");
            assert_eq!(
                uniform.len(),
                MetalBlit::UNIFORM_BYTES,
                "BlitUniform grew: add the member to `shaders/blit.metal`'s \
                 `Blit` struct before widening this constant"
            );

            let actual = mb
                .run(&src_rgba, sw, sh, &uniform, dw, dh)
                .unwrap_or_else(|e| panic!("[{name}] Metal blit failed: {e}"));
            let actual = rgba8_to_packed(&actual);

            assert_eq!(
                (expected.width, expected.height),
                (dw as usize, dh as usize),
                "[{name}] wgpu produced the wrong destination size"
            );
            assert_eq!(
                actual.len(),
                expected.pixels.len(),
                "[{name}] pixel-count mismatch"
            );
            if actual != expected.pixels {
                let mut diffs = 0usize;
                let mut first = None;
                for (i, (&e, &a)) in expected.pixels.iter().zip(actual.iter()).enumerate() {
                    if e != a {
                        diffs += 1;
                        if first.is_none() {
                            first = Some((i, e, a));
                        }
                    }
                }
                let (i, e, a) = first.expect("the vectors differ, so some element differs");
                let (x, y) = (i % dw as usize, i / dw as usize);
                panic!(
                    "[{name}] METAL BLIT IS NOT BYTE-IDENTICAL TO wgpu: {diffs} of {} pixels \
                     differ; first at ({x},{y}): wgpu {e:#010x} != metal {a:#010x}",
                    actual.len()
                );
            }
            eprintln!(
                "  [{name}] {dw}x{dh}: byte-identical over {} pixels",
                actual.len()
            );
        }
    }
}
