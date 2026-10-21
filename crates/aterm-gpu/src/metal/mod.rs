// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The macOS Metal backend, the renderer's arm on macOS. `wgpu` stays on macOS
//! only behind the `wgpu-oracle` feature, as the test oracle every Metal row,
//! frame and present is byte-compared against.
//!
//! * [`ffi`]: the hand-written Objective-C binding to Metal (`CAMetalLayer`
//!   lives in [`swapchain`], beside the QuartzCore link).
//! * [`shaders`]: the MSL twins of the WGSL shaders, so `naga` is not needed.
//! * [`pipelines`]: `MTLRenderPipelineState`s built from
//!   [`crate::pipeline_table`], the rows the wgpu pipelines are built from.
//! * [`loss`]: the device-loss latch. Metal has no device-lost callback; the
//!   signal is per-command-buffer status, classified retryable or lost and
//!   latched sticky.
//! * [`swapchain`]: `CAMetalLayer` with a bounded acquire, present in
//!   scheduled-safe order, and the frame boundary as a type.
//! * [`warm`]: the first window's shaders compiled on a launch worker and
//!   discarded, so the main thread's compile is a Metal cache hit.
//! * `present`, `encoder`, `resources`, `acquire_worker`: the present arm,
//!   command encoding, loss-domain-sealed resources and the off-thread
//!   drawable acquire.
//! * `blit` (test-only): the present blit on Metal, held byte-identical to the
//!   wgpu blit.
//!
//! Every piece of pipeline state the renderer sets (write mask, sampler
//! filters, compile options) is proven by a GPU readback in `tests`: Metal's
//! defaults are permissive, so a missing setter is silent. Scissor and
//! viewport are proven by the scissored and letterboxed differentials.
//!
//! # CPU parity
//!
//! `fs_rain_glow` and `fs_fire_add`/`fs_fire_over` reproduce `aterm_render`'s
//! integer field math op for op; the tests run the shipped MSL functions in a
//! compute kernel against `aterm_render::fire_field` and
//! `aterm_render::halo_weight`. The parity relies on `>>` of a negative `int`
//! being arithmetic, which WGSL and MSL both define.

pub(crate) mod acquire_worker;
#[cfg(test)]
pub(crate) mod blit;
pub(crate) mod encoder;
pub(crate) mod ffi;
pub(crate) mod loss;
pub(crate) mod pipelines;
pub(crate) mod present;
pub(crate) mod resources;
pub(crate) mod shaders;
pub(crate) mod swapchain;
pub(crate) mod warm;

/// The helpers every GPU test module under `metal` shares.
#[cfg(test)]
pub(crate) mod test_support {
    use super::ffi::{self, Device, Obj};

    /// A device for GPU tests; a machine without one ends the test COULD NOT RUN.
    ///
    /// Uses [`Device::preferred`], the device the Metal arm renders on: on a
    /// dual-GPU Mac the system default is the discrete GPU while the wgpu arm
    /// asks for the low-power one, and two GPUs can round a blended channel
    /// differently.
    pub(crate) fn device() -> Device {
        Device::preferred().unwrap_or_else(|| aterm_spec::host::could_not_run("no Metal device"))
    }

    /// `cell.metal`'s 16-byte `Uniforms` block: `float2 screen`,
    /// `float text_blend`, `float pad`, laid out identically in `repr(C)`.
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub(crate) struct CellUniforms {
        pub(crate) screen: [f32; 2],
        pub(crate) text_blend: f32,
        pub(crate) pad: f32,
    }

    /// # Safety
    /// `T` must be `repr(C)` and free of padding-sensitive invariants; the
    /// bytes are copied straight into a GPU-visible buffer.
    pub(crate) unsafe fn as_bytes<T: Copy>(v: &T) -> &[u8] {
        // SAFETY: `T: Copy` + `repr(C)`; read-only, lives only for the copy.
        unsafe { std::slice::from_raw_parts(std::ptr::from_ref(v).cast::<u8>(), size_of::<T>()) }
    }

    /// A bare `CALayer` (+1) — the stand-in for an NSView's root layer (and,
    /// in the swapchain stacking tests, for wgpu's leftover CAMetalLayer).
    pub(crate) fn plain_calayer() -> Obj {
        let _pool = ffi::AutoreleasePool::new();
        // SAFETY: alloc/init, +1, exactly as `Swapchain::new_layer`.
        unsafe {
            let alloc: unsafe extern "C-unwind" fn(ffi::ClassPtr, ffi::Sel) -> ffi::Id =
                ffi::checked_msg();
            let raw = alloc(ffi::class(c"CALayer"), ffi::sel(c"alloc"));
            let init: unsafe extern "C-unwind" fn(ffi::Id, ffi::Sel) -> ffi::Id =
                ffi::checked_msg();
            Obj::from_owned(init(raw, ffi::sel(c"init"))).expect("CALayer init")
        }
    }

    /// The two strings a C-string read of an `NSString` gets wrong, as UTF-16
    /// code units and the text they must read back as: an embedded U+0000
    /// (`-UTF8String` stops there) and an unpaired surrogate (`-UTF8String`
    /// answers NULL; the counted reader gives U+FFFD and keeps the rest).
    pub(crate) const C_STRING_HOLES: [(&[u16], &str); 2] = [
        (&[0x61, 0x0000, 0x62], "a\u{0}b"),
        (&[0x61, 0xD800, 0x62], "a\u{FFFD}b"),
    ];

    /// A +1 `NSString` of exactly `units`, built without a C string so it may
    /// hold U+0000 or an unpaired surrogate, which no Rust `str` can spell.
    pub(crate) fn ns_string_of_units(units: &[u16]) -> Obj {
        // SAFETY: `+alloc` returns a +1 instance that
        // `-initWithCharacters:length:` (`@32@0:8r^S16Q24`) consumes, reading
        // `units.len()` code units during the call and returning +1 or nil.
        unsafe {
            let alloc: unsafe extern "C-unwind" fn(ffi::ClassPtr, ffi::Sel) -> ffi::Id =
                ffi::checked_msg();
            let raw = alloc(ffi::class(c"NSString"), ffi::sel(c"alloc"));
            let init: unsafe extern "C-unwind" fn(ffi::Id, ffi::Sel, *const u16, usize) -> ffi::Id =
                ffi::checked_msg();
            Obj::from_owned(init(
                raw,
                ffi::sel(c"initWithCharacters:length:"),
                units.as_ptr(),
                units.len(),
            ))
        }
        .expect("NSString from UTF-16")
    }

    /// `-[CALayer sublayers].count` (0 when the property is nil).
    pub(crate) fn sublayer_count(layer: &Obj) -> usize {
        let _pool = ffi::AutoreleasePool::new();
        // SAFETY: `sublayers` returns a +0 NSArray (or nil) owned by the
        // pool; `count` is an NSUInteger getter on it.
        unsafe {
            let get: unsafe extern "C-unwind" fn(ffi::Id, ffi::Sel) -> ffi::Id = ffi::checked_msg();
            let arr = get(layer.id(), ffi::sel(c"sublayers"));
            if arr.is_null() {
                return 0;
            }
            let count: unsafe extern "C-unwind" fn(ffi::Id, ffi::Sel) -> usize = ffi::checked_msg();
            count(arr, ffi::sel(c"count"))
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(wgpu_arm)]
    use super::ffi::MtlScissorRect;
    use super::ffi::{
        BlendFactor, BlendOperation, BlendState, ColorWriteMask, CompileOptions, Device,
        LanguageVersion, LoadAction, MtlRegion, Pass, PixelFormat, RenderPipelineDescriptor,
        SamplerDesc, TEXTURE_USAGE_RENDER_TARGET, TEXTURE_USAGE_SHADER_READ,
    };
    use super::test_support::{as_bytes, device};
    #[cfg(wgpu_arm)]
    use super::test_support::{plain_calayer, sublayer_count};
    use super::{ffi, shaders};

    // -----------------------------------------------------------------
    // CPU parity: the shipped MSL field functions, as compute kernels
    // concatenated onto `cell.metal`, against `aterm_render`.
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

    /// EMBERFORGE: the MSL fire field must equal `aterm_render::fire_field`
    /// exactly, at every pixel, for every parameter extreme.
    #[test]
    fn fire_field_matches_the_cpu_bit_for_bit() {
        use aterm_render::fire_field::{
            FireFieldParams, FireRow, fire_precomp, fire_shade_add, fire_shade_over, fire_top_fade,
        };

        let dev = device();
        let lib = dev
            .new_library(&shaders::cell_with_parity_kernels())
            .expect("cell.metal + parity kernels compile");
        let kf = lib.function("k_fire_parity").expect("k_fire_parity");
        let pso = dev.new_compute_pipeline(&kf).expect("fire compute pso");
        let queue = dev.new_command_queue().expect("queue");

        // A case is the GPU param block plus the row count, built through a
        // constructor so each column is named once.
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

        // Baseline, zero temp/strength, both lean extremes at max heat,
        // `phase == u32::MAX`, the top-fade band, minimum cell height with max
        // peak, a degenerate peak, negative coordinates, an offset region.
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
            )
            .expect("fire parity dispatch completes");
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
            let pc = fire_precomp(&cpu);
            for y in 0..h {
                let py = y0 + y;
                let tf = fire_top_fade(py, &cpu);
                let mut row = FireRow::new(py, x0, &cpu, &pc);
                for x in 0..w {
                    let i = (y * w + x) as usize;
                    let px = x0 + x;
                    let c = row.core(px);
                    let add = fire_shade_add(&c, &cpu, tf);
                    let (ink, alpha) = fire_shade_over(&c, &cpu, tf);
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

        let dev = device();
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
            )
            .expect("rain parity dispatch completes");
            // SAFETY: the dispatch blocked until completion.
            let (g_wt, g_add) =
                unsafe { (ffi::buffer_u32s(&b_wt, n), ffi::buffer_u32s(&b_add, n)) };

            for y in 0..h {
                for x in 0..w {
                    let i = (y * w + x) as usize;
                    let (px, py) = (x0 + x, y0 + y);
                    let ny = halo_row_ny(py - cy, ry * ry);
                    // The shader clamps the weight to 255; the CPU leaves that
                    // to `draw_rain_add`, so clamp here too.
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

    // -----------------------------------------------------------------
    // Pipeline state. Metal's defaults (write mask ALL, nearest filtering)
    // make a dropped setter silent, so every check is a readback.
    // -----------------------------------------------------------------

    /// The 32-byte `Probe` uniform in `shaders/state_probe.metal`: `float4`
    /// colour at 0, `float2` destination size at 16, `float2` pad at 24.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct ProbeUniform {
        color: [f32; 4],
        dst: [f32; 2],
        pad: [f32; 2],
    }

    /// The objects every state probe shares: a device, the compiled probe
    /// library, and a queue.
    struct StateProbe {
        dev: Device,
        lib: ffi::Library,
        queue: ffi::Obj,
    }

    impl StateProbe {
        fn new(dev: Device) -> Self {
            let lib = dev
                .new_library(shaders::STATE_PROBE)
                .expect("state_probe.metal compiles");
            let queue = dev.new_command_queue().expect("queue");
            Self { dev, lib, queue }
        }

        /// `vs_probe` + `fragment`, at one destination format, write mask and
        /// blend state.
        fn pipeline(
            &self,
            fragment: &str,
            format: PixelFormat,
            mask: ColorWriteMask,
            blend: Option<BlendState>,
        ) -> ffi::Obj {
            let vs = self.lib.function("vs_probe").expect("vs_probe");
            let fs = self
                .lib
                .function(fragment)
                .unwrap_or_else(|| panic!("{fragment}"));
            let desc = RenderPipelineDescriptor::new().expect("pipeline descriptor");
            desc.set_vertex_function(&vs);
            desc.set_fragment_function(&fs);
            desc.set_color_attachment(format, mask, blend);
            self.dev
                .new_render_pipeline(&desc)
                .unwrap_or_else(|e| panic!("{fragment} pipeline: {e}"))
        }

        fn uniform(&self, u: ProbeUniform) -> ffi::Obj {
            let b = self
                .dev
                .new_buffer(size_of::<ProbeUniform>())
                .expect("probe uniform");
            // SAFETY: `ProbeUniform` is `repr(C)` and the buffer was created at
            // exactly its size; no GPU work is in flight on a fresh buffer.
            unsafe { ffi::buffer_write(&b, as_bytes(&u)) };
            b
        }

        /// A render target pre-filled with `fill`, so a `LoadAction::Load` pass
        /// starts from known bytes and untouched texels stay byte-exact.
        fn filled_target(&self, format: PixelFormat, w: usize, h: usize, fill: &[u8]) -> ffi::Obj {
            assert_eq!(fill.len(), format.bytes_per_texel(), "fill is one texel");
            let tex = self
                .dev
                .new_texture_2d(
                    format,
                    w,
                    h,
                    TEXTURE_USAGE_RENDER_TARGET | TEXTURE_USAGE_SHADER_READ,
                )
                .expect("probe target");
            let bytes: Vec<u8> = fill
                .iter()
                .copied()
                .cycle()
                .take(w * h * fill.len())
                .collect();
            // SAFETY: `tex` is 2-D `format`, `w` x `h`, non-Private storage;
            // `bytes` is exactly `w * h` texels with a matching stride.
            unsafe {
                ffi::texture_upload(
                    &tex,
                    ffi::MtlRegion::full_2d(w, h),
                    &bytes,
                    w * format.bytes_per_texel(),
                );
            }
            tex
        }

        /// Run one pass and return the destination as tightly packed bytes.
        fn read(&self, pass: &Pass<'_>, format: PixelFormat) -> Vec<u8> {
            let row = pass.dst_w * format.bytes_per_texel();
            let n = row * pass.dst_h;
            let rb = self.dev.new_buffer(n).expect("readback");
            ffi::draw_and_read(&self.queue, pass, &rb, row).expect("probe pass runs");
            // SAFETY: `rb` is shared storage of exactly `n` bytes, and
            // `draw_and_read` returned after `waitUntilCompleted`.
            unsafe { ffi::buffer_bytes(&rb, n) }
        }
    }

    /// The colour write mask, one channel at a time.
    ///
    /// Many renderer pipelines use an RGB-only mask so they never disturb the
    /// alpha the blit relies on; Metal's default is all channels. The setup is
    /// a destination with alpha 64/255 (a translucent window) under one
    /// One/One draw whose fragment emits alpha 1.0 (as `fs_fire_add` and
    /// `fs_rain_glow` do): the default mask ends at alpha 255, `COLOR` at 64.
    ///
    /// All six masks are checked because Metal's bit order (alpha is bit 0,
    /// red bit 3) is not the channel order.
    #[test]
    fn color_write_mask_gates_each_channel_on_the_gpu() {
        let dev = device();
        let p = StateProbe::new(dev);
        const W: usize = 8;
        const H: usize = 8;
        const FMT: PixelFormat = PixelFormat::Rgba8Unorm;
        /// Loaded destination: RGB plus the 64/255 translucent-window alpha.
        const DST: [u8; 4] = [10, 20, 30, 64];
        /// Emitted: exact multiples of 1/255, so the store adds no rounding of
        /// its own, and alpha 1.0 as the additive effect shaders emit.
        const SRC: [u8; 4] = [51, 68, 85, 255];

        let uniform = p.uniform(ProbeUniform {
            color: [
                f32::from(SRC[0]) / 255.0,
                f32::from(SRC[1]) / 255.0,
                f32::from(SRC[2]) / 255.0,
                1.0,
            ],
            dst: [W as f32, H as f32],
            pad: [0.0, 0.0],
        });
        // Saturating add on every channel — `FireMode::Add` / `HaloMode::Add`.
        let blend = Some(BlendState {
            source_rgb: BlendFactor::One,
            destination_rgb: BlendFactor::One,
            rgb_operation: BlendOperation::Add,
            source_alpha: BlendFactor::One,
            destination_alpha: BlendFactor::One,
            alpha_operation: BlendOperation::Add,
        });

        // `sum` is what a written channel lands on; an unwritten channel must be
        // byte-exactly what was loaded.
        let sum = |i: usize| (u16::from(DST[i]) + u16::from(SRC[i])).min(255);
        for (label, mask, written) in [
            ("ALL", ColorWriteMask::ALL, [true, true, true, true]),
            ("COLOR", ColorWriteMask::COLOR, [true, true, true, false]),
            ("ALPHA", ColorWriteMask::ALPHA, [false, false, false, true]),
            ("RED", ColorWriteMask::RED, [true, false, false, false]),
            (
                "GREEN|BLUE",
                ColorWriteMask::GREEN | ColorWriteMask::BLUE,
                [false, true, true, false],
            ),
            ("NONE", ColorWriteMask::NONE, [false; 4]),
        ] {
            let pso = p.pipeline("fs_probe_const", FMT, mask, blend);
            let dst = p.filled_target(FMT, W, H, &DST);
            let got = p.read(
                &Pass {
                    pso: &pso,
                    dst: &dst,
                    dst_w: W,
                    dst_h: H,
                    load: LoadAction::Load,
                    viewport: None,
                    scissor: None,
                    src_tex: None,
                    sampler: None,
                    uniform: Some((&uniform, 2)), // state_probe.metal: [[buffer(2)]]
                    vertex_uniform: None,
                    draw: None,
                },
                FMT,
            );
            for (px, texel) in got.as_chunks::<4>().0.iter().enumerate() {
                for (i, ch) in ["R", "G", "B", "A"].into_iter().enumerate() {
                    if written[i] {
                        let want = sum(i);
                        assert!(
                            u16::from(texel[i]).abs_diff(want) <= 1,
                            "[{label}] texel {px} channel {ch}: got {} want ~{want} \
                             (loaded {} + emitted {})",
                            texel[i],
                            DST[i],
                            SRC[i]
                        );
                    } else {
                        assert_eq!(
                            texel[i], DST[i],
                            "[{label}] texel {px} channel {ch} was MASKED OFF but changed \
                             from {} to {} — Metal's default mask is ALL, so this is what \
                             a dropped `setWriteMask:` looks like",
                            DST[i], texel[i]
                        );
                    }
                }
            }
            crate::stderr_line!("  write mask {label}: alpha {} -> {}", DST[3], got[3]);
        }
    }

    /// Draw `fs_probe_sample` over a `dw` x `dh` destination, fetching a
    /// `sw` x `sh` RGBA8 source through `desc`, and return the destination
    /// bytes. The scale direction decides which filter the hardware consults.
    fn sample_through(
        p: &StateProbe,
        (sw, sh): (usize, usize),
        src_bytes: &[u8],
        (dw, dh): (usize, usize),
        desc: SamplerDesc,
    ) -> Vec<u8> {
        const FMT: PixelFormat = PixelFormat::Rgba8Unorm;
        assert_eq!(src_bytes.len(), sw * sh * 4, "source is sw x sh RGBA8");
        let src = p
            .dev
            .new_texture_2d(FMT, sw, sh, TEXTURE_USAGE_SHADER_READ)
            .expect("sample source");
        // SAFETY: `src` is 2-D `Rgba8Unorm` at `sw` x `sh`, non-Private;
        // `src_bytes` was length-checked and the stride is one row.
        unsafe { ffi::texture_upload(&src, MtlRegion::full_2d(sw, sh), src_bytes, sw * 4) };

        let pso = p.pipeline("fs_probe_sample", FMT, ColorWriteMask::ALL, None);
        let uniform = p.uniform(ProbeUniform {
            color: [0.0; 4],
            dst: [dw as f32, dh as f32],
            pad: [0.0; 2],
        });
        let samp = p.dev.new_sampler(desc).expect("sampler");
        let dst = p.filled_target(FMT, dw, dh, &[0, 0, 0, 0]);
        p.read(
            &Pass {
                pso: &pso,
                dst: &dst,
                dst_w: dw,
                dst_h: dh,
                load: LoadAction::Load,
                viewport: None,
                scissor: None,
                src_tex: Some((&src, 0)), // state_probe.metal: [[texture(0)]]
                sampler: Some((&samp, 0)), // state_probe.metal: [[sampler(0)]]
                uniform: Some((&uniform, 2)), // state_probe.metal: [[buffer(2)]]
                vertex_uniform: None,
                draw: None,
            },
            FMT,
        )
    }

    /// `setMagFilter:` reaches the sampled bytes, proven by a magnifying pass
    /// (the only kind that consults it). Several renderer samplers are linear
    /// (bloom, tray, shimmer, which samples sub-texel displacements).
    ///
    /// A 2x2 checkerboard read into 4x4 puts every sample a quarter texel off
    /// centre, so only the mag filter decides the bytes. The min filter is
    /// covered by [`sampler_min_filter_changes_the_minified_bytes`].
    #[test]
    fn sampler_mag_filter_changes_the_magnified_bytes() {
        let dev = device();
        let p = StateProbe::new(dev);
        const DW: usize = 4;

        // 2x2 checkerboard, RGB carrying the pattern and alpha pinned opaque.
        #[rustfmt::skip]
        let src_bytes: [u8; 16] = [
            0, 0, 0, 255,   255, 255, 255, 255,
            255, 255, 255, 255,   0, 0, 0, 255,
        ];
        let nearest = sample_through(&p, (2, 2), &src_bytes, (4, 4), SamplerDesc::NEAREST_CLAMP);
        let linear = sample_through(&p, (2, 2), &src_bytes, (4, 4), SamplerDesc::LINEAR_CLAMP);

        // NEAREST is the checkerboard doubled: sample coordinates 0.25/0.75/
        // 1.25/1.75 truncate to source texels 0/0/1/1 on both axes.
        for (i, texel) in nearest.as_chunks::<4>().0.iter().enumerate() {
            let (px, py) = (i % DW, i / DW);
            let want = if (px / 2) == (py / 2) { 0 } else { 255 };
            assert_eq!(
                texel[0], want,
                "NEAREST ({px},{py}): a nearest fetch can only return a source texel"
            );
        }

        // LINEAR blends wherever both neighbours are in range. At (1,1) the
        // sample sits at source (0.75,0.75) — a quarter texel inside texel 0 on
        // both axes — so the weights are 0.75/0.25 each way and the result is
        // 2 * 0.1875 * 255 = 95.6. At (2,1) they are 0.25/0.75 in x and
        // 0.75/0.25 in y, giving 0.625 * 255 = 159.4.
        let at = |x: usize, y: usize| i32::from(linear[(y * DW + x) * 4]);
        assert!(
            (at(1, 1) - 96).abs() <= 3,
            "LINEAR (1,1): got {}, expected ~96 — a NEAREST sampler returns 0 here",
            at(1, 1)
        );
        assert!(
            (at(2, 1) - 159).abs() <= 3,
            "LINEAR (2,1): got {}, expected ~159 — a NEAREST sampler returns 255 here",
            at(2, 1)
        );
        assert_ne!(
            nearest, linear,
            "the two descriptors produced identical bytes on a magnifying \
             pass, so `setMagFilter:` reached nothing (this pass never \
             consults the MIN filter — that is the minify probe's job)"
        );
    }

    /// `setMinFilter:` reaches the sampled bytes, proven by a minifying pass.
    ///
    /// 4x4 -> 2x2 puts every sample at the corner of a 2x2 source quadrant, so
    /// linear minification averages the quadrant while nearest returns one
    /// texel. Two mixed quadrants (~191 and ~64 against 0 or 255) tell them
    /// apart; two constant quadrants are controls where they must agree.
    #[test]
    fn sampler_min_filter_changes_the_minified_bytes() {
        let dev = device();
        let p = StateProbe::new(dev);

        // Red channel of the 4x4 source, one row per line. Quadrant averages:
        // top-left 191.25, top-right 63.75, bottom-left 255, bottom-right 0.
        #[rustfmt::skip]
        let red: [u8; 16] = [
              0, 255,   255,   0,
            255, 255,     0,   0,
            255, 255,     0,   0,
            255, 255,     0,   0,
        ];
        let src_bytes: Vec<u8> = red.iter().flat_map(|&r| [r, r, r, 255]).collect();
        let nearest = sample_through(&p, (4, 4), &src_bytes, (2, 2), SamplerDesc::NEAREST_CLAMP);
        let linear = sample_through(&p, (4, 4), &src_bytes, (2, 2), SamplerDesc::LINEAR_CLAMP);

        for (i, want_avg) in [191i32, 64, 255, 0].into_iter().enumerate() {
            let (px, py) = (i % 2, i / 2);
            let l = i32::from(linear[i * 4]);
            assert!(
                (l - want_avg).abs() <= 3,
                "LINEAR minify ({px},{py}): got {l}, want ~{want_avg} — the \
                 quadrant average. A pure 0/255 here is a nearest MIN fetch, \
                 which is exactly what a dropped `setMinFilter:` leaves behind"
            );
            let n = nearest[i * 4];
            assert!(
                n == 0 || n == 255,
                "NEAREST minify ({px},{py}): got {n} — a nearest fetch can \
                 only return a source texel"
            );
        }
        // The two mixed quadrants are where the filters cannot agree.
        assert_ne!(
            &nearest[..8],
            &linear[..8],
            "the two descriptors produced identical bytes on a minifying \
             pass, so `setMinFilter:` reached nothing"
        );
    }

    /// Every aterm library compiles with `preserveInvariance` ON (so
    /// `cell.metal`'s `to_ndc` and `hdr_glow.metal`'s inline NDC math compute
    /// the same quad corners, as the HDR crown re-emits aurora quads) and with
    /// `languageVersion` pinned to the macOS 11 floor, not inherited from the
    /// running OS's compiler.
    #[test]
    fn compile_options_pin_invariance_and_the_language_version() {
        let _ = device();
        let opts = CompileOptions::aterm_default().expect("MTLCompileOptions");
        assert!(
            opts.preserve_invariance(),
            "every aterm library must be compiled with preserveInvariance ON"
        );
        assert_eq!(
            opts.language_version(),
            LanguageVersion::V2_3 as usize,
            "the language version must be PINNED to the macOS 11 floor"
        );
    }

    // -----------------------------------------------------------------------
    // Blit differential: the Metal blit must match the wgpu blit byte for byte.
    // -----------------------------------------------------------------------

    use aterm_core::terminal::Terminal;
    #[cfg(wgpu_arm)]
    use aterm_render::Frame;
    use aterm_render::{RenderInput, Theme};

    #[cfg(wgpu_arm)]
    use super::blit::MetalBlit;
    use crate::WindowGpu;
    #[cfg(wgpu_arm)]
    use crate::renderer::{BlitTestEffects, BlitTestTarget};
    #[cfg(wgpu_arm)]
    use crate::{DropOverlay, GpuRenderer, PresentCrop, TrayQuad};

    const ROWS: usize = 6;
    const COLS: usize = 24;

    /// A representative frame (prompt, glyph, saturated SGR runs), so the
    /// differential covers real glyph coverage and colour.
    #[cfg(wgpu_arm)]
    fn representative_input() -> RenderInput {
        let mut term = Terminal::new(ROWS as u16, COLS as u16);
        term.process(b"$ blit check >_\r\n");
        term.process(b"\x1b[31mRED\x1b[0m \x1b[32mGREEN\x1b[0m \x1b[34mBLUE\x1b[0m\r\n");
        term.process(b"\x1b[1mbold\x1b[0m plain 0123456789");
        term.cell_frame(ROWS, COLS)
    }

    /// The Metal spelling of a [`BlitTestTarget`] (no `wgpu` type crosses into
    /// this module).
    #[cfg(wgpu_arm)]
    fn metal_format(t: BlitTestTarget) -> PixelFormat {
        match t {
            BlitTestTarget::Rgba8Unorm => PixelFormat::Rgba8Unorm,
            BlitTestTarget::Bgra8Unorm => PixelFormat::Bgra8Unorm,
            BlitTestTarget::Rgba16Float => PixelFormat::Rgba16Float,
        }
    }

    /// The drop-target overlay the busy arms carry.
    #[cfg(wgpu_arm)]
    const OVERLAY: DropOverlay = DropOverlay {
        accent: 0x0033_88ff,
        wash_a: 40,
        border_a: 200,
        border_scale_q4: 0,
    };

    /// One differential case: name, destination, uniform arms, and how much
    /// larger than the source the destination is (enabling the band branch).
    #[cfg(wgpu_arm)]
    struct Case {
        name: &'static str,
        target: BlitTestTarget,
        fx: BlitTestEffects,
        extra_w: u32,
        extra_h: u32,
        /// Blit the translucent source (`background_opacity < 1`), so `c.a`
        /// varies per pixel inside the content rect.
        translucent_source: bool,
        /// Blit a chrome source with this `ChromeBleed::first`, so the uniform
        /// carries non-zero `chrome_y1` (and `chrome_y0` for `Some(1)`) and the
        /// bands beside chrome rows take the edge-continuation arm. `None` has
        /// no chrome.
        chrome_first: Option<usize>,
    }

    /// The Metal blit must be byte-identical to the wgpu blit on the same
    /// source texels and 112 uniform bytes, across every reachable `fs_blit`
    /// arm. Byte equality catches what the property tests
    /// (`tests/blit_invert.rs`, `tests/blit_bands.rs`) miss: a half-texel offset,
    /// an sRGB-typed destination, a filtered fetch, a swapped binding slot.
    ///
    /// | # | case | destination | reaches |
    /// |--:|---|---|---|
    /// | 1 | exact fit, passthrough | `Rgba8Unorm` | the in-bounds fetch |
    /// | 2 | exact fit, bell invert | `Rgba8Unorm` | `flag` |
    /// | 3 | oversized, bands | `Rgba8Unorm` | the band path and `content_off` |
    /// | 4 | cropped + drop overlay | `Rgba8Unorm` | `overlay`, `visible_y`/`visible_h`, `border_px` |
    /// | 5 | presented format | `Bgra8Unorm` | case 4 on `pick_surface_format`'s choice |
    /// | 6 | linear->sRGB re-encode | `Rgba8Unorm` | `encode_srgb` (`blit.metal`'s `l2s`) |
    /// | 7 | translucent glass | `Bgra8Unorm` | `translucent`, content and band alpha |
    /// | 8 | premultiplied glass | `Bgra8Unorm` | `premult` over `translucent` |
    /// | 9 | HDR | `Rgba16Float` | `hdr` (`hdr_grid_encode3`) |
    /// | 10 | chrome rows, bands | `Bgra8Unorm` | `chrome_y1`, the band edge continuation |
    /// | 11 | chrome under the bell invert | `Rgba8Unorm` | edge continuation with `flag` and a crop |
    /// | 12 | chrome below a surfaceless strip | `Rgba8Unorm` | `chrome_y0` |
    /// | 13 | HDR + scRGB white | `Rgba16Float` | `sdr_white_scale`, grid and bands |
    ///
    /// The HDR cases also check the 8-byte-per-texel readback stride. Not
    /// covered: `hdr` with `translucent`/`premult` (no production caller), and
    /// `encode_srgb` from a real downlevel adapter (it is forced here). The
    /// `edge < border_px` boundary is inert because `edge` is never an integer.
    #[test]
    #[cfg(wgpu_arm)]
    fn blit_matches_wgpu_byte_for_byte() {
        let _ = device();
        let mut gpu = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
        gpu.disarm_metal_for_test();
        let mut win = WindowGpu::new();
        let input = representative_input();

        // Two sources, both captured before any blit: every opaque case blits
        // one offscreen, the translucent cases a second at
        // `background_opacity = 0.55` so alpha varies per pixel.
        let opaque = gpu.render_input(&mut win, &input, None);
        let (sw, sh) = (opaque.width as u32, opaque.height as u32);
        assert!(sw > 0 && sh > 3, "renderer produced an empty offscreen");
        // Unpacking the readback is lossless, so the Metal arm samples the
        // same texels the wgpu arm did.
        let opaque_rgba = opaque.rgba_bytes();

        gpu.set_background_opacity(0.55);
        let glass = gpu.render_input(&mut win, &input, None);
        assert_eq!(
            (glass.width as u32, glass.height as u32),
            (sw, sh),
            "the translucent repaint changed the offscreen size"
        );
        let glass_rgba = glass.rgba_bytes();
        assert!(
            glass_rgba.as_chunks::<4>().0.iter().any(|c| c[3] != 255),
            "the translucent source is fully opaque, so the `translucent` arm \
             would be vacuous inside the content rect"
        );

        // Chrome source: two chrome rows, the second metered, so the
        // edge-continuation arm carries three distinct tones.
        let chrome_bleed = |first: usize| aterm_render::ChromeBleed {
            rows: 2,
            first,
            color: 0x0030_3135,
            seam: Some(0x0060_6164),
            top_extends_cells: false,
            row_edges: [
                Some(aterm_render::ChromeRowEdges {
                    row: 1,
                    left: 0x0050_FA7B,
                    right: 0x0044_4750,
                }),
                None,
                None,
            ],
        };
        gpu.set_background_opacity(1.0);
        gpu.set_chrome_bleed(Some(chrome_bleed(0)));
        let chrome = gpu.render_input(&mut win, &input, None);
        let chrome_rgba = chrome.rgba_bytes();
        // …and the same band below a surfaceless strip row (`first: 1`, macOS's
        // in-grid strip): the strip row and the lip above keep the padding.
        gpu.set_chrome_bleed(Some(chrome_bleed(1)));
        let strip = gpu.render_input(&mut win, &input, None);
        let strip_rgba = strip.rgba_bytes();
        gpu.set_chrome_bleed(None);

        let crop = PresentCrop {
            source_y: 3,
            height: sh - 3,
        };
        let cases = [
            Case {
                name: "exact fit, passthrough",
                target: BlitTestTarget::Rgba8Unorm,
                fx: BlitTestEffects::PLAIN,
                extra_w: 0,
                extra_h: 0,
                translucent_source: false,
                chrome_first: None,
            },
            Case {
                name: "exact fit, bell invert",
                target: BlitTestTarget::Rgba8Unorm,
                fx: BlitTestEffects {
                    invert: true,
                    ..BlitTestEffects::PLAIN
                },
                extra_w: 0,
                extra_h: 0,
                translucent_source: false,
                chrome_first: None,
            },
            Case {
                name: "oversized, W1 bands",
                target: BlitTestTarget::Rgba8Unorm,
                fx: BlitTestEffects::PLAIN,
                extra_w: 37,
                extra_h: 23,
                translucent_source: false,
                chrome_first: None,
            },
            Case {
                name: "cropped + drop overlay",
                target: BlitTestTarget::Rgba8Unorm,
                fx: BlitTestEffects {
                    overlay: Some(OVERLAY),
                    crop: Some(crop),
                    ..BlitTestEffects::PLAIN
                },
                extra_w: 37,
                extra_h: 23,
                translucent_source: false,
                chrome_first: None,
            },
            Case {
                name: "cropped + overlay on the PRESENTED Bgra8Unorm format",
                target: BlitTestTarget::Bgra8Unorm,
                fx: BlitTestEffects {
                    overlay: Some(OVERLAY),
                    crop: Some(crop),
                    ..BlitTestEffects::PLAIN
                },
                extra_w: 37,
                extra_h: 23,
                translucent_source: false,
                chrome_first: None,
            },
            Case {
                name: "linear->sRGB re-encode (encode_srgb)",
                target: BlitTestTarget::Rgba8Unorm,
                fx: BlitTestEffects {
                    encode_srgb: true,
                    ..BlitTestEffects::PLAIN
                },
                extra_w: 0,
                extra_h: 0,
                translucent_source: false,
                chrome_first: None,
            },
            Case {
                name: "translucent glass, content + bands",
                target: BlitTestTarget::Bgra8Unorm,
                fx: BlitTestEffects {
                    translucent: Some(0.55),
                    ..BlitTestEffects::PLAIN
                },
                extra_w: 37,
                extra_h: 23,
                translucent_source: true,
                chrome_first: None,
            },
            Case {
                name: "premultiplied glass (premult over translucent)",
                target: BlitTestTarget::Bgra8Unorm,
                fx: BlitTestEffects {
                    translucent: Some(0.55),
                    premult: true,
                    ..BlitTestEffects::PLAIN
                },
                extra_w: 37,
                extra_h: 23,
                translucent_source: true,
                chrome_first: None,
            },
            Case {
                name: "EDR present (hdr grid clamp)",
                target: BlitTestTarget::Rgba16Float,
                fx: BlitTestEffects {
                    hdr: true,
                    ..BlitTestEffects::PLAIN
                },
                extra_w: 0,
                extra_h: 0,
                translucent_source: false,
                chrome_first: None,
            },
            Case {
                name: "chrome rows continue their edges through the bands",
                target: BlitTestTarget::Bgra8Unorm,
                fx: BlitTestEffects::PLAIN,
                extra_w: 37,
                extra_h: 23,
                translucent_source: false,
                chrome_first: Some(0),
            },
            Case {
                name: "chrome edge continuation under the bell invert",
                target: BlitTestTarget::Rgba8Unorm,
                fx: BlitTestEffects {
                    invert: true,
                    crop: Some(crop),
                    ..BlitTestEffects::PLAIN
                },
                extra_w: 37,
                extra_h: 23,
                translucent_source: false,
                chrome_first: Some(0),
            },
            Case {
                name: "chrome edge continuation below a surfaceless strip",
                target: BlitTestTarget::Rgba8Unorm,
                fx: BlitTestEffects::PLAIN,
                extra_w: 37,
                extra_h: 23,
                translucent_source: false,
                chrome_first: Some(1),
            },
            Case {
                name: "EDR + scRGB reference white, with bands",
                target: BlitTestTarget::Rgba16Float,
                fx: BlitTestEffects {
                    hdr: true,
                    sdr_white_scale: 3.0,
                    ..BlitTestEffects::PLAIN
                },
                extra_w: 37,
                extra_h: 23,
                translucent_source: false,
                chrome_first: None,
            },
        ];

        let mut blits: Vec<(BlitTestTarget, MetalBlit)> = Vec::new();
        for c in cases {
            let (dw, dh) = (sw + c.extra_w, sh + c.extra_h);
            // The wgpu arm re-renders its source; `render_input` is
            // deterministic, so these are the texels captured above.
            gpu.set_chrome_bleed(c.chrome_first.map(chrome_bleed));
            let src_rgba = if c.translucent_source {
                gpu.set_background_opacity(0.55);
                &glass_rgba
            } else if let Some(first) = c.chrome_first {
                gpu.set_background_opacity(1.0);
                if first == 0 {
                    &chrome_rgba
                } else {
                    &strip_rgba
                }
            } else {
                gpu.set_background_opacity(1.0);
                &opaque_rgba
            };
            gpu.render_input(&mut win, &input, None);

            let expected = gpu.blit_effect_bytes_for_test(&mut win, c.fx, c.target, dw, dh);
            let uniform = gpu
                .last_blit_uniform_bytes()
                .expect("the wgpu blit records the uniform it wrote");
            assert_eq!(
                uniform.len(),
                MetalBlit::UNIFORM_BYTES,
                "BlitUniform grew: add the member to `shaders/blit.metal`'s \
                 `Blit` struct before widening this constant"
            );
            // `chrome_y1` rides bytes 28..32 and `chrome_y0` bytes 96..100: the
            // chrome cases carry the end, the strip case the start.
            let chrome_y1 =
                f32::from_ne_bytes([uniform[28], uniform[29], uniform[30], uniform[31]]);
            let chrome_y0 =
                f32::from_ne_bytes([uniform[96], uniform[97], uniform[98], uniform[99]]);
            assert_eq!(
                chrome_y1 > 0.0,
                c.chrome_first.is_some(),
                "[{}] chrome_y1 = {chrome_y1}",
                c.name
            );
            assert_eq!(
                chrome_y0 > 0.0,
                c.chrome_first.is_some_and(|f| f > 0),
                "[{}] chrome_y0 = {chrome_y0}",
                c.name
            );
            assert!(chrome_y0 <= chrome_y1, "[{}] an ordered range", c.name);

            if !blits.iter().any(|(t, _)| *t == c.target) {
                let mb = MetalBlit::new(metal_format(c.target)).unwrap_or_else(|e| {
                    panic!("the Metal blit pipeline must build for {:?}: {e}", c.target)
                });
                crate::stderr_line!("blit differential on {} — {:?}", mb.device_name(), c.target);
                blits.push((c.target, mb));
            }
            let mb = &blits
                .iter()
                .find(|(t, _)| *t == c.target)
                .expect("just inserted")
                .1;

            let actual = mb
                .run(src_rgba, sw, sh, &uniform, dw, dh)
                .unwrap_or_else(|e| panic!("[{}] Metal blit failed: {e}", c.name));

            let texel = metal_format(c.target).bytes_per_texel();
            assert_row_bytes_identical(
                &format!("[{}] THE METAL BLIT on {:?}", c.name, c.target),
                "Measure the cause before touching tolerance",
                dw as usize,
                texel,
                &expected,
                &actual,
            );
            crate::stderr_line!(
                "  [{}] {dw}x{dh} {:?}: byte-identical over {} texels",
                c.name,
                c.target,
                expected.len() / texel
            );
        }
        // Leave the renderer as it was found.
        gpu.set_background_opacity(1.0);
        gpu.set_chrome_bleed(None);
    }

    /// Armed inline-image differential: `render_input` with a real OSC 1337
    /// image through the wgpu arm and the armed Metal arm (which re-mints the
    /// plane from `ImagePlane::metal_texels`). Byte-identical, N=5, on two
    /// inputs; the second adds a translucent image so the plane rebuild (moved
    /// `epoch`) and image alpha blending are exercised. The armed plan must
    /// contain an image stream and the plane's epoch must move.
    #[test]
    #[cfg(wgpu_arm)]
    fn armed_inline_image_plane_matches_the_wgpu_arm_byte_for_byte() {
        use crate::renderer::StreamId;
        const N: usize = 5;
        let _ = device();
        let mut wg = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
        wg.disarm_metal_for_test();
        let mut armed = GpuRenderer::new(18.0, Theme::default()).expect("second renderer");
        armed.arm_metal_for_test();
        wg.debug_block_on_lazy_fallbacks();
        armed.debug_block_on_lazy_fallbacks();
        let mut wg_win = WindowGpu::new();
        let mut armed_win = WindowGpu::new();
        let (cw, ch) = wg.cell_size();

        // Solid-colour RGBA PNG bytes (the inline_image_parity fixture recipe).
        let png = |w: u32, h: u32, rgba: [u8; 4]| -> Vec<u8> {
            let mut pix = Vec::with_capacity((w * h * 4) as usize);
            for _ in 0..(w * h) {
                pix.extend_from_slice(&rgba);
            }
            let mut out = Vec::new();
            {
                let mut enc = aterm_png::Encoder::new(&mut out, w, h);
                enc.set_color(aterm_png::ColorType::Rgba);
                enc.set_depth(aterm_png::BitDepth::Eight);
                let mut writer = enc.write_header().expect("png header");
                writer.write_image_data(&pix).expect("png data");
            }
            out
        };
        let osc = |args: &str, payload: &[u8]| -> Vec<u8> {
            let b64 = aterm_codec::base64::encode(payload).expect("encode");
            let mut out = Vec::new();
            out.extend_from_slice(b"\x1b]1337;File=");
            out.extend_from_slice(args.as_bytes());
            out.push(b':');
            out.extend_from_slice(b64.as_bytes());
            out.extend_from_slice(b"\x1b\\");
            out
        };
        let input_for = |second_image: Option<[u8; 4]>| -> RenderInput {
            let mut term = Terminal::new(ROWS as u16, COLS as u16);
            term.set_cell_pixel_size(cw as u16, ch as u16);
            term.process(b"\x1b[33mimg>\x1b[0m under");
            term.process(b"\r");
            term.process(&osc(
                "inline=1;width=2;height=2",
                &png(2 * cw as u32, 2 * ch as u32, [40, 200, 90, 255]),
            ));
            if let Some(rgba) = second_image {
                // A translucent image at another cell: a distinct placement
                // (plane rebuild ⇒ epoch move) and a live alpha-blend axis.
                term.process(b"\r\n\r\n");
                term.process(&osc(
                    "inline=1;width=2;height=1",
                    &png(2 * cw as u32, ch as u32, rgba),
                ));
            }
            term.process(b"\r\n$ tail");
            term.cell_frame(ROWS, COLS)
        };

        let mut first_epoch = None;
        // Phase 2 changes only the second image's texels at identical dims, so
        // the rebuild epoch is the one moving key component.
        for (phase, input) in [
            (0u32, input_for(None)),
            (1, input_for(Some([220, 40, 160, 128]))),
            (2, input_for(Some([30, 120, 255, 128]))),
        ] {
            for run in 0..N {
                let expected = wg.render_input(&mut wg_win, &input, None);
                let actual = armed.render_input(&mut armed_win, &input, None);
                assert!(
                    armed.metal_render_armed(),
                    "phase {phase} run {run}: the armed renderer must hold its arm"
                );
                // Non-vacuity: the armed plan drew at least one image stream.
                let plan = armed.last_frame_plan_streams_for_test();
                assert!(
                    plan.iter().any(|(s, _)| matches!(
                        s,
                        StreamId::Image
                            | StreamId::ImageUnder
                            | StreamId::ImageBelowBg
                            | StreamId::ImageBgCover
                    )),
                    "phase {phase} run {run}: the fixture decoded no image draw — \
                     the differential would be vacuous (plan: {plan:?})"
                );
                assert_frames_identical(
                    &format!("phase {phase} run {run}: THE ARMED IMAGE-PLANE FRAME"),
                    "Measure the cause before touching tolerance",
                    &expected,
                    &actual,
                );
            }
            // Non-vacuity: resident texels and a moving epoch across the rebuild.
            let (epoch, has_texels) = armed_win
                .image_plane_probe_for_test()
                .expect("the armed window holds an image plane");
            assert!(
                has_texels,
                "phase {phase}: the armed plane must retain its CPU-side texels"
            );
            match first_epoch {
                None => first_epoch = Some(epoch),
                Some(prev) => assert!(
                    epoch > prev,
                    "the two-image rebuild must move the plane epoch \
                     (armed re-upload unarmed otherwise): {epoch} <= {prev}"
                ),
            }
        }
        crate::stderr_line!(
            "armed inline-image differential: byte-identical x {N} runs x 3 \
             inputs (dims-moving rebuild, SAME-DIMS texel rebuild via the \
             epoch salt, translucent image — all armed)"
        );
    }

    /// Armed readback-effects differential: `render_input` with bloom, shimmer
    /// (phase pinned) and a tray card live, wgpu vs armed, byte-identical x N=5.
    /// Turning each layer off must change the armed bytes. After N identical
    /// cards each arm has uploaded once (`tray_uploads`), and a changed card
    /// once more.
    #[test]
    #[cfg(wgpu_arm)]
    fn armed_readback_effects_match_the_wgpu_arm_byte_for_byte() {
        const N: usize = 5;
        let _ = device();
        let mut wg = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
        wg.disarm_metal_for_test();
        let mut armed = GpuRenderer::new(18.0, Theme::default()).expect("second renderer");
        armed.arm_metal_for_test();
        for g in [&mut wg, &mut armed] {
            g.debug_block_on_lazy_fallbacks();
            g.set_bloom(true);
            g.set_shimmer(true);
            // The pass's one wall-clock term, pinned identically (the
            // heat_shimmer suite's discipline).
            g.set_shimmer_phase_for_test(Some(1.375));
        }
        let mut wg_win = WindowGpu::new();
        let mut armed_win = WindowGpu::new();
        let (cw, ch) = wg.cell_size();

        // A warm ember comet on row 4 + the fire-style marker (the heat_shimmer
        // fixture), arming `shimmer_live` and the bloom.
        let mut input = representative_input();
        input.cursor_glow_add = (4..14)
            .map(|c| aterm_render::GlowQuad {
                row: 4,
                x: (c * cw) as u16,
                y: (4 * ch) as u16,
                w: cw as u16,
                h: ch as u16,
                color: aterm_render::premul_rgb(0x00FF_6A00, 230),
                alpha: 0,
                color2: aterm_render::premul_rgb(0x00FF_6A00, 230),
                alpha2: 0,
            })
            .collect();
        input.fire_patch = vec![aterm_render::FirePatch::default()];

        // A deterministic straight-alpha gradient card.
        let (pw, ph) = (40u32, 24u32);
        let card_a: Vec<u8> = (0..pw * ph)
            .flat_map(|i| {
                let (x, y) = (i % pw, i / pw);
                [
                    (x * 6 % 256) as u8,
                    (y * 10 % 256) as u8,
                    170,
                    (40 + (x + y) * 3 % 216) as u8,
                ]
            })
            .collect();
        let mut card_b = card_a.clone();
        for px in card_b.as_chunks_mut::<4>().0 {
            px[2] = 40; // a visibly different card, same dims
        }
        let tray = |rgba: &'static [u8]| TrayQuad {
            rgba,
            pw,
            ph,
            dx: 3 * cw as u32,
            dy: ch as u32,
            scrim: None,
        };
        let card_a: &'static [u8] = card_a.leak();
        let card_b: &'static [u8] = card_b.leak();

        let mut armed_frame = None;
        for run in 0..N {
            let expected = wg.render_input(&mut wg_win, &input, Some(tray(card_a)));
            let actual = armed.render_input(&mut armed_win, &input, Some(tray(card_a)));
            assert!(armed.metal_render_armed(), "run {run}: arm must be live");
            assert_frames_identical(
                &format!("run {run}: THE ARMED EFFECTS FRAME"),
                "Localize by layer: turn the bloom, shimmer and tray off one at a \
                 time and re-diff",
                &expected,
                &actual,
            );
            armed_frame = Some(actual);
        }
        // N identical cards: one upload per arm.
        assert_eq!(
            (wg.tray_uploads(), armed.tray_uploads()),
            (1, 1),
            "the unchanged-bytes card skip must hold on BOTH arms across {N} presents"
        );
        // A changed card (same dims) re-uploads exactly once per arm, and the
        // frames stay byte-identical.
        let expected = wg.render_input(&mut wg_win, &input, Some(tray(card_b)));
        let actual = armed.render_input(&mut armed_win, &input, Some(tray(card_b)));
        assert_eq!(
            expected.pixels, actual.pixels,
            "changed-card frames diverged between the arms"
        );
        assert_eq!(
            (wg.tray_uploads(), armed.tray_uploads()),
            (2, 2),
            "a changed card must upload exactly once more per arm"
        );
        // A change to a few rows uploads those rows, never the whole card, on
        // both arms, and the frames stay byte-identical. The armed arm writes
        // the ring slot not shown, catching up the rows it missed while the
        // other was: after a change to every row, that slot takes every row
        // once; after that, the band since it was last written.
        let stride = pw as usize * 4;
        let rows_changed = |base: &[u8], rows: std::ops::Range<usize>, tint: u8| {
            let mut card = base.to_vec();
            for byte in &mut card[rows.start * stride..rows.end * stride] {
                *byte = byte.wrapping_add(tint);
            }
            &*card.leak()
        };
        let card_c: &'static [u8] = rows_changed(card_b, 5..8, 31);
        let card_d: &'static [u8] = rows_changed(card_c, 10..12, 57);
        for (card, wg_rows, armed_most) in [(card_c, 3, ph as usize), (card_d, 2, 12 - 5)] {
            let (wg_before, armed_before) = (wg.tray_upload_bytes(), armed.tray_upload_bytes());
            let expected = wg.render_input(&mut wg_win, &input, Some(tray(card)));
            let actual = armed.render_input(&mut armed_win, &input, Some(tray(card)));
            assert_eq!(
                expected.pixels, actual.pixels,
                "a band-changed card's frames diverged between the arms"
            );
            assert_eq!(
                wg.tray_upload_bytes() - wg_before,
                (wg_rows * stride) as u64,
                "the wgpu arm uploads the changed rows alone"
            );
            let armed_bytes = armed.tray_upload_bytes() - armed_before;
            assert!(
                armed_bytes <= (armed_most * stride) as u64 && armed_bytes > 0,
                "the armed arm uploaded {armed_bytes} bytes for a band of rows"
            );
        }

        // Non-vacuity: removing each layer must change the armed bytes.
        let base = armed_frame.expect("N runs produced a frame").pixels;
        armed.set_bloom(false);
        let no_bloom = armed.render_input(&mut armed_win, &input, Some(tray(card_a)));
        assert_ne!(
            no_bloom.pixels, base,
            "bloom-off must change the armed frame — the in-place bloom pass \
             painted nothing (vacuous differential)"
        );
        armed.set_bloom(true);
        armed.set_shimmer(false);
        let no_shimmer = armed.render_input(&mut armed_win, &input, Some(tray(card_a)));
        assert_ne!(
            no_shimmer.pixels, base,
            "shimmer-off must change the armed frame — the in-place refract \
             painted nothing (vacuous differential)"
        );
        armed.set_shimmer(true);
        let no_tray = armed.render_input(&mut armed_win, &input, None);
        assert_ne!(
            no_tray.pixels, base,
            "tray-off must change the armed frame — the card bake painted \
             nothing (vacuous differential)"
        );
        crate::stderr_line!(
            "armed readback-effects differential: byte-identical x {N} runs + \
             changed-card run; bloom/shimmer/tray each proven live; card \
             uploads (1 then 2) mirrored on both arms"
        );
    }

    /// Armed tap-ring differential: the production ring with Metal staging
    /// slots rides the armed Submit B onto a real standalone drawable (copy
    /// before commit, status-polled harvest), and every harvested frame must
    /// equal the drawable bytes of the same present. The one-shot
    /// `PresentedFrameTap` captures the first present the same way. Three
    /// different inputs, exactly N frames, zero drops, and the armed arm
    /// verified (`metal_render_armed`).
    #[test]
    fn armed_present_tap_ring_captures_the_submit_b_destination_byte_for_byte() {
        const N: usize = 3;
        let _ = device();
        let mut armed = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
        armed.arm_metal_for_test();
        armed.debug_block_on_lazy_fallbacks();
        let mut win = WindowGpu::new();
        let input_for = |i: usize| -> RenderInput {
            let mut term = Terminal::new(ROWS as u16, COLS as u16);
            term.process(format!("$ tap ring frame {i}\r\n").as_bytes());
            term.process(format!("\x1b[3{}mpayload {}\x1b[0m", (i % 6) + 1, i * 37).as_bytes());
            term.cell_frame(ROWS, COLS)
        };
        // Destination: letterboxed like the translucency differential, so
        // the captured bytes include the bands.
        let (fw, fh) = {
            let f = armed.render_input(&mut win, &input_for(0), None);
            (f.width as u32, f.height as u32)
        };
        let (dw, dh) = (fw + 9, fh + 7);
        assert!(
            armed.metal_render_armed(),
            "the first frame must mint the arm"
        );
        armed
            .video_begin_metal_standin_for_test(
                &mut win,
                dw,
                dh,
                crate::video_tap::CaptureOpts {
                    half_res: false,
                    budget_bytes: crate::video_tap::DEFAULT_BUDGET,
                    fps_cap: None,
                    requested_ms: 0,
                },
            )
            .expect("armed ring begins");
        armed
            .presented_snapshot_begin_metal_standin_for_test(&mut win, dw, dh)
            .expect("armed one-shot begins");
        let mut presented: Vec<Vec<u8>> = Vec::new();
        for i in 0..N {
            let bytes = armed
                .metal_present_bytes_for_test(&mut win, &input_for(i), (dw, dh), false)
                .expect("armed present");
            assert_eq!(bytes.len(), (dw * dh * 4) as usize);
            armed.video_after_present(&mut win, (i as u64 + 1) * 1_000);
            armed
                .presented_snapshot_after_present(&mut win, (i as u64 + 1) * 1_000)
                .expect("snapshot after-present");
            presented.push(bytes);
        }
        assert_ne!(
            presented[0], presented[1],
            "non-vacuity: distinct inputs must present distinct bytes"
        );
        assert_ne!(presented[1], presented[2], "as above");
        let take = armed.video_finish(&mut win).expect("a recording was live");
        assert_eq!(
            (take.frames.len(), take.dropped),
            (N, 0),
            "the armed ring must harvest every present with zero drops \
             (frames={}, dropped={})",
            take.frames.len(),
            take.dropped
        );
        for (i, f) in take.frames.iter().enumerate() {
            assert_eq!((f.w, f.h), (dw, dh), "frame {i} geometry");
            assert_eq!(
                f.rgba, presented[i],
                "frame {i}: THE ARMED RING'S HARVEST IS NOT BYTE-IDENTICAL to \
                 the Submit B destination it rode"
            );
        }
        armed
            .presented_snapshot_finish(&mut win)
            .expect("one-shot finishes");
        let snap = armed
            .presented_snapshot_take(&mut win)
            .and_then(crate::video_tap::RawCapture::convert)
            .expect("one-shot yields its frame");
        assert_eq!(
            snap.rgba, presented[0],
            "the one-shot must capture the FIRST armed present byte-for-byte"
        );
        crate::stderr_line!(
            "armed tap-ring differential: {N} status-polled harvests + the \
             one-shot, all byte-identical to their Submit B destinations \
             ({}x{} letterboxed)",
            dw,
            dh
        );
    }

    /// Armed scissor differential: the same typed-then-scrolled present
    /// sequence through the wgpu arm and the armed arm (residency keyed by
    /// `PresentPrev::on_metal`). Every step's bytes must match, and so must
    /// the cadence (`scissor_taken`, `full_repaints`, `scroll_rescues`). The
    /// scroll notches exercise the whole-row rescue
    /// (`metal_shift_offscreen_band_px`).
    #[test]
    #[cfg(wgpu_arm)]
    fn armed_scissored_present_cadence_matches_wgpu_byte_for_byte() {
        let _ = device();
        let mut wg = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
        wg.disarm_metal_for_test();
        let mut armed = GpuRenderer::new(18.0, Theme::default()).expect("second renderer");
        armed.arm_metal_for_test();
        for g in [&mut wg, &mut armed] {
            g.debug_block_on_lazy_fallbacks();
            // The scissor_repaint recipe: no wall-clock pass may run between
            // the two arms' renders.
            g.set_shimmer(false);
        }
        let mut wg_win = WindowGpu::new();
        let mut armed_win = WindowGpu::new();

        // Scrollback with overshooting glyphs, cursor hidden so the rescue's
        // cursor clause stays quiet.
        let mut term = Terminal::new(ROWS as u16, COLS as u16);
        term.process(b"\x1b[?25l");
        for i in 0..60 {
            term.process(format!("(|_gy) line {i} tail\r\n").as_bytes());
        }

        // Warm frame (Full), three typed deltas (Dirty), six scroll notches
        // (rescue).
        let mut steps = vec![("warm", term.cell_frame(ROWS, COLS))];
        for t in 0..3 {
            term.process(format!("k{t}").as_bytes());
            steps.push(("typed", term.cell_frame(ROWS, COLS)));
        }
        for delta in [2, 2, 2, -1, -1, -1] {
            term.scroll_display(delta);
            steps.push(("scroll", term.cell_frame(ROWS, COLS)));
        }
        for (step, (what, input)) in steps.iter().enumerate() {
            let e = wg.present_input_readback(&mut wg_win, input);
            let a = armed.present_input_readback(&mut armed_win, input);
            assert!(armed.metal_render_armed(), "step {step}: arm live");
            assert_frames_identical(
                &format!("step {step} ({what}): THE ARMED SCISSORED PRESENT"),
                "Measure the cause before touching tolerance",
                &e,
                &a,
            );
        }

        // The cadence must match, and the scissor must be taken.
        assert_eq!(
            (wg.scissor_taken(), wg.full_repaints(), wg.scroll_rescues()),
            (
                armed.scissor_taken(),
                armed.full_repaints(),
                armed.scroll_rescues()
            ),
            "the armed arm's repaint cadence diverged from wgpu's \
             (wgpu scissor/full/rescue vs armed)"
        );
        assert!(
            armed.scissor_taken() >= 3,
            "non-vacuity: the typed deltas must take the scissor on the armed \
             arm (took {} of >= 3)",
            armed.scissor_taken()
        );
        assert!(
            armed.scroll_rescues() >= 2,
            "non-vacuity: the scroll notches must take the E7 rescue on the \
             armed arm (took {} of >= 2)",
            armed.scroll_rescues()
        );
        crate::stderr_line!(
            "armed scissor differential: {} steps byte-identical; cadence \
             scissor={} full={} rescues={} on BOTH arms",
            steps.len(),
            armed.scissor_taken(),
            armed.full_repaints(),
            armed.scroll_rescues()
        );
    }

    /// Submit-A pipelining: N armed frames stay byte-identical to wgpu while the
    /// arm holds each frame's Submit A in flight at return and, by the next
    /// frame's staging, has consumed exactly the prior frames' submits.
    #[test]
    #[cfg(wgpu_arm)]
    fn armed_submit_a_pipelines_and_stays_byte_identical() {
        const N: usize = 6;
        let _ = device();
        let mut wg = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
        wg.disarm_metal_for_test();
        let mut armed = GpuRenderer::new(18.0, Theme::default()).expect("second renderer");
        armed.arm_metal_for_test();
        wg.debug_block_on_lazy_fallbacks();
        armed.debug_block_on_lazy_fallbacks();
        let mut wg_win = WindowGpu::new();
        let mut armed_win = WindowGpu::new();
        for i in 0..N {
            let mut term = Terminal::new(ROWS as u16, COLS as u16);
            term.process(format!("$ pipeline frame {i}\r\n").as_bytes());
            term.process(format!("\x1b[3{}mrow {}\x1b[0m tail", (i % 6) + 1, i * 13).as_bytes());
            let input = term.cell_frame(ROWS, COLS);
            let expected = wg.render_input(&mut wg_win, &input, None);
            let actual = armed.render_input(&mut armed_win, &input, None);
            assert_eq!(
                expected.pixels, actual.pixels,
                "frame {i}: bytes diverged under pipelining"
            );
            let (inflight, awaited) = armed.metal_pipeline_probe_for_test();
            assert!(
                inflight,
                "frame {i}: Submit A must be HELD IN FLIGHT at return — the \
                 pipelining is not live"
            );
            assert_eq!(
                awaited, i as u64,
                "frame {i}: the staging wait must have consumed exactly the \
                 prior frames' submits (the shared-storage discipline)"
            );
        }
        crate::stderr_line!(
            "armed pipelining: {N} frames byte-identical; Submit A held in \
             flight each frame; staging awaited exactly N-1 priors"
        );
    }

    /// Multi-window disarm edge: window A attaches an armed swapchain; window
    /// B's attach fails (no parent layer) and disarms the renderer; A's next
    /// present must drop its armed surface so A's parent has zero sublayers.
    /// Also checks the attach parented, the disarm happened, and the sweep is
    /// a no-op for a never-armed window.
    #[test]
    #[cfg(wgpu_arm)]
    fn a_disarm_unparents_the_previously_armed_windows_stale_layer() {
        let _ = device();
        let _pool = ffi::AutoreleasePool::new();
        let mut armed = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
        armed.arm_metal_for_test();
        armed.debug_block_on_lazy_fallbacks();
        let mut win = WindowGpu::new();
        // Mint the arm (attach requires it live).
        let input = representative_input();
        let _ = armed.render_input(&mut win, &input, None);
        assert!(armed.metal_render_armed(), "arm minted");

        // Window A's parent layer (the winit view-layer stand-in) + attach.
        let parent = plain_calayer();
        let ms = armed
            .metal_attach_for_test(Some(&parent), 64, 48)
            .expect("window A's armed attach succeeds under a live parent");
        assert_eq!(
            sublayer_count(&parent),
            1,
            "non-vacuity: the armed swapchain is PARENTED under window A"
        );
        let mut a_metal = Some(ms);

        // While armed, the sweep must not touch window A's surface.
        armed.metal_disarm_sweep(&mut a_metal);
        assert!(
            a_metal.is_some() && sublayer_count(&parent) == 1,
            "the sweep is a no-op while the renderer is armed"
        );

        // Window B's attach fails (no parent layer), which disarms.
        assert!(
            armed.metal_attach_for_test(None, 64, 48).is_none(),
            "window B's attach must fail without a parent layer"
        );
        assert!(
            !armed.metal_render_armed(),
            "non-vacuity: the failed attach must DISARM the renderer"
        );

        // Window A's next present: the sweep drops the armed surface and
        // the Drop discipline unparents the stale layer.
        armed.metal_disarm_sweep(&mut a_metal);
        assert!(
            a_metal.is_none(),
            "the disarm sweep must drop window A's armed swapchain"
        );
        assert_eq!(
            sublayer_count(&parent),
            0,
            "THE STALE LAYER MUST BE UNPARENTED: window A's parent still \
             holds a sublayer after the disarm sweep — the wgpu frame \
             underneath would present invisibly"
        );

        // A window that never armed: the sweep stays a no-op.
        let mut never_armed: Option<crate::metal::present::MetalWindowSurface> = None;
        armed.metal_disarm_sweep(&mut never_armed);
        assert!(never_armed.is_none());
        crate::stderr_line!(
            "disarm edge: attach parented, failed attach disarmed, the sweep \
             unparented the stale layer (sublayers 1 -> 0)"
        );
    }

    /// Armed present differential with a translucent destination, alpha
    /// compared: wgpu's `present_to_view` body onto a PostMultiplied-shaped
    /// destination (`wgpu_present_bytes_for_test`) against the armed present
    /// onto a standalone `opaque = false` drawable. Opaque (1.0) and
    /// translucent (0.55), N=5 each, letterboxed +9/+7; only the translucent
    /// run may carry sub-255 alpha, and it must.
    #[test]
    #[cfg(wgpu_arm)]
    fn armed_present_translucency_matches_wgpu_end_to_end() {
        const N: usize = 5;
        let _ = device();
        let input = representative_input();
        for (label, opacity) in [("opaque", 1.0f32), ("translucent", 0.55f32)] {
            let translucent = opacity < 1.0;
            let mut wg = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
            wg.disarm_metal_for_test();
            wg.set_background_opacity(opacity);
            let mut armed = GpuRenderer::new(18.0, Theme::default()).expect("second renderer");
            armed.arm_metal_for_test();
            armed.set_background_opacity(opacity);
            let mut wg_win = WindowGpu::new();
            let mut armed_win = WindowGpu::new();
            let (fw, fh) = wg.frame_size(input.rows, input.cols);
            let (dw, dh) = (fw as u32 + 9, fh as u32 + 7);
            let mut translucent_alpha_seen = false;
            for run in 0..N {
                let expected =
                    wg.wgpu_present_bytes_for_test(&mut wg_win, &input, (dw, dh), translucent);
                let actual = armed
                    .metal_present_bytes_for_test(&mut armed_win, &input, (dw, dh), translucent)
                    .expect("the armed present hook runs on this device");
                assert!(
                    armed.metal_render_armed(),
                    "{label} run {run}: the armed renderer minted its arm"
                );
                assert_row_bytes_identical(
                    &format!("{label} run {run}: THE ARMED PRESENT"),
                    "Alpha is compared too — measure the cause before touching tolerance",
                    dw as usize,
                    4,
                    &expected,
                    &actual,
                );
                let sub255 = actual
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .filter(|px| px[3] < 255)
                    .count();
                if translucent {
                    assert!(
                        sub255 > 0,
                        "{label} run {run}: NON-VACUITY — a translucent present must \
                         put sub-255 alpha in the destination (the W5 harness's \
                         structurally-opaque defect class)"
                    );
                    translucent_alpha_seen = true;
                } else {
                    assert_eq!(
                        sub255, 0,
                        "{label} run {run}: an opaque present emits alpha 255 everywhere"
                    );
                }
            }
            if translucent {
                assert!(translucent_alpha_seen);
                crate::stderr_line!(
                    "translucent present differential: byte-identical (alpha included) \
                     over {} texels x {N} runs at opacity {opacity}",
                    (dw * dh) as usize
                );
            }
        }
    }

    /// Armed virtual-present differential: `present_virtual` plus the tap ring
    /// (`virtual_presented_snapshot_current`), wgpu vs armed, byte-identical
    /// including the tap's harvest and swizzle. N=5. The armed window must hold
    /// the Metal offscreen and virtual destination and no wgpu offscreen; the
    /// wgpu twin the reverse.
    #[test]
    #[cfg(wgpu_arm)]
    fn armed_virtual_present_feeds_the_unchanged_tap_ring_byte_for_byte() {
        const N: usize = 5;
        let _ = device();
        let mut wg = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
        wg.disarm_metal_for_test();
        let mut armed = GpuRenderer::new(18.0, Theme::default()).expect("second renderer");
        armed.arm_metal_for_test();
        let mut wg_win = WindowGpu::new();
        let mut armed_win = WindowGpu::new();
        let input = representative_input();
        for run in 0..N {
            assert!(
                wg.present_virtual(&mut wg_win, &input, false, None, None),
                "run {run}: the wgpu virtual present cannot fail"
            );
            assert!(
                armed.present_virtual(&mut armed_win, &input, false, None, None),
                "run {run}: the ARMED virtual present must succeed on this device \
                 (a refusal would drop every headless capture on the armed arm)"
            );
            assert!(
                armed.metal_render_armed(),
                "run {run}: the armed renderer must have minted its Metal arm"
            );
            let expected = wg
                .virtual_presented_snapshot_current(&wg_win, wg_win.resident_input_epoch(), 7)
                .and_then(crate::video_tap::RawCapture::convert)
                .expect("the wgpu tap snapshot completes");
            let actual = armed
                .virtual_presented_snapshot_current(&armed_win, armed_win.resident_input_epoch(), 7)
                .and_then(crate::video_tap::RawCapture::convert)
                .expect("the armed tap snapshot completes");
            assert_eq!(
                (expected.w, expected.h),
                (actual.w, actual.h),
                "run {run}: captured geometry diverged"
            );
            assert_row_bytes_identical(
                &format!("run {run}: THE ARMED VIRTUAL PRESENT'S TAP CAPTURE"),
                "Measure the cause before touching tolerance",
                expected.w as usize,
                4,
                &expected.rgba,
                &actual.rgba,
            );
        }
        assert!(
            armed_win.metal_offscreen.is_some(),
            "non-vacuity: the armed window renders on the METAL offscreen"
        );
        assert!(
            armed_win.metal_virtual_off.is_some(),
            "non-vacuity: the armed window presents into the METAL virtual destination"
        );
        assert!(
            armed_win.offscreen.is_none(),
            "non-vacuity: the armed window must never have run the wgpu tail"
        );
        assert!(
            wg_win.offscreen.is_some(),
            "the wgpu twin renders on the wgpu offscreen"
        );
        assert!(
            wg_win.metal_virtual_off.is_none(),
            "the wgpu twin never touches the Metal arm"
        );
        crate::stderr_line!("armed virtual-present tap differential: byte-identical x {N} runs");
    }

    /// Full-frame differential: `render_input`'s frame, wgpu vs Metal, byte
    /// equal, on fixtures shaped like the parity suites:
    ///
    /// | fixture | rows driven (beyond bg=0 / glyph=1) |
    /// |---|---|
    /// | text_cursor | 2 (colour glyph), cursor streams on 0/1/2 |
    /// | cursor_blend | 3 (`cursor_blend`, opacity < 1) |
    /// | glow_halo | 4 (`glow_add`), 5 (`rain_glow`), 6 (`rain_glow_over`) |
    /// | nova | 4 (`nova_add` through `glow_add`) |
    /// | rain | 5/6 (rain halos, both modes), 11 (rain sprites) |
    /// | fire | 7 (`fire_add`), 8 (`fire_over`), 4 (`glow_under`), glyph halo over the mono atlas |
    /// | sprites | 11 (`sprite_over`: free under+over, cats) |
    /// | scroll, scroll_down | the band-shift twin over the copy verbs, both glide directions |
    /// | five_pass | the worst-case plan: five passes, sRGB/Unorm alternating |
    /// | word_decorations | 9 (`deco_over`, sRGB view) then 10 (`deco_add`, Unorm view), translucent |
    ///
    /// The wgpu arm is `render_input`; the Metal arm replays the recorded plan
    /// through the shared walker (see
    /// `GpuRenderer::frame_differential_for_test`). N=5 per fixture.
    #[test]
    #[cfg(wgpu_arm)]
    fn full_frame_render_input_matches_wgpu_byte_for_byte() {
        use aterm_core::render::{
            FireHaloCell, FireMode, FirePatch, FreeSprite, HaloMode, RainHalo, SceneAtlas,
            SpriteQuad,
        };
        use aterm_render::{CharFg, GlowQuad, premul_rgb};
        use std::sync::Arc;

        const N: usize = 5;
        let _ = device();
        let mut gpu = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
        gpu.disarm_metal_for_test();
        // Bloom and shimmer are present-time layers (rows 12/13); the
        // encode-plan replay compares the offscreen before them.
        gpu.set_bloom(false);
        gpu.set_shimmer(false);
        let mut win = WindowGpu::new();
        let (cw, ch) = gpu.cell_size();
        let (cw16, ch16) = (cw as u16, ch as u16);

        let base_input = || -> RenderInput {
            let mut term = Terminal::new(ROWS as u16, COLS as u16);
            term.process(b"$ frame differential >_\r\n");
            term.process(b"\x1b[31mRED\x1b[0m \x1b[32mGREEN\x1b[0m \x1b[4munder\x1b[0m\r\n");
            term.process(b"\x1b[1mbold\x1b[0m plain 0123456789 \xf0\x9f\x99\x82\r\n");
            term.cell_frame(ROWS, COLS)
        };
        // A deterministic 8x8 RGBA sprite atlas (full-range bytes, varied alpha).
        let scene_atlas = |seed: usize| {
            Arc::new(SceneAtlas {
                width: 8,
                height: 8,
                rgba: (0..8 * 8 * 4)
                    .map(|i| ((i * 13 + seed) % 256) as u8)
                    .collect(),
                version: 1,
            })
        };

        use crate::renderer::StreamId as S;
        let mut fixtures: Vec<(&'static str, RenderInput, &'static [S])> = Vec::new();

        // 1. text + cursor (bg / glyph / colour glyph / cursor streams).
        fixtures.push((
            "text_cursor",
            base_input(),
            &[S::Bg, S::Glyph, S::Color, S::Deco, S::CursorBlock],
        ));

        // 2. glow + halos: the LUMEN aurora over row 1 (glow_parity's shape)
        //    plus Add and Over radial halos (glow_halo_parity's shape).
        {
            let mut input = base_input();
            let base = 0x0050_fa7b;
            for (i, a) in [40u8, 160, 255].iter().enumerate() {
                input.cursor_glow_add.push(GlowQuad {
                    row: 1,
                    x: ((i + 1) * cw) as u16,
                    y: ch16,
                    w: cw16,
                    h: ch16,
                    color: premul_rgb(base, *a),
                    alpha: 0,
                    color2: premul_rgb(base, *a),
                    alpha2: 0,
                });
            }
            for (i, mode) in [HaloMode::Add, HaloMode::Over].into_iter().enumerate() {
                input.glow_halo.push(RainHalo {
                    row: 2,
                    x: ((6 + i * 3) * cw) as u16,
                    y: (2 * ch) as u16,
                    w: cw16,
                    h: ch16,
                    color: if matches!(mode, HaloMode::Add) {
                        premul_rgb(0x00ff_c060, 200)
                    } else {
                        0x0040_30a0
                    },
                    cx: ((6 + i * 3) * cw + cw / 2) as u16,
                    cy: (2 * ch + ch / 2) as u16,
                    rx: (cw * 2) as u16,
                    ry: ch16,
                    mode,
                });
            }
            fixtures.push((
                "glow_halo",
                input,
                &[S::GlowAdd, S::GlowHalo, S::GlowHaloOver],
            ));
        }

        // 3. nova: the supernova additive stream (nova_parity's shape).
        {
            let mut input = base_input();
            for (i, a) in [90u8, 220].iter().enumerate() {
                input.nova_add.push(GlowQuad {
                    row: 3,
                    x: ((2 + i * 4) * cw) as u16,
                    y: (3 * ch) as u16,
                    w: (cw * 2) as u16,
                    h: ch16,
                    color: premul_rgb(0x00ff_79c6, *a),
                    alpha: 0,
                    color2: premul_rgb(0x00ff_79c6, *a),
                    alpha2: 0,
                });
            }
            fixtures.push(("nova", input, &[S::NovaAdd]));
        }

        // 4. rain: bright-head halos (both modes) + rain sprites through the
        //    rain atlas (rain_parity's shape).
        {
            let mut input = base_input();
            input.rain_atlas = Some(scene_atlas(5));
            for i in 0..3u16 {
                input.rain_quads.push(SpriteQuad {
                    row: 4,
                    x: (i * 5) * cw16,
                    y: 4 * ch16,
                    w: 8.min(cw16),
                    h: 8.min(ch16),
                    ax: 0,
                    ay: 0,
                    aw: 8,
                    ah: 8,
                    tint: 0x00ff_ffff,
                    alpha: 255,
                    flip_x: i % 2 == 1,
                });
            }
            input.rain_add.push(RainHalo {
                row: 4,
                x: 2 * cw16,
                y: 4 * ch16,
                w: cw16,
                h: ch16,
                color: premul_rgb(0x0060_ff60, 180),
                cx: 2 * cw16 + cw16 / 2,
                cy: 4 * ch16 + ch16 / 2,
                rx: cw16 * 2,
                ry: ch16,
                mode: HaloMode::Add,
            });
            input.rain_add.push(RainHalo {
                row: 4,
                x: 6 * cw16,
                y: 4 * ch16,
                w: cw16,
                h: ch16,
                color: 0x0020_2040,
                cx: 6 * cw16 + cw16 / 2,
                cy: 4 * ch16 + ch16 / 2,
                rx: cw16,
                ry: ch16,
                mode: HaloMode::Over,
            });
            // A second instance per mode stream, so per-instance stride is
            // exercised (instance 0 never applies it).
            input.rain_add.push(RainHalo {
                row: 5,
                x: 3 * cw16,
                y: 5 * ch16,
                w: cw16,
                h: ch16,
                color: premul_rgb(0x00ff_3020, 150),
                cx: 3 * cw16 + cw16 / 2,
                cy: 5 * ch16 + ch16 / 2,
                rx: cw16,
                ry: ch16 / 2,
                mode: HaloMode::Add,
            });
            input.rain_add.push(RainHalo {
                row: 5,
                x: 7 * cw16,
                y: 5 * ch16,
                w: cw16,
                h: ch16,
                color: 0x0040_1030,
                cx: 7 * cw16 + cw16 / 2,
                cy: 5 * ch16 + ch16 / 2,
                rx: cw16 * 2,
                ry: ch16,
                mode: HaloMode::Over,
            });
            fixtures.push(("rain", input, &[S::RainAdd, S::RainAddOver, S::RainUnder]));
        }

        // 5. fire: Add and Over patches with under-glyph light, charred ink
        //    and the contrast halo.
        {
            let mut input = base_input();
            input.glow_under.push(GlowQuad {
                row: 2,
                x: cw16,
                y: 2 * ch16,
                w: (6 * cw) as u16,
                h: ch16,
                color: premul_rgb(0x00ff_6020, 140),
                alpha: 0,
                color2: premul_rgb(0x00ff_6020, 140),
                alpha2: 0,
            });
            for (i, mode) in [FireMode::Add, FireMode::Over].into_iter().enumerate() {
                input.fire_patch.push(FirePatch {
                    row: 2,
                    x: ((1 + i * 4) * cw) as u16,
                    y: (2 * ch) as u16,
                    w: (3 * cw) as u16,
                    h: ch16,
                    base_y: (3 * ch) as u16,
                    peak_h: (2 * ch) as u16,
                    phase: 512,
                    temp: 200,
                    strength: 220,
                    lean: -2,
                    cov_cap: 200,
                    cell_h: ch16,
                    mode,
                });
                // Instance 1 per mode, so the stride is exercised.
                input.fire_patch.push(FirePatch {
                    row: 4,
                    x: ((2 + i * 4) * cw) as u16,
                    y: (4 * ch) as u16,
                    w: (2 * cw) as u16,
                    h: ch16,
                    base_y: (5 * ch) as u16,
                    peak_h: ch16,
                    phase: 233,
                    temp: 160,
                    strength: 180,
                    lean: 1,
                    cov_cap: 170,
                    cell_h: ch16,
                    mode,
                });
            }
            input.fire_halo.push(FireHaloCell {
                row: 2,
                col: 2,
                strength: 200,
            });
            input.char_fg.push(CharFg {
                row: 2,
                col: 2,
                fg: 0x0020_1008,
            });
            fixtures.push((
                "fire",
                input,
                &[S::FireAdd, S::FireOver, S::GlowUnder, S::GlyphHalo],
            ));
        }

        // 6. sprites: free sprites under and over the text, plus a cat quad —
        //    the shared sprite_over row (free_parity / cat_parity's shape).
        {
            let mut input = base_input();
            input.free_atlas = Some(scene_atlas(11));
            input.cat_atlas = Some(scene_atlas(23));
            for (i, z) in [
                aterm_core::render::FreeZ::UnderText,
                aterm_core::render::FreeZ::OverText,
            ]
            .into_iter()
            .enumerate()
            {
                input.free_sprites.push(FreeSprite {
                    x: (2 + i as i32) * cw as i32,
                    y: (ch / 2) as i32,
                    w: 8,
                    h: 8,
                    ax: 0,
                    ay: 0,
                    aw: 8,
                    ah: 8,
                    tint: 0x00ff_ffff,
                    alpha: 255,
                    flip_x: false,
                    z,
                    sampler: aterm_core::render::FreeSampler::Nearest,
                });
            }
            input.cat_quads.push(SpriteQuad {
                row: 5,
                x: 10 * cw16,
                y: 5 * ch16,
                w: 8.min(cw16),
                h: 8.min(ch16),
                ax: 0,
                ay: 0,
                aw: 8,
                ah: 8,
                tint: 0x00ff_ffff,
                alpha: 255,
                flip_x: false,
            });
            fixtures.push(("sprites", input, &[S::FreeUnder, S::FreeOver, S::CatOver]));
        }

        // 7. scroll: a fractional glide — the band-shift twin runs on the
        //    routed, overlap-refusing copy verbs on both arms.
        {
            let mut input = base_input();
            input.scroll_frac_px = (ch as i32 / 2).max(1);
            input.grid_top_row = 0;
            input.grid_bot_row = ROWS;
            fixtures.push(("scroll", input, &[S::Bg, S::Glyph]));
        }

        // 7b. scroll_down: the negative glide (overscroll bounce), exposing
        //     the top strip and swapping src/dst.
        {
            let mut input = base_input();
            input.scroll_frac_px = -(ch as i32 / 2).max(1);
            input.grid_top_row = 0;
            input.grid_bot_row = ROWS;
            fixtures.push(("scroll_down", input, &[S::Bg, S::Glyph]));
        }

        // The translucent-cursor fixture flips renderer state, so it runs last
        // with its own set/reset.
        let mut results: Vec<String> = Vec::new();
        let mut run_fixture = |gpu: &mut GpuRenderer,
                               win: &mut WindowGpu,
                               name: &str,
                               input: &RenderInput,
                               must_drive: &[S]| {
            for run in 0..N {
                match gpu.frame_differential_for_test(win, input) {
                    Ok(None) => aterm_spec::host::could_not_run("no Metal device"),
                    Err(e) => panic!("{name} run {run}: {e}"),
                    Ok(Some((expected, actual))) => {
                        // The fixture must drive the streams it claims to
                        // sweep.
                        let drawn = gpu.last_frame_plan_streams_for_test();
                        for want in must_drive {
                            assert!(
                                drawn.iter().any(|(stream, _)| stream == want),
                                "{name}: fixture claims to drive {want:?} but the \
                                 plan never drew it — the sweep is vacuous"
                            );
                        }
                        assert_frames_identical(
                            &format!("{name} run {run}: THE METAL FULL FRAME"),
                            "Measure the cause (pipeline state? view alias? clear \
                             encode? sampler? shader twin?) — never loosen the tolerance.",
                            &expected,
                            &actual,
                        );
                        if run == N - 1 {
                            results.push(format!(
                                "{name}: byte-identical over {} pixels x {N} runs",
                                expected.pixels.len()
                            ));
                        }
                    }
                }
            }
        };

        for (name, input, must_drive) in &fixtures {
            run_fixture(&mut gpu, &mut win, name, input, must_drive);
        }
        // 8. five_pass: under-glyph light and fire, base glyphs, bright-head
        //    halos and free sprites over the text force the worst-case plan,
        //    five passes alternating sRGB|Unorm|sRGB|Unorm|sRGB, so load
        //    inheritance, tracker resets and view switches all run.
        let five_pass = {
            let part = |name: &str| {
                let fixture = fixtures.iter().find(|f| f.0 == name);
                fixture.expect("a fixture above").1.clone()
            };
            let (fire, rain, sprites) = (part("fire"), part("rain"), part("sprites"));
            let mut term = Terminal::new(ROWS as u16, COLS as u16);
            term.process(b"$ worst case pass plan >_\r\n");
            term.process(b"\x1b[35mfive\x1b[0m passes \x1b[1mALTERNATE\x1b[0m\r\n");
            let mut input = term.cell_frame(ROWS, COLS);
            input.glow_under = fire.glow_under;
            input.fire_patch = fire.fire_patch;
            input.rain_add = rain.rain_add;
            input.free_atlas = sprites.free_atlas;
            input.free_sprites = sprites.free_sprites;
            input
        };
        let five_streams = [
            S::GlowUnder,
            S::FireAdd,
            S::FireOver,
            S::Glyph,
            S::RainAdd,
            S::RainAddOver,
            S::FreeUnder,
            S::FreeOver,
            S::CursorBlock,
        ];
        run_fixture(&mut gpu, &mut win, "five_pass", &five_pass, &five_streams);
        assert_eq!(
            gpu.last_frame_passes(),
            5,
            "five_pass must force the worst-case sRGB|Unorm|sRGB|Unorm|sRGB plan"
        );
        // 9. word_decorations on a blank row of a translucent ground: the
        //    deco_over row on the sRGB view, then the deco_add row (One/One,
        //    RGB-only mask) on the Unorm view of the same storage, so a
        //    re-encode between them or a dropped alpha mask moves bytes; two
        //    overlapping sparkles saturate.
        {
            use aterm_render::{DecoBlend, DecoGlyph, WordDecoration};
            let mut input = base_input();
            for (col, glyph, blend, color, alpha) in [
                (1, DecoGlyph::Paw, DecoBlend::Over, 0x00F7_A8B8, 128),
                (5, DecoGlyph::RingArc, DecoBlend::Over, 0x00B0_90FF, 210),
                (2, DecoGlyph::Star4, DecoBlend::Add, 0x00FF_D447, 255),
                (2, DecoGlyph::Star4, DecoBlend::Add, 0x00C8_B45A, 140),
            ] {
                input.word_decorations.push(WordDecoration {
                    row: 4,
                    col,
                    dx: 0,
                    dy: 0,
                    glyph,
                    blend,
                    color,
                    alpha,
                });
            }
            gpu.set_background_opacity(0.55);
            run_fixture(
                &mut gpu,
                &mut win,
                "word_decorations",
                &input,
                &[S::WdecoOver, S::WdecoAdd],
            );
            gpu.set_background_opacity(1.0);
        }
        // 10. cursor_blend: the translucent cursor fill (row 3) — and prove
        // the fill really switched to the CursorBlend pipe.
        gpu.set_cursor_opacity(0.5);
        let input = base_input();
        run_fixture(
            &mut gpu,
            &mut win,
            "cursor_blend",
            &input,
            &[S::CursorBlock],
        );
        let blend_fill = gpu
            .last_frame_plan_streams_for_test()
            .iter()
            .any(|&(stream, pipe)| {
                stream == S::CursorBlock && pipe == crate::renderer::DrawPipe::CursorBlend
            });
        assert!(
            blend_fill,
            "cursor_opacity 0.5 must route the cursor fill through CursorBlend (row 3)"
        );
        gpu.set_cursor_opacity(1.0);
        for line in &results {
            crate::stderr_line!("{line}");
        }
    }

    /// Scissored Load frame: present A (full), then present B (dirty), with B's
    /// recorded Load+scissor plan replayed on Metal over a seed of the
    /// pre-encode bytes. The post-B offscreen must be byte-identical across
    /// arms, and every row outside the recorded band must keep the seed's
    /// bytes. A static glow on the edited row keeps the plan at three passes,
    /// so load inheritance at a view boundary runs under a scissor.
    #[test]
    #[cfg(wgpu_arm)]
    fn the_scissored_load_frame_is_byte_identical_across_arms() {
        use aterm_render::{GlowQuad, premul_rgb};

        const N: usize = 5;
        let _ = device();
        let mut gpu = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
        gpu.disarm_metal_for_test();
        gpu.set_bloom(false);
        gpu.set_shimmer(false);
        let mut win = WindowGpu::new();
        let (cw, ch) = gpu.cell_size();
        let cw16 = cw as u16;
        let ch16 = ch as u16;

        // A and B differ in one row's text and share a static glow there, so the
        // dirty diff stays narrow while the plan keeps its Unorm pass.
        let build = |txt: &[u8]| -> RenderInput {
            let mut term = Terminal::new(ROWS as u16, COLS as u16);
            term.process(b"$ scissored load frame\r\n");
            term.process(b"steady row stays put\r\n");
            term.process(txt);
            let mut input = term.cell_frame(ROWS, COLS);
            // The glow is keyed to the edited row but its rect spans three rows,
            // so the recorded scissor is load-bearing: without it the replay
            // would re-add glow over Load-preserved rows.
            input.cursor_glow_add.push(GlowQuad {
                row: 2,
                x: cw16,
                y: ch16,
                w: (4 * cw) as u16,
                h: 3 * ch16,
                color: premul_rgb(0x0050_fa7b, 160),
                alpha: 0,
                color2: premul_rgb(0x0050_fa7b, 160),
                alpha2: 0,
            });
            input
        };
        let a = build(b"typing");
        let b = build(b"typing!");

        // Prime the present sequence: a full-repaint present of A.
        let mut seed = gpu.present_input_readback(&mut win, &a);
        let (w, h) = (seed.width as u32, seed.height as u32);
        for run in 0..N {
            let expected = gpu.present_input_readback(&mut win, &b);
            assert!(
                matches!(
                    gpu.last_frame_load_for_test(),
                    crate::device_layer::FrameLoad::Load
                ),
                "run {run}: the B present did not take the LOAD path — the dirty \
                 diff refused and the fixture is testing the wrong frame"
            );
            let (sx, sy, sw, sh) = gpu
                .last_frame_scissor_for_test()
                .expect("a dirty present records its scissor");
            assert!(
                sx == 0 && sw == w && sh > 0 && sh < h,
                "run {run}: the scissor {sx},{sy} {sw}x{sh} is not a proper \
                 dirty band of the {w}x{h} target"
            );
            assert!(
                (sh as usize) < 3 * ch,
                "run {run}: the {sh}-px band swallowed the whole 3-row glow \
                 rect — the scissor would be untestable (clipped or not, the \
                 bytes would agree)"
            );
            assert!(
                gpu.last_frame_passes() >= 3,
                "run {run}: the scissored plan must still cross view boundaries \
                 (got {} passes)",
                gpu.last_frame_passes()
            );
            let actual =
                match gpu.metal_replay_recorded_plan_for_test(&win, &b, (w, h), Some(&seed)) {
                    Ok(None) => {
                        aterm_spec::host::could_not_run("no Metal device");
                    }
                    Err(e) => panic!("run {run}: {e}"),
                    Ok(Some(f)) => f,
                };
            assert_frames_identical(
                &format!("run {run}: THE SCISSORED LOAD FRAME (scissor {sx},{sy} {sw}x{sh})"),
                "Measure the cause, never loosen the tolerance",
                &expected,
                &actual,
            );
            // Scissor isolation against the seed catches a bug both arms share
            // (ignoring the scissor), which the arm comparison cannot.
            for y in 0..h as usize {
                if y >= sy as usize && y < (sy as usize + sh as usize) {
                    continue;
                }
                let row = y * w as usize..(y + 1) * w as usize;
                assert!(
                    expected.pixels[row.clone()] == seed.pixels[row],
                    "run {run}: row {y} lies OUTSIDE the scissor band \
                     [{sy}, {}) yet its bytes moved — scissor isolation broke",
                    sy + sh
                );
            }
            // Restore A as the prior frame (another dirty present) and reseed.
            seed = gpu.present_input_readback(&mut win, &a);
        }
        crate::stderr_line!("scissored Load frame: byte-identical + isolated x {N} runs");
    }

    /// Present-path differential: one cropped, letterboxed present through the
    /// wgpu body (`present_virtual_with_crop` → `present_to_view`, captured by
    /// the virtual snapshot tap) against the Metal arm's Submit B on a real
    /// standalone swapchain, N=5 for each of `fx_tray` (bloom, shimmer, card),
    /// `fx_only` (bloom, shimmer) and `plain` (the blit samples the offscreen).
    /// The destination is +9/+7 so the bands matter; the SDR crown is pinned
    /// off (its envelope is wall-clock). `fx_tray` must differ from `plain`.
    #[test]
    #[cfg(wgpu_arm)]
    fn present_path_matches_wgpu_virtual_present_byte_for_byte() {
        use aterm_core::render::FirePatch;
        use aterm_render::{GlowQuad, premul_rgb};

        use crate::TrayQuad;

        const N: usize = 5;
        let _ = device();
        let mut gpu = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
        gpu.disarm_metal_for_test();
        gpu.set_shimmer_phase_for_test(Some(0.37));
        gpu.set_sdr_glow_boost(0.0);
        let (cw, ch) = gpu.cell_size();
        let (cw16, ch16) = (cw as u16, ch as u16);

        let mut input = {
            let mut term = Terminal::new(ROWS as u16, COLS as u16);
            term.process(b"$ present differential >_\r\n");
            term.process(b"\x1b[35mfx\x1b[0m over the \x1b[1mfold\x1b[0m\r\n");
            term.cell_frame(ROWS, COLS)
        };
        // Bright glow on row 4 with air above it: feeds the bloom and derives
        // a non-empty shimmer region (haze rises above the hot band).
        for (i, a) in [200u8, 255, 160].iter().enumerate() {
            input.cursor_glow_add.push(GlowQuad {
                row: 4,
                x: ((3 + i * 2) * cw) as u16,
                y: (4 * ch) as u16,
                w: cw16,
                h: ch16,
                color: premul_rgb(0x00ff_8030, *a),
                alpha: 0,
                color2: premul_rgb(0x00ff_8030, *a),
                alpha2: 0,
            });
        }
        // A zero-area fire patch marks the fire style live for `shimmer_live`
        // without rasterizing any field pixel.
        input.fire_patch = vec![FirePatch::default()];

        // A deterministic straight-alpha card: full-range colour sweep, alpha
        // ramp 0..255 — the src-over blend is load-bearing across the ramp.
        let (pw, ph) = (24u32, 10u32);
        let card: Vec<u8> = (0..pw * ph)
            .flat_map(|i| {
                [
                    ((i * 7) % 256) as u8,
                    ((i * 13) % 256) as u8,
                    ((i * 29) % 256) as u8,
                    ((i * 11) % 256) as u8,
                ]
            })
            .collect();
        let tray = TrayQuad {
            rgba: &card,
            pw,
            ph,
            dx: 2 * cw as u32,
            dy: ch as u32,
            scrim: None,
        };

        let (fw_l, fh_l) = gpu.frame_size(ROWS, COLS);
        let crop = PresentCrop {
            source_y: 0,
            height: u32::try_from(fh_l).expect("logical height fits"),
        };
        let dest = (fw_l as u32 + 9, fh_l as u32 + 7);

        let classes: [(&str, bool, bool, Option<TrayQuad<'_>>); 3] = [
            ("fx_tray", true, true, Some(tray)),
            ("fx_only", true, true, None),
            ("plain", false, false, None),
        ];
        let mut presented: Vec<(&str, Vec<u8>)> = Vec::new();
        for (name, bloom, shimmer, tray) in classes {
            gpu.set_bloom(bloom);
            gpu.set_shimmer(shimmer);
            let mut win = WindowGpu::new();
            let mut first: Option<Vec<u8>> = None;
            for run in 0..N {
                let (expected, actual, (dw, dh), armed) = gpu
                    .present_differential_for_test(&mut win, &input, tray, crop, dest)
                    .expect("the present differential runs")
                    .expect("a Metal device exists on this machine");
                assert_eq!((dw, dh), dest);
                assert_eq!(
                    armed,
                    [bloom, shimmer, tray.is_some()],
                    "{name}: every pass this class claims must actually encode \
                     (a None shimmer region or empty glow stream arms nothing)"
                );
                assert_row_bytes_identical(
                    &format!("THE METAL PRESENT ({name}, run {run})"),
                    "Localize before touching tolerance: band-only diffs are the \
                     letterbox uniform, diffs inside the dilated glow bbox are the \
                     bloom pair, region-rect diffs are the shimmer, card-rect diffs \
                     are the tray blend, everything-diffs is the compose copy or the blit",
                    dw as usize,
                    4,
                    &expected,
                    &actual,
                );
                // Steady-state parity too: run 0 is the cold path (fresh
                // offscreen, full copy), later runs the resident path.
                if let Some(f) = &first {
                    assert_eq!(f, &expected, "{name}: the wgpu capture is stable");
                } else {
                    first = Some(expected);
                }
            }
            presented.push((name, first.expect("N > 0")));
            crate::stderr_line!(
                "present differential ({name}): byte-identical x {N} runs at \
                 {}x{} (letterboxed +9/+7)",
                dest.0,
                dest.1
            );
        }

        // Non-vacuity: the effect passes moved presented bytes.
        let full = &presented[0].1;
        let plain = &presented[2].1;
        let moved = full
            .as_chunks::<4>()
            .0
            .iter()
            .zip(plain.as_chunks::<4>().0.iter())
            .filter(|(a, b)| a != b)
            .count();
        assert!(
            moved > 0,
            "bloom+shimmer+tray must move presented bytes vs the plain present \
             — an inert sequence armed nothing"
        );
        crate::stderr_line!("present differential: {moved} px moved by the effect passes");
    }

    /// Shared wgpu-vs-Metal comparator: byte for byte over `w`-wide rows of
    /// `texel`-byte texels, reporting the first divergence (coordinates, both
    /// values, max delta, alpha-only). `name` says what diverged; `hint` maps
    /// divergence shapes to causes.
    #[cfg(wgpu_arm)]
    fn assert_row_bytes_identical(
        name: &str,
        hint: &str,
        w: usize,
        texel: usize,
        expected: &[u8],
        actual: &[u8],
    ) {
        assert_eq!(
            expected.len(),
            actual.len(),
            "{name}: readback size mismatch — one arm's stride is wrong"
        );
        if expected == actual {
            return;
        }
        let colour = texel * 3 / 4;
        let (mut diffs, mut first, mut max_delta, mut alpha_only) = (0usize, None, 0u8, true);
        for (i, (e, a)) in expected
            .chunks_exact(texel)
            .zip(actual.chunks_exact(texel))
            .enumerate()
        {
            if e != a {
                diffs += 1;
                alpha_only &= e[..colour] == a[..colour];
                for (ec, ac) in e.iter().zip(a) {
                    max_delta = max_delta.max(ec.abs_diff(*ac));
                }
                first.get_or_insert((i, e, a));
            }
        }
        let (i, e, a) = first.expect("bytes differ, so a texel differs");
        panic!(
            "{name} IS NOT BYTE-IDENTICAL TO wgpu: {diffs} of {} texels differ, max \
             byte delta {max_delta}, alpha_only={alpha_only}; first at ({},{}): wgpu \
             {e:02x?} != metal {a:02x?}. {hint}",
            expected.len() / texel,
            i % w,
            i / w,
        );
    }

    /// [`assert_row_bytes_identical`]'s twin for a packed [`Frame`]: the
    /// geometry, then every pixel as straight RGBA bytes.
    #[cfg(wgpu_arm)]
    fn assert_frames_identical(name: &str, hint: &str, expected: &Frame, actual: &Frame) {
        assert_eq!(
            (expected.width, expected.height),
            (actual.width, actual.height),
            "{name}: frame geometry diverged"
        );
        if expected.pixels != actual.pixels {
            let (e, a) = (expected.rgba_bytes(), actual.rgba_bytes());
            assert_row_bytes_identical(name, hint, expected.width, 4, &e, &a);
        }
    }

    /// Crown pair from one fixture: `hdr_glow` onto a seeded `Rgba16Float`
    /// target (One/One, both `s2l` branches, the headroom clamp) and `sdr_glow`
    /// onto a seeded `Bgra8Unorm` target (screen blend). Three overlapping
    /// instances exercise stride and blend order; the scissor clips the third;
    /// a non-zero `content_off` exercises band placement. f16 compared as raw
    /// bits.
    #[test]
    #[cfg(wgpu_arm)]
    fn crown_rows_match_wgpu_byte_for_byte() {
        use super::encoder::{EncodeSession, RenderPassDesc, StoreAction};
        use super::loss::LossLatch;
        use crate::pipeline_table::Pipeline;
        use std::sync::Arc;

        let dev = device();
        let mut gpu = crate::renderer::gpu_renderer_for_test(None, 18.0, Theme::default());
        gpu.disarm_metal_for_test();
        const W: usize = 20;
        const H: usize = 14;
        const OFF: [f32; 2] = [3.0, 2.0];
        const BOOST: f32 = 4.0;
        const HEADROOM: f32 = 1.2;
        const SCISSOR: [u32; 4] = [3, 2, 17, 12];
        // (rect, colour): a bright quad (headroom clamp at boost 4), a dark
        // quad (the `s2l` low branch), and an overlapping partially clipped one.
        const INSTANCES: [([u16; 4], [u8; 4]); 3] = [
            ([1, 1, 8, 6], [240, 160, 80, 255]),
            ([5, 3, 6, 5], [9, 6, 3, 255]),
            ([10, 6, 9, 7], [90, 200, 250, 255]),
        ];

        // f16 seed: exactly-representable halves cycling 0.0/0.25/0.5/1.0.
        const HALVES: [u16; 4] = [0x0000, 0x3400, 0x3800, 0x3c00];
        let hdr_seed: Vec<u8> = (0..W * H * 4)
            .flat_map(|i| HALVES[i % 4].to_le_bytes())
            .collect();
        let sdr_seed: Vec<u8> = (0..W * H * 4).map(|i| ((i * 5) % 180) as u8).collect();

        let expected_hdr = gpu.crown_row_bytes_for_test(
            true, &INSTANCES, OFF, BOOST, HEADROOM, &hdr_seed, SCISSOR, W as u32, H as u32,
        );
        assert_ne!(expected_hdr, hdr_seed, "the EDR crown must add light");
        let expected_sdr = gpu.crown_row_bytes_for_test(
            false, &INSTANCES, OFF, 1.0, 0.35, &sdr_seed, SCISSOR, W as u32, H as u32,
        );
        assert_ne!(expected_sdr, sdr_seed, "the SDR crown must brighten");

        // The Metal arm, both rows through one rig.
        let latch = Arc::new(LossLatch::new());
        let session = EncodeSession::new(&dev, Arc::clone(&latch)).expect("session");
        let mint = super::resources::MetalResourceDevice::new(&dev, Arc::clone(&latch));
        let usage = TEXTURE_USAGE_RENDER_TARGET | TEXTURE_USAGE_SHADER_READ;
        #[repr(C)]
        #[derive(Clone, Copy)]
        struct CrownU {
            screen: [f32; 2],
            content_off: [f32; 2],
            boost: f32,
            headroom: f32,
            _pad: [f32; 2],
        }
        // Tight 16-byte `GlowInstance`s: the rect, then the colour at both
        // ends of the ramp (a flat pair, as the wgpu oracle packs it).
        let mut stream_bytes: Vec<u8> = Vec::new();
        for (rect, colour) in INSTANCES {
            for v in rect {
                stream_bytes.extend_from_slice(&v.to_le_bytes());
            }
            stream_bytes.extend_from_slice(&colour);
            stream_bytes.extend_from_slice(&colour);
        }
        assert_eq!(
            stream_bytes.len() as u64,
            crate::pipeline_table::GLOW_LAYOUT.stride * INSTANCES.len() as u64,
            "the fixture packs tight 16-byte GlowInstances"
        );
        let stream = dev.new_buffer(stream_bytes.len()).expect("stream");
        // SAFETY: exactly-sized fresh shared buffer.
        unsafe { ffi::buffer_write(&stream, &stream_bytes) };

        for (name, row_id, boost, headroom, fmt, seed, expected) in [
            (
                "HDR_GLOW",
                Pipeline::HdrGlow,
                BOOST,
                HEADROOM,
                PixelFormat::Rgba16Float,
                &hdr_seed,
                &expected_hdr,
            ),
            (
                "SDR_GLOW",
                Pipeline::SdrGlow,
                1.0,
                0.35,
                PixelFormat::Bgra8Unorm,
                &sdr_seed,
                &expected_sdr,
            ),
        ] {
            let spec = row_id.spec();
            let binds = spec.binds;
            let lib = super::pipelines::compile(&dev, spec).expect("hdr_glow.metal");
            let pso = super::pipelines::build(&dev, &lib, spec, PixelFormat::Bgra8Unorm)
                .expect("the crown row builds");
            let texel = fmt.bytes_per_texel();
            let dst = mint.texture_2d(fmt, W, H, usage).expect("target");
            // SAFETY: fresh managed texture, tight stride.
            unsafe { ffi::texture_upload(dst.obj(), MtlRegion::full_2d(W, H), seed, W * texel) };
            #[expect(clippy::cast_precision_loss, reason = "test extents")]
            let cu = CrownU {
                screen: [W as f32, H as f32],
                content_off: OFF,
                boost,
                headroom,
                _pad: [0.0; 2],
            };
            let ubuf = dev.new_buffer(size_of::<CrownU>()).expect("uniform");
            // SAFETY: repr(C) into an exactly-sized fresh shared buffer; the
            // previous arm's command buffer was waited on.
            unsafe { ffi::buffer_write(&ubuf, as_bytes(&cu)) };
            let row = W * texel;
            let rb = dev.new_buffer(row * H).expect("readback");
            let mut cb = session.begin().expect("cb");
            {
                let pass = cb
                    .render_pass(&RenderPassDesc {
                        target: &dst,
                        load: LoadAction::Load,
                        store: StoreAction::Store,
                        viewport: None,
                        scissor: Some(MtlScissorRect {
                            x: SCISSOR[0] as usize,
                            y: SCISSOR[1] as usize,
                            width: (SCISSOR[2] - SCISSOR[0]) as usize,
                            height: (SCISSOR[3] - SCISSOR[1]) as usize,
                        }),
                    })
                    .expect("crown pass");
                pass.set_pipeline(&pso);
                pass.set_vertex_buffer(
                    &ubuf,
                    binds.vertex_uniform.expect("the crown rows bind at 0") as usize,
                )
                .expect("vertex uniform bind");
                pass.set_fragment_buffer(&ubuf, binds.fragment_buffers[0] as usize);
                pass.set_instance_stream(&stream);
                pass.draw_instanced(
                    super::pipelines::metal_primitive_type(spec.topology),
                    6,
                    INSTANCES.len(),
                )
                .expect("armed draw");
            }
            cb.copy_texture_to_buffer(&dst, W, H, &rb, row)
                .expect("readback copy");
            assert_eq!(
                cb.commit().wait_outcome(),
                super::loss::CbOutcome::Completed
            );
            // SAFETY: shared storage, terminal above.
            let actual = unsafe { ffi::buffer_bytes(&rb, row * H) };
            assert_row_bytes_identical(
                &format!("THE METAL {name} ROW"),
                "Localize the cause before touching tolerance — all-texels is a \
                 bind/uniform fault, edge-only is filtering, seed-region-only is \
                 the Load, delta<=1 is rounding",
                W,
                texel,
                expected,
                &actual,
            );
            crate::stderr_line!(
                "{name} differential on {}: byte-identical over {} texels \
                 ({} target, 3 instances, scissored Load, content_off armed)",
                dev.name(),
                W * H,
                if fmt == PixelFormat::Rgba16Float {
                    "f16 additive"
                } else {
                    "SCREEN-blend Bgra8"
                }
            );
        }
    }
}
