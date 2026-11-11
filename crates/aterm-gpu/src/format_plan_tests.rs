// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
