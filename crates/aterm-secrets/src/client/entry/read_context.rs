// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Ordinary entry reads borrowed from an existing local vault observation.

use super::*;

/// A candidate in the authenticated recipient view. A present entry whose
/// materialized file cannot be verified is unavailable, never absent.
#[derive(Debug)]
pub enum EntryRead {
    /// Authenticated bytes that match this entry's materialization policy.
    Present(Box<VerifiedEntry>),
    /// No such entry in the authenticated view visible to this recipient.
    Absent(Box<EntryVersion>),
    /// The source, custody or materialized entry cannot be verified.
    Unavailable,
}

/// A reader borrowed for one admission callback. It owns no lock and grants
/// no provider, budget or shared-table writing rights.
pub struct EntryReader<'a> {
    paths: &'a Paths,
    observation: &'a StateLock,
    version: VaultVersion,
}

impl std::fmt::Debug for EntryReader<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EntryReader").finish_non_exhaustive()
    }
}

impl EntryReader<'_> {
    /// Read an ordinary candidate under the same authenticated vault version
    /// that established this reader, without acquiring another vault-state lock or fetching data.
    /// `Absent` means absent from this recipient's verified view, not absent
    /// for every recipient. An unavailable result must not enable a fallback.
    ///
    /// # Errors
    /// The name is invalid or reserved, or its source cannot be safely read.
    pub fn observe(&self, name: &str) -> crate::Result<EntryRead> {
        validate_reading(name, Reading::Materialized)?;
        if !self.observation.observes(self.paths) {
            return Ok(EntryRead::Unavailable);
        }
        let mut version = None;
        let mut present = false;
        let entry = read_entry_held_observing(
            self.paths,
            &self.version.config,
            name,
            Reading::Materialized,
            &mut version,
            (self.observation, &mut present),
            &mut None,
        )?;
        let Some(version) = version else {
            return Ok(EntryRead::Unavailable);
        };
        #[cfg(test)]
        let same = MUTANT.get() == 4 || version.vault == self.version;
        #[cfg(not(test))]
        let same = version.vault == self.version;
        if !same {
            return Ok(EntryRead::Unavailable);
        }
        #[cfg(test)]
        let present = present && MUTANT.get() != 2;
        Ok(match entry {
            Some((entry, _)) => EntryRead::Present(Box::new(entry)),
            None if present => EntryRead::Unavailable,
            None => EntryRead::Absent(Box::new(version)),
        })
    }
}

impl VaultLook<'_> {
    /// Authenticate an ordinary entry reader without requiring a selected
    /// entry. A valid view with no entries can therefore support an all-absent
    /// decision. Unconfigured, incomplete or denied state does not enter the
    /// callback. This acquires no additional vault-state lock.
    /// Each candidate's materialization is checked by `EntryReader::observe`.
    ///
    /// Acquire the application writer before this look. The callback must read
    /// every candidate relevant to its decision and compare those facts with
    /// its prepared plan before persisting one bounded local intent. Do not
    /// reenter the vault, acquire another writer or use the network inside it.
    /// The caller retains the look after return, error or unwind; replay and
    /// dispatch follow custody release. This grants neither native catalog
    /// absence nor provider, budget or shared-table writing authority.
    ///
    /// # Errors
    /// The configured local source cannot be safely read or authenticated.
    /// A callback's durable effects remain its own.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "DbEntryView",
            action = "Case",
            reason = "The environment chooses candidate, custody and authentication state."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbEntryView",
            action = "Read",
            project = "aterm_secrets::client::entry::read_context::view_tests::transition"
        )
    )]
    pub fn with_entry_reader<T>(
        &self,
        paths: &Paths,
        expected: &Config,
        admit: impl FnOnce(&EntryReader<'_>) -> T,
    ) -> crate::Result<EntryAdmission<T>> {
        if expected.signer.is_none() {
            return Ok(EntryAdmission::Unavailable);
        }
        let Some((observation, _)) = self.held_at(paths) else {
            #[cfg(test)]
            if VIEW_MUTANT.get() == 2
                && let StateObservation::Held(lock) = StateLock::observe_classified(paths)?
            {
                return read_view_borrowed(paths, expected, &lock, admit);
            }
            return Ok(EntryAdmission::Unavailable);
        };
        #[cfg(test)]
        let current = VIEW_MUTANT.get() == 1 || observation.observes(paths);
        #[cfg(not(test))]
        let current = observation.observes(paths);
        if !current {
            return Ok(EntryAdmission::Unavailable);
        }
        read_view_borrowed(paths, expected, observation, admit)
    }
}

fn read_view_borrowed<T>(
    paths: &Paths,
    expected: &Config,
    observation: &StateLock,
    admit: impl FnOnce(&EntryReader<'_>) -> T,
) -> crate::Result<EntryAdmission<T>> {
    let version = match view::read(paths, expected, view::Request::View, observation)? {
        Some(view) => view.version,
        None => return Ok(EntryAdmission::Unavailable),
    };
    let reader = EntryReader {
        paths,
        observation,
        version,
    };
    Ok(EntryAdmission::Accepted(admit(&reader)))
}

impl EntrySnapshot {
    /// Admit this ordinary materialized entry using an already-held vault
    /// look. The callback may inspect other candidates through `EntryReader`
    /// and persist one prepared local intent. Missing or stale custody refuses
    /// without acquiring another vault-state lock. The caller retains ownership of the
    /// look, including when the callback errors or unwinds.
    ///
    /// Acquire the application writer before the vault look. Do not reenter
    /// the vault, acquire another writer, sign in or use the network in the
    /// callback. Replay and dispatch follow source-custody release. This is
    /// local admission against cooperating writers, not remote freshness or
    /// protection against arbitrary uncoordinated file edits.
    ///
    /// # Errors
    /// The entry name is invalid or reserved, or the local authenticated source
    /// cannot be safely read. A callback's durable effects remain its own.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "DbEntryReadContext",
            action = "Case",
            reason = "The environment selects source, custody and candidate changes."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbEntryReadContext",
            action = "Admit",
            project = "aterm_secrets::client::entry::read_context::tests::transition"
        )
    )]
    pub fn admit_in<T>(
        &self,
        paths: &Paths,
        look: &VaultLook<'_>,
        admit: impl FnOnce(&VerifiedEntry, &EntryReader<'_>) -> T,
    ) -> crate::Result<EntryAdmission<T>> {
        validate_reading(&self.name, Reading::Materialized)?;
        if self.config.signer.is_none() {
            return Ok(EntryAdmission::Unavailable);
        }
        let Some((observation, _)) = look.held_at(paths) else {
            #[cfg(test)]
            if MUTANT.get() == 3
                && let StateObservation::Held(lock) = StateLock::observe_classified(paths)?
            {
                return self.admit_borrowed(paths, &lock, admit);
            }
            return Ok(EntryAdmission::Unavailable);
        };
        // A look's memo may retain its first path observation. Intent admission
        // checks the actual lock path again, without that memo.
        #[cfg(test)]
        let current = MUTANT.get() == 1 || observation.observes(paths);
        #[cfg(not(test))]
        let current = observation.observes(paths);
        if !current {
            return Ok(EntryAdmission::Unavailable);
        }
        self.admit_borrowed(paths, observation, admit)
    }

    fn admit_borrowed<T>(
        &self,
        paths: &Paths,
        observation: &StateLock,
        admit: impl FnOnce(&VerifiedEntry, &EntryReader<'_>) -> T,
    ) -> crate::Result<EntryAdmission<T>> {
        let Some((entry, _)) = read_entry_with_version_held(
            paths,
            &self.config,
            &self.name,
            Reading::Materialized,
            &mut None,
            observation,
        )?
        else {
            return Ok(EntryAdmission::Unavailable);
        };
        if !entry.snapshot.continues(self) {
            return Ok(EntryAdmission::Changed);
        }
        let reader = EntryReader {
            paths,
            observation,
            version: entry.snapshot.version().vault,
        };
        Ok(EntryAdmission::Accepted(admit(&entry, &reader)))
    }
}

#[cfg(test)]
std::thread_local! {
    static MUTANT: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
    static VIEW_MUTANT: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod view_tests;
