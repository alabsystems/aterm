// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A window-start failure: the one exact artifact, by build and DMG digest, that a window here
//! launched as its successor and saw fail to start. It is the update record's
//! [`start_failure`](aterm_update_core::update_record::UpdateRecord::start_failure)
//! ([`Cause::StartFailed`](aterm_update_core::update_record::Cause::StartFailed),
//! `Updates/record.json`), not a file of its own and not the record's hold slot: it expires on
//! the window's crash schedule (4 h, then 24 h), `aterm update retry` cuts it short, and nothing
//! that ends or replaces the hold (a newer offer, a build observed running, a hold on other
//! bytes, a peer's proof) touches it, since none of them says why those bytes failed to start
//! here.
//!
//! The window that ran the handoff writes it, so it outlives that window. Only windowless
//! parties are held by it: while it holds, a headless boot apply and the node job's look leave
//! those bytes staged for a window (`headless_refusal`, `staged_for_a_start`), and the supersede
//! chain over a failed activation does not install them. A window lane never reads it as a
//! hold: a window of those bytes that presents withdraws it, as does a Window-witnessed settle
//! of their trial. A lapsed one stays as the record's history: it no longer holds anything, but
//! it is still evidence, so a headless settle of a trial of those bytes keeps a window re-trial
//! at the count it carried (`names`). A window-start failure of other bytes replaces it.

use serde::{Deserialize, Serialize};

/// One artifact a window here saw fail to start, as `aterm ctl update status` shows it: a view
/// of the update record's `start_failure`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowStartFailure {
    /// The build the bytes are.
    pub build: u64,
    /// SHA-256 of the DMG the bytes came from, canonical lowercase hex.
    pub dmg_sha256: String,
    /// When the window booked the failure (RFC3339 UTC).
    pub at: String,
    /// The build the window that saw it runs.
    pub by_build: u64,
    /// How the successor failed, in the window's diagnostic words; never a policy input.
    pub signature: String,
}

/// The bytes a window's successor was launched from, as the window names them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailedStartBytes<'a> {
    /// A downloaded artifact: its build and the DMG digest the stage, the trial and the receipt
    /// key it by (any case).
    Download {
        /// The artifact's build.
        build: u64,
        /// Its DMG's SHA-256, hex.
        dmg_sha256: &'a str,
    },
    /// The build already installed in the window's place (an activation): its digest is the one
    /// the installed receipt names for that build, or there is none to name.
    Installed {
        /// The installed build.
        build: u64,
    },
}

/// The most characters of a signature the record keeps.
#[cfg(target_os = "macos")]
const MAX_SIGNATURE_CHARS: usize = 120;

impl WindowStartFailure {
    /// Whether this record names exactly `build` with `dmg_sha256` (hex, any case).
    #[must_use]
    pub fn names(&self, build: u64, dmg_sha256: &str) -> bool {
        self.build == build && self.dmg_sha256.eq_ignore_ascii_case(dmg_sha256.trim())
    }
}

#[cfg(target_os = "macos")]
impl WindowStartFailure {
    /// The view of the record's window-start failure `hold`.
    fn of(hold: &aterm_update_core::update_record::Hold) -> Self {
        Self {
            build: hold.build,
            dmg_sha256: hold.sha.hex(),
            at: aterm_types::rfc3339::format_rfc3339(hold.at),
            by_build: hold.by,
            signature: hold.why.clone(),
        }
    }
}

/// The `(build, digest)` key `bytes` names: `Ok(None)` for an installed build the installed
/// receipt does not name (a bundle aterm-update did not install), whose bytes no record can
/// name. An installed build's digest is the installed receipt's for that build, never a digest
/// made up for it.
///
/// # Errors
/// A download's build is 0 or its digest is not a SHA-256.
#[cfg(target_os = "macos")]
fn key_of(
    staging: &crate::paths::Staging,
    bytes: FailedStartBytes<'_>,
) -> Result<Option<(u64, aterm_update_core::update_record::Sha)>, String> {
    match bytes {
        FailedStartBytes::Download { build, dmg_sha256 } => crate::hold::sha(dmg_sha256)
            .filter(|_| build != 0)
            .map(|sha| Some((build, sha)))
            .ok_or_else(|| format!("build {build}'s digest {dmg_sha256:?} is not a SHA-256")),
        FailedStartBytes::Installed { build } => Ok(crate::manifest::InstalledReceipt::read(
            &staging.installed_receipt(),
        )
        .filter(|receipt| receipt.build_number == build)
        .and_then(|receipt| crate::hold::sha(&receipt.dmg_sha256))
        .map(|sha| (build, sha))),
    }
}

/// Book that a window running `by_build` saw `bytes` fail to start at `now` (unix seconds), as
/// the update record's window-start failure ([`UpdateRecord::start_failed`]), durably: a logout
/// or restart right after is the case it serves. The record's hold, whatever it holds, is left
/// as it is and never keeps the booking out; the same bytes already booked and still held are
/// booked already. Nothing is written when `bytes` names no artifact.
///
/// [`UpdateRecord::start_failed`]: aterm_update_core::update_record::UpdateRecord::start_failed
///
/// # Errors
/// `bytes` names no artifact, or the lock or the write failed: the record is as it was.
#[cfg(target_os = "macos")]
pub(crate) fn record(
    staging: &crate::paths::Staging,
    by_build: u64,
    bytes: FailedStartBytes<'_>,
    signature: &str,
    now: u64,
) -> Result<(), String> {
    let (build, sha) = key_of(staging, bytes)?.ok_or_else(|| match bytes {
        FailedStartBytes::Installed { build } | FailedStartBytes::Download { build, .. } => {
            format!("the installed receipt names no artifact of build {build}")
        }
    })?;
    let signature: String = signature
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(MAX_SIGNATURE_CHARS)
        .collect();
    let failure = aterm_update_core::update_record::Failure {
        build,
        version: String::new(),
        sha,
        cause: aterm_update_core::update_record::Cause::StartFailed,
        witness: aterm_update_core::update_record::Witness::Window,
        why: signature,
    };
    aterm_update_core::update_record::UpdateRecord::edit(&staging.record(), |record| {
        record.start_failed(failure, by_build, now);
    })
    .map(|_| ())
    .map_err(|error| format!("write the update record's window-start failure: {error}"))
}

/// End the window-start failure if it names exactly `bytes`, live or lapsed (a window of those
/// bytes started, [`UpdateRecord::end_start_failure`]): `Ok(true)` when it did, `Ok(false)`
/// when it names other bytes or there is none, or `bytes` is an installed build the installed
/// receipt does not name, which no record can name.
///
/// [`UpdateRecord::end_start_failure`]: aterm_update_core::update_record::UpdateRecord::end_start_failure
///
/// # Errors
/// A download's build or digest is malformed, or the lock or the write failed: nothing ended.
#[cfg(target_os = "macos")]
pub(crate) fn withdraw(
    staging: &crate::paths::Staging,
    bytes: FailedStartBytes<'_>,
) -> Result<bool, String> {
    let Some((build, sha)) = key_of(staging, bytes)? else {
        return Ok(false);
    };
    aterm_update_core::update_record::UpdateRecord::edit(&staging.record(), |record| {
        record.end_start_failure(build, &sha)
    })
    .map(|(_, ended)| ended)
    .map_err(|error| format!("write the update record: {error}"))
}

/// [`withdraw`] by a lane that judged `build`'s bytes `dmg_sha256` (`why`: a window settled
/// their trial), said when it ended the hold and warned when it could not.
#[cfg(target_os = "macos")]
pub(crate) fn withdraw_judged(
    staging: &crate::paths::Staging,
    build: u64,
    dmg_sha256: &str,
    why: &str,
) {
    match withdraw(staging, FailedStartBytes::Download { build, dmg_sha256 }) {
        Ok(true) => crate::log(&format!(
            "build {build}'s bytes, which a window here saw fail to start, were judged ({why}): \
             the hold for that failure is ended"
        )),
        Ok(false) => {}
        Err(error) => crate::warn(&format!(
            "build {build}'s bytes were judged ({why}), but the hold for a window here having \
             seen them fail to start could not be ended: {error}"
        )),
    }
}

/// The record's window-start failure, live or lapsed: `Ok(None)` when the record is absent, not
/// a record, or names none; `Err` when a present file
/// could not be read. Read closed: unlike the gates every lane asks ([`crate::hold::held`]),
/// whose unreadable record holds nothing, a windowless lane defers while it cannot tell.
///
/// # Errors
/// The record file is present and the read did not get through it.
#[cfg(target_os = "macos")]
pub(crate) fn read(staging: &crate::paths::Staging) -> Result<Option<WindowStartFailure>, String> {
    Ok(slot(staging)?.as_ref().map(WindowStartFailure::of))
}

/// The record's window-start failure, live or lapsed, as [`read`] reads it.
#[cfg(target_os = "macos")]
fn slot(
    staging: &crate::paths::Staging,
) -> Result<Option<aterm_update_core::update_record::Hold>, String> {
    let path = staging.record();
    match aterm_update_core::update_record::UpdateRecord::read(&path) {
        Ok(record) => Ok(record.and_then(|record| record.start_failure)),
        Err(error) if error.kind() == std::io::ErrorKind::InvalidData => Ok(None),
        Err(error) => Err(format!("read {}: {error}", path.display())),
    }
}

/// The window-start failure when it names exactly `build` with `dmg_sha256` and holds now:
/// what holds the windowless lanes.
///
/// # Errors
/// As [`read`].
#[cfg(target_os = "macos")]
pub(crate) fn held(
    staging: &crate::paths::Staging,
    build: u64,
    dmg_sha256: &str,
) -> Result<Option<WindowStartFailure>, String> {
    let now = crate::install::unix_now_secs();
    Ok(slot(staging)?
        .filter(|hold| hold.live(now))
        .as_ref()
        .map(WindowStartFailure::of)
        .filter(|failure| failure.names(build, dmg_sha256)))
}

/// Whether the window-start failure names exactly `build` with `dmg_sha256` and holds now.
///
/// # Errors
/// As [`read`].
#[cfg(target_os = "macos")]
pub(crate) fn holds(
    staging: &crate::paths::Staging,
    build: u64,
    dmg_sha256: &str,
) -> Result<bool, String> {
    held(staging, build, dmg_sha256).map(|failure| failure.is_some())
}

/// Whether the record names exactly `build` with `dmg_sha256` as bytes a window here saw fail to
/// start, live or lapsed: the evidence a headless settle carries their window re-trial on.
///
/// # Errors
/// As [`read`].
#[cfg(target_os = "macos")]
pub(crate) fn names(
    staging: &crate::paths::Staging,
    build: u64,
    dmg_sha256: &str,
) -> Result<bool, String> {
    Ok(read(staging)?.is_some_and(|failure| failure.names(build, dmg_sha256)))
}

/// A headless boot's answer for the admitted stage `ready`: `NoUpdate` for the bytes a window
/// here saw fail to start, which stay staged for a window, a passing `Deferred` when the record
/// cannot be read, and `None` to go on. Retires nothing and arms nothing; said once per
/// artifact per process.
#[cfg(target_os = "macos")]
pub(crate) fn headless_refusal(
    staging: &crate::paths::Staging,
    current_build: u64,
    ready: &crate::manifest::Ready,
) -> Option<crate::ApplyOutcome> {
    match held(staging, ready.build_number, &ready.dmg_sha256) {
        Ok(None) => None,
        Ok(Some(record)) => {
            static SAID: std::sync::Mutex<std::collections::BTreeSet<(u64, String)>> =
                std::sync::Mutex::new(std::collections::BTreeSet::new());
            let first = SAID
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert((record.build, record.dmg_sha256.clone()));
            if first {
                crate::log(&format!(
                    "boot apply: build {} (sha256 {}) is staged, and a window running build {} \
                     saw these exact bytes fail to start at {} ({}): a headless start leaves \
                     them staged for a window; a window that presents them, `aterm update \
                     retry` or the hold's lapse ends this",
                    record.build, record.dmg_sha256, record.by_build, record.at, record.signature
                ));
            }
            Some(crate::ApplyOutcome::NoUpdate)
        }
        Err(error) => Some(crate::install::records_unreadable_deferral(
            staging,
            current_build,
            ready,
            crate::manifest::records_unreadable(error),
        )),
    }
}

#[cfg(all(test, target_os = "macos"))]
#[path = "window_start_failure_tests.rs"]
mod tests;
