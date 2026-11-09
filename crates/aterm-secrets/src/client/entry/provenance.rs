// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Semantic source evidence retained from the entry's actual selection.

use super::*;
use crate::membership_log::{Log, RecordId};
use std::sync::Arc;

#[cfg(test)]
std::thread_local! {
    static MUTANT: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

/// The authenticated source that selected the entry's bytes. A trusted log may
/// also have been consulted for denial while an envelope supplied the value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryOrigin {
    /// An authenticated envelope locker supplied the value.
    Envelope,
    /// The authenticated membership log supplied the selected value.
    Log,
}

#[derive(Clone)]
pub(super) enum Source {
    Envelope,
    Log {
        history: Arc<Log>,
        register: Option<Register>,
    },
}

#[derive(Clone)]
pub(super) struct Register {
    pub(super) audience: Audience,
    pub(super) operation: RecordId,
    pub(super) wrapper: RecordId,
    pub(super) ambiguous: bool,
    pub(super) complete: bool,
}

impl Source {
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbEntryProvenance",
            action = "Observe",
            project = "aterm_secrets::client::entry::provenance::tests::transition"
        )
    )]
    pub(super) fn selected(
        selected: Option<super::super::log_entries::LogSelection>,
        log: Option<&Log>,
        entry: &Entry,
    ) -> crate::Result<Self> {
        let Some(selected) = selected else {
            #[cfg(test)]
            if MUTANT.get() == 1
                && let Some(log) = log
            {
                return Ok(Self::Log {
                    history: Arc::new(log.clone()),
                    register: None,
                });
            }
            return Ok(Self::Envelope);
        };
        let log = log.ok_or_else(|| Error::new("selected entry log is unavailable"))?;
        let key = (entry.audience.clone(), entry.name.clone());
        let register = match (selected.operations.get(&key), selected.puts.get(&key)) {
            (Some(operation), Some(wrapper)) => Some(Register {
                audience: entry.audience.clone(),
                operation: *operation,
                wrapper: *wrapper,
                ambiguous: selected.conflicts.contains(&key),
                complete: selected.complete.contains(&entry.audience),
            }),
            (None, None) => None, // Synthetic reserved descriptors are not text registers.
            _ => return Err(Error::new("selected entry register evidence is incomplete")),
        };
        Ok(Self::Log {
            history: Arc::new(log.clone()),
            register,
        })
    }
}

impl EntrySnapshot {
    /// Which source actually supplied the captured bytes, independently of
    /// other authenticated history retained for policy and denial checks.
    #[must_use]
    pub const fn origin(&self) -> EntryOrigin {
        match self.source {
            Source::Envelope => EntryOrigin::Envelope,
            Source::Log { .. } => EntryOrigin::Log,
        }
    }

    /// The selected semantic text operation, unchanged by a carry or reseal.
    /// This public record identifier contains no credential fingerprint and
    /// establishes no current authority when detached from the observation.
    #[must_use]
    pub fn semantic_operation(&self) -> Option<RecordId> {
        match &self.source {
            Source::Log {
                register: Some(register),
                ..
            } => Some(register.operation),
            _ => None,
        }
    }

    /// The selected encryption wrapper. A different wrapper can carry the
    /// same semantic operation and therefore does not prove a new revision.
    #[must_use]
    pub fn wrapper_operation(&self) -> Option<RecordId> {
        match &self.source {
            Source::Log {
                register: Some(register),
                ..
            } => Some(register.wrapper),
            _ => None,
        }
    }

    /// Whether this observation selects a strictly later semantic write in
    /// the same source, recipient and exact audience as `previous`.
    ///
    /// Carries, unrelated edits, envelope-only observations, lost or unreadable
    /// audience history and conflicting selections cannot prove this. Competing
    /// host audiences are judged by the same causal resolver as the value;
    /// a deterministic tie-break alone cannot settle a conflict. This is revision evidence, not an
    /// automatic reactivation, credential grant or global freshness guarantee.
    #[must_use]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "DbEntryProvenance",
            action = "Case",
            reason = "The environment chooses source delivery, edits, reseals and concurrent histories."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbEntryProvenance",
            action = "Observe",
            project = "aterm_secrets::client::entry::provenance::tests::transition"
        )
    )]
    pub fn supersedes(&self, previous: &Self) -> bool {
        let (
            Source::Log {
                history,
                register: Some(current),
            },
            Source::Log {
                history: before,
                register: Some(old),
            },
        ) = (&self.source, &previous.source)
        else {
            return false;
        };
        let (operation, prior_operation) = (current.operation, old.operation);
        #[cfg(test)]
        let (operation, prior_operation) = if MUTANT.get() == 2 {
            (current.wrapper, old.wrapper)
        } else {
            (operation, prior_operation)
        };
        let causal = history.past_of(&[operation]).holds(&prior_operation);
        #[cfg(test)]
        let causal = causal || MUTANT.get() == 3;
        let complete = current.complete && old.complete;
        #[cfg(test)]
        let complete = complete || MUTANT.get() == 4;
        self.version().not_before(&previous.version())
            && history.network() == before.network()
            && current.audience == old.audience
            && !current.ambiguous
            && !old.ambiguous
            && complete
            && operation != prior_operation
            && causal
    }
}

#[cfg(test)]
mod tests;
