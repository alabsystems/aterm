// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// The single source of truth for the offscreen colour-target formats.
//
// wgpu checks at draw time that a pass's attachment view format equals its
// pipeline's target format, and aborts the pass otherwise. Every such choice is
// a pure function here, so a pipeline and its attachment cannot be edited
// apart. `the_srgb_view_is_a_declared_alias_of_srgb_storage` checks that each
// view format is one the texture declares, and the `gpu_pipeline_format` ay
// proof checks the equality, for both `srgb_offscreen` states.

#[cfg(wgpu_arm)]
use wgpu::TextureFormat;

/// The stored offscreen format, equal to the default view (`off.view`) format.
/// Native keeps `Rgba8Unorm` and aliases an sRGB view for the linear-light base
/// passes; downlevel (GLES/WebGL2) cannot alias, so the texture itself is
/// `Rgba8UnormSrgb`. Stored bytes are sRGB either way.
///
/// Every pipeline whose pass attaches `off.view` (additive glow and deco-add,
/// bloom composite and extract, tray, test/readback blit) builds with this.
#[must_use]
#[inline]
#[cfg(wgpu_arm)]
pub(crate) const fn offscreen_format(srgb_offscreen: bool) -> TextureFormat {
    if srgb_offscreen {
        TextureFormat::Rgba8Unorm
    } else {
        TextureFormat::Rgba8UnormSrgb
    }
}

/// The sRGB view format attached by the base, cursor and deco-over passes so
/// blending composites in linear light. Always `Rgba8UnormSrgb` (the alias on
/// native, the texture's own format on downlevel).
#[must_use]
#[inline]
#[cfg(wgpu_arm)]
pub(crate) fn offscreen_srgb_view_format(srgb_offscreen: bool) -> TextureFormat {
    offscreen_format(srgb_offscreen).add_srgb_suffix()
}

/// The `view_formats` alias list the offscreen texture must declare so the sRGB
/// view can be created (wgpu panics otherwise). Native only; downlevel needs
/// none.
#[must_use]
#[inline]
#[cfg(wgpu_arm)]
pub(crate) const fn offscreen_view_formats(srgb_offscreen: bool) -> &'static [TextureFormat] {
    if srgb_offscreen {
        &[TextureFormat::Rgba8UnormSrgb]
    } else {
        &[]
    }
}

/// The HDR present gate as pure decision functions, exhaustively checked by
/// `tests/hdr_gate.rs`'s full enumeration.
///
/// Attach ([`hdr_swapchain_wants_f16`], `GpuRenderer::create_window_surface`):
/// `Rgba16Float` iff `hdr_glow` is on and the surface offers it; otherwise the
/// 8-bit non-sRGB pick. The macOS Metal arm narrows this by screen
/// ([`hdr_swapchain_wants_f16_on_screen`]) and upgrades an 8-bit window live
/// via [`hdr_screen_upgrade_wants_f16`] (`GpuRenderer::upgrade_surface_for_screen`).
/// The swapchain sets `wantsExtendedDynamicRangeContent` from
/// `format == Rgba16Float`.
///
/// Present ([`hdr_present_plan`], `present_input`): keyed on the swapchain's
/// actual format, so an 8-bit surface never linear-encodes or boosts, and an
/// f16 surface keeps decoding correctly with the boost gated on `hdr_glow`.
///
/// # Invariants
/// * `hdr_glow == false` means f16 is never chosen, hence no linear encode and
///   no boost pass.
/// * `blit_linear_encode == swapchain_is_f16`: the encode follows the surface,
///   not the config.
/// * `glow_boost_pass ⇒ swapchain_is_f16 ∧ glow_nonempty ∧ hdr_glow`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HdrPlan {
    /// The blit decodes the offscreen's sRGB bytes to linear (clamped to 1.0) for
    /// the extended-linear-sRGB f16 swapchain.
    pub blit_linear_encode: bool,
    /// Run the HDR aurora pass after the blit: re-emit the glow quads additively
    /// above 1.0, clamped to the panel headroom (`aterm_render::hdr`).
    pub glow_boost_pass: bool,
}

/// Attach: pick `Rgba16Float` for the swapchain? See [`HdrPlan`].
#[must_use]
#[inline]
pub const fn hdr_swapchain_wants_f16(hdr_glow: bool, supports_f16: bool) -> bool {
    hdr_glow && supports_f16
}

/// The attach gate narrowed by screen (macOS Metal arm): [`hdr_swapchain_wants_f16`]
/// and the window's screen reports EDR potential
/// (`NSScreen.maximumPotentialExtendedDynamicRangeColorComponentValue`) above 1.0.
/// On a potential-1.0 screen an f16 swapchain draws no crown (zero headroom, and
/// [`sdr_boost_pass`] is off on f16) yet costs more GPU time and memory.
///
/// The potential, unlike the current value the present reads as `edr_max`, is
/// brightness-independent and fixed per `NSScreen`; a display-settings change can
/// create a new `NSScreen`, so the frontend re-checks 8-bit windows via
/// [`hdr_screen_upgrade_wants_f16`]. `None` (unresolved screen) keeps the
/// unconditional pick; a non-finite potential counts as SDR.
#[must_use]
#[inline]
pub fn hdr_swapchain_wants_f16_on_screen(
    hdr_glow: bool,
    supports_f16: bool,
    screen_edr_potential: Option<f32>,
) -> bool {
    hdr_swapchain_wants_f16(hdr_glow, supports_f16)
        && screen_edr_potential.is_none_or(|p| aterm_render::hdr::sanitize_edr_max(p) > 1.0)
}

/// Live SDR-to-f16 upgrade on the macOS Metal arm
/// (`GpuRenderer::upgrade_surface_for_screen`): an 8-bit swapchain becomes
/// `Rgba16Float` iff the attach gate would pick f16 and the window's current
/// screen positively reports EDR potential. Unlike the attach gate, `None`
/// upgrades nothing (a window in transit stays as it is). Implies
/// [`hdr_swapchain_wants_f16_on_screen`]. `swapchain_is_f16` keeps the function
/// total; callers short-circuit f16 surfaces before the AppKit read.
#[must_use]
#[inline]
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

/// The monitor-change re-pick's decision, in the order the throttled re-query
/// needs: the f16 surface and the proven gate first, and only past them the
/// screen read `screen_edr_potential`, an AppKit read the re-query must not
/// pay for every 8-bit window. Equal to [`hdr_screen_upgrade_wants_f16`] over
/// the potential it reads.
#[must_use]
#[inline]
pub fn hdr_screen_upgrade_decision(
    hdr_glow: bool,
    supports_f16: bool,
    swapchain_is_f16: bool,
    screen_edr_potential: impl FnOnce() -> Option<f32>,
) -> bool {
    if swapchain_is_f16 || !hdr_swapchain_wants_f16(hdr_glow, supports_f16) {
        return false;
    }
    hdr_screen_upgrade_wants_f16(hdr_glow, supports_f16, false, screen_edr_potential())
}

/// PRESENT: what the HDR path does THIS present. See [`HdrPlan`].
#[must_use]
#[inline]
pub const fn hdr_present_plan(
    hdr_glow: bool,
    swapchain_is_f16: bool,
    glow_nonempty: bool,
) -> HdrPlan {
    HdrPlan {
        blit_linear_encode: swapchain_is_f16,
        glow_boost_pass: hdr_glow && swapchain_is_f16 && glow_nonempty,
    }
}

/// What to do after configuring an already-live swapchain.
///
/// DX12 recreates the swapchain on every `Surface::configure`, resetting its
/// colour space, and Windows can disable HDR without any reconfigure. An f16
/// surface is valid only when scRGB was re-tagged; otherwise it falls back
/// atomically to the retained SDR format, since DWM would read linear pixels as
/// gamma 2.2 while capture claimed extended-linear.
///
/// Bound to `aterm_models_render::hdr_reconfigure_retag_model` through the
/// concrete apply path by
/// `renderer::tests::hdr_reconfigure_apply_conforms_to_retag_model`.
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

/// Projection onto the `HdrReconfigureRetag` variables `<<stage, retagged,
/// is_f16, capture_linear>>`, built only by
/// [`crate::WindowGpu::project_hdr_reconfigure_state`].
///
/// Only `capture_linear` is read back off applied state, so the check compares
/// the model's successor with what `apply_hdr_reconfigure_plan` /
/// `apply_hdr_surface_upgrade` actually produced. `is_f16` is the planner's
/// resolved format; `stage`/`retagged` are drive coordinates. Test-only.
#[cfg(all(test, wgpu_arm))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HdrReconfigureProjection {
    /// Model phase: confirmed HDR (0), retained eligible SDR (1), resolved (2).
    pub(crate) stage: u8,
    /// Whether the attempted scRGB re-tag succeeded.
    pub(crate) retagged: bool,
    /// Whether the plan resolved to `Rgba16Float` (planner-derived; the
    /// configure happens on the `GpuSurface`).
    pub(crate) is_f16: bool,
    /// Whether the window's applied capture metadata declares extended-linear
    /// sRGB; an apply that skips its half violates
    /// `CaptureMatchesSurfaceEncoding`.
    pub(crate) capture_linear: bool,
}

/// Resolve the post-reconfigure surface encoding from the current format and
/// whether scRGB was re-established. Each `HdrReconfigureRetag` action is
/// anchored here and on the apply that performs its metadata half
/// (`WindowGpu::apply_hdr_reconfigure_plan`, or
/// `WindowGpu::apply_hdr_surface_upgrade` for `UpgradeSucceeds`).
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "HdrReconfigureRetag",
        action = "RetagSucceeds",
        project = "aterm_gpu::WindowGpu::project_hdr_reconfigure_state"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "HdrReconfigureRetag",
        action = "RetagFails",
        project = "aterm_gpu::WindowGpu::project_hdr_reconfigure_state"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "HdrReconfigureRetag",
        action = "EnterSdrFallback",
        project = "aterm_gpu::WindowGpu::project_hdr_reconfigure_state"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "HdrReconfigureRetag",
        action = "UpgradeSucceeds",
        project = "aterm_gpu::WindowGpu::project_hdr_reconfigure_state"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "HdrReconfigureRetag",
        action = "UpgradeFails",
        project = "aterm_gpu::WindowGpu::project_hdr_reconfigure_state"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HdrReconfigureRetag",
        action = "BuggyRetagFailureIgnored",
        reason = "Buggy=1 negative control only; a failed re-tag of an f16 surface plans \
                  FallbackToSdr, never KeepHdr"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HdrReconfigureRetag",
        action = "BuggyUpgradeFailureKeepsF16",
        reason = "Buggy=1 negative control only; an upgrade whose tag failed plans \
                  FallbackToSdr and restores the SDR format"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HdrReconfigureRetag",
        action = "BuggyEscapeKeepsLinearCapture",
        reason = "Buggy=1 negative control only; apply_hdr_reconfigure_plan reconciles capture \
                  metadata with the SDR format it selects, and renderer.rs's unapplied-escape \
                  control reads the slip back off a real window"
    )
)]
#[must_use]
#[inline]
pub const fn hdr_reconfigure_plan(
    swapchain_is_f16: bool,
    scrgb_retagged: bool,
) -> HdrReconfigurePlan {
    match (swapchain_is_f16, scrgb_retagged) {
        (false, _) => HdrReconfigurePlan::KeepSdr,
        (true, true) => HdrReconfigurePlan::KeepHdr,
        (true, false) => HdrReconfigurePlan::FallbackToSdr,
    }
}

/// Live SDR-to-HDR: is an f16 upgrade admitted on this present?
/// `supports_f16` is the attach-time capability (not tied to the opt-in, which
/// can be hot-reloaded); `output_hdr_enabled` is the throttled Windows output
/// probe. The scRGB tag still goes through [`hdr_reconfigure_plan`].
#[must_use]
#[inline]
pub const fn hdr_live_upgrade_wants_f16(
    hdr_glow: bool,
    supports_f16: bool,
    output_hdr_enabled: bool,
) -> bool {
    hdr_glow && supports_f16 && output_hdr_enabled
}

/// Whether to run the SDR glow-boost pass: the swapchain is not f16 (exclusive
/// with the HDR boost), glow instances exist, and the budget is positive.
/// Separate from [`HdrPlan`] so the Tier-1 model stays stable. It draws into the
/// swapchain only; the offscreen is untouched.
#[must_use]
#[inline]
pub const fn sdr_boost_pass(swapchain_is_f16: bool, glow_nonempty: bool, budget: f32) -> bool {
    !swapchain_is_f16 && glow_nonempty && budget > 0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    // For both `srgb_offscreen` states the sRGB view format is the texture format
    // or a declared alias (wgpu's create_view panics otherwise), and the stored
    // bytes are sRGB-encoded, the contract readback and the destination share
    // (the `gpu_pipeline_format` ay property).
    #[test]
    #[cfg(wgpu_arm)]
    fn the_srgb_view_is_a_declared_alias_of_srgb_storage() {
        for srgb in [true, false] {
            let tex = offscreen_format(srgb);
            let view = offscreen_srgb_view_format(srgb);
            assert!(
                view == tex || offscreen_view_formats(srgb).contains(&view),
                "srgb={srgb}: the sRGB view format is neither the texture format nor a declared alias"
            );
            assert_eq!(
                tex.add_srgb_suffix(),
                TextureFormat::Rgba8UnormSrgb,
                "srgb={srgb}"
            );
        }
    }

    /// The SDR boost never fires on f16, with an empty stream, or at a
    /// non-positive or invalid budget.
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

    /// The screen narrowing only removes f16 from the base gate, does so exactly
    /// on potential <= 1.0 or a non-finite read, and changes nothing for an
    /// unresolved screen.
    #[test]
    fn screen_gate_narrows_only_on_a_known_sdr_screen() {
        let potentials = [
            None,
            Some(1.0),  // an external SDR monitor
            Some(2.0),  // a MacBook Pro built-in panel (measured)
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
        // Non-vacuity: the five SDR answers each narrow the one (on, supported) corner.
        assert_eq!(narrowed, 5, "the screen gate must bite on every SDR answer");
        // The pinned corners, spelled out.
        assert!(hdr_swapchain_wants_f16_on_screen(true, true, None));
        assert!(hdr_swapchain_wants_f16_on_screen(true, true, Some(2.0)));
        assert!(!hdr_swapchain_wants_f16_on_screen(true, true, Some(1.0)));
        assert!(!hdr_swapchain_wants_f16_on_screen(false, true, Some(16.0)));
    }

    /// The live re-pick moves an 8-bit surface to f16 only on a positive EDR
    /// answer (never on `None` or an f16 surface), and upgrade implies the
    /// on-screen pick implies the base gate for every input.
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
        // Non-vacuity: the three EDR answers each upgrade the one (on, supported,
        // 8-bit) corner.
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
