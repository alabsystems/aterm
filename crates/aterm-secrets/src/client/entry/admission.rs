// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A local admission point protected against cooperating vault writers.

use super::*;

/// A current-entry check distinguishes temporary contention from a changed or
/// unavailable source. None of the refusal variants invokes the callback.
#[derive(Debug)]
pub enum EntryAdmission<T> {
    /// The callback ran with the shared vault state guard held. The value is its
    /// result, including any application error it chose to return.
    Accepted(T),
    /// A vault writer holds the state lock, or the lock path changed during its
    /// acquisition. Retry after releasing any application writer.
    Busy,
    /// No authenticated, safely materialized entry is currently available in
    /// the captured trust context. No state or missing lock file was created.
    Unavailable,
    /// A readable entry no longer continues the captured bytes, policy,
    /// recipient or history. Replan from a fresh observation.
    Changed,
}

#[cfg(test)]
std::thread_local! {
    static RELEASE_BEFORE_ADMISSION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

impl EntrySnapshot {
    /// Revalidate an ordinary materialized entry and retain its shared vault
    /// guard through `admit`. Acquisition is nonblocking. An unrelated signed
    /// update may pass; changed bytes, policy, recipient or a history rollback
    /// cannot continue this snapshot.
    ///
    /// An application may durably record a prepared local intent in `admit`.
    /// Take its own writer **before** calling this method; release that writer
    /// before retrying a refusal. The callback must not acquire a vault writer,
    /// wait for another application writer, sign in, contact a provider or send
    /// network traffic. Prepare inputs before admission, persist the intent in
    /// the callback, then replay or publish it after this method returns.
    /// Source-revision policy must inspect the fresh entry passed to the
    /// callback. Byte and policy continuity alone does not preserve a verdict
    /// about semantic provenance computed before acquiring the guard.
    ///
    /// This orders admission against cooperating local vault writers only. It
    /// grants no provider access, budget, remote freshness or authority after
    /// the callback returns. It does not protect against uncoordinated file
    /// edits. Reserved and typed-token entries cannot use this ordinary API.
    ///
    /// # Errors
    /// The name is invalid or reserved, or the local state, signed cache,
    /// decryption or materialized file cannot be safely read. Errors and unwinds
    /// release the guard; a callback's durable effects are its responsibility.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "DbEntryAdmission",
            action = "Case",
            reason = "The environment selects the captured source and competing writer scenario."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbEntryAdmission",
            action = "Admit",
            project = "aterm_secrets::client::entry::admission::tests::transition"
        )
    )]
    pub fn admit<T>(
        &self,
        paths: &Paths,
        admit: impl FnOnce(&VerifiedEntry) -> T,
    ) -> crate::Result<EntryAdmission<T>> {
        validate_reading(&self.name, Reading::Materialized)?;
        if self.config.signer.is_none() {
            return Ok(EntryAdmission::Unavailable);
        }
        let lock = match StateLock::observe_classified(paths)? {
            StateObservation::Busy => return Ok(EntryAdmission::Busy),
            StateObservation::Absent => return Ok(EntryAdmission::Unavailable),
            StateObservation::Held(lock) => lock,
        };
        let observation = Observing::new(lock, paths);
        let Some((entry, _)) = read_entry_with_version_held(
            paths,
            &self.config,
            &self.name,
            Reading::Materialized,
            &mut None,
            observation.lock(),
        )?
        else {
            return Ok(EntryAdmission::Unavailable);
        };
        if !entry.snapshot.continues(self) {
            return Ok(EntryAdmission::Changed);
        }
        #[cfg(test)]
        let observation = if RELEASE_BEFORE_ADMISSION.get() {
            drop(observation);
            None
        } else {
            Some(observation)
        };
        let result = admit(&entry);
        drop(observation);
        Ok(EntryAdmission::Accepted(result))
    }
}

#[cfg(test)]
mod tests;
