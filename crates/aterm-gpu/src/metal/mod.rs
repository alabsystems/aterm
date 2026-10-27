// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The macOS Metal backend, the renderer's arm on macOS.

pub(crate) mod acquire_worker;
#[cfg(test)]
pub(crate) mod blit;
pub(crate) mod encoder;
pub(crate) mod fetch;
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
    pub(crate) fn device() -> Device {
        Device::preferred().unwrap_or_else(|| aterm_spec::host::could_not_run("no Metal device"))
    }

    /// `cell.metal`'s 16-byte `Uniforms` block: `float2 screen`, `float text_blend`, `float pad`, laid out identically in `repr(C)`.
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub(crate) struct CellUniforms {
        pub(crate) screen: [f32; 2],
        pub(crate) text_blend: f32,
        pub(crate) pad: f32,
    }

    /// View `v` as a byte slice for GPU upload.
    ///
    /// # Safety
    /// `T` must be `repr(C)` and free of padding-sensitive invariants; the bytes are copied
    /// straight into a GPU-visible buffer.
    pub(crate) unsafe fn as_bytes<T: Copy>(v: &T) -> &[u8] {
        // SAFETY: `T: Copy` + `repr(C)`; read-only, lives only for the copy.
        unsafe { std::slice::from_raw_parts(std::ptr::from_ref(v).cast::<u8>(), size_of::<T>()) }
    }

    /// A bare `CALayer` (+1).
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

    /// The two strings a C-string read of an `NSString` gets wrong, as UTF-16 code units and the text they must read back as.
    pub(crate) const C_STRING_HOLES: [(&[u16], &str); 2] = [
        (&[0x61, 0x0000, 0x62], "a\u{0}b"),
        (&[0x61, 0xD800, 0x62], "a\u{FFFD}b"),
    ];

    /// A +1 `NSString` of exactly `units`, built without a C string so it may hold U+0000 or an unpaired surrogate, which no Rust `str` can spell.
    pub(crate) fn ns_string_of_units(units: &[u16]) -> Obj {
        // SAFETY: `+alloc` returns a +1 instance that `-initWithCharacters:length:` (`@32@0:8r^S16Q24`) consumes, reading `units.len()` code units during the call and returning +1 or nil.
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
        // SAFETY: `sublayers` returns a +0 NSArray (or nil) owned by the pool; `count` is an NSUInteger getter on it.
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
    use super::ffi::{
        BlendFactor, BlendOperation, BlendState, ColorWriteMask, CompileOptions, Device,
        LanguageVersion, LoadAction, MtlRegion, Pass, PixelFormat, RenderPipelineDescriptor,
        SamplerDesc, TEXTURE_USAGE_RENDER_TARGET, TEXTURE_USAGE_SHADER_READ,
    };
    use super::test_support::{as_bytes, device};
    use super::{ffi, shaders};

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
            // SAFETY: `FireParityParams` is `repr(C)` and the buffer was just created at exactly its size; no GPU work is in flight.
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
            // SAFETY: `dispatch_compute` blocked on `waitUntilCompleted`, so every GPU write to these shared buffers has landed.
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
        assert!(
            nonzero * 4 > total,
            "parity sweep is too sparse to be meaningful: {nonzero}/{total} non-zero"
        );
    }

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

    /// `k_udiv_parity`'s parameters, as `parity_kernel.metal` declares them.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct UdivParityParams {
        d0: u32,
        q_max: u32,
        r_all: u32,
    }

    /// `cell.metal`'s `udiv_small` is the integer divide over every divide its callers make: the glow ramp's `n / 2w` (a quad up to 65535 px wide, a quotient of at most 255) and the fx sprite texel's `l·side / dst` (a destination side up to 65535, a quotient below the art's side, at most the fx atlas's 4096). Every divisor up to 2^17 meets every quotient up to 4096 at the remainders a float estimate gets wrong first (0, 1, d − 2, d − 1), and every divisor up to 512 meets every remainder.
    #[test]
    fn udiv_small_is_the_integer_divide_over_its_callers_domain() {
        const Q_MAX: u32 = crate::renderer::fx_gpu::FX_ATLAS_CAP;
        const D_EDGE: u32 = 1 << 17;
        const D_EVERY: u32 = 512;
        const CHUNK: u32 = 1 << 14;

        let dev = device();
        let lib = dev
            .new_library(&shaders::cell_with_parity_kernels())
            .expect("cell.metal + parity kernels compile");
        let kf = lib.function("k_udiv_parity").expect("k_udiv_parity");
        let pso = dev.new_compute_pipeline(&kf).expect("udiv compute pso");
        let queue = dev.new_command_queue().expect("queue");

        let mut sweeps = vec![(1, D_EVERY, 1)];
        sweeps.extend((0..D_EDGE / CHUNK).map(|k| (1 + k * CHUNK, CHUNK, 0)));
        let mut divisors = 0u64;
        for (d0, count, r_all) in sweeps {
            let p = UdivParityParams {
                d0,
                q_max: Q_MAX,
                r_all,
            };
            let pbuf = dev
                .new_buffer(size_of::<UdivParityParams>())
                .expect("params");
            // SAFETY: `UdivParityParams` is `repr(C)` and the buffer was just created at exactly its size; no GPU work is in flight.
            unsafe { ffi::buffer_write(&pbuf, as_bytes(&p)) };
            let n = count as usize;
            let b_bad = dev.new_buffer(n * 4).expect("bad");
            let b_first = dev.new_buffer(n * 4).expect("first");
            ffi::dispatch_compute(
                &queue,
                &pso,
                &[&pbuf, &b_bad, &b_first],
                ffi::MtlSize {
                    width: n,
                    height: 1,
                    depth: 1,
                },
            )
            .expect("udiv parity dispatch completes");
            // SAFETY: `dispatch_compute` blocked on `waitUntilCompleted`, so every GPU write to these shared buffers has landed.
            let (bad, first) =
                unsafe { (ffi::buffer_u32s(&b_bad, n), ffi::buffer_u32s(&b_first, n)) };
            for (i, (&bad, &first)) in bad.iter().zip(&first).enumerate() {
                let d = d0 + i as u32;
                assert_eq!(
                    bad,
                    0,
                    "udiv_small disagrees with the integer divide {bad} times at divisor {d}, first \
                     at {first} / {d} = {}",
                    first / d
                );
            }
            divisors += u64::from(count);
        }
        assert_eq!(
            divisors,
            u64::from(D_EVERY + D_EDGE),
            "the sweep covers every divisor"
        );
    }

    /// The 32-byte `Probe` uniform in `shaders/state_probe.metal`: `float4`
    /// colour at 0, `float2` destination size at 16, `float2` pad at 24.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct ProbeUniform {
        color: [f32; 4],
        dst: [f32; 2],
        pad: [f32; 2],
    }

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
            // SAFETY: `ProbeUniform` is `repr(C)` and the buffer was created at exactly its size; no GPU work is in flight on a fresh buffer.
            unsafe { ffi::buffer_write(&b, as_bytes(&u)) };
            b
        }

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
            // SAFETY: `tex` is 2-D `format`, `w` x `h`, non-Private storage; `bytes` is exactly `w * h` texels with a matching stride.
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

        fn read(&self, pass: &Pass<'_>, format: PixelFormat) -> Vec<u8> {
            let row = pass.dst_w * format.bytes_per_texel();
            let n = row * pass.dst_h;
            let rb = self.dev.new_buffer(n).expect("readback");
            ffi::draw_and_read(&self.queue, pass, &rb, row).expect("probe pass runs");
            // SAFETY: `rb` is shared storage of exactly `n` bytes, and `draw_and_read` returned after `waitUntilCompleted`.
            unsafe { ffi::buffer_bytes(&rb, n) }
        }
    }

    #[test]
    fn color_write_mask_gates_each_channel_on_the_gpu() {
        let dev = device();
        let p = StateProbe::new(dev);
        const W: usize = 8;
        const H: usize = 8;
        const FMT: PixelFormat = PixelFormat::Rgba8Unorm;
        const DST: [u8; 4] = [10, 20, 30, 64];
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
        let blend = Some(BlendState {
            source_rgb: BlendFactor::One,
            destination_rgb: BlendFactor::One,
            rgb_operation: BlendOperation::Add,
            source_alpha: BlendFactor::One,
            destination_alpha: BlendFactor::One,
            alpha_operation: BlendOperation::Add,
        });

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
                    uniform: Some((&uniform, 2)),
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
        // SAFETY: `src` is 2-D `Rgba8Unorm` at `sw` x `sh`, non-Private; `src_bytes` was length-checked and the stride is one row.
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
                src_tex: Some((&src, 0)),
                sampler: Some((&samp, 0)),
                uniform: Some((&uniform, 2)),
                vertex_uniform: None,
                draw: None,
            },
            FMT,
        )
    }

    #[test]
    fn sampler_mag_filter_changes_the_magnified_bytes() {
        let dev = device();
        let p = StateProbe::new(dev);
        const DW: usize = 4;

        #[rustfmt::skip]
        let src_bytes: [u8; 16] = [
            0, 0, 0, 255,   255, 255, 255, 255,
            255, 255, 255, 255,   0, 0, 0, 255,
        ];
        let nearest = sample_through(&p, (2, 2), &src_bytes, (4, 4), SamplerDesc::NEAREST_CLAMP);
        let linear = sample_through(&p, (2, 2), &src_bytes, (4, 4), SamplerDesc::LINEAR_CLAMP);

        for (i, texel) in nearest.as_chunks::<4>().0.iter().enumerate() {
            let (px, py) = (i % DW, i / DW);
            let want = if (px / 2) == (py / 2) { 0 } else { 255 };
            assert_eq!(
                texel[0], want,
                "NEAREST ({px},{py}): a nearest fetch can only return a source texel"
            );
        }

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

    #[test]
    fn sampler_min_filter_changes_the_minified_bytes() {
        let dev = device();
        let p = StateProbe::new(dev);

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
        assert_ne!(
            &nearest[..8],
            &linear[..8],
            "the two descriptors produced identical bytes on a minifying \
             pass, so `setMinFilter:` reached nothing"
        );
    }

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

    use aterm_core::terminal::Terminal;
    use aterm_render::{RenderInput, Theme};

    use crate::WindowGpu;

    const ROWS: usize = 6;
    const COLS: usize = 24;

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
        let take = armed
            .video_finish(&mut win)
            .expect("a recording was live")
            .settle();
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
}
