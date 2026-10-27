// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! amacs's wire protocol: the `edit` request with its ticket replies and `await edit` (`edit`), and the file-URI codec (`uri`). std only, so a client (aterm-ctl) links no editor engine. amacs re-exports both as `amacs::protocol::{edit, uri}`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

// A contracted fn is written once, through `trust_contract_fn!`: under Trust's
// verifying session (`cfg(trust_verify)`) it carries its native `ensures`
// clauses, and every other compiler (stock rustc in the foreign cells) gets the
// plain fn and never parses them. Each clause cites a theorem stated over
// `trust_import_amacs_protocol__<module path>__<fn>`, the kernel definition Trust mints
// from the compiled body, so the proof is about this code, not a copy of it.
// The arms are crates/aterm-log/src/lib.rs's; crates/amacs/SPECS.md lists this crate's
// two contracts until its first lane run measures them again.
// Defined before the modules so every module can use them.
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

// A `clean { … }` proof island: only Trust lexes it, so every other compiler
// drops its tokens.
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

pub mod edit;
pub mod uri;
