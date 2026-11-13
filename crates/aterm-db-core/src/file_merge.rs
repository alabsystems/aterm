// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The shared-file merge kernels (docs/DESIGN-shared-files-2026-11-11.md §2, §4 "Publisher",
//! §5 rule 1; model `SharedFile`, `aterm_models_db::shared_file_model`).
//!
//! Each kernel is its island's definition (`file_merge_settle_island.rs`,
//! `file_merge_owned_island.rs`), which proves its law for inputs of any size on every Trust
//! build. The kernels take slices and options, which the compiler does not import today, so no
//! clause binds them: the tests hold each to the answer sheet its island proves.

/// The digest of one operation, which names one candidate of a register.
pub type Digest = [u8; 32];

/// A Commons machine uuid.
pub type Uuid = [u8; 16];

/// Whether every digest of `of` is one of `set`.
fn covers(set: &[Digest], of: &[Digest]) -> bool {
    of.iter().all(|digest| set.contains(digest))
}

/// Whether the publisher writes an intent's cell: exactly when the candidates the register
/// holds `now` are, as a set, the candidates the intent observed (`expected`), so the write
/// replaces just what its author saw. Order and repeats count for nothing. A register that
/// gained a candidate the intent never observed, or lost one it did, has moved, and its intent
/// is held.
#[must_use]
pub fn settle(now: &[Digest], expected: &[Digest]) -> bool {
    covers(now, expected) && covers(expected, now)
}

/// Whether a mine row or a log entry projects: exactly when its key names a machine uuid
/// (`key`) and the record's signer resolved to that same machine (`signer`). A key that names
/// no machine and a signer that resolved to none are both inert, each whatever the other is.
#[must_use]
pub fn eligible_owned(key: Option<Uuid>, signer: Option<Uuid>) -> bool {
    match (key, signer) {
        (Some(key), Some(signer)) => key == signer,
        _ => false,
    }
}

#[cfg(test)]
#[path = "file_merge_tests.rs"]
mod tests;
