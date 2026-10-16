// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The program table's invariants — the real ones `crates/atpkg/tests/spec_coherence.rs`
//! held over the hand-kept spec.

use std::collections::BTreeSet;

use atpkg_publish::programs::{self, PIN_TRIPLE, PROGRAMS, RUSTC_GROUP, TRUST_EXPOSES};

#[test]
fn every_program_is_named_once() {
    let names: BTreeSet<&str> = PROGRAMS.iter().map(|p| p.name).collect();
    assert_eq!(
        names.len(),
        PROGRAMS.len(),
        "a duplicate row would render two [programs.x] tables"
    );
}

/// A missing member version-splits the tuple; an extra member that cannot stage aborts
/// the whole group on every client (§7).
#[test]
fn the_rustc_group_is_exactly_the_trust_tuple() {
    let got: BTreeSet<&str> = programs::group_members(RUSTC_GROUP)
        .map(|p| p.name)
        .collect();
    assert_eq!(
        got,
        BTreeSet::from(["trust", "trust-ir", "trust-cg", "trust-vc"])
    );
    for p in programs::group_members(RUSTC_GROUP) {
        assert_eq!(
            p.darwin_triples(),
            [PIN_TRIPLE],
            "{}: the group moves on one target",
            p.name
        );
    }
}

#[test]
fn the_trust_bundle_never_exposes_ty() {
    assert!(
        !TRUST_EXPOSES.contains(&"ty"),
        "the standalone ty pack owns that name"
    );
    assert!(programs::get("ty").is_some_and(|p| p.is_plain()));
}

#[test]
fn every_program_packs_for_the_pin_triple() {
    for p in PROGRAMS {
        assert!(
            p.darwin_triples().contains(&PIN_TRIPLE),
            "{} has no row where `pin` points",
            p.name
        );
    }
}
