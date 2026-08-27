// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE LAUNCH KITTY (owner ruling, 2026-08-17: *"I think it's too hard to
//! have the cat changing too much. let's have the cat generated at aterm
//! launch and per computer"*).
//!
//! THE BASE companion cat for the whole aterm process: its seed is minted
//! exactly once, at `App` construction, from the workspace's audited CSPRNG
//! surface ([`aterm_uds::rand::fill`]), and decoded through
//! [`aterm_effects::kitty_registry::KittyLook::for_launch`]. It is what the
//! prompt wears in every window and session until aterm is launched again (a
//! second, concurrent aterm instance on the same computer mints its own). A
//! seamless in-place update re-execs a fresh process and therefore counts as
//! a launch — the updated aterm arrives with its own kitty, which is the
//! honest reading of "generated at launch".
//!
//! This retires the per-session kitty (2026-07-26, a different breed per tab)
//! and the old "shell" program cat. The per-app kitties stay — the owner
//! likes them (*"I like the different cats"*) — but ride above this base only
//! through the TENURE gate in `app_kitty.rs` (a program earns the cursor by
//! holding the pane; the cat lingers after it exits), which is what makes
//! the switching rare and deliberate instead of "all the time". Discovery
//! never repoints the cat: an ambient or typed sighting still COLLECTS into
//! the Kitty Log, but the cat on glass does not change for it.
//!
//! THE PRECEDENCE LAW lives in [`companion_precedence`] and has three rungs:
//! a pinned FAVOURITE (the user's explicit choice — the one reason strong
//! enough to override everything; also the way to KEEP a launch kitty you
//! like across launches) beats the tenured PROGRAM cat, which beats the
//! launch kitty, the floor for everything else.

use aterm_effects::kitty_registry::KittyLook;

/// Mint the process's launch-kitty seed. Called ONCE, at `App` construction
/// (never on the render path): eight bytes from the OS CSPRNG through the
/// audited helper, folded into a `u64`.
///
/// FAILURE POSTURE (the helper's documented contract: the caller decides, and
/// must never retry via its own device read): with no entropy source the seed
/// degrades to a documented NON-SECRET fallback mixed from the wall clock and
/// the pid — never a constant, because a constant would hand every fallback-
/// path install the SAME cat, exactly the "every install wears the same cat"
/// regression the derived floor exists to prevent. The seed is a toy's
/// identity, not a credential, so a weak fallback costs nothing but variety.
pub(crate) fn mint_launch_seed() -> u64 {
    let mut bytes = [0u8; 8];
    match aterm_uds::rand::fill(&mut bytes) {
        Ok(()) => u64::from_le_bytes(bytes),
        Err(_) => fallback_seed(),
    }
}

/// The non-secret fallback for [`mint_launch_seed`]: wall-clock nanoseconds
/// folded with the pid. Varies per launch by construction (two launches
/// cannot share both a nanosecond stamp and a pid); documented as weak.
fn fallback_seed() -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    let pid = u64::from(std::process::id());
    nanos ^ pid.rotate_left(32) ^ 0x9E37_79B9_7F4A_7C15
}

/// WHICH RUNG of [`companion_precedence`] won a verdict — the winner report
/// the rate law reads at the render sync sites (kitty-motion §2.0.4, Rungs:
/// *"`companion_precedence` reports which arm won; the sync site maps
/// `Rung::Program => tenure.arrival()`, every other rung to Quiet"*). The
/// LOOK still travels alone through `App::companion_verdict`'s bare
/// `KittyLook` return (five production callers and a locked test suite
/// compare it directly); the rung rides beside it on `WindowState` so the
/// sync sites can tell a program-rung win from a favourite or launch win —
/// only a PROGRAM win may ever carry the tenure gate's arrival ceremony.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CompanionRung {
    /// The pinned favourite won. The user's explicit choice announces
    /// nothing (the USER-ACT-ONLY precedent): always quiet.
    Favourite,
    /// The tenured program cat won — the ONE rung whose arrival may be a
    /// ceremony, as ruled by `app_kitty::KittyTenure::arrival`.
    Program,
    /// The launch kitty floor won — "no stronger claim". The base cat is
    /// always home: always quiet.
    Launch,
}

/// THE COMPANION PRECEDENCE LAW (owner rulings, 2026-08-07 and 2026-08-17),
/// the ONE place the order is stated — every dressing surface (the
/// single-pane present, the split/composed present, and both capture
/// splices) resolves through `App::companion_verdict`, which calls here:
///
///   favourite > program (with tenure) > launch kitty
///
///   1. A PINNED FAVOURITE owns the companion look (standing owner law): the
///      user chose that cat, and only a reason outranks a choice. Pinning is
///      also how a launch kitty you like is kept across launches.
///   2. THE PROGRAM CAT — but only once it has EARNED the cursor: `app` is
///      the focused pane's claim AFTER `app_kitty::KittyTenure` (a program
///      that has held the pane for `TENURE`, lingering `RELEASE` after it
///      exits), never the raw, flapping block state.
///   3. THE LAUNCH KITTY: the process's own base cat, minted once at launch —
///      the face of "no stronger claim". There is no session rung and no
///      discovery rung: neither is a choice, and each made the cat change.
///
/// Returns the winning look AND [`CompanionRung`] names the arm that won, so
/// the rate law's sync sites can ration the theater without a second, drifting
/// re-derivation of this order.
#[must_use]
pub(crate) fn companion_precedence(
    favourite: Option<KittyLook>,
    app: Option<KittyLook>,
    launch: KittyLook,
) -> (KittyLook, CompanionRung) {
    if let Some(look) = favourite {
        (look, CompanionRung::Favourite)
    } else if let Some(look) = app {
        (look, CompanionRung::Program)
    } else {
        (launch, CompanionRung::Launch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (d) The precedence law, rung by rung: a pinned favourite beats the
    /// tenured program cat, the program cat beats the launch kitty, and the
    /// launch kitty is the floor — and each verdict NAMES its winning rung,
    /// so the rate law's sync sites can tell a program win from the rest.
    #[test]
    fn precedence_favourite_beats_program_beats_the_launch_kitty_floor() {
        let favourite = KittyLook {
            coat: 2,
            ..KittyLook::default()
        }
        .normalized();
        let app = KittyLook::for_app("claude");
        let launch = KittyLook::for_launch(0x5EED);
        assert!(
            favourite != launch && app != launch && favourite != app,
            "fixture: the three rungs are distinguishable"
        );
        assert_eq!(
            companion_precedence(Some(favourite), Some(app), launch),
            (favourite, CompanionRung::Favourite),
            "a pinned favourite owns the companion look"
        );
        assert_eq!(
            companion_precedence(None, Some(app), launch),
            (app, CompanionRung::Program),
            "a program that earned the cursor outranks the base cat"
        );
        assert_eq!(
            companion_precedence(None, None, launch),
            (launch, CompanionRung::Launch),
            "the launch kitty is the floor"
        );
    }

    /// The seed is minted from real entropy: two mints differ (a 64-bit
    /// collision is astronomically unlikely), and the fallback is itself
    /// launch-varying rather than a constant.
    #[test]
    fn minted_seeds_vary_and_the_fallback_is_not_a_constant() {
        let a = mint_launch_seed();
        let b = mint_launch_seed();
        assert_ne!(a, b, "two CSPRNG draws must not coincide");
        // The fallback mixes the clock: at least it is not the mixing constant
        // itself, and it never returns zero for a live clock.
        let f = fallback_seed();
        assert_ne!(
            f, 0x9E37_79B9_7F4A_7C15,
            "the fallback is not the bare constant"
        );
    }
}
