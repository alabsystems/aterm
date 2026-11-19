// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fold of dated evidence ([`Ledger`]): for each subject the newest first reading decides.
//!
//! A word stored once and read back as a fact is the defect this replaces: on 2026-11-13 a
//! stored "signed out" overrode a login's own dated evidence (it served all day) and held Claude
//! Code upgrades for over eight hours. Here nothing is stored but readings, each with the date of
//! its own evidence, and the verdict is always the newest of them.

use std::collections::BTreeMap;

use crate::obs::{CredentialPrint, Obs, Subject, WitnessId};

/// What one [`Ledger::observe`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Observed {
    /// A new witness, newer than every reading of its subject before it: it decides now.
    Decides,
    /// A new witness, recorded, that is no newer than the reading that decides: arrival order
    /// never decides.
    Older,
    /// A witness read before: the same observation, which changes nothing and renews nothing,
    /// whatever date, value or subject it carries this time.
    Reread,
}

/// Dated, witnessed readings of `K` and, for each subject, the newest of them.
///
/// The order is the evidence's own date ([`Obs::at`]), then `K`'s order, then the witness's: at
/// one date, the greater value decides, so a value type's declared order is its precedence at a
/// tie; at one date and value, the greater [`WitnessId`] decides. That last rule is arbitrary but
/// fixed, and since a ledger holds one first reading per witness it makes the order total, so the
/// deciding reading (its witness and source included) is one reading of the set. A witness is
/// recorded once, with the subject, date and value of its first reading; a later reading of it is
/// a re-read ([`Observed::Reread`]). So the verdict is a function of the set of first readings,
/// never of the order they arrived in, of when they were read again, or of another subject's.
#[derive(Clone, Debug)]
pub struct Ledger<K> {
    /// Every witness's first reading.
    readings: BTreeMap<WitnessId, Obs<K>>,
    /// Each subject's deciding witness: the newest of its first readings.
    newest: BTreeMap<Subject, WitnessId>,
}

impl<K> Default for Ledger<K> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K> Ledger<K> {
    /// An empty ledger.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            readings: BTreeMap::new(),
            newest: BTreeMap::new(),
        }
    }

    /// The reading that decides for `subject`: the newest of its first readings.
    #[must_use]
    pub fn newest_of(&self, subject: &Subject) -> Option<&Obs<K>> {
        self.newest
            .get(subject)
            .and_then(|witness| self.readings.get(witness))
    }

    /// Every first reading of `subject`, in witness order.
    pub fn readings_of<'a>(&'a self, subject: &'a Subject) -> impl Iterator<Item = &'a Obs<K>> {
        self.readings
            .values()
            .filter(move |obs| &obs.subject == subject)
    }

    /// The login verdict for the credential print current now: the newest reading of that print.
    /// A reading of any other print, an earlier generation of the same row included, says
    /// nothing here, so a renewal starts with none.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "LoginEvidence",
            action = "Observe",
            project = "aterm_evidence::ledger::conformance::project"
        )
    )]
    #[must_use]
    pub fn verdict(&self, print_now: &CredentialPrint) -> Option<&Obs<K>> {
        self.newest_of(&Subject::Credential(*print_now))
    }
}

impl<K: Ord> Ledger<K> {
    /// Record one reading. A witness read before changes nothing ([`Observed::Reread`]); a new
    /// one is kept, and decides for its subject when it is newer than the reading that decides
    /// now ([`Observed::Decides`]), else it is [`Observed::Older`].
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "LoginEvidence",
            action = "Observe",
            project = "aterm_evidence::ledger::conformance::project"
        )
    )]
    pub fn observe(&mut self, obs: Obs<K>) -> Observed {
        if self.readings.contains_key(&obs.witness) {
            return Observed::Reread;
        }
        let decides = self
            .newest_of(&obs.subject)
            .is_none_or(|newest| newer(&obs, newest));
        if decides {
            self.newest.insert(obs.subject.clone(), obs.witness.clone());
        }
        self.readings.insert(obs.witness.clone(), obs);
        if decides {
            Observed::Decides
        } else {
            Observed::Older
        }
    }
}

/// Whether `a` is newer evidence than `b`: by the date of the evidence, then by value, then by
/// witness, so of two different first readings exactly one is newer.
fn newer<K: Ord>(a: &Obs<K>, b: &Obs<K>) -> bool {
    #[cfg(test)]
    if STICKY_WORD.with(std::cell::Cell::get) && a.value < b.value {
        return false;
    }
    (a.at, &a.value, &a.witness) > (b.at, &b.value, &b.witness)
}

#[cfg(test)]
thread_local! {
    /// The negative control's seam: a greater word that decides is never replaced by newer
    /// evidence of a lesser one, as the stored "signed out" of 2026-11-13 was not.
    static STICKY_WORD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Restore (`true`) or remove the sticky word on this thread, for the conformance's negative
/// control.
#[cfg(test)]
pub(crate) fn restore_sticky_word(on: bool) {
    STICKY_WORD.with(|sticky| sticky.set(on));
}

/// The anchor host of LoginEvidence's environment: the actions that compose the next reading,
/// which no ledger function performs.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LoginEvidence",
        action = "Later",
        reason = "Environment: the next reading is dated later; the caller dates it"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LoginEvidence",
        action = "Struck",
        reason = "Environment: the next reading says struck; the caller reads it"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LoginEvidence",
        action = "Foreign",
        reason = "Environment: the next reading is of another print; the caller attributes it"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "LoginEvidence",
        action = "Again",
        reason = "Environment: the next reading is a witness read before; the caller reads it again"
    )
)]
#[doc(hidden)]
pub fn login_evidence_environment() {}

#[cfg(test)]
#[path = "ledger_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "ledger_conformance_tests.rs"]
mod conformance;
