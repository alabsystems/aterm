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
//! through the TENURE gate (a program earns the cursor by holding the pane;
//! the cat lingers after it exits), which is what makes the switching rare
//! and deliberate instead of "all the time". Discovery never repoints the
//! cat: an ambient or typed sighting still COLLECTS into the Kitty Log, but
//! the cat on glass does not change for it.
//!
//! The tenure gate and THE PRECEDENCE LAW — a pinned FAVOURITE beats the
//! tenured PROGRAM cat, which beats this launch kitty — are the engine's
//! ([`aterm_effects::companion_look`]); what stays here is the seed's
//! entropy, which a host owns.

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
    let nanos = aterm_time::since_epoch().map_or(0, |d| d.as_nanos() as u64);
    let pid = u64::from(std::process::id());
    nanos ^ pid.rotate_left(32) ^ 0x9E37_79B9_7F4A_7C15
}

#[cfg(test)]
mod tests {
    use super::*;

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
