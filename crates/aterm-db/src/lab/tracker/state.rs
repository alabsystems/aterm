// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE STATE RULE (docs/DESIGN-lab-tracker-2026-11-14.md §4.1, §7 finding 4; package TK-13): a
//! package's state, settled on every read from the records that wrote it.
//!
//! A package's `state` cell is a register like any other, so it takes whatever anyone writes
//! and keeps every concurrent write as a candidate. The guard that judges a write sees no
//! principal and no history and runs only where the write is made (§7, finding 4), so nothing
//! held where a write is made can keep two machines, each making a change its own view allows,
//! from leaving a state neither would have reached. This module judges instead where the state
//! is read, which every machine does alike over the same records:
//!
//! - **A change counts only if the state it saw allows it.** The state a write saw is the one
//!   that stood among the writes of that package's state its record observed. A package's first
//!   state is as it is stated (a birth, an import). From there: `ready` to `running` by a claim
//!   (the record writes the package's `claim` with its state); `running` to `done` on evidence
//!   (the record cites a run or any other piece); any state but `done` to `blocked` or
//!   `dropped` with a reason; `running` or `blocked` back to `ready` (a claim released, a block
//!   lifted). Nothing else: a `done` that saw `ready` does not count, nor one that cites
//!   nothing; `dropped` is left only through a reason; and no write of a state leaves `done`,
//!   a revert of the done included (a wrong done is withdrawn by an approved change, TK-15).
//!   A write that restores the cell's absence (a revert of the package's birth) leaves the
//!   package with no state, and the next state it is given is a first state again.
//! - **A change that does not count changes nothing.** The package reads as it did before it,
//!   and a later write that saw it saw that state.
//! - **Concurrent changes that count settle** as done before dropped before blocked before
//!   running before ready ([`PACKAGE_STATE_FIRST`]).
//! - **The one that lost is shown, not hidden.** Each write of the package's state that no
//!   later write of it saw, and that is not the state that stands, is listed with what its
//!   record says ([`Settled::lost`], [`Settled::refused`]) and is a finding ([`findings`]) until
//!   someone who has seen it states the package's state again.
//! - **Credit for done goes to the record that wrote done** ([`Settled::credit`]): the
//!   principal of the write whose change to `done` stands, never whoever holds the claim, and
//!   never a later write that only said `done` again.
//!
//! Nothing is stored and nothing is repaired: the cell keeps its candidates, the records their
//! history, and every reader computes the same answer from them ([`settle`]). A face that reads
//! the cell alone shows the conflict's first word by the same order, which is the state that
//! stands only where every change counted; a reader of the tracker takes it from here
//! ([`States::over`]).

use super::checks::Finding;
use super::tables::{PACKAGE, PACKAGE_STATE_FIRST};
use crate::database::Change;
use crate::said::Said;
use crate::typed::{
    Table,
    cell::{CellKey, CellValue, Cells, Projection},
    check,
    values::Principal,
};
use astream_wire::sync::Digest;
use aterm_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// The package columns the rule reads.
const STATE: &str = "state";
const CLAIM: &str = "claim";

/// One write of a package's state, as its record made it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Write {
    /// The record that wrote it.
    pub operation: Digest,
    /// The state it wrote; none where it wrote no state of the tracker's (a revert that
    /// restored the cell's absence, a newer build's word): the package then holds no state
    /// this build reads, until a later write gives it one.
    pub state: Option<&'static str>,
    /// Whether its record wrote the package's claim with it: a change made by a claim.
    pub claimed: bool,
    /// What its record says of who made it, why and on what evidence; none for a record that
    /// says nothing.
    pub said: Option<Said>,
    /// The other writes of this package's state its record observed.
    saw: BTreeSet<Digest>,
}

impl Write {
    /// Whether its record gives a reason.
    fn reasoned(&self) -> bool {
        self.said
            .as_ref()
            .and_then(|said| said.reason.as_deref())
            .is_some_and(|reason| !reason.trim().is_empty())
    }

    /// Whether its record cites evidence: a run, or any other piece.
    fn evidenced(&self) -> bool {
        self.said
            .as_ref()
            .is_some_and(|said| !said.evidence.is_empty())
    }

    /// Whether the state `saw` allows this write's change to `to` (§4.1).
    fn allowed(&self, saw: &str, to: &str) -> bool {
        match to {
            "running" => saw == "ready" && self.claimed,
            "done" => saw == "running" && self.evidenced(),
            "blocked" | "dropped" => saw != "done" && self.reasoned(),
            "ready" => matches!(saw, "running" | "blocked"),
            _ => false,
        }
    }
}

/// Every write of a package's state among `records` (each a record's operation, its changes
/// and what it says), by the package's key: what [`settle`] reads. `observes` says whether the
/// first record causally observed the second.
pub fn writes<'a>(
    records: impl IntoIterator<Item = (Digest, &'a [Change], Option<&'a Said>)>,
    observes: impl Fn(&Digest, &Digest) -> bool,
) -> BTreeMap<String, Vec<Write>> {
    let mut found: BTreeMap<String, Vec<Write>> = BTreeMap::new();
    for (operation, changes, said) in records {
        let cells = || {
            changes
                .iter()
                .filter(|change| change.collection == PACKAGE.name)
                .filter_map(|change| Some((CellKey::parse(&change.key).ok()?, change)))
                .filter(|(key, _)| key.elem.is_none())
        };
        // What a change holds: nothing for a restored absence or a value no reader decodes.
        let held = |change: &Change| {
            change
                .value
                .as_ref()
                .and_then(|value| CellValue::decode(value).ok())
                .map(|value| value.x)
        };
        for (key, change) in cells().filter(|(key, _)| key.col == STATE) {
            let word = held(change);
            let write = Write {
                operation,
                state: PACKAGE_STATE_FIRST
                    .iter()
                    .copied()
                    .find(|state| word.as_ref().and_then(Value::as_str) == Some(state)),
                claimed: cells().any(|(other, claim)| {
                    other.pk == key.pk
                        && other.col == CLAIM
                        && held(claim).is_some_and(|by| !by.is_null())
                }),
                said: said.cloned(),
                saw: BTreeSet::new(),
            };
            let of = found.entry(key.pk.to_owned()).or_default();
            // A record states one cell once; a second change to it replaces the first.
            of.retain(|earlier| earlier.operation != operation);
            of.push(write);
        }
    }
    for of in found.values_mut() {
        let operations: Vec<Digest> = of.iter().map(|write| write.operation).collect();
        for write in of.iter_mut() {
            write.saw = operations
                .iter()
                .copied()
                .filter(|earlier| {
                    *earlier != write.operation && observes(&write.operation, earlier)
                })
                .collect();
        }
    }
    found
}

/// A package's state as the rule settles it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settled {
    /// The state that stands; none where the package holds no state this build reads (its
    /// latest writes restored the cell's absence, or wrote a newer build's word).
    pub state: Option<&'static str>,
    /// The writes whose change stands: one, or each of those that made the same change at once.
    pub by: Vec<Write>,
    /// The writes no later write saw that are not what stands (another state, or none), though
    /// the state each saw allowed it: what settling passed over.
    pub lost: Vec<Write>,
    /// The writes no later write saw that the state each saw does not allow: they do not count.
    pub refused: Vec<Write>,
}

impl Settled {
    /// Who is credited with the package's done (§4.1): the principal of each record whose
    /// change to `done` stands. Empty for a package that is not done, and for a record that
    /// says nothing of who made it. It never reads the package's claim.
    pub fn credit(&self) -> Vec<&Principal> {
        self.by
            .iter()
            .filter(|_| self.state == Some("done"))
            .filter_map(|write| write.said.as_ref().map(|said| &said.principal))
            .collect()
    }

    /// One package's writes, settled.
    fn of(mut writes: Vec<Write>) -> Self {
        let held: BTreeSet<Digest> = writes.iter().map(|write| write.operation).collect();
        for write in &mut writes {
            write.saw.retain(|earlier| held.contains(earlier));
        }
        // A record that observed another observed all that one did, and it too: the fewer a
        // write saw, the earlier it is, so this order never judges a write before one it saw.
        writes.sort_by_key(|write| (write.saw.len(), write.operation));
        // The writes whose change counts, and the ones the state they saw does not allow.
        let mut counted: Vec<usize> = Vec::new();
        let mut refused: BTreeSet<Digest> = BTreeSet::new();
        for (at, write) in writes.iter().enumerate() {
            let Some(to) = write.state else {
                // No state of the tracker's: the package holds none from here, and the next
                // write that saw this one states its first.
                counted.push(at);
                continue;
            };
            let seen: Vec<usize> = counted
                .iter()
                .copied()
                .filter(|&earlier| write.saw.contains(&writes[earlier].operation))
                .collect();
            match standing(&writes, &seen).0 {
                // The package's first state is as it is stated.
                None => counted.push(at),
                // Saying the state it saw again changes nothing, and is no one's change.
                Some(saw) if saw == to => {}
                Some(saw) if write.allowed(saw, to) => counted.push(at),
                Some(_) => {
                    refused.insert(write.operation);
                }
            }
        }
        let (state, by) = standing(&writes, &counted);
        // What no later write of the state saw: the cell's candidates.
        let unseen = |write: &&Write| {
            !writes
                .iter()
                .any(|later| later.saw.contains(&write.operation))
        };
        Self {
            state,
            lost: writes
                .iter()
                .filter(unseen)
                .filter(|write| write.state != state && !refused.contains(&write.operation))
                .cloned()
                .collect(),
            refused: writes
                .iter()
                .filter(unseen)
                .filter(|write| refused.contains(&write.operation))
                .cloned()
                .collect(),
            by: by.into_iter().map(|at| writes[at].clone()).collect(),
        }
    }
}

/// The state that stands among the `counted` writes of `writes`, and the writes that made it:
/// of those none of the others saw, the ones whose state settles first.
fn standing(writes: &[Write], counted: &[usize]) -> (Option<&'static str>, Vec<usize>) {
    let latest: Vec<usize> = counted
        .iter()
        .copied()
        .filter(|&at| {
            !counted
                .iter()
                .any(|&other| writes[other].saw.contains(&writes[at].operation))
        })
        .collect();
    let state = PACKAGE_STATE_FIRST
        .iter()
        .copied()
        .find(|state| latest.iter().any(|&at| writes[at].state == Some(state)));
    let by = latest
        .into_iter()
        .filter(|&at| writes[at].state == state)
        .collect();
    (state, by)
}

/// Every package's state as the rule settles it, by the package's key.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct States {
    pub packages: BTreeMap<String, Settled>,
}

impl States {
    /// The state of the package keyed `pk` that stands.
    pub fn state(&self, pk: &str) -> Option<&'static str> {
        self.packages.get(pk).and_then(|settled| settled.state)
    }

    /// Give `packages`, a projection of the package table, each row's settled state as the
    /// value its `state` column shows ([`crate::typed::cell::Row::get`]). The candidates stay
    /// in the row's fields as they were written.
    pub fn over(&self, packages: &mut Projection) {
        for (pk, settled) in &self.packages {
            if let (Some(row), Some(state)) = (packages.rows.get_mut(pk), settled.state) {
                row.claims
                    .insert(STATE.to_owned(), Some(Value::from(state)));
            }
        }
    }

    /// What the rule found: a package with a change settling passed over (`state-outranked`),
    /// and one with a change that does not count (`state-not-allowed`). A finding names the
    /// package, never a state or a writer; [`Settled`] holds those.
    pub fn findings(&self) -> Vec<Finding> {
        let mut found = Vec::new();
        for (pk, settled) in &self.packages {
            for (kind, any) in [
                ("state-outranked", !settled.lost.is_empty()),
                ("state-not-allowed", !settled.refused.is_empty()),
            ] {
                if any {
                    found.push(Finding {
                        kind,
                        collection: PACKAGE.name,
                        pks: vec![pk.clone()],
                    });
                }
            }
        }
        found
    }
}

/// Settle every package's state from the writes of it ([`writes`]).
pub fn settle(writes: BTreeMap<String, Vec<Write>>) -> States {
    States {
        packages: writes
            .into_iter()
            .map(|(pk, of)| (pk, Settled::of(of)))
            .collect(),
    }
}

/// Every finding over `table`'s cells collections ([`super::checks::tracker`]) with the state
/// rule's own ([`States::findings`]). A package whose state the rule settled is no `conflict`
/// of the table layer's: the rule has read every candidate of that cell, and says which lost.
pub fn findings(
    table: &Table,
    cells: &Cells,
    states: &States,
) -> (Vec<Finding>, Vec<check::Finding>) {
    let (mut found, mut generic) = super::checks::tracker(table, cells);
    generic.retain(|finding| {
        !matches!(
            finding,
            check::Finding::Conflict { collection, pk, column }
                if collection == PACKAGE.name && column == STATE && states.state(pk).is_some()
        )
    });
    found.extend(states.findings());
    found.sort_unstable();
    found.dedup();
    (found, generic)
}

/// What a state finding of `kind` means, in a person's words ([`super::checks::sentence`]).
pub(super) fn sentence(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "state-outranked" => {
            "A package's state was changed more than once at the same time; it reads the change \
             that settles first, and the other is kept in its history. State it again to settle it"
        }
        "state-not-allowed" => {
            "A change to a package's state is not one the state it saw allows, so it does not \
             count and the package reads as it did before. State it again to settle it"
        }
        _ => return None,
    })
}

/// A state finding of `kind`'s heading ([`super::checks::heading`]).
pub(super) fn heading(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "state-outranked" => "State changed twice at once",
        "state-not-allowed" => "State change does not count",
        _ => return None,
    })
}

/// One read of the tracker with every package's state settled: the table's view, and the
/// states the rule reads from the same log. The records past a writer's quota project nothing,
/// so they are no write of a state either. The log is this generation's: `lab/tracker` has
/// one, and a roll into a second restates a register with no writer, reason or evidence, so
/// the package that declares one settles over the records the roll keeps (TK-31).
///
/// # Errors
/// As [`crate::att::Handle::view`].
#[cfg(unix)]
pub fn read(handle: &crate::att::Handle) -> crate::Result<(crate::att::View, States)> {
    let (view, _, mut found) = handle.usage_with(|_, log| {
        writes(
            log.eligible_iter().filter_map(|(operation, command)| {
                Some((operation.header.id, command.changes()?, command.said()))
            }),
            |later, earlier| log.observes(later, earlier),
        )
    })?;
    if !view.quota.over.is_empty() {
        for of in found.values_mut() {
            of.retain(|write| !view.quota.over.contains(&write.operation));
        }
        found.retain(|_, of| !of.is_empty());
    }
    let states = settle(found);
    Ok((view, states))
}
