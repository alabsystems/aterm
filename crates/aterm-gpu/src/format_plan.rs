// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

/// The HDR present gate as pure decision functions, exhaustively checked by `tests/hdr_gate.rs`'s full enumeration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HdrPlan {
    /// The blit decodes the offscreen's sRGB bytes to linear (clamped to 1.0) for the extended-linear-sRGB f16 swapchain.
    pub blit_linear_encode: bool,
    /// Run the HDR aurora pass after the blit.
    pub glow_boost_pass: bool,
}

/// Attach: pick `Rgba16Float` for the swapchain? See [`HdrPlan`].
#[must_use]
#[inline]
pub const fn hdr_swapchain_wants_f16(hdr_glow: bool, supports_f16: bool) -> bool {
    hdr_glow && supports_f16
}

/// The attach gate narrowed by screen (macOS Metal arm).
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

/// Live SDR-to-f16 upgrade on the macOS Metal arm (`GpuRenderer::upgrade_surface_for_screen`).
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

/// The monitor-change re-pick's decision, in the order the throttled re-query needs.
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

/// Present: what the HDR path does on this present.
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HdrReconfigurePlan {
    /// The surface was already SDR.
    KeepSdr,
    /// The f16 swapchain was successfully re-tagged scRGB.
    KeepHdr,
    /// Re-tagging the recreated f16 swapchain failed.
    FallbackToSdr,
}

/// Resolve the post-reconfigure surface encoding from the current format and whether scRGB was re-established.
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

/// Live SDR-to-HDR.
#[must_use]
#[inline]
pub const fn hdr_live_upgrade_wants_f16(
    hdr_glow: bool,
    supports_f16: bool,
    output_hdr_enabled: bool,
) -> bool {
    hdr_glow && supports_f16 && output_hdr_enabled
}

/// Whether to run the SDR glow-boost pass.
#[must_use]
#[inline]
pub const fn sdr_boost_pass(swapchain_is_f16: bool, glow_nonempty: bool, budget: f32) -> bool {
    !swapchain_is_f16 && glow_nonempty && budget > 0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The SDR boost never fires on f16, with an empty stream, or at a
    /// non-positive or invalid budget.
    #[test]
    fn sdr_boost_gate_is_exclusive_and_fail_off() {
        assert!(sdr_boost_pass(false, true, 0.1));
        assert!(!sdr_boost_pass(true, true, 0.1));
        assert!(!sdr_boost_pass(false, false, 0.1));
        assert!(!sdr_boost_pass(false, true, 0.0));
        assert!(!sdr_boost_pass(false, true, -1.0));
        assert!(!sdr_boost_pass(false, true, f32::NAN));
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
            Some(1.0),
            Some(2.0),
            Some(16.0),
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
        assert_eq!(narrowed, 5, "the screen gate must bite on every SDR answer");
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
        assert_eq!(upgraded, 3, "the upgrade must bite on every EDR answer");
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
