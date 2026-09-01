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
/// `GpuRenderer::create_window_surface`): the swapchain is `Rgba16Float` (wgpu's
/// Metal backend then auto-sets `wantsExtendedDynamicRangeContent`) iff the user
/// opted in (`hdr_glow = true`) AND the surface offers the format. Off or
/// unsupported → the legacy non-sRGB 8-bit pick, byte-identical to pre-M3.
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
/// `aterm_spec::derive::hdr_reconfigure_retag_model` by `tests/hdr_gate.rs`.
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

/// Resolve the post-reconfigure surface encoding from the ACTUAL current format
/// and the result of re-establishing scRGB on the recreated swapchain.
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

    // Non-vacuity / load-bearing: the pre-fix C1/C2 form — a pipeline hard-coding
    // Rgba8Unorm while attaching off.view — DOES violate the invariant on the
    // downlevel backend, so the test above has teeth (mirrors the ay `*_sat`
    // load-bearing obligation). If this ever stops violating, the guard is dead.
    #[test]
    fn hardcoded_unorm_target_violates_invariant_on_downlevel() {
        let buggy_target = TextureFormat::Rgba8Unorm; // the old bloom/tray/test-blit constant
        let attachment = offscreen_format(false); // downlevel off.view == Rgba8UnormSrgb
        assert_ne!(
            buggy_target, attachment,
            "the C1/C2 mismatch must be real, else the invariant is vacuous"
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
    /// creatable on the GPU by `metal::tests::the_four_renderer_formats_exist`),
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
}
