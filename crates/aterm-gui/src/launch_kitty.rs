// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The launch kitty: the base companion cat for the whole aterm process.
//!
//! Its seed is minted once, at `App` construction, from [`aterm_uds::rand::fill`]
//! and decoded through [`aterm_effects::kitty_registry::KittyLook::for_launch`].
//! The prompt wears it in every window and session until aterm launches again; a
//! concurrent instance mints its own, and a seamless update re-execs and so
//! counts as a launch. Discovery never repoints it: a sighting collects into the
//! Kitty Log without changing the cat on glass.
//!
//! The tenure gate and the precedence (a pinned favourite beats the tenured
//! program cat, which beats this launch kitty) live in
//! [`aterm_effects::companion_look`]; only the seed's entropy is the host's.

/// Mint the process's launch-kitty seed, once, at `App` construction: eight
/// bytes from the OS CSPRNG folded into a `u64`.
///
/// With no entropy source the seed falls back to a non-secret clock-and-pid mix,
/// never a constant: a constant would give every fallback install the same cat.
/// The helper's contract leaves failure to the caller, which must not retry with
/// its own device read.
pub(crate) fn mint_launch_seed() -> u64 {
    let mut bytes = [0u8; 8];
    match aterm_uds::rand::fill(&mut bytes) {
        Ok(()) => u64::from_le_bytes(bytes),
        Err(_) => fallback_seed(),
    }
}

/// The non-secret fallback for [`mint_launch_seed`]: wall-clock nanoseconds
/// folded with the pid, so two launches cannot share it.
fn fallback_seed() -> u64 {
    let nanos = aterm_time::since_epoch().map_or(0, |d| d.as_nanos() as u64);
    let pid = u64::from(std::process::id());
    nanos ^ pid.rotate_left(32) ^ 0x9E37_79B9_7F4A_7C15
}
