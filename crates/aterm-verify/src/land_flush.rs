// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Explicit delivery of one existing spool snapshot, independent of a landing run.

use std::path::Path;

use crate::land_record::Signed;
use crate::land_spool::Spool;

/// The acknowledged snapshot and work that could not be completed.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Flushed {
    /// Valid records offered to an append that acknowledged them under its existing
    /// retention and first-FAIL rules, not a count of newly created journal entries.
    pub appended: usize,
    /// Snapshot files or record bodies that did not read and remain spooled.
    pub invalid: Vec<String>,
    /// Acknowledged records whose byte-conditional cleanup failed.
    pub cleanup: Vec<String>,
    /// Cancellation observed after acknowledgment; all snapshot cleanup was still attempted.
    pub interrupted: bool,
}

impl Flushed {
    /// Whether this captured snapshot completed, independently of later spool arrivals.
    pub(super) fn complete(&self) -> bool {
        self.invalid.is_empty() && self.cleanup.is_empty() && !self.interrupted
    }
}

/// Deliver only the records already present in one snapshot. No signer, run or new record is
/// needed. Invalid entries remain in place; a successful append permits cleanup only through
/// [`Spool::done`], which preserves a concurrent replacement's different bytes. Cleanup remains
/// finite even when more records arrive or cancellation follows acknowledgment.
///
/// The caller supplies the existing append unchanged, including its admission deadline,
/// mutation owner and unknown-outcome policy. This function never retries that append.
///
/// # Errors
/// Cancellation before an append, an unreadable spool listing, or an append refusal. No
/// snapshot cleanup happens unless the append acknowledges it.
pub(super) fn flush(
    root: &Path,
    spool: &dyn Spool,
    append: &mut dyn FnMut(&Path, &[Signed], u64) -> Result<(), String>,
    now: u64,
    stopped: &dyn Fn() -> bool,
) -> Result<Flushed, String> {
    let check = || {
        if stopped() {
            Err("record flush interrupted before publication".to_owned())
        } else {
            Ok(())
        }
    };
    check()?;
    let pending = spool.pending()?;
    check()?;
    let mut result = Flushed {
        invalid: pending
            .unreadable
            .into_iter()
            .map(|(file, why)| format!("{} ({why})", file.display()))
            .collect(),
        ..Flushed::default()
    };
    let mut records = Vec::new();
    for signed in pending.records {
        check()?;
        match super::unreadable(&signed) {
            Some(why) => result.invalid.push(format!("{} ({why})", signed.path)),
            None => records.push(signed),
        }
    }
    check()?;
    if records.is_empty() {
        return Ok(result);
    }
    append(root, &records, now)?;
    result.appended = records.len();
    result.interrupted = stopped();
    for signed in &records {
        if let Err(why) = spool.done(signed) {
            result.cleanup.push(format!("{} ({why})", signed.path));
        }
    }
    result.interrupted |= stopped();
    Ok(result)
}

#[cfg(test)]
#[path = "land_flush_tests.rs"]
mod tests;
