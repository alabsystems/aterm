// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! amacs's document I/O, shared by both faces: the native Editor view aterm-gui
//! hosts, and the terminal face.
//!
//! - [`io`] performs no I/O: the save and draft-journal plans, the reducers that
//!   accept a host's outcome only against the still-pending plan's proof, and the
//!   journal's codec and keyed identity.
//! - [`host`] is the file-capability host: admission of one `file:` URI into a
//!   grant (bounded UTF-8 read, symlink refusal), the atomic save (same-directory
//!   temporary, file sync, rename, directory sync) and the advisory-lock door
//!   every `flock` contender goes through.
//!
//! aterm-gui re-exports both under their old names (`native_document_io`,
//! `native_document_host`). Its crash journal (`native_document_journal`) stays
//! there until the terminal face keeps drafts (docs/DESIGN-amacs-2026-10-19.md
//! §4.7).

// A save or lock step riding inside a `debug_assert!` vanishes from a release
// build: the lint refuses the shape, as it does in aterm-gui, where this code
// used to live.
#![deny(clippy::debug_assert_with_mut_call)]

// A contracted fn is written once, through `trust_contract_fn!`: under Trust's
// verifying session (`cfg(trust_verify)`) it carries its native `ensures`
// clause, and every other compiler (stock rustc in the foreign cells) gets the
// plain fn and never parses the clause. Each cited theorem is stated over
// `trust_import_amacs_files__<module>__<fn>`, the kernel definition Trust mints
// from the compiled body, so the proof is about this code, not a copy of it
// (SPECS.md lists them). The arms are aterm-log's (crates/aterm-log/src/lib.rs),
// and the modules below see them because they are defined first.
#[cfg(trust_verify)]
macro_rules! trust_contract_fn {
    (
        $(#[$meta:meta])*
        $vis:vis fn $name:ident($($arg:ident : $arg_ty:ty),* $(,)?) -> $ret:ident
        clauses [$($clauses:tt)+]
        { $($body:tt)* }
    ) => {
        $(#[$meta])*
        $vis fn $name($($arg: $arg_ty),*) -> $ret
            $($clauses)+
        { $($body)* }
    };
}

#[cfg(not(trust_verify))]
macro_rules! trust_contract_fn {
    (
        $(#[$meta:meta])*
        $vis:vis fn $name:ident($($arg:ident : $arg_ty:ty),* $(,)?) -> $ret:ident
        clauses [$($clauses:tt)+]
        { $($body:tt)* }
    ) => {
        $(#[$meta])*
        $vis fn $name($($arg: $arg_ty),*) -> $ret { $($body)* }
    };
}

#[cfg(trust_verify)]
macro_rules! verify_island {
    ($($tt:tt)*) => {
        $($tt)*
    };
}

#[cfg(not(trust_verify))]
macro_rules! verify_island {
    ($($tt:tt)*) => {};
}

pub mod host;
pub mod io;

/// A document store whose write capability a test mints: a test process that
/// handles no untrusted input is its own trusted launcher.
#[cfg(test)]
pub(crate) fn test_document_store() -> amacs::store::DocumentStore {
    // SAFETY: the `root_authority` contract is "trusted launcher, before
    // untrusted input", which a unit test satisfies.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    amacs::store::DocumentStore::new(authority.grant(aterm_cap::Tier::Trusted))
}
