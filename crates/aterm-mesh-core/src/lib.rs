// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pure decision kernels of the one mesh, each held to a Clean island that proves its safety law
//! for every size (`docs/DESIGN-one-mesh-proofs-2026-11-05.md`).
//!
//! The crate has no dependencies, no proc macros and no anchors, so every Trust build and every
//! `verify` elaborates its spec islands (`cfg(clean_islands)`). A decision is a scalar `contract!`
//! kernel whose clause cites an island definition (`ensures result == spec_k(..) by k_is_exact`);
//! the binding islands, which prove each compiled kernel equal to its spec and close each law for
//! the World that asks the compiled kernels, are checked where Trust imports function bodies:
//! the Trust lane (`targo --unverified ship trust run -p aterm-mesh-core`, `cfg(trust_verify)`)
//! until TG12 keeps them in every build. The folds over a view are plain Rust held to the island
//! by an answer sheet that a test recomputes on every build, stock and cross builds included.
//!
//! Each property owns one module, its spec island (`<prop>_island.rs`, mounted from inside the
//! module) and its binding island (`<prop>_binding_island.rs`, mounted at the end of this file),
//! so the steps that prove them edit disjoint files but for one mount line here.
#![forbid(unsafe_code)]

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

// The temporal prelude every property's island opens (`AtermMeshTemporal`), mounted first.
#[cfg(clean_islands)]
#[rustfmt::skip]
#[path = "temporal_island.rs"]
mod temporal;

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
