// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Kani bounded model checking proofs for the containment policy mapping.
//!
//! These harnesses are `#[cfg(kani)]`-gated (the module itself is declared
//! under `#[cfg(kani)]` in `lib.rs`), so a plain `cargo build`/`cargo test`
//! COMPILES THEM OUT ENTIRELY and discharges nothing. They are discharged only
//! by trust-mc run deliberately via the opt-in `scripts/verify-kani-proofs.sh`;
//! the unit tests in `policy.rs` and `mode.rs` pin the same table in the default
//! test lane. The properties:
//! - Mode ordering agrees with the numeric level
//! - Capabilities match mode (policy consistency)
//! - Monotonic capabilities (downgrade never increases any capability)
//! - Containment mode is maximally restrictive
//! - The policy is total on every mode

use crate::capability::{FsCapability, NetworkCapability, ProcessCapability};
use crate::mode::ContainmentMode;
use crate::policy::ContainmentPolicy;

/// Helper: construct a `ContainmentMode` from a symbolic u8.
///
/// Returns the mode for levels 0..=3 (matching `repr(u8)` encoding).
/// Panics on invalid level (callers must `kani::assume(level <= 3)`).
fn mode_from_level(level: u8) -> ContainmentMode {
    match level {
        0 => ContainmentMode::Containment,
        1 => ContainmentMode::Safety,
        2 => ContainmentMode::User,
        3 => ContainmentMode::Master,
        _ => unreachable!("caller must assume level <= 3"),
    }
}

// -----------------------------------------------------------------------
// Property 1: Mode ordering is consistent with numeric level
// -----------------------------------------------------------------------

/// For any two valid modes, Rust `Ord` ordering matches the numeric level:
/// Master(3) > User(2) > Safety(1) > Containment(0).
/// Proves `mode_a >= mode_b ⟺ level(mode_a) >= level(mode_b)`.
#[kani::proof]
fn mode_ordering_matches_level_encoding() {
    let a: u8 = kani::any();
    let b: u8 = kani::any();
    kani::assume(a <= 3);
    kani::assume(b <= 3);

    let mode_a = mode_from_level(a);
    let mode_b = mode_from_level(b);

    // Ord impl must agree with numeric level
    kani::assert(
        (mode_a >= mode_b) == (a >= b),
        "mode ordering must match the numeric level",
    );
}

// -----------------------------------------------------------------------
// Property 2: Capabilities always match mode (CapabilitiesMatchMode)
// -----------------------------------------------------------------------

/// For every mode, the policy functions return exactly the documented values
/// for all 4 × 3 = 12 mappings.
// Asserted via the per-field policy functions directly (not `caps.field as u8`):
// `capabilities(mode)` is `Capabilities { network: network(mode), .. }` by
// construction, so the obligation is identical, but trust-mc drops the enum-field
// discriminant on a `field as u8` cast of a struct returned from an enum-arg fn
// (`loaded-aggregate-extract-field`). Three fields stay under the ≤4-per-harness
// bound that keeps AY's symbolic-mode blowup well inside the cap.
#[kani::proof]
fn capabilities_match_mode_policy() {
    let level: u8 = kani::any();
    kani::assume(level <= 3);
    let mode = mode_from_level(level);

    // Network: Containment=0, Safety=User=Master=1
    let expected_net: u8 = match level {
        0 => 0,
        1..=3 => 1,
        _ => unreachable!(),
    };
    kani::assert(
        ContainmentPolicy::network(mode) as u8 == expected_net,
        "PolicyNetwork mismatch",
    );
    // Fs: Containment=0, Safety=1, User=2, Master=3
    kani::assert(
        ContainmentPolicy::fs(mode) as u8 == level,
        "PolicyFs mismatch",
    );
    // Process: Containment=0, Safety=1, User=2, Master=2
    let expected_proc: u8 = match level {
        0 => 0,
        1 => 1,
        2 | 3 => 2,
        _ => unreachable!(),
    };
    kani::assert(
        ContainmentPolicy::process(mode) as u8 == expected_proc,
        "PolicyProcess mismatch",
    );
}

// -----------------------------------------------------------------------
// Property 3: Monotonic capabilities (MonotonicCapabilities)
// -----------------------------------------------------------------------

/// For any two modes where `lower <= higher`, every capability of
/// `lower` is ≤ the corresponding capability of `higher`.
///
/// Capabilities only decrease when mode decreases. This proves the contrapositive: no single capability can increase when
/// mode decreases.
#[kani::proof]
fn monotonic_capabilities_for_all_mode_pairs() {
    let a: u8 = kani::any();
    let b: u8 = kani::any();
    kani::assume(a <= 3);
    kani::assume(b <= 3);
    kani::assume(a <= b); // a is the lower mode

    let lower = mode_from_level(a);
    let higher = mode_from_level(b);
    let cl = ContainmentPolicy::capabilities(lower);
    let ch = ContainmentPolicy::capabilities(higher);

    // Every capability of the lower mode must be ≤ the higher mode
    kani::assert(cl.network <= ch.network, "network monotonicity violated");
    kani::assert(cl.fs <= ch.fs, "fs monotonicity violated");
    kani::assert(cl.process <= ch.process, "process monotonicity violated");
}

// -----------------------------------------------------------------------
// Property 4: Containment mode is maximally restrictive (ContainmentMinimal)
// -----------------------------------------------------------------------

/// Containment mode has the minimum value (0) for every capability.
#[kani::proof]
fn containment_mode_is_maximally_restrictive() {
    let caps = ContainmentPolicy::capabilities(ContainmentMode::Containment);

    kani::assert(caps.network as u8 == 0, "ContainmentHasNoNetwork");
    kani::assert(caps.fs as u8 == 0, "Containment fs = TmpOnly");
    kani::assert(caps.process as u8 == 0, "Containment process = NoFork");

    // Cross-check with specific enum variants (not just numeric)
    kani::assert(
        caps.network == NetworkCapability::None,
        "network must be None",
    );
    kani::assert(caps.fs == FsCapability::TmpOnly, "fs must be TmpOnly");
    kani::assert(
        caps.process == ProcessCapability::NoFork,
        "process must be NoFork",
    );
}

// -----------------------------------------------------------------------
// Property 5: Non-escalation — downgrade never increases any capability
// -----------------------------------------------------------------------

/// For any pair of modes where `from > to` (downgrade), every capability
/// of the target mode is strictly ≤ the source mode. No escalation possible
/// through mode downgrade.
///
/// NonEscalation — mode can NEVER increase in capability.
#[kani::proof]
fn downgrade_never_escalates_any_capability() {
    let from: u8 = kani::any();
    let to: u8 = kani::any();
    kani::assume(from <= 3);
    kani::assume(to <= 3);
    kani::assume(to < from); // strict downgrade

    let source = mode_from_level(from);
    let target = mode_from_level(to);
    let cs = ContainmentPolicy::capabilities(source);
    let ct = ContainmentPolicy::capabilities(target);

    // Every capability must decrease or stay the same
    kani::assert(ct.network <= cs.network, "network escalated on downgrade");
    kani::assert(ct.fs <= cs.fs, "fs escalated on downgrade");
    kani::assert(ct.process <= cs.process, "process escalated on downgrade");
}

// -----------------------------------------------------------------------
// Property 6: Policy is a total function on all modes
// -----------------------------------------------------------------------

/// The policy functions produce an in-range capability for every mode variant —
/// no panic, no UB. Per-field functions, not `capabilities(mode).field as u8`,
/// for the trust-mc `loaded-aggregate-extract-field` reason given at Property 2.
#[kani::proof]
fn policy_is_total_on_all_modes() {
    let level: u8 = kani::any();
    kani::assume(level <= 3);
    let mode = mode_from_level(level);
    kani::assert(
        ContainmentPolicy::network(mode) as u8 <= 1,
        "network out of range",
    );
    kani::assert(ContainmentPolicy::fs(mode) as u8 <= 3, "fs out of range");
    kani::assert(
        ContainmentPolicy::process(mode) as u8 <= 2,
        "process out of range",
    );
}
