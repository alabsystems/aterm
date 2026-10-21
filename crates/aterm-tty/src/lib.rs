// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Terminal I/O for a program that draws on its controlling terminal, with no
//! editor knowledge (docs/DESIGN-amacs-2026-10-19.md §5).
//!
//! amacs's terminal face is its first user, but nothing here knows about
//! documents or commands. It is filled slice by slice:
//!
//! - **T1**: [`decode`], a pure byte-to-event decoder for keys (legacy C0,
//!   ESC as Alt, `CSI … ~`, SS3 and kitty CSI-u at flags 1 and 4), SGR mouse
//!   reports, bracketed paste, focus and probe replies, proved by round-trips
//!   through aterm's own encoders;
//! - **T2**: [`tty`] (the terminal a program reads and draws on: the
//!   controlling terminal, or an in-memory one on a virtual clock for tests),
//!   `raw` (raw mode and the private modes, put back on drop, panic, SIGINT,
//!   SIGTERM, SIGHUP and suspension), [`signals`] (SIGWINCH, SIGTSTP and SIGCONT
//!   through a self-pipe), [`probe`] (the capability probe) and [`screen`] (the
//!   diffing writer, checked against aterm's own grid);
//! - **T4**: `windows`, the Windows console arm.
//!
//! The decoder's byte classes are Trust contracts, each a Clean theorem about the
//! compiled function (SPECS.md).

// A contracted fn is written once, through `trust_contract_fn!` (the idiom of
// crates/aterm-log/src/lib.rs): under Trust's verifying session
// (`cfg(trust_verify)`) it carries its native `ensures` clause, and every other
// compiler (stock rustc in the foreign cells, Trust with verification off) gets the
// plain fn and never parses the clause.
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

// The theorems a contract cites are stated over `trust_import_aterm_tty__…`, the kernel
// definition Trust mints from the compiled body only while it verifies, so their island
// compiles under `cfg(trust_verify)` alone.
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

// The specs themselves (Clean definitions of the standards' tables, and the lemmas the
// proofs use) name no import, so they are kernel-checked at every Trust build:
// `cfg(clean_islands)` is set by this crate's build script whenever the compiler is
// Trust (crates/aterm-log/clean_islands.rs). A stock rustc parses a cfg'd-out item's
// body all the same, so the tokens go through a macro that drops them there.
#[cfg(clean_islands)]
macro_rules! spec_island {
    ($($tt:tt)*) => {
        $($tt)*
    };
}

#[cfg(not(clean_islands))]
macro_rules! spec_island {
    ($($tt:tt)*) => {};
}

pub mod decode;
pub mod probe;
#[cfg(unix)]
pub mod raw;
pub mod screen;
pub mod signals;
pub mod tty;
