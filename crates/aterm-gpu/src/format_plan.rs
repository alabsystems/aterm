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
#[path = "format_plan_tests.rs"]
mod tests;
