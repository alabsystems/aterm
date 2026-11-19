// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! SHARED FILES, AS EVERY PROGRAM READS THEM (docs/DESIGN-shared-files-2026-11-11.md).
//!
//! A shared file is a named table every Mac of the lab holds a copy of. Its **replica** is
//! Commons' signed log, which aterm-db owns; its **snapshot** is the last verified projection of
//! that log, a plain local file; its **face** is the TSV a person opens. This crate is the
//! snapshot's side: the name grammar ([`Name`]), where the parts live ([`Root`]), the snapshot
//! and status codecs ([`Snapshot`], [`State`]), the state word every answer carries
//! ([`Word`], [`Why`]), the read ([`File::read`]), the one resolver every row and account name
//! passes ([`resolve`]) with the account layer over it ([`account`]: an account's names and the
//! account a word names), the outbox of this Mac's unsigned writes ([`outbox`],
//! [`File::write`]) and the verbs of `aterm file` that need no typed table ([`cli`]).
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

pub mod account;
pub mod cli;
pub mod file;
pub mod frontier;
pub mod layout;
#[cfg(any(unix, windows))]
mod local;
pub mod name;
pub mod outbox;
pub mod rendered;
mod resolver;
pub mod retain;
pub mod snapshot;
pub mod status;

pub use account::AccountNames;
#[cfg(any(unix, windows))]
pub use file::Look;
pub use file::{File, Opened, Pass, Projected, Projection, Read, Recorded, Restored};
pub use frontier::{At, Frontier, Witnessed};
pub use layout::Root;
pub use name::{Name, NotAName};
#[cfg(any(unix, windows))]
pub use outbox::Claim;
#[cfg(any(test, debug_assertions))]
pub use outbox::owner_act_for_test;
pub use outbox::{
    Accepted, Done, Draft, Dropped, Edit, Held, Intent, Mark, Outbox, OwnerAct, Refused,
};
pub use rendered::Rendered;
pub use resolver::{ACCOUNT_HANDLES, Named, Resolved, many_accounts, no_account, resolve};
pub use snapshot::{Column, Row, Snapshot, Source};
pub use status::{Face, Kept, State, Status, Why, Word};

/// The most bytes of a face a pass writes: the grid's limit (`amacs::table::MAX_TABLE_BYTES`).
/// A table whose face would be larger shows fewer columns, its widest left out
/// ([`snapshot::face_columns`]); every column stays in the snapshot and in `aterm file export`.
/// Only where the key alone does not fit, or the rows pass [`MAX_FACE_RECORDS`], is no face
/// written: a pass removes the face it wrote.
pub const MAX_FACE_BYTES: u64 = 4 * 1024 * 1024;
/// An editable TSV body uses the same file bound as the snapshot it represents.
pub const MAX_EXPORT_BODY_BYTES: u64 = snapshot::MAX_BYTES;
/// The edit planner bounds parsed cells, including the header and empty positions.
pub const MAX_EXPORT_CELLS: usize = 4_194_304;
/// An editable TSV extends its revision header by a dot, two hex digests and a plus sign.
pub const MAX_EXPORT_BYTES: u64 = MAX_EXPORT_BODY_BYTES + 1 + 64 + 1 + 64;

/// The most records of a face, its header included: the grid's limit
/// (`amacs::table::MAX_TABLE_RECORDS`). A table with more rows has no face.
pub const MAX_FACE_RECORDS: usize = 100_000;

/// The most cells of a face's record: the grid's limit (`amacs::table::MAX_TABLE_COLUMNS`). A
/// table with more columns shows fewer, its widest left out ([`snapshot::face_columns`]).
pub const MAX_FACE_COLUMNS: usize = 1024;

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
