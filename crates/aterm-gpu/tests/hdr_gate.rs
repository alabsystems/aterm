// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance and on-GPU clamp laws for the HDR glow present gate.
//!
//! ## Boolean policy
//!
//! * Tier-1 (this file): [`aterm_gpu::hdr_swapchain_wants_f16`] (attach) and
//!   [`aterm_gpu::hdr_present_plan`] (present) have finite boolean domains, so
//!   every attach-to-present chain and every raw plan input is enumerated, with
//!   non-vacuity controls. The macOS screen-narrowed attach and live re-pick
//!   imply the base gate for every input (`format_plan`'s unit tests), so this
//!   enumeration bounds them.
//!
//! ## On-GPU clamp laws
//!
//! The `gpu_*` tests drive the real `fs_blit` `hdr` arm and HDR aurora pass
//! through `present_hdr_for_test` (an `Rgba16Float` stand-in, half-float
//! readback) and check the laws `aterm_render::hdr` proves abstractly:
//! (1) grid clamp: the non-additive stream stays within 1.0 and equals the
//! linear decode of the SDR readback within f16 quantization;
//! (2) additive clamp: with the aurora pass on, no channel exceeds the
//! sanitized EDR max, and some channel exceeds 1.0.
//! No GPU or no font: COULD NOT RUN.

use aterm_gpu::{
    hdr_present_plan, hdr_screen_upgrade_decision, hdr_screen_upgrade_wants_f16,
    hdr_swapchain_wants_f16,
};
// The GPU halves render through the wgpu oracle's HDR present seam.
#[cfg(wgpu_arm)]
use {
    crate::common,
    aterm_core::terminal::Terminal,
    aterm_gpu::{GpuRenderer, WindowGpu},
    aterm_render::{GlowQuad, RenderInput, hdr, premul_rgb},
};

/// Iterate every (hdr_glow, supports_f16, glow_nonempty) tuple over {false,true}^3.
fn all_inputs() -> impl Iterator<Item = (bool, bool, bool)> {
    (0u32..8).map(|bits| {
        let b = |i: u32| (bits >> i) & 1u32 == 1;
        (b(0), b(1), b(2))
    })
}

/// Chain the attach decision into the present plan for every input and check
/// the three invariants, plus the raw plan over all surface states
/// (including live-flip mismatches no attach chain reaches).
#[test]
fn hdr_gate_exhaustive_attach_present_chain() {
    let mut boosted = 0usize;
    let mut f16_chosen = 0usize;
    for (hdr_glow, supports_f16, glow) in all_inputs() {
        // Attach: the swapchain format decision (create_window_surface's seam).
        let is_f16 = hdr_swapchain_wants_f16(hdr_glow, supports_f16);
        // Present: the per-frame plan (present_input's seam).
        let plan = hdr_present_plan(hdr_glow, is_f16, glow);

        // F16NeedsSupport: the EDR format is never picked without support.
        assert!(
            !is_f16 || supports_f16,
            "({hdr_glow},{supports_f16},{glow}): f16 without surface support"
        );
        // SdrInvariance: hdr off => nothing HDR anywhere in the chain.
        if !hdr_glow {
            assert!(
                !is_f16 && !plan.blit_linear_encode && !plan.glow_boost_pass,
                "({hdr_glow},{supports_f16},{glow}): SDR invariance violated"
            );
        }
        // BoostNeedsLinearF16: a >1.0 emission only on a linear-decoded f16 target.
        assert!(
            !plan.glow_boost_pass || (plan.blit_linear_encode && is_f16),
            "({hdr_glow},{supports_f16},{glow}): boost without a linear f16 blit"
        );
        // The encode follows the surface.
        assert_eq!(
            plan.blit_linear_encode, is_f16,
            "({hdr_glow},{supports_f16},{glow}): encode must track the actual format"
        );
        boosted += usize::from(plan.glow_boost_pass);
        f16_chosen += usize::from(is_f16);
    }
    // Non-vacuity: the gate is live, so the invariants above are not vacuous.
    assert_eq!(f16_chosen, 2, "f16 is chosen iff hdr_glow AND supports_f16");
    assert_eq!(
        boosted, 1,
        "the aurora pass fires exactly for (on, f16, glow)"
    );

    // Raw plan over every surface state: an 8-bit surface never decodes or
    // boosts, an f16 surface always decodes, and `hdr_glow` off kills the boost
    // even on an f16 surface.
    for (hdr_glow, is_f16, glow) in all_inputs() {
        let plan = hdr_present_plan(hdr_glow, is_f16, glow);
        assert_eq!(plan.blit_linear_encode, is_f16);
        assert_eq!(plan.glow_boost_pass, hdr_glow && is_f16 && glow);
    }
}

/// A tiny glow-bearing frame: an all-background grid with a row of premultiplied
/// additive glow quads (the glow_parity construction), cursor hidden so the
/// pixels are exactly bg + aurora.
#[cfg(wgpu_arm)]
fn glow_input(cpu_cell: (usize, usize), rows: usize, cols: usize) -> RenderInput {
    let (cw, ch) = cpu_cell;
    let mut t = Terminal::new(rows as u16, cols as u16);
    let mut input = t.cell_frame(rows, cols);
    input.cursor_visible = false;
    let base = 0x0050_FA7B; // Dracula green
    for (i, a) in [40u8, 90, 160, 220, 255].iter().enumerate() {
        let col = i + 1;
        input.cursor_glow_add.push(GlowQuad {
            row: 1,
            x: (col * cw) as u16,
            y: ch as u16,
            w: cw as u16,
            h: ch as u16,
            color: premul_rgb(base, *a),
            // Additive light (see `GlowQuad::alpha`).
            alpha: 0,
            color2: premul_rgb(base, *a),
            alpha2: 0,
        });
    }
    input
}

#[cfg(wgpu_arm)]
fn gpu(px: f32) -> GpuRenderer {
    // The EDR gates drive the wgpu present stand-ins (the wgpu offscreen and
    // test-hdr destinations).
    let mut g = common::wgpu_oracle(px, aterm_render::Theme::default());
    // Deterministic expectations: the bloom halo is a separate additive layer
    // over the offscreen, orthogonal to the laws under test.
    g.set_bloom(false);
    g.set_shimmer(false);
    g
}

/// Grid clamp on the real pipeline: with `hdr_glow` on and headroom, the blit
/// (no aurora) stays in [0, 1], and each channel equals
/// `aterm_render::hdr::hdr_grid_encode` of the SDR readback byte within f16
/// quantization.
#[test]
#[cfg(wgpu_arm)]
fn gpu_hdr_blit_grid_clamped_and_linear() {
    let mut gpu = gpu(18.0);
    gpu.set_hdr_glow(true);
    let mut win = WindowGpu::new();
    win.set_edr_max(2.0);
    let (cw, ch) = gpu.cell_size();
    let input = glow_input((cw, ch), 4, 16);

    // The SDR readback is the source of truth.
    let sdr = gpu.render_input(&mut win, &input, None);
    // The EDR present without the aurora pass: the pure grid stream.
    let (hdrpix, w, h) = gpu.present_hdr_for_test(&mut win, &input, false);
    assert_eq!((w as usize, h as usize), (sdr.width, sdr.height));

    let mut max_dev = 0.0f32;
    for (i, &px) in sdr.pixels.iter().enumerate() {
        for (ch_i, shift) in [(0usize, 16u32), (1, 8), (2, 0)] {
            let byte = ((px >> shift) & 0xff) as u8;
            let got = hdrpix[i * 4 + ch_i];
            assert!(
                (0.0..=1.0).contains(&got),
                "pixel {i} ch {ch_i}: grid stream escaped [0,1]: {got}"
            );
            let want = hdr::hdr_grid_encode(byte);
            max_dev = max_dev.max((got - want).abs());
        }
    }
    // f16 quantization near 1.0 is ~4.9e-4; GPU pow adds noise well below 5e-3.
    assert!(
        max_dev <= 5e-3,
        "EDR grid must be the linear decode of the SDR bytes (max dev {max_dev})"
    );
}

/// Reference-white scale on grid and bands: an scRGB present at
/// `sdr_white_scale = 3.0` into a destination 7 px larger than the grid. Content
/// pixels equal `scrgb_present_channel(sdr byte, 3.0)` and band pixels the same
/// encode of the theme background.
#[test]
#[cfg(wgpu_arm)]
fn gpu_hdr_blit_bands_take_the_reference_white_scale() {
    let mut gpu = gpu(18.0);
    gpu.set_hdr_glow(true);
    let mut win = WindowGpu::new();
    win.set_edr_max(2.0);
    win.set_sdr_white_scale(3.0);

    // Bright white text gives the scale something to lift above 1.0.
    let (rows, cols) = (4usize, 16usize);
    let mut t = Terminal::new(rows as u16, cols as u16);
    t.process(b"band \x1b[97mWHITE\x1b[0m fit");
    let mut input = t.cell_frame(rows, cols);
    input.cursor_visible = false;

    let sdr = gpu.render_input(&mut win, &input, None);
    let (fw, fh) = (sdr.width, sdr.height);
    let (hdrpix, w, h) = gpu.present_hdr_sized_for_test(&mut win, &input, false, 7, 7);
    assert_eq!((w as usize, h as usize), (fw + 7, fh + 7));
    let (ox, oy) = (
        aterm_render::band_offset(w as usize, fw),
        aterm_render::band_offset_y(h as usize, fh),
    );
    assert_eq!(
        (ox, oy),
        (3, if cfg!(target_os = "linux") { 0 } else { 3 }),
        "x: a 7px remainder splits 3 leading / 4 trailing; y: platform policy"
    );

    let bg = aterm_render::Theme::default().bg & 0x00ff_ffff;
    let (mut content_px, mut band_px) = (0usize, 0usize);
    let mut max_dev = 0.0f32;
    for y in 0..h as usize {
        for x in 0..w as usize {
            let (sx, sy) = (x as i64 - ox, y as i64 - oy);
            let in_frame = sx >= 0 && sy >= 0 && (sx as usize) < fw && (sy as usize) < fh;
            let src = if in_frame {
                content_px += 1;
                sdr.pixels[sy as usize * fw + sx as usize]
            } else {
                band_px += 1;
                bg
            };
            for (ch_i, shift) in [(0usize, 16u32), (1, 8), (2, 0)] {
                let byte = ((src >> shift) & 0xff) as u8;
                let got = hdrpix[(y * w as usize + x) * 4 + ch_i];
                assert!(
                    (0.0..=3.0).contains(&got),
                    "({x},{y}) ch {ch_i}: escaped [0, scale]: {got}"
                );
                let want = hdr::scrgb_present_channel(byte, 3.0);
                max_dev = max_dev.max((got - want).abs());
            }
        }
    }
    // Non-vacuity: the two arms cover the whole surface.
    assert_eq!(content_px, fw * fh);
    assert_eq!(band_px, w as usize * h as usize - fw * fh);
    // f16 quantization in [2, 4) is ~2e-3 per step; GPU pow noise is well below.
    assert!(
        max_dev <= 8e-3,
        "grid + bands must be the SAME scRGB encode of their bytes (max dev {max_dev})"
    );
    // Non-vacuity: the scale lifts the stream above reference white.
    assert!(
        hdrpix.iter().any(|&v| v > 1.0),
        "a 3.0x present of white text must exceed 1.0 somewhere"
    );
}

/// Additive clamp: with the aurora pass on a 2.0x panel, no channel exceeds
/// the sanitized EDR max and the aurora emits above 1.0. On an SDR panel
/// (`edr_max = 1.0`) the output equals the pass-less present.
#[test]
#[cfg(wgpu_arm)]
fn gpu_hdr_aurora_bounded_by_edr_and_nonvacuous() {
    let mut gpu = gpu(18.0);
    gpu.set_hdr_glow(true);
    let mut win = WindowGpu::new();
    let (cw, ch) = gpu.cell_size();
    let input = glow_input((cw, ch), 4, 16);

    // 2.0x EDR panel: bounded by 2.0, and some pixel must exceed 1.0.
    win.set_edr_max(2.0);
    let edr_bound = hdr::sanitize_edr_max(2.0);
    let (lit, _, _) = gpu.present_hdr_for_test(&mut win, &input, true);
    let mut over_white = 0usize;
    for (i, &v) in lit.iter().enumerate() {
        if i % 4 == 3 {
            continue; // alpha: COLOR write-mask leaves the blit's 1.0
        }
        assert!(
            v >= 0.0 && v <= edr_bound + 1e-3,
            "channel {i} exceeded the panel EDR max: {v} > {edr_bound}"
        );
        if v > 1.0 {
            over_white += 1;
        }
    }
    assert!(
        over_white > 0,
        "NON-VACUITY: the EDR aurora must emit above reference white somewhere"
    );

    // SDR panel: headroom 0 -> the pass adds nothing (bit-identical output).
    win.set_edr_max(1.0);
    let (flat_boost, _, _) = gpu.present_hdr_for_test(&mut win, &input, true);
    let (flat_plain, _, _) = gpu.present_hdr_for_test(&mut win, &input, false);
    assert_eq!(
        flat_boost, flat_plain,
        "zero headroom must make the aurora pass a provable no-op"
    );
    for &v in &flat_boost {
        assert!(v <= 1.0, "SDR-panel present must stay at reference white");
    }
}

/// SDR invariance at present: with `hdr_glow` off the plan keeps the aurora
/// pass off even when asked and glow exists; no channel exceeds 1.0.
#[test]
#[cfg(wgpu_arm)]
fn gpu_hdr_off_never_boosts() {
    let mut gpu = gpu(18.0);
    gpu.set_hdr_glow(false);
    let mut win = WindowGpu::new();
    win.set_edr_max(16.0); // maximum temptation
    let (cw, ch) = gpu.cell_size();
    let input = glow_input((cw, ch), 4, 16);
    let (pix, _, _) = gpu.present_hdr_for_test(&mut win, &input, true);
    for (i, &v) in pix.iter().enumerate() {
        assert!(
            v <= 1.0,
            "channel {i}: hdr_glow off must never emit above reference white ({v})"
        );
    }
}

// ---------------------------------------------------------------------------
// The HDR crown's per-fragment white boost
// ---------------------------------------------------------------------------

/// One rainbow-kitty palette mark as the crown sees it: the premultiplied
/// colour and its composited coverage.
#[cfg(wgpu_arm)]
struct Mark {
    name: &'static str,
    rgb: u32,
    cov: u8,
    /// A white core: the marks the hierarchy ranks hotter than any hue.
    white_core: bool,
}

/// The palette: five white cores and the coloured company they must not drag
/// over reference white with them.
#[cfg(wgpu_arm)]
const PALETTE: &[Mark] = &[
    // The meteor's head and the landing pin.
    Mark {
        name: "meteor nucleus core",
        rgb: 0x00FF_FFFF,
        cov: 118,
        white_core: true,
    },
    Mark {
        name: "meteor white shoulder",
        rgb: 0x00FF_FFFF,
        cov: 118,
        white_core: true,
    },
    Mark {
        name: "landing pin nucleus",
        rgb: 0x00FF_FFFF,
        cov: 118,
        white_core: true,
    },
    // The field m1 core, the brightest small thing that persists.
    Mark {
        name: "field star core m1",
        rgb: 0x00FF_FFFF,
        cov: 143,
        white_core: true,
    },
    // The caret's flare frame, opaque white for one frame.
    Mark {
        name: "caret flare frame",
        rgb: 0x00FF_FFFF,
        cov: 255,
        white_core: true,
    },
    // Coloured marks: spectrum stops (a zero channel, so no white part), the
    // tinted body, the coma's half-white blend, and the two hot-edge inks.
    Mark {
        name: "star bar / m3 body",
        rgb: 0x00FF_0000,
        cov: 98,
        white_core: false,
    },
    Mark {
        name: "transient m2 body",
        rgb: 0x0066_66FF,
        cov: 130,
        white_core: false,
    },
    Mark {
        name: "ring vertex",
        rgb: 0x0000_FF00,
        cov: 130,
        white_core: false,
    },
    Mark {
        name: "fan grain (gold)",
        rgb: 0x00FF_FF00,
        cov: 80,
        white_core: false,
    },
    Mark {
        name: "halo (violet stop)",
        rgb: 0x0094_00D3,
        cov: 61,
        white_core: false,
    },
    Mark {
        name: "meteor coma",
        rgb: 0x00FF_7F7F,
        cov: 40,
        white_core: false,
    },
    Mark {
        name: "hot edge (lifted red)",
        rgb: 0x00FF_5D5D,
        cov: 118,
        white_core: false,
    },
    Mark {
        name: "hot edge (lifted violet)",
        rgb: 0x00D1_64FF,
        cov: 118,
        white_core: false,
    },
    Mark {
        name: "aurora veil",
        rgb: 0x0000_00FF,
        cov: 40,
        white_core: false,
    },
];

/// CPU twin of `HDR_GLOW_SHADER::white_part` and the boost,
/// `1 + 2*smoothstep(0.35, 0.60, min(r, g, b))` capped at 3x. Exactly 1.0 below
/// the knee.
#[cfg(wgpu_arm)]
fn white_boost(premul: [f32; 3]) -> f32 {
    let w = premul[0].min(premul[1]).min(premul[2]);
    let t = ((w - 0.35) / (0.60 - 0.35)).clamp(0.0, 1.0);
    (1.0 + 2.0 * (t * t * (3.0 - 2.0 * t))).min(3.0)
}

/// The premultiplied channels one [`Mark`] hands the crown.
#[cfg(wgpu_arm)]
fn mark_channels(m: &Mark) -> [f32; 3] {
    let p = premul_rgb(m.rgb, m.cov);
    [
        ((p >> 16) & 0xff) as f32 / 255.0,
        ((p >> 8) & 0xff) as f32 / 255.0,
        (p & 0xff) as f32 / 255.0,
    ]
}

/// A frame carrying the whole [`PALETTE`], one cell-sized quad per mark, laid
/// left to right from (row 1, col 1). Returns the frame and each mark's cell.
#[cfg(wgpu_arm)]
fn palette_input(
    cpu_cell: (usize, usize),
    rows: usize,
    cols: usize,
) -> (RenderInput, Vec<(usize, usize)>) {
    let (cw, ch) = cpu_cell;
    let mut t = Terminal::new(rows as u16, cols as u16);
    let mut input = t.cell_frame(rows, cols);
    input.cursor_visible = false;
    let per_row = cols - 2;
    let mut cells = Vec::new();
    for (i, m) in PALETTE.iter().enumerate() {
        let (row, col) = (1 + i / per_row, 1 + i % per_row);
        input.cursor_glow_add.push(GlowQuad {
            row: row as u16,
            x: (col * cw) as u16,
            y: (row * ch) as u16,
            w: cw as u16,
            h: ch as u16,
            color: premul_rgb(m.rgb, m.cov),
            // Additive light (see `GlowQuad::alpha`).
            alpha: 0,
            color2: premul_rgb(m.rgb, m.cov),
            alpha2: 0,
        });
        cells.push((row, col));
    }
    (input, cells)
}

/// The HDR crown boosts each fragment by its white part,
/// `1 + 2*smoothstep(0.35, 0.60, min(r, g, b))` capped at 3x and clamped to the
/// headroom. Measured on the real pipeline as crowned minus pass-less, over the
/// palette:
///
///   * every coloured mark is multiplied by exactly 1.0;
///   * every white core climbs, and is brighter than a coloured mark of the
///     same coverage on the same channel;
///   * the pixels over reference white are exactly the predicted ones, none
///     within 3 % of the boundary.
#[test]
#[cfg(wgpu_arm)]
fn only_the_whitest_cores_exceed_reference_white() {
    let mut gpu = gpu(18.0);
    gpu.set_hdr_glow(true);
    let mut win = WindowGpu::new();
    // A 4.0x panel: headroom 3.0 == the cap, so nothing but opaque white is
    // clamped and every factor in the curve is observable.
    win.set_edr_max(4.0);
    let headroom = hdr::additive_headroom(4.0);
    assert_eq!(headroom, 3.0);
    let (cw, ch) = gpu.cell_size();
    let (rows, cols) = (6usize, 9usize);
    let (input, cells) = palette_input((cw, ch), rows, cols);

    let (plain, w, _h) = gpu.present_hdr_for_test(&mut win, &input, false);
    let (lit, _, _) = gpu.present_hdr_for_test(&mut win, &input, true);

    let mut over_white_cells = 0usize;
    for (m, &(row, col)) in PALETTE.iter().zip(&cells) {
        let (px, py) = (col * cw + cw / 2, row * ch + ch / 2);
        let base = (py * w as usize + px) * 4;
        let c = mark_channels(m);
        let hot = white_boost(c);
        assert_eq!(
            m.white_core,
            hot > 1.0,
            "{}: white part {:.3} -> factor {hot:.3}, which disagrees with the design's rank",
            m.name,
            c[0].min(c[1]).min(c[2])
        );
        let mut any_over = false;
        for (i, &ch_v) in c.iter().enumerate() {
            let lin = hdr::srgb_channel_to_linear(ch_v);
            let want_add = (lin * hdr::HDR_GLOW_BOOST * hot).min(headroom).max(0.0);
            let got_add = lit[base + i] - plain[base + i];
            assert!(
                (got_add - want_add).abs() <= 8e-3,
                "{} ch {i}: the crown added {got_add} where the §L5 curve says \
                 {want_add} (linear {lin:.4} x factor {hot:.3})",
                m.name
            );
            // Predicted composite on top of the measured pass-less present.
            let want_total = plain[base + i] + want_add;
            assert!(
                (want_total - 1.0).abs() > 0.03,
                "{} ch {i}: predicted {want_total} sits on the reference-white \
                 boundary — pick a coverage that makes the comparison mean something",
                m.name
            );
            assert_eq!(
                lit[base + i] > 1.0,
                want_total > 1.0,
                "{} ch {i}: measured {} vs predicted {want_total} across reference white",
                m.name,
                lit[base + i]
            );
            any_over |= lit[base + i] > 1.0;
        }
        over_white_cells += usize::from(any_over);
        println!(
            "{:<26} rgb {:06x} cov {:>3} white {:.3} factor {:.3} add {:.4} lit {:.4}",
            m.name,
            m.rgb,
            m.cov,
            c[0].min(c[1]).min(c[2]),
            hot,
            lit[base] - plain[base],
            lit[base]
        );
    }
    // Non-vacuity: some mark ends up over reference white.
    assert!(
        over_white_cells > 0,
        "no mark reached reference white — the differential is vacuous"
    );

    // At the same coverage and channel, the white core out-emits its coloured
    // twin.
    let white = PALETTE
        .iter()
        .position(|m| m.name == "field star core m1")
        .expect("white core");
    let gold = PALETTE
        .iter()
        .position(|m| m.name == "fan grain (gold)")
        .expect("gold grain");
    let sample = |i: usize, chan: usize| {
        let (row, col) = cells[i];
        let (px, py) = (col * cw + cw / 2, row * ch + ch / 2);
        let b = (py * w as usize + px) * 4 + chan;
        (lit[b], lit[b] - plain[b])
    };
    // Re-emit the gold grain at the white core's coverage so only the white
    // part differs, then compare the red channel both marks carry at 1.0.
    let mut twin = input.clone();
    twin.cursor_glow_add[gold].color = premul_rgb(PALETTE[gold].rgb, PALETTE[white].cov);
    let (twin_plain, _, _) = gpu.present_hdr_for_test(&mut win, &twin, false);
    let (twin_lit, _, _) = gpu.present_hdr_for_test(&mut win, &twin, true);
    let (grow, gcol) = cells[gold];
    let gb = ((grow * ch + ch / 2) * w as usize + (gcol * cw + cw / 2)) * 4;
    let (white_lit, white_add) = sample(white, 0);
    let (gold_lit, gold_add) = (twin_lit[gb], twin_lit[gb] - twin_plain[gb]);
    println!(
        "same coverage {}: white core add {white_add:.4} lit {white_lit:.4} \
         vs gold grain add {gold_add:.4} lit {gold_lit:.4}",
        PALETTE[white].cov
    );
    assert!(
        white_add > gold_add * 1.5,
        "at equal coverage the white core must out-emit the coloured mark by \
         its whiteness factor ({white_add} vs {gold_add})"
    );
    assert!(
        white_lit > 1.0 && gold_lit < 1.0,
        "the white core crosses reference white where its coloured twin does \
         not ({white_lit} vs {gold_lit})"
    );
}

/// The live half of the screen gate: a window that attached 8-bit on a non-EDR
/// screen is re-picked by `GpuRenderer::upgrade_surface_for_screen` (from the
/// frontend's monitor-change hook and throttled headroom re-query). The re-pick
/// decides through `hdr_screen_upgrade_decision`, which reads the screen only
/// past the cheap gates: an f16 surface or a gate-refused window never pays the
/// AppKit read the throttled re-query would make for every 8-bit window. Over
/// every input it equals `hdr_screen_upgrade_wants_f16`, whose implication of
/// the attach gate and need of a resolved screen are `format_plan`'s unit tests.
#[test]
fn metal_monitor_change_upgrade_is_the_attach_gate_on_positive_evidence() {
    for (hdr_glow, supports_f16, _) in all_inputs() {
        for swapchain_is_f16 in [false, true] {
            for potential in [None, Some(1.0), Some(2.0), Some(16.0), Some(f32::NAN)] {
                let mut read = false;
                let decided =
                    hdr_screen_upgrade_decision(hdr_glow, supports_f16, swapchain_is_f16, || {
                        read = true;
                        potential
                    });
                assert_eq!(
                    decided,
                    hdr_screen_upgrade_wants_f16(
                        hdr_glow,
                        supports_f16,
                        swapchain_is_f16,
                        potential
                    ),
                    "({hdr_glow},{supports_f16},{swapchain_is_f16},{potential:?})"
                );
                let cheap_refusal =
                    swapchain_is_f16 || !hdr_swapchain_wants_f16(hdr_glow, supports_f16);
                assert_eq!(
                    read, !cheap_refusal,
                    "({hdr_glow},{supports_f16},{swapchain_is_f16}): the screen is read \
                     exactly when the cheap gates pass"
                );
            }
        }
    }
}
