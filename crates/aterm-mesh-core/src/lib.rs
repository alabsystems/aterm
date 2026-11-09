// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pure decision kernels of the one mesh, each held to a Clean island that proves its safety law
//! for every size (`docs/DESIGN-one-mesh-proofs-2026-11-05.md`).
//!
//! The crate has no dependencies, no proc macros and no anchors (PM-1). Its spec islands are
//! elaborated in `aterm-mesh-core-islands` (`cfg(clean_islands)`), which every build of the `aterm`
//! package, a bare build and this crate's tests check and nothing on the mesh's compile chain waits
//! for (its lib.rs says why): the spec islands name no Rust, so outside the Trust lane this crate's
//! compile carries none of them. A decision is a scalar `contract!` kernel whose clause cites an
//! island definition (`ensures result == spec_k(..) by k_is_exact`); the binding islands, which
//! prove each compiled kernel equal to its spec and close each law for the World that asks the
//! compiled kernels, are checked where Trust imports function bodies: the Trust lane (`targo
//! --unverified ship trust run -p aterm-mesh-core`, `cfg(trust_verify)`), which mounts the spec
//! islands here too, until TG12 keeps them in every build. The folds over a view are plain Rust
//! held to the island by an answer sheet that a test recomputes on every build, stock and cross
//! builds included; the tests depend on `aterm-mesh-core-islands`, so a test build checks the
//! islands whose text they read.
//!
//! Each property owns one module, its spec island (`<prop>_island.rs`, a line of
//! `aterm-mesh-core-islands`' `islands.rs`) and its binding island (`<prop>_binding_island.rs`,
//! mounted at the end of this file), so the steps that prove them edit disjoint files but for that
//! line and one mount here.
#![forbid(unsafe_code)]

// The answer-sheet tests read the islands' text: a test build checks the islands first.
#[cfg(test)]
use aterm_mesh_core_islands as _;

#[cfg(trust_verify)]
macro_rules! verify_island { ($($body:tt)*) => {$($body)*}; }
#[cfg(not(trust_verify))]
macro_rules! verify_island {
    ($($body:tt)*) => {};
}

#[cfg(trust_verify)]
macro_rules! contract {
    ($(#[$meta:meta])* $vis:vis fn $name:ident($($arg:ident : $ty:ty),*) -> $ret:ident
     clauses [$($clauses:tt)+] {$($body:tt)*}) => {
        $(#[$meta])* $vis fn $name($($arg:$ty),*) -> $ret $($clauses)+ {$($body)*}
    };
}
#[cfg(not(trust_verify))]
macro_rules! contract {
    ($(#[$meta:meta])* $vis:vis fn $name:ident($($arg:ident : $ty:ty),*) -> $ret:ident
     clauses [$($clauses:tt)+] {$($body:tt)*}) => {
        $(#[$meta])* $vis fn $name($($arg:$ty),*) -> $ret {$($body)*}
    };
}

// The spec islands on the Trust lane, where the contracts' clauses cite their spec kernels and the
// binding islands below build on them: `aterm-mesh-core-islands`' list, mounted first, which opens
// with the temporal prelude every property's island opens (`AtermMeshTemporal`).
#[cfg(trust_verify)]
#[path = "../../aterm-mesh-core-islands/src/islands.rs"]
mod spec;

pub mod revocation;

pub mod capacity;
pub mod convergence;
pub mod gap_heal;
pub mod inbox_move;
pub mod no_lost_edit;
pub mod relay;
pub mod relay_digest;
pub mod relay_live;

// Each property's binding island names `trust_import_` terms (the compiled kernels), which defers
// it and every island after it, so the binding islands come last, after every spec island.
verify_island! {
    #[rustfmt::skip]
    #[path = "revocation_binding_island.rs"]
    mod revocation_binding;
    #[rustfmt::skip]
    #[path = "revocation_no_leak_binding_island.rs"]
    mod revocation_no_leak_binding;
    #[rustfmt::skip]
    #[path = "capacity_binding_island.rs"]
    mod capacity_binding;
}
verify_island! {
    #[rustfmt::skip]
    #[path = "relay_binding_island.rs"]
    mod relay_binding;
}
verify_island! {
    #[rustfmt::skip]
    #[path = "no_lost_edit_binding_island.rs"]
    mod no_lost_edit_binding;
}
verify_island! {
    #[rustfmt::skip]
    #[path = "relay_live_binding_island.rs"]
    mod relay_live_binding;
}
verify_island! {
    #[rustfmt::skip]
    #[path = "inbox_move_binding_island.rs"]
    mod inbox_move_binding;
}
verify_island! {
    #[rustfmt::skip]
    #[path = "relay_digest_binding_island.rs"]
    mod relay_digest_binding;
}
verify_island! {
    #[rustfmt::skip]
    #[path = "convergence_binding_island.rs"]
    mod convergence_binding;
}
verify_island! {
    #[rustfmt::skip]
    #[path = "gap_heal_binding_island.rs"]
    mod gap_heal_binding;
}
