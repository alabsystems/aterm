// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance + on-GPU clamp laws for the M3 phase-B EDR ("HDR glow")
//! present gate.
//!
//! ## Two-tier proof (the boolean policy)
//!
//! * **Tier-0 (abstract, model-checked by the Trust `ty` compiler)** — the
//!   `HdrPresentGate` derived model (`aterm_spec::derive::hdr_present_gate_model`)
//!   carries `SdrInvariance` / `BoostNeedsLinearF16` / `F16NeedsSupport`.
//!   `cargo test -p aterm-spec`
//!   (`derived_hdr_present_gate_proves_and_catches_hdr_without_optin`) runs the
//!   REAL `ty` binary over the whole bounded state space: it PROVES the
//!   invariants at `Buggy=0` and CATCHES the config-ignoring attach at `Buggy=1`.
//! * **Tier-1 (concrete, this file)** — the SHIPPING policy functions
//!   [`aterm_gpu::hdr_swapchain_wants_f16`] (Attach) and
//!   [`aterm_gpu::hdr_present_plan`] (Present) have a finite boolean domain, so
//!   we don't sample: every 2^3 Attach→Present chain AND every 2^3 raw plan
//!   input (covering the live config-flip states where the surface format no
//!   longer matches the config) is enumerated — a complete proof for the real
//!   code, with non-vacuity controls.
//!
//! ## On-GPU clamp laws (the float pipeline)
//!
//! The `gpu_*` tests drive the REAL `fs_blit` `hdr` arm and the REAL EDR aurora
//! pass through `present_hdr_for_test` (an `Rgba16Float` swapchain stand-in,
//! half-float readback) and assert the two laws `aterm_render::hdr` proves in
//! the abstract (exhaustive sweeps + trust-mc):
//! (1) GRID CLAMP — the non-additive stream never exceeds 1.0, and equals the
//! linear decode of the SDR readback byte-for-byte (within f16 quantization);
//! (2) ADDITIVE CLAMP — with the aurora pass on, no channel exceeds the
//! sanitized EDR max, and (non-vacuity) some channel genuinely exceeds 1.0.
//! Gated: no GPU / no system font -> the test no-ops (returns), like the other
//! differential suites.

use aterm_core::terminal::Terminal;
use aterm_gpu::{
    GpuRenderer, HdrReconfigurePlan, WindowGpu, hdr_live_upgrade_wants_f16, hdr_present_plan,
    hdr_reconfigure_plan, hdr_swapchain_wants_f16,
};
use aterm_render::{GlowQuad, RenderInput, hdr, premul_rgb};
use aterm_spec::derive::hdr_reconfigure_retag_model;

/// Iterate every (hdr_glow, supports_f16, glow_nonempty) tuple over {false,true}^3.
fn all_inputs() -> impl Iterator<Item = (bool, bool, bool)> {
    (0u32..8).map(|bits| {
        let b = |i: u32| (bits >> i) & 1u32 == 1;
        (b(0), b(1), b(2))
    })
}

/// Tier-1, complete: chain the SHIPPING Attach decision into the SHIPPING
/// Present plan for every input combination and check the three model
/// invariants on the outcome — plus the raw plan over ALL surface states
/// (including the live-flip mismatches an Attach chain cannot reach).
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
        // The encode follows the SURFACE, exactly.
        assert_eq!(
            plan.blit_linear_encode, is_f16,
            "({hdr_glow},{supports_f16},{glow}): encode must track the actual format"
        );
        boosted += usize::from(plan.glow_boost_pass);
        f16_chosen += usize::from(is_f16);
    }
    // NON-VACUITY: the invariants above must not hold because the gate is dead.
    assert_eq!(f16_chosen, 2, "f16 is chosen iff hdr_glow AND supports_f16");
    assert_eq!(
        boosted, 1,
        "the aurora pass fires exactly for (on, f16, glow)"
    );

    // The RAW plan over every surface state — covers a swapchain whose format no
    // longer matches what the current config would pick (live flips): an 8-bit
    // surface NEVER decodes/boosts, an f16 surface ALWAYS decodes, and switching
    // hdr_glow off kills the boost even on a still-f16 surface.
    for (hdr_glow, is_f16, glow) in all_inputs() {
        let plan = hdr_present_plan(hdr_glow, is_f16, glow);
        assert_eq!(plan.blit_linear_encode, is_f16);
        assert_eq!(plan.glow_boost_pass, hdr_glow && is_f16 && glow);
    }
}

/// Tier-1 lifecycle conformance for an f16 surface rebuilt by any live
/// reconfiguration or checked after a same-size Windows HDR-state change. Drive
/// the genuine shipping policy, project its result onto the derived model, and
/// require the exact modeled successor for both re-tag outcomes.
#[test]
fn hdr_reconfigure_retag_policy_conforms_and_old_ignore_failure_is_rejected() {
    let model = hdr_reconfigure_retag_model();
    let initial = model.init_state();
    let mut downgrade_fallbacks = 0usize;

    for retagged in [false, true] {
        let action = if retagged {
            "RetagSucceeds"
        } else {
            "RetagFails"
        };
        assert!(
            model.action_enabled(action, &initial),
            "{action} must be enabled for the initial confirmed-HDR surface"
        );
        let successors = model.successors(action, &initial);
        assert_eq!(
            successors.len(),
            1,
            "{action} must have one deterministic modeled successor"
        );
        let modeled = &successors[0];

        let plan = hdr_reconfigure_plan(true, retagged);
        let (is_f16, capture_linear) = match plan {
            HdrReconfigurePlan::KeepHdr => (1, 1),
            HdrReconfigurePlan::FallbackToSdr => {
                downgrade_fallbacks += 1;
                (0, 0)
            }
            HdrReconfigurePlan::KeepSdr => {
                panic!("an existing f16 surface cannot take the KeepSdr arm")
            }
        };
        assert_eq!(modeled["stage"], 2);
        assert_eq!(modeled["retagged"], if retagged { 1 } else { 0 });
        assert_eq!(modeled["is_f16"], is_f16);
        assert_eq!(modeled["capture_linear"], capture_linear);
        for invariant in [
            "FailedRetagFallsBackAtomically",
            "ResolvedF16RequiresSuccessfulRetag",
            "AwaitingUpgradeIsSdr",
            "CaptureMatchesSurfaceEncoding",
            "ValuesBounded",
        ] {
            assert!(
                model.check_invariant(invariant, modeled),
                "{action} shipping projection violated {invariant}: {modeled:?}"
            );
        }
    }
    assert_eq!(
        downgrade_fallbacks, 1,
        "exactly the failed f16 re-tag must select SDR fallback"
    );

    // Symmetric live HDR-on recovery starts from the retained capable SDR
    // surface. After the output probe admits an attempt, configure+tag has the
    // same two shipping policy outcomes as every other f16 recreation.
    let sdr = model
        .successors("EnterSdrFallback", &initial)
        .into_iter()
        .next()
        .expect("the model must expose an eligible SDR fallback");
    assert_eq!(sdr["stage"], 1);
    assert_eq!(sdr["is_f16"], 0);
    assert_eq!(sdr["capture_linear"], 0);
    let mut upgrade_fallbacks = 0usize;
    for retagged in [false, true] {
        let action = if retagged {
            "UpgradeSucceeds"
        } else {
            "UpgradeFails"
        };
        assert!(model.action_enabled(action, &sdr));
        let modeled = model
            .successors(action, &sdr)
            .into_iter()
            .next()
            .expect("upgrade action must have one successor");
        let plan = hdr_reconfigure_plan(true, retagged);
        let (is_f16, capture_linear) = match plan {
            HdrReconfigurePlan::KeepHdr => (1, 1),
            HdrReconfigurePlan::FallbackToSdr => {
                upgrade_fallbacks += 1;
                (0, 0)
            }
            HdrReconfigurePlan::KeepSdr => {
                panic!("an admitted f16 upgrade cannot take the KeepSdr arm")
            }
        };
        assert_eq!(modeled["stage"], 2);
        assert_eq!(modeled["retagged"], if retagged { 1 } else { 0 });
        assert_eq!(modeled["is_f16"], is_f16);
        assert_eq!(modeled["capture_linear"], capture_linear);
        for invariant in [
            "FailedRetagFallsBackAtomically",
            "ResolvedF16RequiresSuccessfulRetag",
            "AwaitingUpgradeIsSdr",
            "CaptureMatchesSurfaceEncoding",
            "ValuesBounded",
        ] {
            assert!(
                model.check_invariant(invariant, &modeled),
                "{action} shipping projection violated {invariant}: {modeled:?}"
            );
        }
    }
    assert_eq!(
        upgrade_fallbacks, 1,
        "a failed upgrade tag must restore exactly one SDR fallback"
    );

    // SDR reconfigures never become HDR merely because a meaningless re-tag
    // boolean is true.
    for retagged in [false, true] {
        assert_eq!(
            hdr_reconfigure_plan(false, retagged),
            HdrReconfigurePlan::KeepSdr
        );
    }

    // NEGATIVE CONTROL: the pre-fix live reconfigure ignored `false` and kept
    // both f16 and linear capture metadata. That state is neither the model
    // successor nor invariant-safe.
    let mut ignored_failure = initial.clone();
    ignored_failure.insert("stage", 2);
    ignored_failure.insert("retagged", 0);
    ignored_failure.insert("is_f16", 1);
    ignored_failure.insert("capture_linear", 1);
    assert!(
        !model
            .successors("RetagFails", &initial)
            .contains(&ignored_failure),
        "old ignore-failure transition must not conform"
    );
    assert!(
        !model.check_invariant("FailedRetagFallsBackAtomically", &ignored_failure),
        "negative control must violate the atomic fallback law"
    );
    assert!(
        !model.check_invariant("ResolvedF16RequiresSuccessfulRetag", &ignored_failure),
        "negative control must expose untagged f16"
    );

    // NEGATIVE CONTROL: an HDR-on upgrade configured f16 but its subsequent tag
    // failed, while the old path left that plausible f16 surface live. Capture
    // stayed honestly SDR, exposing both the format/tag and metadata mismatch.
    let mut failed_upgrade = sdr.clone();
    failed_upgrade.insert("stage", 2);
    failed_upgrade.insert("retagged", 0);
    failed_upgrade.insert("is_f16", 1);
    failed_upgrade.insert("capture_linear", 0);
    assert!(
        !model
            .successors("UpgradeFails", &sdr)
            .contains(&failed_upgrade),
        "failed upgrade must not leave an untagged f16 surface"
    );
    assert!(
        !model.check_invariant("FailedRetagFallsBackAtomically", &failed_upgrade),
        "failed-upgrade negative control must violate atomic SDR fallback"
    );
    assert!(
        !model.check_invariant("CaptureMatchesSurfaceEncoding", &failed_upgrade),
        "failed-upgrade negative control must expose metadata/format mismatch"
    );
}

/// The live-upgrade admission gate retains raw f16 support independently of the
/// attach-time opt-in, then requires the CURRENT opt-in and CURRENT Windows
/// output state. Enumerate the complete boolean domain and pin the hot-reload
/// case that an attach-frozen `hdr_capable` bit lost.
#[test]
fn hdr_live_upgrade_gate_is_complete_and_hot_reloadable() {
    let mut admitted = 0usize;
    for (hdr_glow, supports_f16, output_hdr_enabled) in all_inputs() {
        let got = hdr_live_upgrade_wants_f16(hdr_glow, supports_f16, output_hdr_enabled);
        let expected = hdr_glow && supports_f16 && output_hdr_enabled;
        assert_eq!(
            got, expected,
            "upgrade gate drifted for ({hdr_glow},{supports_f16},{output_hdr_enabled})"
        );
        admitted += usize::from(got);
    }
    assert_eq!(admitted, 1, "only the all-true upgrade tuple is admitted");

    let supports_f16 = true;
    assert!(
        !hdr_live_upgrade_wants_f16(false, supports_f16, true),
        "live opt-out must suppress an SDR upgrade"
    );
    assert!(
        hdr_live_upgrade_wants_f16(true, supports_f16, true),
        "turning the live opt-in on must use retained raw f16 support"
    );

    // NEGATIVE CONTROL: freezing opt-in+support together at attach while opt-in
    // was false permanently rejects the same capable surface after hot reload.
    let old_attach_frozen_capable = hdr_swapchain_wants_f16(false, supports_f16);
    assert!(!old_attach_frozen_capable);
    assert!(
        old_attach_frozen_capable != hdr_live_upgrade_wants_f16(true, supports_f16, true),
        "the test must distinguish the old attach-frozen policy"
    );
}

/// Bind the generic reconfigure policy above to every existing-surface call
/// site. Initial attach configures a fresh local `surface` and has its own
/// immediate tag/fallback gate; once a `GpuSurface` is live, direct
/// `surf.surface.configure` is permitted only inside the shared transaction.
/// Present also performs exactly one bounded reconciliation for same-size
/// Windows HDR toggles in either direction, which need not produce a surface
/// lifecycle event.
///
/// This intentionally scans source rather than mocking wgpu: the regression was
/// a call-site omission, and neither CI nor non-Windows unit tests can force the
/// DX12 swapchain recreation that clears the colour-space tag.
#[test]
fn every_live_surface_reconfigure_routes_through_hdr_recovery() {
    let source = include_str!("../src/renderer.rs");

    let resize = source
        .split_once("    pub fn resize_surface(")
        .expect("resize_surface must remain present")
        .1
        .split_once("    fn configure_surface_retagging_scrgb(")
        .expect("shared reconfigure helper must follow resize_surface")
        .0;
    assert_eq!(
        resize
            .matches("self.configure_surface_retagging_scrgb(")
            .count(),
        1,
        "resize must route exactly once through the shared HDR recovery"
    );
    assert!(
        !resize.contains("surf.surface.configure("),
        "resize must not bypass the shared HDR recovery"
    );

    let helper = source
        .split_once("    fn configure_surface_retagging_scrgb(")
        .expect("shared reconfigure helper must remain present")
        .1
        .split_once("    fn tag_swapchain_scrgb(")
        .expect("platform tag helper must follow shared reconfigure helper")
        .0;
    assert_eq!(
        helper.matches("surf.surface.configure(").count(),
        2,
        "shared transaction must configure once, plus exactly one SDR fallback"
    );
    assert!(
        helper.contains("hdr_reconfigure_plan(was_hdr, scrgb_retagged)"),
        "shared transaction must drive the model-bound shipping policy"
    );
    assert!(
        helper.contains("win.apply_hdr_reconfigure_plan(plan)"),
        "shared transaction must reconcile capture/HDR metadata"
    );
    assert_eq!(
        helper
            .matches("self.finish_surface_color_space_recovery(")
            .count(),
        2,
        "post-configure and same-size live validation must share one recovery decision"
    );

    let reconcile = source
        .split_once("    fn reconcile_live_hdr_state_if_due(")
        .expect("live HDR-state reconciliation must remain present")
        .1
        .split_once("    /// Whether this surface's containing Windows output")
        .expect("side-effect-free output probe must follow reconciliation")
        .0;
    assert!(
        reconcile.contains("self.hdr_glow && surf.supports_f16"),
        "SDR polling must follow the current opt-in plus retained raw capability"
    );
    assert!(
        reconcile.contains("hdr_live_upgrade_wants_f16("),
        "the upgrade attempt must pass the exhaustive shipping gate"
    );
    let probe = reconcile
        .find("Self::surface_output_hdr_enabled(&surf.surface)")
        .expect("SDR upgrade must probe output HDR state");
    let select_f16 = reconcile
        .find("surf.config.format = wgpu::TextureFormat::Rgba16Float")
        .expect("admitted upgrade must select f16");
    let configure = reconcile
        .find("self.configure_surface_retagging_scrgb(")
        .expect("admitted upgrade must use shared configure recovery");
    assert!(
        probe < select_f16 && select_f16 < configure,
        "probe must precede f16 selection, which must precede shared configure+tag"
    );

    let present = source
        .split_once("    fn present_input_with_crop(")
        .expect("present_input_with_crop must remain present")
        .1
        .split_once("    fn present_to_view(")
        .expect("present_to_view must follow present_input_with_crop")
        .0;
    assert_eq!(
        present
            .matches("self.configure_surface_retagging_scrgb(")
            .count(),
        2,
        "live alpha changes and Outdated/Lost recovery must share HDR recovery"
    );
    assert_eq!(
        present
            .matches("self.reconcile_live_hdr_state_if_due(win, surf);")
            .count(),
        1,
        "present must reconcile same-size system HDR changes before acquisition"
    );
    assert!(
        !present.contains("surf.surface.configure("),
        "present recovery paths must not configure the surface directly"
    );

    let runtime = source
        .split_once("#[cfg(test)]\nmod tests {")
        .map_or(source, |(runtime, _)| runtime);
    assert_eq!(
        runtime.matches("surf.surface.configure(").count(),
        2,
        "no live-surface configure may escape the shared transaction"
    );
}

/// A tiny glow-bearing frame: an all-background grid with a row of
/// premultiplied LUMEN quads (the glow_parity construction), cursor hidden so
/// the pixels are exactly bg + aurora.
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
            // ADDITIVE light (see `GlowQuad::alpha`).
            alpha: 0,
        });
    }
    input
}

fn gpu(px: f32) -> Option<GpuRenderer> {
    match GpuRenderer::new(px, aterm_render::Theme::default()) {
        Ok(mut g) => {
            // THE FLIP: the EDR gates drive the wgpu present stand-ins (the
            // wgpu offscreen + test-hdr destinations) — the WGPU ORACLE arm,
            // asked for by name post-flip.
            #[cfg(target_os = "macos")]
            g.disarm_metal_for_oracle();
            // Deterministic expectations: the bloom halo is a separate additive
            // layer over the offscreen, orthogonal to the laws under test.
            g.set_bloom(false);
            g.set_shimmer(false);
            Some(g)
        }
        Err(e) => {
            eprintln!("SKIP: no GPU/font available: {e}");
            None
        }
    }
}

/// GRID CLAMP LAW on the REAL pipeline: with `hdr_glow` on and real headroom,
/// the blit stream (no aurora pass) never leaves [0, 1] — and every channel
/// equals `aterm_render::hdr::hdr_grid_encode` of the SDR readback byte within
/// f16 quantization, so the EDR grid is exactly the linear reading of the SDR
/// frame in the app-owned EDR destination encoding at reference white. The
/// compositor transform is unobserved.
#[test]
fn gpu_hdr_blit_grid_clamped_and_linear() {
    let Some(mut gpu) = gpu(18.0) else { return };
    gpu.set_hdr_glow(true);
    let mut win = WindowGpu::new();
    win.set_edr_max(2.0);
    let (cw, ch) = gpu.cell_size();
    let input = glow_input((cw, ch), 4, 16);

    // The SDR source of truth (this also proves the readback path is UNTOUCHED
    // by the HDR config: same bytes as every parity suite pins).
    let sdr = gpu.render_input(&mut win, &input, None);
    // The EDR present WITHOUT the aurora pass: pure grid stream.
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

/// REFERENCE-WHITE SCALE on the REAL pipeline, grid AND bands: a Windows-style
/// scRGB present (`sdr_white_scale = 3.0`, a 240-nit SDR-white desktop) into a
/// destination 7 px larger than grid fit. Every content pixel equals
/// `scrgb_present_channel(sdr byte, 3.0)` and every remainder-band pixel equals
/// the SAME encode of the theme background — the bands are the same sheet as
/// the grid. Measured on glass before the fix: the band arm skipped the scale,
/// so the strips composed 1/3 as bright as the grid beside them.
#[test]
fn gpu_hdr_blit_bands_take_the_reference_white_scale() {
    let Some(mut gpu) = gpu(18.0) else { return };
    gpu.set_hdr_glow(true);
    let mut win = WindowGpu::new();
    win.set_edr_max(2.0);
    win.set_sdr_white_scale(3.0);

    // Real content (bright white text) so the scale has something above 1.0 to
    // lift — an all-background frame would make the non-vacuity check moot.
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
    // NON-VACUITY: both arms genuinely exercised, over the whole surface.
    assert_eq!(content_px, fw * fh);
    assert_eq!(band_px, w as usize * h as usize - fw * fh);
    // f16 quantization in [2, 4) is ~2e-3 per step; GPU pow noise is well below.
    assert!(
        max_dev <= 8e-3,
        "grid + bands must be the SAME scRGB encode of their bytes (max dev {max_dev})"
    );
    // NON-VACUITY: the scale genuinely lifts the stream above reference white.
    assert!(
        hdrpix.iter().any(|&v| v > 1.0),
        "a 3.0x present of white text must exceed 1.0 somewhere"
    );
}

/// ADDITIVE CLAMP LAW on the REAL pipeline: with the aurora pass on and a
/// 2.0x panel, no channel exceeds the sanitized EDR max — and (non-vacuity)
/// the aurora genuinely emits ABOVE reference white, the whole point of M3.
/// On an SDR panel (edr_max = 1.0, headroom 0) the pass provably adds nothing:
/// output equals the pass-less present exactly.
#[test]
fn gpu_hdr_aurora_bounded_by_edr_and_nonvacuous() {
    let Some(mut gpu) = gpu(18.0) else { return };
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

/// SDR INVARIANCE at the present seam, on the REAL pipeline: with `hdr_glow`
/// OFF the plan keeps the aurora pass off even when the caller asks for it and
/// glow quads exist — no channel ever exceeds 1.0. (The attach seam's half —
/// an SDR window never gets an f16 swapchain at all — is the exhaustive test
/// above plus the readback byte-identity every parity suite pins.)
#[test]
fn gpu_hdr_off_never_boosts() {
    let Some(mut gpu) = gpu(18.0) else { return };
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

/// SOURCE-SCAN CLOSURE (the scRGB re-tag): every live-surface reconfigure must
/// go through `Renderer::configure_surface_retagging_scrgb`.
///
/// DX12 rebuilds the swapchain on EVERY `Surface::configure`, not just a resize,
/// and each rebuild reverts to the DXGI gamma-2.2 default colour space. A bare
/// `surf.surface.configure(..)` at any call site therefore silently drops the
/// scRGB tag and washes out an EDR (f16) present, with >1.0 clipped by DWM.
///
/// This defect was live for the composite-alpha / copyable-usage reconcile:
/// `swapchain_usage_for(surf.copyable, win.video.is_some())` flips when a
/// recording arms, so starting a `video` capture on an EDR window reconfigured
/// the swapchain and lost the tag mid-session. The domain is not a value space
/// we can enumerate, so the closure is pinned structurally here — this is a
/// Windows/DX12 path, so it cannot be exercised on a macOS or Linux CI runner.
#[test]
fn every_surface_reconfigure_retags_scrgb() {
    const RENDERER: &str = include_str!("../src/renderer.rs");

    let bare = RENDERER
        .match_indices("surf.surface.configure(")
        .filter(|(i, _)| {
            // The one legitimate occurrence is the helper's own body; identify it
            // by the retag that must immediately follow within the same function.
            let tail = &RENDERER[*i..];
            let window = &tail[..tail.len().min(400)];
            !window.contains("tag_swapchain_scrgb")
        })
        .count();

    assert_eq!(
        bare, 0,
        "found {bare} bare `surf.surface.configure(..)` call(s) in renderer.rs that do not \
         re-tag scRGB; route them through `configure_surface_retagging_scrgb` instead"
    );

    assert!(
        RENDERER.contains("fn configure_surface_retagging_scrgb"),
        "the retag helper must exist for this closure to mean anything"
    );
}

// ---------------------------------------------------------------------------
// WHITE IS HOT — the EDR crown's per-fragment boost (design §L5)
// ---------------------------------------------------------------------------

/// One mark of the rainbow-kitty palette as it reaches the crown: the colour
/// the effect emits and the composited coverage §3.2 pins for it. The stream
/// carries PREMULTIPLIED colour, so this is exactly what `fs_hdr_glow` sees.
struct Mark {
    name: &'static str,
    rgb: u32,
    cov: u8,
    /// A white CORE — the marks §3.2 ranks 1, 2, 4 (m1) and 5, the ones the
    /// hierarchy says are hotter than any hue.
    white_core: bool,
}

/// The palette, straight off the design: five white cores and the coloured
/// company they must not drag over reference white with them.
const PALETTE: &[Mark] = &[
    // §3.2 rank 1 / 5 / 2 — the meteor's head and the landing pin.
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
    // §3.2 rank 4 — the field m1 core, the brightest small thing that persists.
    Mark {
        name: "field star core m1",
        rgb: 0x00FF_FFFF,
        cov: 143,
        white_core: true,
    },
    // §3.2 rank 2 — the caret's flare frame, opaque white for one frame.
    Mark {
        name: "caret flare frame",
        rgb: 0x00FF_FFFF,
        cov: 255,
        white_core: true,
    },
    // Everything coloured: spectrum stops (a zero channel, so no white part at
    // any coverage), the m2's 60 %-tint body, the coma's half-white blend, and
    // the two hot-edge inks §4.1 lifted furthest toward white.
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

/// The CPU twin of `HDR_GLOW_SHADER::white_part` + the §L5 boost curve —
/// `1 + 2*smoothstep(0.35, 0.60, min(r, g, b))`, capped at 3x linear. Below
/// the knee this is EXACTLY 1.0, and `x * 1.0` is exact in IEEE-754, so a mark
/// with no white part emits the very bits it emitted before the crown learned
/// about white.
fn white_boost(premul: [f32; 3]) -> f32 {
    let w = premul[0].min(premul[1]).min(premul[2]);
    let t = ((w - 0.35) / (0.60 - 0.35)).clamp(0.0, 1.0);
    (1.0 + 2.0 * (t * t * (3.0 - 2.0 * t))).min(3.0)
}

/// The premultiplied channels one [`Mark`] hands the crown.
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
            // ADDITIVE light (see `GlowQuad::alpha`).
            alpha: 0,
        });
        cells.push((row, col));
    }
    (input, cells)
}

/// WHITE IS HOT (§L5). The EDR crown prices every fragment by the WHITE PART of
/// its own colour — `1 + 2*smoothstep(0.35, 0.60, min(r, g, b))`, capped at 3x
/// linear and clamped, as ever, to the panel's real headroom. Measured on the
/// REAL pipeline as the difference between the pass-less and the crowned
/// present, mark by mark, over the whole rainbow-kitty palette:
///
///   * every coloured mark — the spectrum stops of the star bars, the ring and
///     the fan grains, the m2's 60 %-tint body, the coma, and BOTH of §4.1's
///     hot-edge inks at the transient cap — is multiplied by EXACTLY 1.0, i.e.
///     emits what it emitted before this law existed;
///   * every white core climbs, and a white core is strictly brighter on glass
///     than a coloured mark of the SAME coverage on the same channel — which is
///     the whole sentence "white is hot", and which was an EQUALITY before;
///   * the pixels that end up over reference white are exactly the ones the
///     twin predicts, with no mark left within 3 % of the boundary to make the
///     comparison a coin flip.
#[test]
fn only_the_whitest_cores_exceed_reference_white() {
    let Some(mut gpu) = gpu(18.0) else { return };
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
            // The predicted composite, taken off the measured pass-less present
            // so the SDR grid underneath is the pipeline's own answer.
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
    // NON-VACUITY: the law has both kinds of mark to separate, and something
    // really does end up over reference white.
    assert!(
        over_white_cells > 0,
        "no mark reached reference white — the differential is vacuous"
    );

    // "WHITE IS HOT", stated as the comparison it names: at the SAME coverage
    // and on the SAME channel, the white core out-emits its coloured twin.
    // Before the per-fragment boost these two were EQUAL.
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
    // part differs, then compare the RED channel both marks carry at 1.0.
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

/// AN SDR PANEL IS BYTE-IDENTICAL (§L5). The crown's new appetite is spent
/// strictly inside the headroom the panel actually reports: at `edr_max = 1.0`
/// the pass is skipped whole and the present is bit-for-bit the pass-less one,
/// over the ENTIRE palette — including the white cores that would triple on an
/// EDR panel. (The SDR *crown* on a non-f16 swapchain is `fs_sdr_glow`, a
/// separate fragment this law never touched.)
#[test]
fn an_sdr_panel_is_byte_identical() {
    let Some(mut gpu) = gpu(18.0) else { return };
    gpu.set_hdr_glow(true);
    let mut win = WindowGpu::new();
    let (cw, ch) = gpu.cell_size();
    let (input, cells) = palette_input((cw, ch), 6, 9);

    win.set_edr_max(1.0);
    assert_eq!(hdr::additive_headroom(1.0), 0.0);
    let (boosted, w, _) = gpu.present_hdr_for_test(&mut win, &input, true);
    let (plain, _, _) = gpu.present_hdr_for_test(&mut win, &input, false);
    assert_eq!(
        boosted, plain,
        "zero headroom must leave the whitened crown a provable no-op"
    );
    for &v in &boosted {
        assert!(v <= 1.0, "an SDR panel must stay at reference white ({v})");
    }

    // NON-VACUITY: the very same frame on a panel WITH headroom does boost —
    // and boosts only the white part. Without this the equality above would
    // hold just as well for a crown that had been deleted.
    win.set_edr_max(4.0);
    let (edr_plain, _, _) = gpu.present_hdr_for_test(&mut win, &input, false);
    let (edr_lit, _, _) = gpu.present_hdr_for_test(&mut win, &input, true);
    let factor = |i: usize| {
        let (row, col) = cells[i];
        let b = ((row * ch + ch / 2) * w as usize + (col * cw + cw / 2)) * 4;
        let c = mark_channels(&PALETTE[i]);
        let lin = hdr::srgb_channel_to_linear(c[0]);
        (edr_lit[b] - edr_plain[b]) / lin
    };
    let core = PALETTE
        .iter()
        .position(|m| m.name == "field star core m1")
        .expect("core");
    let bar = PALETTE
        .iter()
        .position(|m| m.name == "star bar / m3 body")
        .expect("bar");
    println!(
        "edr 4.0x: white core factor {:.3}, coloured bar factor {:.3}",
        factor(core),
        factor(bar)
    );
    assert!(
        factor(core) > 2.0,
        "the m1 core must climb on a panel with headroom (factor {})",
        factor(core)
    );
    assert!(
        (factor(bar) - 1.0).abs() <= 1e-2,
        "a coloured mark must stay at reference emission (factor {})",
        factor(bar)
    );
}
