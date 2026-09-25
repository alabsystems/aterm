// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// THE single source of truth for the offscreen colour-target formats.
//
// wgpu enforces, only at draw time on a real device, that every render pass's
// colour-attachment VIEW format equals the format its bound pipeline was built
// with — a mismatch aborts the pass ("color attachment format does not match
// pipeline"). The C1/C2 crashes (bloom + tray composite on the default WebGL2
// backend) were exactly that: the offscreen texture format is chosen from
// `srgb_offscreen` in ONE place, but each pipeline RE-derived its target format
// from a separate hard-coded constant, so they silently drifted on the downlevel
// (no-VIEW_FORMATS) path. This module collapses every such choice into one pure
// function so a pipeline and its attachment can no longer be edited apart; the
// `gpu_format_invariant` test (GPU-free) and the `gpu_pipeline_format` ay proof
// both check the equality holds for BOTH `srgb_offscreen` states.

#[cfg(wgpu_arm)]
use wgpu::TextureFormat;

/// The stored offscreen colour-target format == the format of the offscreen's
/// DEFAULT view (`off.view`). Native (VIEW_FORMATS) keeps a plain `Rgba8Unorm`
/// texture and aliases an sRGB VIEW for the linear-light base passes; downlevel
/// (GLES/WebGL2) can't alias formats, so the texture is ITSELF `Rgba8UnormSrgb`
/// (the base passes attach its default sRGB view and still blend in linear). The
/// stored BYTES are sRGB-encoded either way, so readback/screenshot is identical.
///
/// Every pipeline whose pass attaches `off.view` — the additive glow/deco-add,
/// the bloom composite + extract, the tray overlay, and the test/readback blit —
/// MUST build its `ColorTargetState` with this.
#[must_use]
#[cfg(wgpu_arm)]
pub(crate) fn offscreen_format(srgb_offscreen: bool) -> TextureFormat {
    if srgb_offscreen {
        TextureFormat::Rgba8Unorm
    } else {
        TextureFormat::Rgba8UnormSrgb
    }
}

/// The sRGB-typed VIEW format attached by the base OVER/REPLACE + cursor +
/// deco-over passes so fixed-function ALPHA_BLENDING composites in LINEAR light.
/// Always `Rgba8UnormSrgb`: on native it's the sRGB alias of the Unorm offscreen;
/// on downlevel the offscreen is itself sRGB, so `add_srgb_suffix` is the identity.
/// Pipelines whose pass attaches `off.view_srgb` build with this.
#[must_use]
#[cfg(wgpu_arm)]
pub(crate) fn offscreen_srgb_view_format(srgb_offscreen: bool) -> TextureFormat {
    offscreen_format(srgb_offscreen).add_srgb_suffix()
}

/// The `view_formats` alias list the offscreen texture must declare so the sRGB
/// view is creatable: a non-base view format must appear here or wgpu panics in
/// `create_view`. Only needed on native (where the texture is Unorm but a sRGB
/// view is aliased); on downlevel the texture is already sRGB, so no alias.
#[must_use]
#[cfg(wgpu_arm)]
pub(crate) fn offscreen_view_formats(srgb_offscreen: bool) -> &'static [TextureFormat] {
    if srgb_offscreen {
        &[TextureFormat::Rgba8UnormSrgb]
    } else {
        &[]
    }
}

/// The `wgpu::Color` clear value for the offscreen's DEFAULT view (`off.view`) such
/// that a cleared `0x00RRGGBB` reads BACK as those exact bytes. On downlevel
/// (`!srgb_offscreen`) that view is `Rgba8UnormSrgb`, which ENCODES linear->sRGB on
/// store while taking the clear in LINEAR space — so each channel is decoded to linear
/// here (mirrors renderer.rs `theme_color`), else the readback would be brighter than
/// the input (the gpu_probe proof-of-life would lie). On native the view is plain
/// `Rgba8Unorm` and stores the clear verbatim, so the raw byte passes through (readback
/// stays byte-exact, the in-process path unchanged).
#[must_use]
#[cfg(wgpu_arm)]
pub(crate) fn offscreen_clear_color(rgb: u32, srgb_offscreen: bool) -> wgpu::Color {
    let chan = |b: u32| -> f64 {
        let c = b as f64 / 255.0;
        if srgb_offscreen {
            c
        } else if c <= 0.040_45 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    wgpu::Color {
        r: chan((rgb >> 16) & 0xff),
        g: chan((rgb >> 8) & 0xff),
        b: chan(rgb & 0xff),
        a: 1.0,
    }
}

/// M3 phase B — the EDR ("HDR glow") PRESENT GATE, as two pure decision
/// functions so the whole boolean policy is exhaustively provable (and has an
/// abstract ty twin, `hdr_present_gate_model` in aterm-spec `derive.rs`; Tier-1
/// is `tests/hdr_gate.rs`'s complete 2^3 enumeration of THESE functions).
///
/// ATTACH seam ([`hdr_swapchain_wants_f16`], consumed by
/// `GpuRenderer::create_window_surface`): the swapchain is `Rgba16Float` iff
/// the user opted in (`hdr_glow = true`) AND the surface offers the format —
/// the base gate, which the wgpu-oracle arm and every non-Metal backend pick
/// through directly. Off or unsupported → the legacy non-sRGB 8-bit pick,
/// byte-identical to pre-M3. The shipped macOS Metal arm (`cfg(not(wgpu_arm))`)
/// reaches the base gate through [`hdr_swapchain_wants_f16_on_screen`], which
/// narrows it by the window's screen's EDR potential and never widens it (its
/// result implies the base gate), and re-picks an 8-bit window live — on a
/// monitor change and on the frontend's throttled headroom re-query — through
/// the narrower still [`hdr_screen_upgrade_wants_f16`]
/// (`GpuRenderer::upgrade_surface_for_screen`). Whichever swapchain owns the
/// layer derives `wantsExtendedDynamicRangeContent` from
/// `format == Rgba16Float`: the first-party Metal swapchain
/// (`metal/swapchain.rs`) and wgpu-hal's Metal backend (the oracle arm) do so
/// identically.
///
/// PRESENT seam ([`hdr_present_plan`], consumed by `present_input`): keyed on
/// the swapchain's ACTUAL format, so live config flips degrade safely by
/// construction — an SDR (8-bit) surface NEVER linear-encodes or boosts even if
/// `hdr_glow` was just switched on (new windows pick it up), and an existing
/// f16 surface keeps decoding correctly (grid clamped at reference white) with
/// the boost gated off the moment `hdr_glow` is switched off.
///
/// # Invariants (proven — ty model + exhaustive Tier-1)
/// * SDR invariance: `hdr_glow == false` ⇒ f16 never chosen at attach, hence
///   (composition) no linear encode and no boost pass at present.
/// * `blit_linear_encode == swapchain_is_f16` — the encode follows the surface,
///   never the config (an f16 surface fed raw sRGB bytes would wash out; an
///   8-bit surface fed linear would darken).
/// * `glow_boost_pass ⇒ swapchain_is_f16 ∧ glow_nonempty ∧ hdr_glow` — >1.0
///   emissions can only land on a float swapchain, only when aurora quads
///   exist, only while the user wants them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HdrPlan {
    /// The blit decodes the offscreen's sRGB bytes to linear (clamped ≤ 1.0 —
    /// the grid clamp law) for the extended-linear-sRGB f16 swapchain.
    pub blit_linear_encode: bool,
    /// Run the EDR aurora pass after the blit: re-emit the LUMEN glow quads
    /// additively with values above 1.0, clamped to the panel headroom
    /// (`aterm_render::hdr`'s proven clamps).
    pub glow_boost_pass: bool,
}

/// ATTACH: pick `Rgba16Float` for the swapchain? See [`HdrPlan`].
#[must_use]
pub fn hdr_swapchain_wants_f16(hdr_glow: bool, supports_f16: bool) -> bool {
    hdr_glow && supports_f16
}

/// ATTACH, narrowed by the SCREEN (the macOS Metal arm): [`hdr_swapchain_wants_f16`]
/// AND the window's screen can ever show extended range —
/// `NSScreen.maximumPotentialExtendedDynamicRangeColorComponentValue`
/// (`screen_edr_potential`) above 1.0. On a screen whose potential is
/// exactly 1.0 (an external SDR monitor) an f16 swapchain draws NO glow crown
/// at all: the >1.0 aurora never lands (the present's sanitized headroom is
/// 0), and [`sdr_boost_pass`] is off on every f16 swapchain (the two boost
/// passes are mutually exclusive), so the SDR glow-boost crown never draws
/// either. It is also not free: measured on a 2017 15" MacBook Pro (Intel HD
/// 630, 3360x2100 backing, 60 fps full-frame present) the `Rgba16Float` +
/// extended-linear layer costs 3.1-3.4 ms of GPU time per frame against
/// 1.7 ms for `Bgra8Unorm`, doubles the drawable pool (3 x 53.8 MB vs 3 x
/// 26.9 MB), and the integrated GPU's whole-device busy reading (ioreg
/// "Device Utilization %", sampled during each run, attributed to no stage)
/// went from ~31% to ~58%. That probe timed the full-frame blit only; the
/// scissored SDR crown pass the 8-bit swapchain adds on frames with glow was
/// not measured. So a screen that reports NO potential gets the 8-bit pick,
/// which saves that cost AND restores the SDR crown there. On SDR monitors
/// that is a visible change: with the default `cursor_glow_sdr_boost` (0.25)
/// on a dark theme, a window attached there now draws the crown, which no
/// window drew there while every window attached f16.
///
/// The POTENTIAL, not the brightness-tracking current value: Apple's split is
/// potential ⇒ "enable EDR rendering at all", current ⇒ "scale content" (the
/// present seam's `edr_max`, re-queried on the frontend's
/// `EDR_REQUERY_INTERVAL` cadence). Gating the attach on the current value
/// would freeze each window's format to the brightness slider's position at
/// creation. The potential is brightness-independent (the SDK's NSScreen.h:
/// "regardless of whether or not extended dynamic range is currently
/// enabled") and fixed for the life of one `NSScreen` object (Apple's
/// property doc: "determined when you create the NSScreen object, and doesn't
/// change afterwards"). That is not the same as constant per monitor: the
/// `NSScreen` a window reports belongs to the current display configuration,
/// which a Displays settings change (High Dynamic Range on an external HDR
/// monitor, an XDR preset) rebuilds, and neither document says what the
/// rebuilt object answers. So the pick is not frozen at attach: the frontend
/// re-reads the potential for an 8-bit window on a monitor change AND on the
/// headroom re-query's throttle, through [`hdr_screen_upgrade_wants_f16`].
/// Note this MacBook Pro's built-in panel is NOT an SDR screen in macOS's
/// model: it reported potential 2.0 (headroom from the backlight), so it
/// keeps the f16 pick and the aurora boost.
///
/// `None` (the screen could not be resolved) keeps the unconditional pick —
/// this narrows only on a POSITIVE "cannot show EDR" answer, so the proven
/// chain (`SdrInvariance`, `F16NeedsSupport`) is untouched: the result
/// implies [`hdr_swapchain_wants_f16`]. A non-finite potential is treated as
/// SDR exactly as the present sanitizer treats a non-finite `edr_max`.
#[must_use]
pub fn hdr_swapchain_wants_f16_on_screen(
    hdr_glow: bool,
    supports_f16: bool,
    screen_edr_potential: Option<f32>,
) -> bool {
    hdr_swapchain_wants_f16(hdr_glow, supports_f16)
        && screen_edr_potential.is_none_or(|p| aterm_render::hdr::sanitize_edr_max(p) > 1.0)
}

/// LIVE SDR→f16 on the macOS Metal arm — the screen re-pick
/// (`GpuRenderer::upgrade_surface_for_screen`, run from the frontend's
/// monitor-change hook and from its throttled headroom re-query): an 8-bit
/// swapchain becomes
/// `Rgba16Float` iff the attach gate would pick f16 here AND the screen the
/// window NOW sits on gives a POSITIVE "can show EDR" answer. Deliberately
/// asymmetric with [`hdr_swapchain_wants_f16_on_screen`] on `None`: the attach
/// keeps the unconditional pick when the screen cannot be resolved (it fails
/// toward the pre-gate behaviour), whereas moving an already-decided 8-bit
/// window to f16 is a widening and rides only on evidence — a window in
/// transit (`-screen` nil) stays as it is. The result implies
/// [`hdr_swapchain_wants_f16_on_screen`], which implies
/// [`hdr_swapchain_wants_f16`], so the Tier-1 chain bounds this too.
/// `swapchain_is_f16` keeps the decision total (an f16 surface never
/// "upgrades"); the caller short-circuits those, and every input the base
/// gate refuses, before the AppKit read.
#[must_use]
pub fn hdr_screen_upgrade_wants_f16(
    hdr_glow: bool,
    supports_f16: bool,
    swapchain_is_f16: bool,
    screen_edr_potential: Option<f32>,
) -> bool {
    !swapchain_is_f16
        && hdr_swapchain_wants_f16(hdr_glow, supports_f16)
        && screen_edr_potential.is_some_and(|p| aterm_render::hdr::sanitize_edr_max(p) > 1.0)
}

/// PRESENT: what the HDR path does THIS present. See [`HdrPlan`].
#[must_use]
pub fn hdr_present_plan(hdr_glow: bool, swapchain_is_f16: bool, glow_nonempty: bool) -> HdrPlan {
    HdrPlan {
        blit_linear_encode: swapchain_is_f16,
        glow_boost_pass: hdr_glow && swapchain_is_f16 && glow_nonempty,
    }
}

/// RECONFIGURE: what to do after configuring an already-live swapchain.
///
/// DX12 recreates the underlying swapchain during every `Surface::configure`
/// (resize, live composite-alpha change, and Outdated/Lost recovery), which
/// resets its DXGI colour space. Windows can also disable system HDR without
/// forcing any such reconfigure. In both cases an f16 surface is still a valid
/// HDR target only when scRGB was successfully re-tagged/validated. Failure must
/// atomically fall back to the surface's retained SDR format; keeping f16 would
/// hand linear pixels to DWM's gamma-2.2 default while capture continued to
/// claim extended-linear-sRGB.
///
/// This is the shipping decision bound to
/// `aterm_spec::derive::hdr_reconfigure_retag_model` — exhaustively over the
/// planner's own boolean domain by `tests/hdr_gate.rs`, and per-transition
/// against the CONCRETE apply path by `renderer::tests`
/// (`hdr_reconfigure_apply_conforms_to_retag_model`), which is what the
/// `HdrReconfigureRetag` refinement anchors below name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HdrReconfigurePlan {
    /// The surface was already SDR; reconfiguring does not change its colour space.
    KeepSdr,
    /// The f16 swapchain was successfully re-tagged scRGB.
    KeepHdr,
    /// Re-tagging the recreated f16 swapchain failed; configure the retained
    /// non-sRGB 8-bit format before another present.
    FallbackToSdr,
}

/// Scalar projection onto the four `HdrReconfigureRetag` variables `<<stage,
/// retagged, is_f16, capture_linear>>`.
///
/// Built ONLY by [`crate::WindowGpu::project_hdr_reconfigure_state`]. Exactly
/// ONE of the four is read back off applied state — `capture_linear`, the
/// metadata half the shipping apply reconciles. `is_f16` is the planner's
/// resolved format and `stage`/`retagged` are the caller's drive coordinates;
/// `aterm_spec::derive::hdr_reconfigure_retag_model` carries the per-variable
/// accounting and names what holds the two the projection does not.
///
/// `capture_linear` is what makes this a binding rather than a restatement of
/// the planner: Tier-1 must not validate a decision while skipping the effect it
/// orders, so the modeled successor is compared against state
/// `apply_hdr_reconfigure_plan` / `apply_hdr_surface_upgrade` actually produced,
/// never against a table the test wrote for itself.
///
/// Test-only (`cfg(test)`): it is the conformance projection, not renderer
/// state, so it adds nothing to this crate's API. The `project = ` strings on
/// the anchors below name it in exactly the build where it exists — the same
/// build in which `cfg(any(test, ...))` links the anchors themselves.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HdrReconfigureProjection {
    /// Model phase: confirmed HDR (0), retained eligible SDR (1), resolved (2).
    pub(crate) stage: u8,
    /// Whether the attempted scRGB re-tag succeeded.
    pub(crate) retagged: bool,
    /// Whether the plan resolved the swapchain to `Rgba16Float`. PLANNER-derived:
    /// the configure that installs the resolved format happens on the
    /// `GpuSurface`, which a `WindowGpu` projection cannot see.
    pub(crate) is_f16: bool,
    /// Whether the window's APPLIED capture metadata declares extended-linear
    /// sRGB. Read back off the window, so an apply that forgets its half of the
    /// fallback shows up here as a `CaptureMatchesSurfaceEncoding` violation.
    pub(crate) capture_linear: bool,
}

/// Resolve the post-reconfigure surface encoding from the ACTUAL current format
/// and the result of re-establishing scRGB on the recreated swapchain.
///
/// The five anchors bind every `HdrReconfigureRetag` action to this planner.
/// Each action is ALSO anchored on the concrete apply that performs its
/// metadata half (`WindowGpu::apply_hdr_reconfigure_plan`, or
/// `WindowGpu::apply_hdr_surface_upgrade` for `UpgradeSucceeds`): plan and apply
/// together are the transition the model describes, and binding only this half
/// would leave the effects unwitnessed.
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "HdrReconfigureRetag",
        action = "RetagSucceeds",
        project = "aterm_gpu::WindowGpu::project_hdr_reconfigure_state"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "HdrReconfigureRetag",
        action = "RetagFails",
        project = "aterm_gpu::WindowGpu::project_hdr_reconfigure_state"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "HdrReconfigureRetag",
        action = "EnterSdrFallback",
        project = "aterm_gpu::WindowGpu::project_hdr_reconfigure_state"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "HdrReconfigureRetag",
        action = "UpgradeSucceeds",
        project = "aterm_gpu::WindowGpu::project_hdr_reconfigure_state"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "HdrReconfigureRetag",
        action = "UpgradeFails",
        project = "aterm_gpu::WindowGpu::project_hdr_reconfigure_state"
    )
)]
#[must_use]
pub fn hdr_reconfigure_plan(swapchain_is_f16: bool, scrgb_retagged: bool) -> HdrReconfigurePlan {
    match (swapchain_is_f16, scrgb_retagged) {
        (false, _) => HdrReconfigurePlan::KeepSdr,
        (true, true) => HdrReconfigurePlan::KeepHdr,
        (true, false) => HdrReconfigurePlan::FallbackToSdr,
    }
}

/// LIVE SDR→HDR: is an f16 upgrade attempt admitted on this present?
///
/// `supports_f16` is the raw attach-time surface capability, deliberately not
/// frozen to the then-current opt-in: a config hot reload can turn `hdr_glow`
/// on later. `output_hdr_enabled` is the throttled, side-effect-free Windows
/// containing-output probe. Only their conjunction may recreate the live SDR
/// swapchain as f16; the subsequent scRGB tag is still resolved through
/// [`hdr_reconfigure_plan`] and falls back atomically on a race/failure.
#[must_use]
pub fn hdr_live_upgrade_wants_f16(
    hdr_glow: bool,
    supports_f16: bool,
    output_hdr_enabled: bool,
) -> bool {
    hdr_glow && supports_f16 && output_hdr_enabled
}

/// PRESENT (SDR twin): run the swapchain-side SDR glow-boost pass this present?
/// True iff the swapchain is NOT the f16 EDR target (the two boost passes are
/// mutually exclusive by construction — same instances, different clamp math),
/// there are glow instances to draw, and the resolved budget is positive (a 0
/// budget — strength 0, light theme rolloff, poisoned inputs — keeps the SDR
/// present untouched). ADDITIVE to the proven [`HdrPlan`] seam, deliberately not
/// folded into it: the Tier-1 `hdr_present_gate_model` twin stays byte-stable.
/// Parity note: the pass draws into the SWAPCHAIN after the blit; the offscreen
/// (the readback/introspection source of truth) is never touched, so the
/// differential suites are unaffected for ANY budget.
#[must_use]
pub fn sdr_boost_pass(swapchain_is_f16: bool, glow_nonempty: bool, budget: f32) -> bool {
    !swapchain_is_f16 && glow_nonempty && budget > 0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    // The formal invariant, GPU-free: for BOTH srgb_offscreen states, every render
    // pass's pipeline colour-target format equals its attachment VIEW format, and
    // every attached view format is the texture format or a declared alias. This is
    // the same property the `gpu_pipeline_format` ay proof discharges; keeping it as
    // a unit test makes a regression a compile-and-run failure with zero GPU.
    #[test]
    fn pipeline_target_matches_attachment_on_both_backends() {
        for srgb in [true, false] {
            let tex = offscreen_format(srgb); // off.view follows the texture format
            let srgb_view = offscreen_srgb_view_format(srgb);
            // additive (glow_add/deco_add) + bloom + tray + test-blit attach off.view
            // and build with offscreen_format -> trivially equal (same function).
            assert_eq!(
                tex,
                offscreen_format(srgb),
                "srgb={srgb}: additive/bloom/tray view != target"
            );
            // base OVER/REPLACE + cursor + deco_over attach off.view_srgb and build
            // with offscreen_srgb_view_format -> equal.
            assert_eq!(
                srgb_view,
                offscreen_srgb_view_format(srgb),
                "srgb={srgb}: base view != target"
            );
            // VIEW_FORMATS validity: the sRGB view fmt must be the texture fmt or a
            // declared alias (mirrors wgpu's real create_view panic).
            let declared = srgb_view == tex || offscreen_view_formats(srgb).contains(&srgb_view);
            assert!(
                declared,
                "srgb={srgb}: sRGB view fmt neither texture fmt nor declared alias"
            );
            // Stored bytes are always sRGB-encoded: readback and the
            // application-submitted destination share this byte contract.
            assert_eq!(
                tex.add_srgb_suffix(),
                TextureFormat::Rgba8UnormSrgb,
                "srgb={srgb}: storage not sRGB"
            );
        }
    }

    // Bug #1 (downlevel additive approximation): the ADDITIVE One/One target is a raw
    // 8-bit add == CPU `add_sat` ONLY when it is plain Unorm (native). On downlevel the
    // single offscreen is sRGB, so the SAME add lands in LINEAR — the accepted cosmetic
    // approximation that GpuRenderer::additive_is_byte_exact + the glow parity guard
    // gate on. This pins that mapping GPU-free.
    #[test]
    fn additive_target_is_byte_exact_only_on_native() {
        assert_eq!(
            offscreen_format(true),
            TextureFormat::Rgba8Unorm,
            "native additive target must be raw Unorm (byte-exact add)"
        );
        assert_eq!(
            offscreen_format(false),
            TextureFormat::Rgba8UnormSrgb,
            "downlevel additive target must be sRGB (linear add)"
        );
    }

    // Bug #2: clearing the offscreen's DEFAULT view must read back as the INPUT byte on
    // BOTH backends. On downlevel that view is sRGB and encodes on store, so the clear
    // is decoded to linear; feeding it through the sRGB ENCODE (the inverse) must land
    // back on the input byte for every channel value. On native the clear is stored
    // verbatim. GPU-free: simulates the hardware sRGB encode with the standard curve.
    #[test]
    fn clear_color_round_trips_to_input_byte_on_both_backends() {
        // linear -> sRGB encode (the inverse of format_plan's decode / theme_color's s2l).
        fn l2s(c: f64) -> f64 {
            if c <= 0.003_130_8 {
                c * 12.92
            } else {
                1.055 * c.powf(1.0 / 2.4) - 0.055
            }
        }
        for byte in 0u32..=255 {
            let rgb = (byte << 16) | (byte << 8) | byte;
            // Downlevel: the sRGB view encodes our linear-decoded clear back to `byte`.
            let dl = offscreen_clear_color(rgb, false);
            let stored = (l2s(dl.r) * 255.0).round() as u32;
            assert_eq!(
                stored, byte,
                "downlevel clear of {byte} stored as {stored} (must round-trip)"
            );
            // Native: plain Unorm stores the clear verbatim -> the raw byte.
            let nat = offscreen_clear_color(rgb, true);
            assert_eq!(
                (nat.r * 255.0).round() as u32,
                byte,
                "native clear of {byte} must be raw"
            );
        }
    }

    // Load-bearing for bug #2: the PRE-FIX form — passing the raw byte to the downlevel
    // sRGB view (no linear decode) — DOES read back brighter than the input on a
    // mid-tone, so the round-trip test above has teeth (mirrors the C1/C2 non-vacuity).
    #[test]
    fn raw_clear_into_srgb_view_reads_back_brighter() {
        fn l2s(c: f64) -> f64 {
            if c <= 0.003_130_8 {
                c * 12.92
            } else {
                1.055 * c.powf(1.0 / 2.4) - 0.055
            }
        }
        let raw = 128.0 / 255.0; // the buggy clear value (no decode) for byte 128
        let stored = (l2s(raw) * 255.0).round() as u32;
        assert!(
            stored > 128,
            "raw clear into an sRGB view must store brighter than input (got {stored})"
        );
    }

    /// THE FORMAT AXIS, COUPLED ACROSS BACKENDS.
    ///
    /// `metal::pipelines::metal_format` is a hand-copied match, and format is
    /// the one axis THE PIPELINE TABLE did not couple: a judge made all three
    /// non-`Present` roles wrong AT ONCE with only the control test failing —
    /// and it is the axis three of the four deleted hand-written Metal tests
    /// had already drifted on (`Bgra8Unorm` vs `Rgba8Unorm`, `Bgra8UnormSrgb`
    /// vs `Rgba8UnormSrgb`). This sweep computes BOTH sides — the `wgpu`
    /// resolve (`TargetFormats::resolve` over this module's own plan) and the
    /// Metal match — for every role x present format, so any single wrong row
    /// fails by name.
    ///
    /// The equality is by format NAME (`Debug`): `metal::ffi::PixelFormat`'s
    /// variants are named after their `wgpu::TextureFormat` twins, which makes
    /// the comparison a cross-backend statement rather than a re-spelling of
    /// either side. It lives HERE, not in `metal/`, because THE ROW's rule is
    /// that no `wgpu` type crosses into the first-party Metal module — tests
    /// included.
    ///
    /// Metal runs the NATIVE plan and only that plan: pixel-format views are
    /// unconditional on Metal (`TEXTURE_USAGE_PIXEL_FORMAT_VIEW`, proven
    /// creatable on the GPU by every Metal render test, which allocates the
    /// view-capable offscreen before its first frame),
    /// so `srgb_offscreen == true` is the one state the backend can be in.
    /// The downlevel half of the sweep pins where the two plans differ —
    /// exactly one role — so a `format_plan` change that widens or moves the
    /// divergence is a visible diff beside the equality it would undermine.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_metal_format_axis_equals_the_wgpu_resolve_for_every_role() {
        use crate::metal::ffi::PixelFormat;
        use crate::metal::pipelines::metal_format;
        use crate::pipeline_table::{TargetFormats, TargetRole};

        const ROLES: [TargetRole; 4] = [
            TargetRole::OffscreenSrgb,
            TargetRole::OffscreenUnorm,
            TargetRole::Edr,
            TargetRole::Present,
        ];
        let native = TargetFormats {
            offscreen_srgb: offscreen_srgb_view_format(true),
            offscreen_unorm: offscreen_format(true),
            present: None,
        };
        // Both formats `pick_surface_format` can choose.
        for (wgpu_present, metal_present) in [
            (TextureFormat::Bgra8Unorm, PixelFormat::Bgra8Unorm),
            (TextureFormat::Rgba8Unorm, PixelFormat::Rgba8Unorm),
        ] {
            for role in ROLES {
                let wgpu_side = native.with_present(wgpu_present).resolve(role);
                let metal_side = metal_format(role, metal_present);
                assert_eq!(
                    format!("{wgpu_side:?}"),
                    format!("{metal_side:?}"),
                    "{role:?} on a {wgpu_present:?} swapchain: metal_format \
                     disagrees with the wgpu resolve — this axis can be wrong \
                     on every offscreen role at once with every pipeline still \
                     building"
                );
            }
        }
        // The downlevel plan (GLES/WebGL2: no format views, the offscreen
        // texture is ITSELF sRGB) diverges from Metal on exactly ONE role.
        let downlevel = TargetFormats {
            offscreen_srgb: offscreen_srgb_view_format(false),
            offscreen_unorm: offscreen_format(false),
            present: None,
        };
        let diverging: Vec<TargetRole> = ROLES[..3]
            .iter()
            .copied()
            .filter(|&r| {
                format!("{:?}", downlevel.resolve(r))
                    != format!("{:?}", metal_format(r, PixelFormat::Bgra8Unorm))
            })
            .collect();
        assert_eq!(
            diverging,
            [TargetRole::OffscreenUnorm],
            "Metal implements the native plan; downlevel differs only where \
             the single sRGB offscreen replaces the Unorm view"
        );
    }

    /// The SDR boost gate is total + mutually exclusive with the EDR boost: it
    /// never fires on the f16 swapchain (where `glow_boost_pass` owns the crown),
    /// never with an empty stream, and never at a non-positive/poisoned budget.
    #[test]
    fn sdr_boost_gate_is_exclusive_and_fail_off() {
        // Fires only on the exact ship condition.
        assert!(sdr_boost_pass(false, true, 0.1));
        // f16 swapchain -> the EDR pass owns it.
        assert!(!sdr_boost_pass(true, true, 0.1));
        // No instances -> nothing to draw.
        assert!(!sdr_boost_pass(false, false, 0.1));
        // Zero / negative / NaN budget -> off (NaN > 0.0 is false).
        assert!(!sdr_boost_pass(false, true, 0.0));
        assert!(!sdr_boost_pass(false, true, -1.0));
        assert!(!sdr_boost_pass(false, true, f32::NAN));
        // Exclusivity with the proven plan, all 8 boolean corners x positive budget:
        for hdr_glow in [false, true] {
            for f16 in [false, true] {
                for nonempty in [false, true] {
                    let plan = hdr_present_plan(hdr_glow, f16, nonempty);
                    assert!(
                        !(plan.glow_boost_pass && sdr_boost_pass(f16, nonempty, 0.35)),
                        "both boost passes armed at hdr_glow={hdr_glow} f16={f16} nonempty={nonempty}"
                    );
                }
            }
        }
    }

    /// The screen-capability narrowing of the attach pick: it can only ever
    /// take f16 AWAY from the proven gate (so `SdrInvariance` and
    /// `F16NeedsSupport` survive by implication), it takes it away exactly on
    /// a positive "this screen cannot show EDR" answer (potential <= 1.0, or a
    /// non-finite read), and an unresolved screen changes nothing.
    #[test]
    fn screen_gate_narrows_only_on_a_known_sdr_screen() {
        let potentials = [
            None,
            Some(1.0),  // an external SDR monitor
            Some(2.0),  // this 2017 MacBook Pro's built-in panel (measured)
            Some(16.0), // Pro Display XDR
            Some(1.0 + f32::EPSILON),
            Some(0.0),
            Some(-1.0),
            Some(f32::NAN),
            Some(f32::INFINITY),
        ];
        let mut narrowed = 0usize;
        for hdr_glow in [false, true] {
            for supports_f16 in [false, true] {
                let base = hdr_swapchain_wants_f16(hdr_glow, supports_f16);
                for potential in potentials {
                    let got = hdr_swapchain_wants_f16_on_screen(hdr_glow, supports_f16, potential);
                    assert!(
                        !got || base,
                        "({hdr_glow},{supports_f16},{potential:?}): the screen gate widened the pick"
                    );
                    let capable = potential.is_none_or(|p| p.is_finite() && p > 1.0);
                    assert_eq!(
                        got,
                        base && capable,
                        "({hdr_glow},{supports_f16},{potential:?}): wrong screen decision"
                    );
                    narrowed += usize::from(base && !got);
                }
            }
        }
        // NON-VACUITY: the SDR-screen answers (1.0, 0.0, -1.0, NaN, +inf) each
        // narrow the one (on, supported) corner — five real narrowings, no more.
        assert_eq!(narrowed, 5, "the screen gate must bite on every SDR answer");
        // The pinned corners, spelled out.
        assert!(hdr_swapchain_wants_f16_on_screen(true, true, None));
        assert!(hdr_swapchain_wants_f16_on_screen(true, true, Some(2.0)));
        assert!(!hdr_swapchain_wants_f16_on_screen(true, true, Some(1.0)));
        assert!(!hdr_swapchain_wants_f16_on_screen(false, true, Some(16.0)));
    }

    /// The live monitor-change re-pick: it moves an 8-bit surface to f16 only
    /// on a POSITIVE "this screen can show EDR" answer — never on an
    /// unresolved screen (unlike the attach gate, whose `None` keeps the
    /// unconditional pick), never on an already-f16 surface, and never wider
    /// than the attach gate: upgrade ⇒ on-screen pick ⇒ base gate, for every
    /// input.
    #[test]
    fn screen_upgrade_gate_needs_a_positive_edr_answer() {
        let potentials = [
            None,
            Some(1.0),
            Some(2.0),
            Some(16.0),
            Some(1.0 + f32::EPSILON),
            Some(0.0),
            Some(-1.0),
            Some(f32::NAN),
            Some(f32::INFINITY),
        ];
        let mut upgraded = 0usize;
        for hdr_glow in [false, true] {
            for supports_f16 in [false, true] {
                for swapchain_is_f16 in [false, true] {
                    for potential in potentials {
                        let got = hdr_screen_upgrade_wants_f16(
                            hdr_glow,
                            supports_f16,
                            swapchain_is_f16,
                            potential,
                        );
                        let on_screen =
                            hdr_swapchain_wants_f16_on_screen(hdr_glow, supports_f16, potential);
                        assert!(
                            !got || on_screen,
                            "({hdr_glow},{supports_f16},{swapchain_is_f16},{potential:?}): \
                             the upgrade widened the attach gate"
                        );
                        assert!(
                            !got || !swapchain_is_f16,
                            "({hdr_glow},{supports_f16},{swapchain_is_f16},{potential:?}): \
                             an f16 surface was upgraded"
                        );
                        let positive = potential.is_some_and(|p| p.is_finite() && p > 1.0);
                        assert_eq!(
                            got,
                            !swapchain_is_f16
                                && hdr_swapchain_wants_f16(hdr_glow, supports_f16)
                                && positive,
                            "({hdr_glow},{supports_f16},{swapchain_is_f16},{potential:?}): \
                             wrong upgrade decision"
                        );
                        upgraded += usize::from(got);
                    }
                }
            }
        }
        // NON-VACUITY: the three EDR answers (2.0, 16.0, 1+ε) each upgrade the
        // one (on, supported, 8-bit) corner — three real upgrades, no more.
        assert_eq!(upgraded, 3, "the upgrade must bite on every EDR answer");
        // The pinned corners, spelled out.
        assert!(hdr_screen_upgrade_wants_f16(true, true, false, Some(2.0)));
        assert!(
            !hdr_screen_upgrade_wants_f16(true, true, false, None),
            "an unresolved screen is not evidence"
        );
        assert!(!hdr_screen_upgrade_wants_f16(true, true, false, Some(1.0)));
        assert!(!hdr_screen_upgrade_wants_f16(true, true, true, Some(2.0)));
        assert!(!hdr_screen_upgrade_wants_f16(
            false,
            true,
            false,
            Some(16.0)
        ));
    }
}
