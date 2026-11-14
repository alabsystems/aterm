// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! SHARED FILES, AS EVERY PROGRAM READS THEM (docs/DESIGN-shared-files-2026-11-11.md).
//!
//! A shared file is a named table every Mac of the lab holds a copy of. Its **replica** is
//! Commons' signed log, which aterm-db owns; its **snapshot** is the last verified projection of
//! that log, a plain local file; its **face** is the TSV a person opens. This crate is the
//! snapshot's side: the name grammar ([`Name`]), where the parts live ([`Root`]), the snapshot
//! and status codecs ([`Snapshot`], [`State`]), the state word every answer carries
//! ([`Word`], [`Why`]), the read ([`File::read`]), the outbox of this Mac's unsigned writes
//! ([`outbox`], [`File::write`]) and the verbs of `aterm file` that need no typed table
//! ([`cli`]).
//!
//! It does not depend on aterm-db, aterm-secrets or the transport, so no session, vault or
//! writer-generation code can reach a read: a read answers from local files in every custody
//! state, and its only words for the replica's condition are `live`, `held`, `never` and `off`.
//! aterm-db's materializer is the one writer of a snapshot, through [`File::record`], which
//! holds the rules a snapshot is replaced under; aterm-db's publisher is the one reader of the
//! outbox that signs, and it trusts nothing it finds there.

#![forbid(unsafe_code)]
// On a Trust compiler the `SharedFile` anchor is a `macro_rules!` attribute
// (crates/aterm-spec-anchor/anchor_rules.rs), which only test and debug builds expand.
#![cfg_attr(all(anchor_rules, any(test, debug_assertions)), feature(macro_attr))]

pub mod cli;
pub mod file;
pub mod frontier;
pub mod layout;
pub mod name;
pub mod outbox;
pub mod snapshot;
pub mod status;

pub use file::{File, Opened, Pass, Projected, Read, Recorded};
pub use frontier::Frontier;
pub use layout::Root;
pub use name::{Name, NotAName};
#[cfg(any(unix, windows))]
pub use outbox::Claim;
pub use outbox::{
    Accepted, Done, Draft, Dropped, Edit, Held, Intent, Mark, Outbox, OwnerAct, Refused,
};
pub use snapshot::{Column, Row, Snapshot, Source};
pub use status::{Face, Kept, State, Status, Why, Word};

/// The most bytes of a face a pass writes: the grid's limit (`amacs::table::MAX_TABLE_BYTES`).
/// A larger table keeps its snapshot and has no face: a pass removes the face it wrote.
pub const MAX_FACE_BYTES: u64 = 4 * 1024 * 1024;

/// `aterm file`'s exit codes (section 3).
pub mod exit {
    /// The verb answered.
    pub const OK: u8 = 0;
    /// The words name no verb, no file, no row or no column.
    pub const USAGE: u8 = 2;
    /// Problems, with nothing queued or recorded.
    pub const PROBLEMS: u8 = 3;
    /// A conflict was recorded.
    pub const CONFLICT: u8 = 4;
}

#[cfg(test)]
mod tests;
