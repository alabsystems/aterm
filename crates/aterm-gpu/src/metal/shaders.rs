// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The MSL twins of aterm's six WGSL shaders, plus their entry-point rosters.
//!
//! # Why these are files and the WGSL was not
//!
//! The WGSL lives in six `const &str` literals in `renderer.rs` (lines 272,
//! 759, 926, 1016, 1071, 1146) — there is not one `.wgsl` file in the tree.
//! The MSL is kept in `crates/aterm-gpu/shaders/*.metal` and pulled in with
//! `include_str!` instead, because a `.metal` file is what the headless
//! compile test, an editor, and `xcrun metal` (where it exists) can all read.
//!
//! # THE FORMAT LAW — the thing a port silently gets wrong
//!
//! `renderer.rs::pick_surface_format` (7811-7826) deliberately chooses a
//! **non-sRGB** surface format (`Bgra8Unorm`, else `Rgba8Unorm`, else any
//! non-sRGB non-`Rgba16Float` format) and NEVER an `*_sRGB` one. That is not an
//! oversight to be "fixed" during the port:
//!
//! * The base OVER/REPLACE passes render into an **sRGB-typed VIEW** of the
//!   offscreen texture. The fragment shaders emit **linear light** (`s2l`), the
//!   view re-encodes on store, and so fixed-function blending composites in
//!   linear light — which is what makes the GPU match the CPU `blend`.
//! * The ADDITIVE passes (`fs_glow`, `fs_rain_glow`, `fs_fire_add`,
//!   `fs_deco_add`) bind the **Unorm** view of the SAME texture and emit RAW,
//!   un-decoded values, so the One/One add is byte-exact against the CPU
//!   `add_sat`.
//! * The final blit then writes already-sRGB-encoded bytes to the swapchain. If
//!   the swapchain were sRGB-typed it would encode a SECOND time and the whole
//!   frame would wash out.
//!
//! Metal expresses the same pairing with
//! `newTextureViewWithPixelFormat:` over a texture created with
//! [`TEXTURE_USAGE_PIXEL_FORMAT_VIEW`](super::ffi::TEXTURE_USAGE_PIXEL_FORMAT_VIEW):
//! `Bgra8Unorm` <-> `Bgra8UnormSrgb` are a view-compatible pair, so the trick
//! ports exactly rather than needing an approximation.
//!
//! The four formats the renderer asks for — `Bgra8Unorm`, `Rgba8Unorm`,
//! `R8Unorm` (the glyph atlas) and `Rgba16Float` (the HDR/EDR path) — are all
//! declared in [`super::ffi::PixelFormat`] and all exercised by
//! [`super::tests`].

//! # THE PORT TABLE
//!
//! WGSL lines are the literal bodies in `renderer.rs` (the `r#"` open/close
//! lines excluded); MSL lines are the `.metal` files beside this module.
//!
//! | shader   | WGSL | MSL | what changed |
//! |----------|-----:|----:|--------------|
//! | `cell`     | 479 | 496 | Attribute structs replace `@location` params (`[[attribute(n)]]` + `[[stage_in]]`); `@interpolate(flat)` -> `[[flat]]`; `bitcast<i32>` -> `as_type<int>`; the rain weight and both fire shading tails factored into shared `static inline` helpers so the parity kernel calls the SAME code the fragments do. All integer math otherwise op-for-op. |
//! | `blit`     | 153 | 121 | `textureLoad(t, vec2<i32>(p), 0)` -> `t.read(uint2(p), 0)` (both truncate toward zero, and `p` is bounds-checked non-negative first); `select` kept as-is; the long W1/M3/M5/H1 rationale comments condensed, no logic touched. |
//! | `hdr_glow` |  65 |  68 | Uniform becomes a `constant HdrU&` argument on both stages; `select(lo, hi, c > 0.04045)` maps 1:1 (MSL `select(a,b,cond)` is `cond ? b : a`, the same argument order as WGSL). |
//! | `tray`     |  39 |  41 | Uniform/texture/sampler become function arguments; 4-vertex triangle-strip corner table unchanged. |
//! | `bloom`    |  36 |  36 | Identical apart from the argument-binding form; the 5x5 loop, `exp` weights and normalization are unchanged. |
//! | `shimmer`  |  80 |  81 | `array<vec4<f32>,16>` -> `float4 heat[16]` (same 16-byte stride, so the Rust struct is unchanged); `textureSampleLevel(..., 0.0)` -> `sample(..., level(0.0))`; `heat_at` takes the uniform by reference since MSL has no module-scope uniform. |
//!
//! Totals: 852 WGSL -> 843 MSL (plus a 62-line verification-only
//! `parity_kernel.metal` that is never part of a shipping pipeline).
//!
//! # Constructs with NO direct MSL equivalent
//!
//! There are none in this shader set — every WGSL construct aterm uses mapped
//! 1:1, and nothing was approximated. The four that needed care, and why each
//! is exact rather than close:
//!
//! * **Arithmetic right shift on negative integers.** `fire_core` evaluates
//!   `(body0 - 128) * edge >> 8` where the left operand can be negative. WGSL
//!   defines `i32 >>` as sign-replicating; MSL pins the same for signed types
//!   (this is NOT the C++ implementation-defined case). The fire differential
//!   in `super::tests` is what proves it empirically.
//! * **Wrapping `u32` arithmetic.** `fire_hash`'s splitmix multiplies rely on
//!   wraparound; both languages define unsigned overflow as wrapping.
//! * **Flat interpolation of integers.** WGSL needs an explicit
//!   `@interpolate(flat)`; MSL *requires* integers in a `stage_in` struct to be
//!   `[[flat]]`. The provoking-vertex rule never enters into it because every
//!   flat value here is per-INSTANCE and therefore identical across the quad's
//!   vertices.
//! * **Uniform buffer layout.** WGSL uniforms use std140; MSL `constant`
//!   buffers use natural C layout. For all four uniform blocks here every
//!   member is <= 16 bytes and naturally aligned, and both layouts agree
//!   member-for-member — so the existing Rust structs are byte-identical and
//!   UNCHANGED. `BlitUniform` (96 bytes) and `ShimmerU`'s `float4[16]` are the
//!   two worth re-checking if a field is ever added.

/// The cell shader: backgrounds, the LUMEN aurora, the PHOSPHOR rain halo, the
/// EMBERFORGE fire field, glyphs, decorations and sprites. The WGSL twin is
/// `renderer.rs::SHADER`.
pub(crate) const CELL: &str = include_str!("../../shaders/cell.metal");
/// The offscreen -> swapchain blit, including the W1 remainder bands, the
/// bell-flash invert, the drop-target overlay and the M3 EDR encode. WGSL twin:
/// `renderer.rs::BLIT_SHADER`.
pub(crate) const BLIT: &str = include_str!("../../shaders/blit.metal");
/// The swapchain-side aurora crown, HDR and SDR arms. WGSL twin:
/// `renderer.rs::HDR_GLOW_SHADER`.
pub(crate) const HDR_GLOW: &str = include_str!("../../shaders/hdr_glow.metal");
/// The tray/overlay texture blit. WGSL twin: `renderer.rs::TRAY_SHADER`.
pub(crate) const TRAY: &str = include_str!("../../shaders/tray.metal");
/// The 5x5 gaussian bloom tap. WGSL twin: `renderer.rs::BLOOM_SHADER`.
pub(crate) const BLOOM: &str = include_str!("../../shaders/bloom.metal");
/// The EMBERFORGE heat-haze displacement. WGSL twin:
/// `renderer.rs::SHIMMER_SHADER`.
pub(crate) const SHIMMER: &str = include_str!("../../shaders/shimmer.metal");

/// Every (library, source, entry points) triple, so one test can prove the
/// whole shader set compiles and every entry point the renderer will ask for
/// actually resolves.
pub(crate) const LIBRARIES: &[(&str, &str, &[&str])] = &[
    (
        "cell",
        CELL,
        &[
            "vs_bg",
            "fs_bg",
            "fs_glow",
            "vs_rain_glow",
            "fs_rain_glow",
            "fs_rain_glow_over",
            "vs_fire",
            "fs_fire_add",
            "fs_fire_over",
            "vs_glyph",
            "fs_glyph",
            "fs_glyph_color",
            "fs_deco_over",
            "fs_deco_add",
            "fs_sprite_over",
        ],
    ),
    ("blit", BLIT, &["vs_blit", "fs_blit"]),
    (
        "hdr_glow",
        HDR_GLOW,
        &["vs_hdr_glow", "fs_hdr_glow", "fs_sdr_glow"],
    ),
    ("tray", TRAY, &["vs_tray", "fs_tray"]),
    ("bloom", BLOOM, &["vs_fs_bloom", "fs_bloom"]),
    ("shimmer", SHIMMER, &["vs_fs_shimmer", "fs_shimmer"]),
];

/// The verification-only compute kernels, CONCATENATED onto [`CELL`] by the
/// parity test so the math under test is literally the shipped math. Never
/// compiled into a shipping pipeline — see the file's own header.
pub(crate) const PARITY_KERNEL: &str = include_str!("../../shaders/parity_kernel.metal");

/// [`CELL`] plus [`PARITY_KERNEL`], the source the parity test compiles.
pub(crate) fn cell_with_parity_kernels() -> String {
    format!("{CELL}\n{PARITY_KERNEL}")
}
