// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! This copy's update record (`Updates/record.json`, [`aterm_update_core::update_record`]) as
//! the macOS updater reads and writes it: the gates ask [`held`], and every writer goes through
//! [`edit`]. An unreadable record holds nothing (the gates fail open: a trial is bounded by its
//! launches, so a lost hold costs one more trial, never a copy that stops updating).

use aterm_update_core::update_record::{self, Cause, Failure, Hold, Sha, UpdateRecord, Witness};

use crate::paths::Staging;

/// The digest `hex` names, or `None` for one that is not a SHA-256.
#[must_use]
pub(crate) fn sha(hex: &str) -> Option<Sha> {
    Sha::parse(hex.trim())
}

/// This copy's record (a fresh one where there is none or it is unreadable).
#[must_use]
pub(crate) fn load(staging: &Staging) -> UpdateRecord {
    UpdateRecord::load(&staging.record())
}

/// The live hold on the artifact `(build, sha256)` at `now`, if one holds it.
#[must_use]
pub(crate) fn held(staging: &Staging, build: u64, sha256: &str, now: u64) -> Option<Hold> {
    let sha = sha(sha256)?;
    let record = load(staging);
    record
        .holds(build, &sha, now)
        .then_some(record.hold)
        .flatten()
}

/// Change the record with `change` ([`UpdateRecord::edit`]); a write that fails is logged, and
/// `None` returned: the change is lost, which at worst lets a held build be tried again.
pub(crate) fn edit<R>(
    staging: &Staging,
    what: &str,
    change: impl FnOnce(&mut UpdateRecord) -> R,
) -> Option<(UpdateRecord, R)> {
    match UpdateRecord::edit(&staging.record(), change) {
        Ok(done) => Some(done),
        Err(error) => {
            crate::warn(&format!(
                "could not write the update record ({what}): {error}"
            ));
            None
        }
    }
}

/// The artifact `(build, sha256)` of `version` failed to download, verify, stage or start as
/// a candidate, for `why`: hold it on the stage schedule
/// ([`aterm_update_core::update_record::STAGE_SPANS`]). A failure of the artifact already held,
/// while its hold holds, changes nothing. Returns the hold that now stands on it, if one does.
pub(crate) fn stage_failed(
    staging: &Staging,
    build: u64,
    version: &str,
    sha256: &str,
    why: &str,
    now: u64,
) -> Option<Hold> {
    let sha = sha(sha256)?;
    let (record, _) = edit(staging, "hold a failed stage", |record| {
        record.set_hold(
            Failure {
                build,
                version: version.to_owned(),
                sha,
                cause: Cause::Stage,
                witness: Witness::Headless,
                why: why.to_owned(),
            },
            now,
        )
    })?;
    record
        .hold
        .filter(|hold| hold.build == build && hold.sha == sha && hold.live(now))
}

/// When `hold` lapses, as a local clock time ([`update_record::clock_words`]).
#[must_use]
pub(crate) fn until_words(hold: &Hold) -> String {
    update_record::clock_words(hold.until, crate::unix_now())
}

/// Why a gate refuses `hold`'s artifact, in words that end with what ends it.
#[must_use]
pub(crate) fn refusal(hold: &Hold) -> String {
    let version = if hold.version.is_empty() {
        String::new()
    } else {
        format!(" ({})", hold.version)
    };
    format!(
        "build {}{version} is held until {} ({}, try {}): a newer build, `aterm update retry` \
         or that time ends the hold",
        hold.build,
        until_words(hold),
        hold.why,
        hold.tries
    )
}

#[cfg(test)]
#[path = "hold_tests.rs"]
mod tests;
